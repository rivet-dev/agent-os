use super::vfs::{
    normalize_path, MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem,
    VirtualStat, VirtualUtimeSpec,
};
use base64::Engine;
use std::collections::BTreeSet;

const MAX_SNAPSHOT_DEPTH: usize = 1024;
const OVERLAY_METADATA_ROOT: &str = "/.agentos-overlay";
const OVERLAY_WHITEOUT_DIR: &str = "/.agentos-overlay/whiteouts";
const OVERLAY_OPAQUE_DIR: &str = "/.agentos-overlay/opaque";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    Ephemeral,
    ReadOnly,
}

#[derive(Debug)]
pub struct OverlayFileSystem {
    lowers: Vec<MemoryFileSystem>,
    upper: Option<MemoryFileSystem>,
    writes_locked: bool,
}

#[derive(Debug, Clone, Copy)]
enum OverlayMarkerKind {
    Whiteout,
    Opaque,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct OverlayCopyUpUsage {
    total_bytes: u64,
    inode_count: usize,
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
            writes_locked: false,
        }
    }

    pub fn lock_writes(&mut self) {
        self.writes_locked = true;
    }

    fn normalized(path: &str) -> String {
        normalize_path(path)
    }

    fn parent_path(path: &str) -> String {
        let normalized = Self::normalized(path);
        if normalized == "/" {
            return String::from("/");
        }

        match normalized.rsplit_once('/') {
            Some(("", _)) | None => String::from("/"),
            Some((parent, _)) => String::from(parent),
        }
    }

    fn basename(path: &str) -> String {
        let normalized = Self::normalized(path);
        if normalized == "/" {
            return String::from("/");
        }
        normalized
            .rsplit('/')
            .find(|component| !component.is_empty())
            .unwrap_or("")
            .to_owned()
    }

    fn validate_destination_parent(&mut self, path: &str) -> VfsResult<()> {
        let parent = Self::parent_path(path);
        let resolved_parent = self.resolve_merged_path(&parent, true, 0)?;
        let stat = self.merged_lstat(&resolved_parent)?;
        if !stat.is_directory {
            return Err(Self::not_directory(&parent));
        }
        Ok(())
    }

    fn resolved_destination_path(&self, path: &str) -> VfsResult<String> {
        let parent = Self::parent_path(path);
        let resolved_parent = self.resolve_merged_path(&parent, true, 0)?;
        Ok(Self::join_path(&resolved_parent, &Self::basename(path)))
    }

    fn resolve_merged_path(
        &self,
        path: &str,
        follow_final_symlink: bool,
        depth: usize,
    ) -> VfsResult<String> {
        if depth > MAX_SNAPSHOT_DEPTH {
            return Err(VfsError::new(
                "ELOOP",
                format!("too many symbolic links while resolving '{path}'"),
            ));
        }

        let normalized = Self::normalized(path);
        if normalized == "/" {
            return Ok(normalized);
        }

        let components: Vec<&str> = normalized
            .split('/')
            .filter(|component| !component.is_empty())
            .collect();
        let mut current = String::from("/");

        for (index, component) in components.iter().enumerate() {
            let candidate = Self::join_path(&current, component);
            let is_final = index + 1 == components.len();
            let should_follow = !is_final || follow_final_symlink;

            if should_follow {
                if let Ok(stat) = self.merged_lstat(&candidate) {
                    if stat.is_symbolic_link {
                        let target = self.read_link_inner(&candidate)?;
                        let target_path = if target.starts_with('/') {
                            Self::normalized(&target)
                        } else {
                            Self::normalized(&Self::join_path(
                                &Self::parent_path(&candidate),
                                &target,
                            ))
                        };
                        let remainder = components[index + 1..].join("/");
                        let next_path = if remainder.is_empty() {
                            target_path
                        } else {
                            Self::normalized(&Self::join_path(&target_path, &remainder))
                        };
                        return self.resolve_merged_path(
                            &next_path,
                            follow_final_symlink,
                            depth + 1,
                        );
                    }

                    if !is_final && !stat.is_directory {
                        return Err(Self::not_directory(&candidate));
                    }
                }
            } else if let Ok(stat) = self.merged_lstat(&candidate) {
                if !is_final && !stat.is_directory {
                    return Err(Self::not_directory(&candidate));
                }
            }

            current = candidate;
        }

        Ok(current)
    }

    fn destination_parent_copy_up_paths(&self, path: &str) -> VfsResult<Vec<String>> {
        let parent = Self::parent_path(path);
        let mut paths = Vec::new();
        let mut seen = BTreeSet::new();
        self.collect_destination_parent_copy_up_paths(&parent, &mut paths, &mut seen, 0)?;
        Ok(paths)
    }

    fn collect_destination_parent_copy_up_paths(
        &self,
        parent: &str,
        paths: &mut Vec<String>,
        seen: &mut BTreeSet<String>,
        depth: usize,
    ) -> VfsResult<()> {
        if depth > MAX_SNAPSHOT_DEPTH {
            return Err(VfsError::new(
                "ELOOP",
                format!("too many symbolic links while resolving '{parent}'"),
            ));
        }

        let normalized = Self::normalized(parent);
        if normalized == "/" {
            return Ok(());
        }

        let components: Vec<&str> = normalized
            .split('/')
            .filter(|component| !component.is_empty())
            .collect();
        let mut current = String::from("/");
        for (index, component) in components.iter().enumerate() {
            current = Self::join_path(&current, component);
            let stat = self.merged_lstat(&current)?;

            if stat.is_symbolic_link {
                if !self.has_entry_in_upper(&current) && seen.insert(current.clone()) {
                    paths.push(current.clone());
                }

                let target = self.read_link_inner(&current)?;
                let target_path = if target.starts_with('/') {
                    Self::normalized(&target)
                } else {
                    Self::normalized(&Self::join_path(&Self::parent_path(&current), &target))
                };
                let remainder = components[index + 1..].join("/");
                let next_parent = if remainder.is_empty() {
                    target_path
                } else {
                    Self::normalized(&Self::join_path(&target_path, &remainder))
                };
                return self.collect_destination_parent_copy_up_paths(
                    &next_parent,
                    paths,
                    seen,
                    depth + 1,
                );
            }

            if self.find_lower_by_entry(&current).is_some()
                && !self.has_entry_in_upper(&current)
                && seen.insert(current.clone())
            {
                paths.push(current.clone());
            }
        }

        Ok(())
    }

    fn encode_marker_path(path: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path)
    }

    fn marker_directory(kind: OverlayMarkerKind) -> &'static str {
        match kind {
            OverlayMarkerKind::Whiteout => OVERLAY_WHITEOUT_DIR,
            OverlayMarkerKind::Opaque => OVERLAY_OPAQUE_DIR,
        }
    }

    fn marker_path(kind: OverlayMarkerKind, path: &str) -> String {
        format!(
            "{}/{}",
            Self::marker_directory(kind),
            Self::encode_marker_path(&Self::normalized(path))
        )
    }

    fn is_internal_metadata_path(path: &str) -> bool {
        let normalized = Self::normalized(path);
        normalized == OVERLAY_METADATA_ROOT
            || normalized.starts_with(&(String::from(OVERLAY_METADATA_ROOT) + "/"))
    }

    /// Returns true if `path`, or the location it resolves to through symlinks,
    /// lands in the reserved overlay metadata namespace.
    ///
    /// The lexical [`is_internal_metadata_path`] check alone is bypassable: the
    /// underlying `MemoryFileSystem` follows symlinks, so a guest-created symlink
    /// whose resolved target enters `/.agentos-overlay` (directly, or via a
    /// symlink to an ancestor such as `/`) would slip past a purely lexical guard
    /// and let the guest read or tamper with whiteout/opaque markers (e.g.
    /// resurrecting a deleted lower-layer file). Resolving before the check
    /// closes that hole while leaving ordinary symlinks unaffected.
    fn touches_internal_metadata(&self, path: &str) -> bool {
        if Self::is_internal_metadata_path(path) {
            return true;
        }
        if let Ok(resolved) = self.resolve_merged_path(path, true, 0) {
            if Self::is_internal_metadata_path(&resolved) {
                return true;
            }
        }
        if let Ok(resolved) = self.resolved_destination_path(path) {
            if Self::is_internal_metadata_path(&resolved) {
                return true;
            }
        }
        false
    }

    fn entry_touches_internal_metadata(&self, path: &str) -> bool {
        Self::is_internal_metadata_path(path)
            || self
                .resolved_destination_path(path)
                .is_ok_and(|resolved| Self::is_internal_metadata_path(&resolved))
    }

    fn hidden_root_entry_name() -> &'static str {
        ".agentos-overlay"
    }

    fn should_hide_directory_entry(path: &str, entry: &str) -> bool {
        let normalized = Self::normalized(path);
        normalized == "/" && entry == Self::hidden_root_entry_name()
    }

    fn should_ignore_raw_directory_entry(
        upper: Option<&MemoryFileSystem>,
        path: &str,
        entry: &str,
    ) -> bool {
        if entry == "." || entry == ".." || Self::should_hide_directory_entry(path, entry) {
            return true;
        }

        let entry_path = Self::join_path(path, entry);
        Self::marker_exists_in_upper(upper, OverlayMarkerKind::Whiteout, &entry_path)
    }

    fn check_copy_up_usage_limits(
        usage: &OverlayCopyUpUsage,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        if let Some(limit) = max_bytes {
            if usage.total_bytes > limit {
                return Err(VfsError::new(
                    "ENOSPC",
                    format!(
                        "overlay rename copy-up bytes {} exceed configured limit {}",
                        usage.total_bytes, limit
                    ),
                ));
            }
        }

        if let Some(limit) = max_inodes {
            if usage.inode_count > limit {
                return Err(VfsError::new(
                    "ENOSPC",
                    format!(
                        "overlay rename copy-up inodes {} exceed configured limit {}",
                        usage.inode_count, limit
                    ),
                ));
            }
        }

        Ok(())
    }

    fn add_copy_up_usage(
        usage: &mut OverlayCopyUpUsage,
        bytes: u64,
        inodes: usize,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        usage.total_bytes = usage.total_bytes.saturating_add(bytes);
        usage.inode_count = usage.inode_count.saturating_add(inodes);
        Self::check_copy_up_usage_limits(usage, max_bytes, max_inodes)
    }

    fn remaining_inode_budget(
        usage: &OverlayCopyUpUsage,
        max_inodes: Option<usize>,
    ) -> Option<usize> {
        max_inodes.map(|limit| limit.saturating_sub(usage.inode_count))
    }

    fn copy_up_directory_entries_limited(
        &mut self,
        path: &str,
        max_entries: Option<usize>,
    ) -> VfsResult<Vec<String>> {
        let Some(max_entries) = max_entries else {
            return self.read_dir(path);
        };

        match self.read_dir_limited(path, max_entries) {
            Ok(entries) => Ok(entries),
            Err(error) if error.code() == "ENOMEM" => Err(VfsError::new(
                "ENOSPC",
                format!("overlay rename copy-up directory '{path}' exceeds configured inode limit"),
            )),
            Err(error) => Err(error),
        }
    }

    fn directory_has_visible_entries_limited(&mut self, path: &str) -> VfsResult<bool> {
        match self.read_dir_limited(path, 1) {
            Ok(entries) => Ok(!entries.is_empty()),
            Err(error) if error.code() == "ENOMEM" => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn memory_subtree_usage_limited(
        filesystem: &mut MemoryFileSystem,
        path: &str,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<OverlayCopyUpUsage> {
        let mut usage = OverlayCopyUpUsage::default();
        let mut visited = BTreeSet::new();
        let mut pending = vec![Self::normalized(path)];
        while let Some(current_path) = pending.pop() {
            let stat = filesystem.lstat(&current_path)?;
            if visited.insert((stat.dev, stat.ino)) {
                let bytes = if stat.is_directory && !stat.is_symbolic_link {
                    0
                } else {
                    stat.size
                };
                Self::add_copy_up_usage(&mut usage, bytes, 1, max_bytes, max_inodes)?;
            }

            if stat.is_directory && !stat.is_symbolic_link {
                let remaining = Self::remaining_inode_budget(&usage, max_inodes);
                let children = if let Some(max_entries) = remaining {
                    filesystem.read_dir_limited(&current_path, max_entries)?
                } else {
                    filesystem.read_dir(&current_path)?
                };
                for entry in children.into_iter().rev() {
                    if matches!(entry.as_str(), "." | "..") {
                        continue;
                    }
                    if Self::should_hide_directory_entry(&current_path, &entry) {
                        continue;
                    }
                    pending.push(Self::join_path(&current_path, &entry));
                }
            }
        }

        Ok(usage)
    }

    fn memory_subtree_released_usage(
        filesystem: &mut MemoryFileSystem,
        path: &str,
    ) -> VfsResult<OverlayCopyUpUsage> {
        let mut usage = OverlayCopyUpUsage::default();
        let mut visited = BTreeSet::new();
        let mut pending = vec![Self::normalized(path)];
        while let Some(current_path) = pending.pop() {
            let stat = filesystem.lstat(&current_path)?;
            if visited.insert((stat.dev, stat.ino)) {
                let subtree_links = filesystem.link_count_in_subtree(stat.ino, path) as u64;
                if stat.is_directory || stat.nlink <= subtree_links {
                    let bytes = if stat.is_directory && !stat.is_symbolic_link {
                        0
                    } else {
                        stat.size
                    };
                    Self::add_copy_up_usage(&mut usage, bytes, 1, None, None)?;
                }
            }

            if stat.is_directory && !stat.is_symbolic_link {
                for entry in filesystem.read_dir(&current_path)?.into_iter().rev() {
                    if matches!(entry.as_str(), "." | "..") {
                        continue;
                    }
                    if Self::should_hide_directory_entry(&current_path, &entry) {
                        continue;
                    }
                    pending.push(Self::join_path(&current_path, &entry));
                }
            }
        }

        Ok(usage)
    }

    fn upper_usage_limited(
        &mut self,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<OverlayCopyUpUsage> {
        let Some(upper) = self.upper.as_mut() else {
            return Ok(OverlayCopyUpUsage::default());
        };

        Self::memory_subtree_usage_limited(upper, "/", max_bytes, max_inodes)
    }

    fn upper_subtree_released_usage(&mut self, path: &str) -> VfsResult<OverlayCopyUpUsage> {
        let Some(upper) = self.upper.as_mut() else {
            return Ok(OverlayCopyUpUsage::default());
        };

        if !upper.exists(path) {
            return Ok(OverlayCopyUpUsage::default());
        }

        Self::memory_subtree_released_usage(upper, path)
    }

    fn collect_copy_up_usage_limited(
        &mut self,
        path: &str,
        usage: &mut OverlayCopyUpUsage,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        let mut pending = vec![(Self::normalized(path), 0usize)];
        while let Some((current_path, depth)) = pending.pop() {
            if depth > MAX_SNAPSHOT_DEPTH {
                return Err(VfsError::new(
                    "EINVAL",
                    format!("overlay snapshot depth limit exceeded at '{current_path}'"),
                ));
            }

            let stat = self.merged_lstat(&current_path)?;
            if !self.has_entry_in_upper(&current_path) {
                let bytes = if stat.is_symbolic_link {
                    self.read_link_inner(&current_path)?.len() as u64
                } else if stat.is_directory {
                    0
                } else {
                    stat.size
                };
                Self::add_copy_up_usage(usage, bytes, 1, max_bytes, max_inodes)?;
            }

            if stat.is_directory && !stat.is_symbolic_link {
                let children = self.copy_up_directory_entries_limited(&current_path, max_inodes)?;
                for entry in children.into_iter().rev() {
                    pending.push((Self::join_path(&current_path, &entry), depth + 1));
                }
            }
        }

        Ok(())
    }

    fn collect_single_copy_up_usage_limited(
        &mut self,
        path: &str,
        usage: &mut OverlayCopyUpUsage,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        if self.has_entry_in_upper(path) {
            return Ok(());
        }

        let stat = self.merged_lstat(path)?;
        let bytes = if stat.is_symbolic_link {
            self.read_link_inner(path)?.len() as u64
        } else if stat.is_directory {
            0
        } else {
            stat.size
        };
        Self::add_copy_up_usage(usage, bytes, 1, max_bytes, max_inodes)
    }

    pub fn check_rename_copy_up_limits(
        &mut self,
        old_path: &str,
        new_path: &str,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        let old_normalized = Self::normalized(old_path);
        let new_normalized = Self::normalized(new_path);
        if Self::is_internal_metadata_path(&old_normalized)
            || Self::is_internal_metadata_path(&new_normalized)
        {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == "/" {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == new_normalized {
            return Ok(());
        }

        let source_stat = self.merged_lstat(old_path)?;
        if self.writes_locked {
            self.writable_upper(&old_normalized)?;
        }
        self.validate_destination_parent(&new_normalized)?;
        let resolved_new_normalized = self.resolved_destination_path(&new_normalized)?;

        if old_normalized == resolved_new_normalized {
            return Ok(());
        }

        if source_stat.is_directory
            && resolved_new_normalized.starts_with(&(old_normalized.clone() + "/"))
        {
            return Err(VfsError::new(
                "EINVAL",
                format!(
                    "cannot move '{}' into its own descendant '{}'",
                    old_path, new_path
                ),
            ));
        }

        let destination_parent_copy_up_paths =
            self.destination_parent_copy_up_paths(&new_normalized)?;

        if let Ok(destination_stat) = self.merged_lstat(&resolved_new_normalized) {
            if destination_stat.is_directory
                && !destination_stat.is_symbolic_link
                && self.directory_has_visible_entries_limited(&resolved_new_normalized)?
            {
                return Err(Self::not_empty(&resolved_new_normalized));
            }
        }

        let mut usage = self.upper_usage_limited(None, None)?;
        if self.has_entry_in_upper(&resolved_new_normalized) {
            let destination_usage = self.upper_subtree_released_usage(&resolved_new_normalized)?;
            usage.total_bytes = usage
                .total_bytes
                .saturating_sub(destination_usage.total_bytes);
            usage.inode_count = usage
                .inode_count
                .saturating_sub(destination_usage.inode_count);
        }
        Self::check_copy_up_usage_limits(&usage, max_bytes, max_inodes)?;
        for path in destination_parent_copy_up_paths {
            self.collect_single_copy_up_usage_limited(&path, &mut usage, max_bytes, max_inodes)?;
        }
        self.collect_copy_up_usage_limited(&old_normalized, &mut usage, max_bytes, max_inodes)?;

        Self::check_copy_up_usage_limits(&usage, max_bytes, max_inodes)
    }

    fn marker_exists(&self, kind: OverlayMarkerKind, path: &str) -> bool {
        Self::marker_exists_in_upper(self.upper.as_ref(), kind, path)
    }

    fn marker_exists_in_upper(
        upper: Option<&MemoryFileSystem>,
        kind: OverlayMarkerKind,
        path: &str,
    ) -> bool {
        upper.is_some_and(|filesystem| filesystem.exists(&Self::marker_path(kind, path)))
    }

    fn is_whited_out(&self, path: &str) -> bool {
        self.marker_exists(OverlayMarkerKind::Whiteout, path)
    }

    fn ensure_metadata_directories_in_upper(&mut self, path: &str) -> VfsResult<()> {
        let upper = self.writable_upper(path)?;
        upper.mkdir(OVERLAY_METADATA_ROOT, true)?;
        upper.mkdir(OVERLAY_WHITEOUT_DIR, true)?;
        upper.mkdir(OVERLAY_OPAQUE_DIR, true)?;
        Ok(())
    }

    fn set_marker(&mut self, kind: OverlayMarkerKind, path: &str, present: bool) -> VfsResult<()> {
        let marker_path = Self::marker_path(kind, path);
        if present {
            self.ensure_metadata_directories_in_upper(path)?;
            self.writable_upper(path)?
                .write_file(&marker_path, Self::normalized(path).into_bytes())?;
            return Ok(());
        }

        if self
            .upper
            .as_ref()
            .is_some_and(|upper| upper.exists(&marker_path))
        {
            self.writable_upper(path)?.remove_file(&marker_path)?;
        }
        Ok(())
    }

    fn add_whiteout(&mut self, path: &str) -> VfsResult<()> {
        self.set_marker(OverlayMarkerKind::Whiteout, path, true)
    }

    fn remove_whiteout(&mut self, path: &str) -> VfsResult<()> {
        self.set_marker(OverlayMarkerKind::Whiteout, path, false)
    }

    fn mark_opaque_directory(&mut self, path: &str) -> VfsResult<()> {
        self.set_marker(OverlayMarkerKind::Opaque, path, true)
    }

    fn clear_opaque_directory(&mut self, path: &str) -> VfsResult<()> {
        self.set_marker(OverlayMarkerKind::Opaque, path, false)
    }

    fn clear_path_metadata(&mut self, path: &str) -> VfsResult<()> {
        self.remove_whiteout(path)?;
        self.clear_opaque_directory(path)
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
        if Self::is_internal_metadata_path(path) {
            return Err(Self::entry_not_found(path));
        }
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

    /// `read_link` body without the resolving metadata guard, for use by the
    /// internal symlink-resolution helpers (`resolve_merged_path` and friends).
    /// The public `read_link` wraps this with `touches_internal_metadata`;
    /// resolution must not call back into that wrapper or it would recurse on a
    /// symlink that points at itself's resolution path.
    fn read_link_inner(&self, path: &str) -> VfsResult<String> {
        if Self::is_internal_metadata_path(path) {
            return Err(Self::entry_not_found(path));
        }
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

    fn ensure_ancestor_directories_in_upper(&mut self, path: &str) -> VfsResult<()> {
        if Self::is_internal_metadata_path(path) {
            return Err(VfsError::permission_denied("mkdir", path));
        }
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
        let xattrs = match self.lowers[lower_index].list_xattrs(path, false) {
            Ok(names) => names
                .into_iter()
                .map(|name| {
                    self.lowers[lower_index]
                        .get_xattr(path, &name, false)
                        .map(|value| (name, value))
                })
                .collect::<VfsResult<Vec<_>>>()?,
            Err(error) if error.code() == "EOPNOTSUPP" => Vec::new(),
            Err(error) => return Err(error),
        };

        if stat.is_symbolic_link {
            let target = self.lowers[lower_index].read_link(path)?;
            let upper = self.writable_upper(path)?;
            upper.symlink(&target, path)?;
            for (name, value) in xattrs {
                upper.set_xattr(path, &name, value, 0, false)?;
            }
            return Ok(());
        }

        if stat.is_directory {
            let upper = self.writable_upper(path)?;
            upper.mkdir(path, false)?;
            upper.chmod(path, stat.mode)?;
            upper.chown(path, stat.uid, stat.gid)?;
            for (name, value) in xattrs {
                upper.set_xattr(path, &name, value, 0, false)?;
            }
            self.mark_opaque_directory(path)?;
            return Ok(());
        }

        let data = self.lowers[lower_index].read_file(path)?;
        let upper = self.writable_upper(path)?;
        upper.write_file(path, data)?;
        upper.chmod(path, stat.mode)?;
        upper.chown(path, stat.uid, stat.gid)?;
        for (name, value) in xattrs {
            upper.set_xattr(path, &name, value, 0, false)?;
        }
        Ok(())
    }

    fn materialize_destination_parent_in_upper(&mut self, path: &str) -> VfsResult<()> {
        if self.has_entry_in_upper(path) {
            return Ok(());
        }

        if self
            .merged_lstat(path)
            .is_ok_and(|stat| stat.is_symbolic_link)
        {
            return self.copy_up_path(path);
        }

        self.ensure_ancestor_directories_in_upper(path)?;
        let stat = self.merged_lstat(path)?;
        if !stat.is_directory || stat.is_symbolic_link {
            return Err(Self::not_directory(path));
        }

        let upper = self.writable_upper(path)?;
        upper.create_dir(path)?;
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

            let stat = self.merged_lstat(&current_path)?;

            if stat.is_symbolic_link {
                entries.push(OverlaySnapshotEntry {
                    path: current_path.clone(),
                    stat,
                    kind: OverlaySnapshotKind::Symlink(self.read_link_inner(&current_path)?),
                });
                continue;
            }

            if stat.is_directory {
                entries.push(OverlaySnapshotEntry {
                    path: current_path.clone(),
                    stat,
                    kind: OverlaySnapshotKind::Directory,
                });

                let children = self.read_dir_with_types_inner(&current_path)?;
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
                self.clear_opaque_directory(&entry.path)?;
                self.add_whiteout(&entry.path)?;
            } else {
                self.clear_path_metadata(&entry.path)?;
            }
        }

        Ok(())
    }

    fn directory_has_raw_children(&mut self, path: &str) -> VfsResult<bool> {
        let normalized = Self::normalized(path);
        let mut directory_exists = false;

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(entries) = upper.read_dir(&normalized) {
                directory_exists = true;
                if entries.into_iter().any(|entry| {
                    !Self::should_ignore_raw_directory_entry(Some(&*upper), &normalized, &entry)
                }) {
                    return Ok(true);
                }
            }
        }

        let upper = self.upper.as_ref();
        for lower in self.lowers.iter_mut().rev() {
            if let Ok(entries) = lower.read_dir(&normalized) {
                directory_exists = true;
                if entries.into_iter().any(|entry| {
                    !Self::should_ignore_raw_directory_entry(upper, &normalized, &entry)
                }) {
                    return Ok(true);
                }
            }
        }

        if !directory_exists {
            return Err(Self::directory_not_found(path));
        }

        Ok(false)
    }

    fn read_dir_with_types_inner(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = Vec::<VirtualDirEntry>::new();
        let mut seen = BTreeSet::<String>::new();
        let upper = self.upper.as_ref();
        let include_lowers = !Self::marker_exists_in_upper(upper, OverlayMarkerKind::Opaque, path);

        if include_lowers {
            for lower in self.lowers.iter_mut().rev() {
                if let Ok(lower_entries) = lower.read_dir_with_types(path) {
                    directory_exists = true;
                    for entry in lower_entries {
                        if entry.name == "."
                            || entry.name == ".."
                            || Self::should_hide_directory_entry(path, &entry.name)
                        {
                            continue;
                        }
                        let child_path = if normalized == "/" {
                            format!("/{}", entry.name)
                        } else {
                            format!("{normalized}/{}", entry.name)
                        };
                        if Self::marker_exists_in_upper(
                            upper,
                            OverlayMarkerKind::Whiteout,
                            &child_path,
                        ) || seen.contains(&entry.name)
                        {
                            continue;
                        }
                        seen.insert(entry.name.clone());
                        entries.push(entry);
                    }
                }
            }
        }

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(upper_entries) = upper.read_dir_with_types(path) {
                directory_exists = true;
                for entry in upper_entries {
                    if entry.name == "."
                        || entry.name == ".."
                        || Self::should_hide_directory_entry(path, &entry.name)
                    {
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

    fn marker_paths_in_upper(&mut self, kind: OverlayMarkerKind) -> VfsResult<Vec<String>> {
        let Some(upper) = self.upper.as_mut() else {
            return Ok(Vec::new());
        };

        let marker_dir = Self::marker_directory(kind);
        let entries = match upper.read_dir(marker_dir) {
            Ok(entries) => entries,
            Err(error) if error.code() == "ENOENT" => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };

        let mut marker_paths = Vec::new();
        for entry in entries {
            if entry == "." || entry == ".." {
                continue;
            }

            let marker_file = Self::join_path(marker_dir, &entry);
            let marker_path =
                String::from_utf8(upper.read_file(&marker_file).map_err(|_| {
                    VfsError::io(format!("invalid overlay marker '{marker_file}'"))
                })?)
                .map_err(|_| VfsError::io(format!("invalid overlay marker '{marker_file}'")))?;
            marker_paths.push(Self::normalized(&marker_path));
        }

        Ok(marker_paths)
    }

    fn path_in_subtree(path: &str, root: &str) -> bool {
        path == root || path.starts_with(&(String::from(root) + "/"))
    }

    fn clear_subtree_metadata(&mut self, path: &str) -> VfsResult<()> {
        let normalized = Self::normalized(path);
        for kind in [OverlayMarkerKind::Whiteout, OverlayMarkerKind::Opaque] {
            for marker_path in self.marker_paths_in_upper(kind)? {
                if Self::path_in_subtree(&marker_path, &normalized) {
                    self.set_marker(kind, &marker_path, false)?;
                }
            }
        }
        Ok(())
    }

    fn copy_subtree_metadata(&mut self, old_root: &str, new_root: &str) -> VfsResult<()> {
        let old_normalized = Self::normalized(old_root);
        let new_normalized = Self::normalized(new_root);

        for kind in [OverlayMarkerKind::Whiteout, OverlayMarkerKind::Opaque] {
            for marker_path in self.marker_paths_in_upper(kind)? {
                if Self::path_in_subtree(&marker_path, &old_normalized) {
                    let destination =
                        Self::rebase_path(&marker_path, &old_normalized, &new_normalized);
                    self.set_marker(kind, &destination, true)?;
                }
            }
        }

        Ok(())
    }

    fn stage_snapshot_entries_in_upper(
        &mut self,
        entries: &[OverlaySnapshotEntry],
    ) -> VfsResult<()> {
        for entry in entries {
            match &entry.kind {
                OverlaySnapshotKind::Directory => {
                    if !self.has_entry_in_upper(&entry.path) {
                        self.ensure_ancestor_directories_in_upper(&entry.path)?;
                        self.writable_upper(&entry.path)?.create_dir(&entry.path)?;
                    }
                    self.writable_upper(&entry.path)?
                        .chmod(&entry.path, entry.stat.mode)?;
                    self.writable_upper(&entry.path)?.chown(
                        &entry.path,
                        entry.stat.uid,
                        entry.stat.gid,
                    )?;
                    self.mark_opaque_directory(&entry.path)?;
                }
                OverlaySnapshotKind::File(data) => {
                    if self.has_entry_in_upper(&entry.path) {
                        continue;
                    }
                    self.ensure_ancestor_directories_in_upper(&entry.path)?;
                    self.writable_upper(&entry.path)?
                        .write_file(&entry.path, data.clone())?;
                    self.writable_upper(&entry.path)?
                        .chmod(&entry.path, entry.stat.mode)?;
                    self.writable_upper(&entry.path)?.chown(
                        &entry.path,
                        entry.stat.uid,
                        entry.stat.gid,
                    )?;
                }
                OverlaySnapshotKind::Symlink(target) => {
                    if self.has_entry_in_upper(&entry.path) {
                        continue;
                    }
                    self.ensure_ancestor_directories_in_upper(&entry.path)?;
                    self.writable_upper(&entry.path)?
                        .symlink(target, &entry.path)?;
                }
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
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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
        if self.touches_internal_metadata(path) {
            return Err(Self::directory_not_found(path));
        }
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = BTreeSet::new();
        let upper = self.upper.as_ref();
        let include_lowers = !Self::marker_exists_in_upper(upper, OverlayMarkerKind::Opaque, path);

        if include_lowers {
            for lower in self.lowers.iter_mut().rev() {
                if let Ok(lower_entries) = lower.read_dir(path) {
                    directory_exists = true;
                    for entry in lower_entries {
                        if entry == "."
                            || entry == ".."
                            || Self::should_hide_directory_entry(path, &entry)
                        {
                            continue;
                        }
                        let child_path = if normalized == "/" {
                            format!("/{entry}")
                        } else {
                            format!("{normalized}/{entry}")
                        };
                        if !Self::marker_exists_in_upper(
                            upper,
                            OverlayMarkerKind::Whiteout,
                            &child_path,
                        ) {
                            entries.insert(entry);
                        }
                    }
                }
            }
        }

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(upper_entries) = upper.read_dir(path) {
                directory_exists = true;
                for entry in upper_entries {
                    if entry == "."
                        || entry == ".."
                        || Self::should_hide_directory_entry(path, &entry)
                    {
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
        if self.touches_internal_metadata(path) {
            return Err(Self::directory_not_found(path));
        }
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = BTreeSet::new();
        let upper = self.upper.as_ref();
        let include_lowers = !Self::marker_exists_in_upper(upper, OverlayMarkerKind::Opaque, path);

        if include_lowers {
            for lower in self.lowers.iter_mut().rev() {
                let lower_entries = match lower.read_dir_filtered_limited(
                    path,
                    max_entries.saturating_sub(entries.len()),
                    |entry| {
                        if entry == "."
                            || entry == ".."
                            || Self::should_hide_directory_entry(path, entry)
                        {
                            return false;
                        }
                        let child_path = if normalized == "/" {
                            format!("/{entry}")
                        } else {
                            format!("{normalized}/{entry}")
                        };
                        !Self::marker_exists_in_upper(
                            upper,
                            OverlayMarkerKind::Whiteout,
                            &child_path,
                        ) && !entries.contains(entry)
                    },
                ) {
                    Ok(entries) => entries,
                    Err(error) if error.code() == "ENOENT" || error.code() == "ENOTDIR" => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                directory_exists = true;
                for entry in lower_entries {
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

        if let Some(upper) = self.upper.as_mut() {
            let upper_entries = match upper.read_dir_filtered_limited(
                path,
                max_entries.saturating_sub(entries.len()),
                |entry| {
                    entry != "."
                        && entry != ".."
                        && !Self::should_hide_directory_entry(path, entry)
                        && !entries.contains(entry)
                },
            ) {
                Ok(entries) => entries,
                Err(error) if error.code() == "ENOENT" => Vec::new(),
                Err(error) => return Err(error),
            };
            directory_exists = directory_exists || upper.exists(path);
            for entry in upper_entries {
                if entry == "." || entry == ".." || Self::should_hide_directory_entry(path, &entry)
                {
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

        if !directory_exists {
            return Err(Self::directory_not_found(path));
        }

        Ok(entries.into_iter().collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        if self.touches_internal_metadata(path) {
            return Err(Self::directory_not_found(path));
        }
        self.read_dir_with_types_inner(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("open", path));
        }
        self.clear_path_metadata(path)?;
        if self.find_lower_by_entry(path).is_some() {
            self.copy_up_path(path)?;
        } else {
            self.ensure_ancestor_directories_in_upper(path)?;
        }
        self.writable_upper(path)?.write_file(path, content.into())
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("open", path));
        }
        self.clear_path_metadata(path)?;
        if self.path_exists_in_merged_view(path) {
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?
            .create_file_exclusive(path, content.into())
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("open", path));
        }
        self.clear_path_metadata(path)?;
        if self.find_lower_by_entry(path).is_some() {
            self.copy_up_path(path)?;
        } else {
            self.ensure_ancestor_directories_in_upper(path)?;
        }
        self.writable_upper(path)?.append_file(path, content.into())
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("mkdir", path));
        }
        self.clear_path_metadata(path)?;
        if self.path_exists_in_merged_view(path) {
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("mkdir", path));
        }
        self.clear_path_metadata(path)?;
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

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("mknod", path));
        }
        self.clear_path_metadata(path)?;
        if self.path_exists_in_merged_view(path) {
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?.mknod(path, mode, rdev)
    }

    fn exists(&self, path: &str) -> bool {
        if self.touches_internal_metadata(path) {
            return false;
        }
        self.path_exists_in_merged_view(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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
        if self.entry_touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("unlink", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        // POSIX unlink(2) removes the directory entry itself and never follows
        // a symlink leaf, so existence must be checked with lstat semantics.
        // `exists()` resolves symlinks, which made dangling symlinks (for
        // example git's `.git/tXXXXXX` symlink-support probe) unremovable:
        // the entry stayed listed by readdir while unlink reported ENOENT.
        let lower_exists = self.find_lower_by_entry(path).is_some();
        let upper_exists = self.has_entry_in_upper(path);
        if !lower_exists && !upper_exists {
            return Err(Self::entry_not_found(path));
        }
        if upper_exists {
            self.writable_upper(path)?.remove_file(path)?;
        } else {
            self.writable_upper(path)?;
        }
        self.clear_opaque_directory(path)?;
        self.add_whiteout(path)?;
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = Self::normalized(path);
        if self.touches_internal_metadata(&normalized) {
            return Err(VfsError::permission_denied("rmdir", path));
        }
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

        if self.directory_has_raw_children(path)? {
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
            self.clear_opaque_directory(path)?;
            self.add_whiteout(path)?;
        } else {
            self.clear_path_metadata(path)?;
        }
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_normalized = Self::normalized(old_path);
        let new_normalized = Self::normalized(new_path);
        if self.touches_internal_metadata(&old_normalized)
            || self.touches_internal_metadata(&new_normalized)
        {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == "/" {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == new_normalized {
            return Ok(());
        }

        let source_stat = self.merged_lstat(old_path)?;
        self.validate_destination_parent(&new_normalized)?;
        let resolved_new_normalized = self.resolved_destination_path(&new_normalized)?;

        if old_normalized == resolved_new_normalized {
            return Ok(());
        }

        if source_stat.is_directory
            && resolved_new_normalized.starts_with(&(old_normalized.clone() + "/"))
        {
            return Err(VfsError::new(
                "EINVAL",
                format!(
                    "cannot move '{}' into its own descendant '{}'",
                    old_path, new_path
                ),
            ));
        }

        for path in self.destination_parent_copy_up_paths(&new_normalized)? {
            self.materialize_destination_parent_in_upper(&path)?;
        }

        let mut snapshot_entries = Vec::new();
        self.collect_snapshot_entries(&old_normalized, &mut snapshot_entries)?;

        self.clear_path_metadata(&resolved_new_normalized)?;
        self.clear_subtree_metadata(&resolved_new_normalized)?;
        if let Ok(destination_stat) = self.merged_lstat(&resolved_new_normalized) {
            if destination_stat.is_directory
                && !destination_stat.is_symbolic_link
                && self.directory_has_visible_entries_limited(&resolved_new_normalized)?
            {
                return Err(Self::not_empty(&resolved_new_normalized));
            }

            if self.has_entry_in_upper(&resolved_new_normalized) {
                if destination_stat.is_directory && !destination_stat.is_symbolic_link {
                    self.writable_upper(&resolved_new_normalized)?
                        .remove_dir(&resolved_new_normalized)?;
                } else {
                    self.writable_upper(&resolved_new_normalized)?
                        .remove_file(&resolved_new_normalized)?;
                }
            }
        }

        self.stage_snapshot_entries_in_upper(&snapshot_entries)?;
        self.copy_subtree_metadata(&old_normalized, &resolved_new_normalized)?;
        self.writable_upper(&old_normalized)?
            .rename(&old_normalized, &resolved_new_normalized)?;
        self.remove_snapshot_entries(&snapshot_entries)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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
        if self.touches_internal_metadata(link_path) {
            return Err(VfsError::permission_denied("symlink", link_path));
        }
        self.clear_path_metadata(link_path)?;
        self.ensure_ancestor_directories_in_upper(link_path)?;
        self.writable_upper(link_path)?.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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
        if self.touches_internal_metadata(old_path) || self.touches_internal_metadata(new_path) {
            return Err(VfsError::permission_denied("link", new_path));
        }
        self.clear_path_metadata(new_path)?;
        self.copy_up_path(old_path)?;
        self.ensure_ancestor_directories_in_upper(new_path)?;
        self.writable_upper(new_path)?.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("chmod", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.chmod(path, mode)
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
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("chown", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .chown_spec(path, uid, gid, follow_symlinks)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("lchown", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.has_entry_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.lchown(path, uid, gid)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        if self.touches_internal_metadata(path) || self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        let upper_exists = if follow_symlinks {
            self.exists_in_upper(path)
        } else {
            self.has_entry_in_upper(path)
        };
        if upper_exists {
            return self
                .upper
                .as_mut()
                .expect("upper must exist when path exists")
                .get_xattr(path, name, follow_symlinks);
        }
        let lower_index = if follow_symlinks {
            self.find_lower_by_exists(path)
        } else {
            self.find_lower_by_entry(path).map(|(index, _)| index)
        }
        .ok_or_else(|| Self::entry_not_found(path))?;
        self.lowers[lower_index].get_xattr(path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        if self.touches_internal_metadata(path) || self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        let upper_exists = if follow_symlinks {
            self.exists_in_upper(path)
        } else {
            self.has_entry_in_upper(path)
        };
        if upper_exists {
            return self
                .upper
                .as_mut()
                .expect("upper must exist when path exists")
                .list_xattrs(path, follow_symlinks);
        }
        let lower_index = if follow_symlinks {
            self.find_lower_by_exists(path)
        } else {
            self.find_lower_by_entry(path).map(|(index, _)| index)
        }
        .ok_or_else(|| Self::entry_not_found(path))?;
        self.lowers[lower_index].list_xattrs(path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("setxattr", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        let upper_exists = if follow_symlinks {
            self.exists_in_upper(path)
        } else {
            self.has_entry_in_upper(path)
        };
        if !upper_exists {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .set_xattr(path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("removexattr", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        let upper_exists = if follow_symlinks {
            self.exists_in_upper(path)
        } else {
            self.has_entry_in_upper(path)
        };
        if !upper_exists {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .remove_xattr(path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("utime", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.utimes(path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("utime", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .utimes_spec(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("truncate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.truncate(path, length)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("fallocate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.allocate(path, offset, length)
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("fallocate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .insert_range(path, offset, length)
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("fallocate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .collapse_range(path, offset, length)
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("fallocate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .zero_range(path, offset, length, keep_size)
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("fallocate", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.punch_hole(path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        if self.touches_internal_metadata(path) || self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            self.writable_upper(path)?.allocated_ranges(path)
        } else {
            let Some(index) = self.find_lower_by_exists(path) else {
                return Err(Self::entry_not_found(path));
            };
            self.lowers[index].allocated_ranges(path)
        }
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        if self.touches_internal_metadata(path) || self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            self.writable_upper(path)?.unwritten_ranges(path)
        } else {
            let Some(index) = self.find_lower_by_exists(path) else {
                return Err(Self::entry_not_found(path));
            };
            self.lowers[index].unwritten_ranges(path)
        }
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        if self.touches_internal_metadata(path) {
            return Err(Self::entry_not_found(path));
        }
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

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        if self.touches_internal_metadata(path) {
            return Err(VfsError::permission_denied("pwrite", path));
        }
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?
            .pwrite(path, content.into(), offset)
    }
}

#[cfg(test)]
mod tests {
    use super::{OverlayFileSystem, OverlayMode};
    use crate::posix::vfs::{MemoryFileSystem, VfsResult, VirtualFileSystem};

    #[test]
    fn symlink_into_metadata_namespace_cannot_read_or_resurrect_whiteouts() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/data", true).expect("create lower directory");
        lower
            .write_file("/data/secret.txt", b"secret".to_vec())
            .expect("seed lower file");

        let mut overlay = OverlayFileSystem::with_upper(vec![lower], MemoryFileSystem::new());

        // Delete a lower-layer file: a whiteout marker is written under the
        // reserved metadata root and the file disappears from the merged view.
        overlay
            .remove_file("/data/secret.txt")
            .expect("whiteout lower file");
        assert!(!overlay.exists("/data/secret.txt"));

        // A guest symlink whose target is the metadata root must not become a
        // window into the reserved namespace.
        overlay
            .symlink("/.agentos-overlay/whiteouts", "/escape")
            .expect("creating the symlink itself is allowed");

        // Listing through the symlink must be denied, not disclose markers.
        assert!(
            overlay.read_dir("/escape").is_err(),
            "listing the metadata namespace via a symlink must be denied"
        );

        // Removing the whiteout marker through the symlink must be denied, so the
        // deleted lower-layer file cannot be resurrected.
        assert!(
            overlay.remove_file("/escape/anything").is_err(),
            "tampering with metadata via a symlink must be denied"
        );
        assert!(
            !overlay.exists("/data/secret.txt"),
            "deleted lower-layer file must stay deleted"
        );

        // The same bypass via a symlink to an ancestor (e.g. `/`) is also closed.
        overlay
            .symlink("/", "/rootlink")
            .expect("symlink to root is allowed");
        assert!(
            overlay
                .read_dir("/rootlink/.agentos-overlay/whiteouts")
                .is_err(),
            "metadata must be unreachable via an ancestor symlink too"
        );
    }

    #[test]
    fn whiteouts_persist_when_overlay_reopens_with_same_upper() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/data", true).expect("create lower directory");
        lower
            .write_file("/data/base.txt", b"base".to_vec())
            .expect("seed lower file");
        let lower_snapshot = lower.snapshot();

        let mut overlay = OverlayFileSystem::with_upper(
            vec![MemoryFileSystem::from_snapshot(lower_snapshot.clone())],
            MemoryFileSystem::new(),
        );
        overlay
            .remove_file("/data/base.txt")
            .expect("whiteout lower file");

        let upper = overlay.upper.take().expect("overlay upper");
        let restored_lower = MemoryFileSystem::from_snapshot(lower_snapshot);
        let mut restored = OverlayFileSystem::with_upper(vec![restored_lower], upper);

        assert!(!restored.exists("/data/base.txt"));
        assert_eq!(
            restored.read_dir("/data").expect("read merged directory"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn remove_file_does_not_follow_a_self_referential_symlink() {
        let mut overlay = OverlayFileSystem::new(Vec::new(), OverlayMode::Ephemeral);
        overlay
            .symlink("self", "/self")
            .expect("create self-referential symlink");

        overlay
            .remove_file("/self")
            .expect("unlink the symlink entry without following it");

        assert_error_code(overlay.lstat("/self"), "ENOENT");
    }

    #[test]
    fn copied_up_directories_become_opaque_and_hide_overlay_metadata() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/data", true).expect("create lower directory");
        lower
            .write_file("/data/base.txt", b"base".to_vec())
            .expect("seed lower file");

        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        overlay
            .chmod("/data", 0o700)
            .expect("copy up lower directory");

        assert_eq!(
            overlay.read_dir("/data").expect("read opaque directory"),
            Vec::<String>::new()
        );
        let root_entries = overlay.read_dir("/").expect("read root");
        assert!(!root_entries.iter().any(|entry| entry == ".agentos-overlay"));
    }

    #[test]
    fn remove_dir_succeeds_when_only_lower_children_are_whited_out() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/a", true).expect("create lower directory");
        lower
            .write_file("/a/c", b"child".to_vec())
            .expect("seed lower child");

        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        overlay.remove_file("/a/c").expect("whiteout lower child");
        overlay
            .remove_dir("/a")
            .expect("remove merged-empty directory");

        assert!(!overlay.exists("/a"));
        assert_error_code(overlay.read_dir("/a"), "ENOENT");
    }

    #[test]
    fn rename_clears_destination_whiteout() {
        let lower = MemoryFileSystem::new();
        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        overlay
            .write_file("/archive.zip", b"old".to_vec())
            .expect("create old archive");
        overlay
            .remove_file("/archive.zip")
            .expect("whiteout old archive");
        overlay
            .write_file("/workspace-temp", b"new".to_vec())
            .expect("create replacement");

        overlay
            .rename("/workspace-temp", "/archive.zip")
            .expect("rename replacement over whiteout");

        assert_eq!(
            overlay
                .read_file("/archive.zip")
                .expect("read renamed file"),
            b"new"
        );
        assert!(!overlay.exists("/workspace-temp"));
    }

    #[test]
    fn remove_file_unlinks_dangling_symlink() {
        // git's symlink-support probe creates `.git/tXXXXXX -> testing` (a
        // dangling symlink), lstats it, and unlinks it. unlink(2) must remove
        // the link itself without resolving it.
        let lower = MemoryFileSystem::new();
        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        overlay.mkdir("/repo", true).expect("create directory");
        overlay
            .symlink("testing", "/repo/probe")
            .expect("create dangling symlink");
        assert!(
            overlay
                .lstat("/repo/probe")
                .expect("lstat dangling symlink")
                .is_symbolic_link
        );

        overlay
            .remove_file("/repo/probe")
            .expect("unlink dangling symlink");

        assert!(overlay.lstat("/repo/probe").is_err());
        assert_eq!(
            overlay.read_dir("/repo").expect("read emptied directory"),
            Vec::<String>::new()
        );
        overlay
            .remove_dir("/repo")
            .expect("rmdir emptied directory");
    }

    #[test]
    fn remove_file_unlinks_dangling_symlink_from_lower_layer() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/repo", true).expect("create lower directory");
        lower
            .symlink("missing-target", "/repo/probe")
            .expect("seed lower dangling symlink");

        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        overlay
            .remove_file("/repo/probe")
            .expect("whiteout lower dangling symlink");

        assert!(overlay.lstat("/repo/probe").is_err());
        overlay
            .remove_dir("/repo")
            .expect("rmdir merged-empty directory");
    }

    #[test]
    fn remove_dir_still_rejects_visible_children() {
        let mut lower = MemoryFileSystem::new();
        lower.mkdir("/a", true).expect("create lower directory");
        lower
            .write_file("/a/c", b"child".to_vec())
            .expect("seed lower child");

        let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
        assert_error_code(overlay.remove_dir("/a"), "ENOTEMPTY");
        assert!(overlay.exists("/a/c"));
    }

    fn assert_error_code<T: std::fmt::Debug>(result: VfsResult<T>, expected: &str) {
        let error = result.expect_err("expected operation to fail");
        assert_eq!(error.code(), expected);
    }
}
