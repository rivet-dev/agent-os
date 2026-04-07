//! Host bridge filesystem and permission plumbing extracted from service.rs.

use crate::google_drive_plugin::GoogleDriveMountPlugin;
use crate::host_dir_plugin::HostDirMountPlugin;
use crate::s3_plugin::S3MountPlugin;
use crate::sandbox_agent_plugin::SandboxAgentMountPlugin;
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, filesystem_permission_capability,
    normalize_path, plugin_error,
};
use crate::state::{BridgeError, SharedBridge, HOST_REALPATH_MAX_SYMLINK_DEPTH};
use crate::{NativeSidecarBridge, SidecarError};

use agent_os_bridge::{
    ChmodRequest, CreateDirRequest, FileKind, FileMetadata, FilesystemAccess, PathRequest,
    ReadDirRequest, ReadFileRequest, RenameRequest, SymlinkRequest, TruncateRequest,
    WriteFileRequest,
};
use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, FileSystemPluginRegistry, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::MountedVirtualFileSystem;
use agent_os_kernel::permissions::{
    CommandAccessRequest, EnvAccessRequest, FsAccessRequest, FsOperation, NetworkAccessRequest,
    PermissionDecision, Permissions,
};
use agent_os_kernel::vfs::{
    MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub(crate) struct HostFilesystem<B> {
    bridge: SharedBridge<B>,
    vm_id: String,
    links: Arc<Mutex<HostFilesystemLinkState>>,
}

#[derive(Debug, Clone, Default)]
struct HostFilesystemMetadataState {
    uid: Option<u32>,
    gid: Option<u32>,
    atime_ms: Option<u64>,
    mtime_ms: Option<u64>,
    ctime_ms: Option<u64>,
    birthtime_ms: Option<u64>,
}

impl HostFilesystemMetadataState {
    fn apply_to_stat(&self, stat: &mut VirtualStat) {
        if let Some(uid) = self.uid {
            stat.uid = uid;
        }
        if let Some(gid) = self.gid {
            stat.gid = gid;
        }
        if let Some(atime_ms) = self.atime_ms {
            stat.atime_ms = atime_ms;
        }
        if let Some(mtime_ms) = self.mtime_ms {
            stat.mtime_ms = mtime_ms;
        }
        if let Some(ctime_ms) = self.ctime_ms {
            stat.ctime_ms = ctime_ms;
        }
        if let Some(birthtime_ms) = self.birthtime_ms {
            stat.birthtime_ms = birthtime_ms;
        }
    }
}

#[derive(Debug, Clone)]
struct HostFilesystemLinkedInode {
    canonical_path: String,
    paths: BTreeSet<String>,
    metadata: HostFilesystemMetadataState,
}

#[derive(Debug, Default)]
struct HostFilesystemLinkState {
    next_ino: u64,
    path_to_ino: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, HostFilesystemLinkedInode>,
}

#[derive(Debug, Clone)]
struct HostFilesystemTrackedIdentity {
    canonical_path: String,
    ino: u64,
    nlink: u64,
    metadata: HostFilesystemMetadataState,
}

impl<B> HostFilesystem<B> {
    pub(crate) fn new(bridge: SharedBridge<B>, vm_id: impl Into<String>) -> Self {
        Self {
            bridge,
            vm_id: vm_id.into(),
            links: Arc::new(Mutex::new(HostFilesystemLinkState {
                next_ino: 1,
                ..HostFilesystemLinkState::default()
            })),
        }
    }

    fn vfs_error(error: SidecarError) -> VfsError {
        VfsError::io(error.to_string())
    }

    fn bridge_path_not_found(op: &'static str, path: &str) -> VfsError {
        VfsError::new("ENOENT", format!("no such file or directory, {op} '{path}'"))
    }

    fn link_state_error() -> VfsError {
        VfsError::io("native sidecar host filesystem link state lock poisoned")
    }

    fn current_time_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn file_metadata_to_stat(
        metadata: FileMetadata,
        identity: Option<&HostFilesystemTrackedIdentity>,
    ) -> VirtualStat {
        let mut stat = VirtualStat {
            mode: metadata.mode,
            size: metadata.size,
            blocks: if metadata.size == 0 {
                0
            } else {
                metadata.size.div_ceil(512)
            },
            dev: 1,
            rdev: 0,
            is_directory: metadata.kind == FileKind::Directory,
            is_symbolic_link: metadata.kind == FileKind::SymbolicLink,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
            ino: identity.map_or(0, |tracked| tracked.ino),
            nlink: identity.map_or(1, |tracked| tracked.nlink),
            uid: 0,
            gid: 0,
        };
        if let Some(identity) = identity {
            identity.metadata.apply_to_stat(&mut stat);
        }
        stat
    }

    fn tracked_identity(&self, path: &str) -> VfsResult<Option<HostFilesystemTrackedIdentity>> {
        let normalized = normalize_path(path);
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        Ok(links.path_to_ino.get(&normalized).and_then(|ino| {
            links
                .inodes
                .get(ino)
                .map(|inode| HostFilesystemTrackedIdentity {
                    canonical_path: inode.canonical_path.clone(),
                    ino: *ino,
                    nlink: inode.paths.len() as u64,
                    metadata: inode.metadata.clone(),
                })
        }))
    }

    fn tracked_identity_for_stat(
        &self,
        path: &str,
    ) -> VfsResult<Option<HostFilesystemTrackedIdentity>>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let normalized = normalize_path(path);
        if let Some(identity) = self.tracked_identity(&normalized)? {
            return Ok(Some(identity));
        }

        let resolved = self.realpath(&normalized)?;
        if resolved == normalized {
            return Ok(None);
        }

        self.tracked_identity(&resolved)
    }

    fn tracked_successor(&self, path: &str) -> VfsResult<Option<String>> {
        let normalized = normalize_path(path);
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        Ok(links
            .path_to_ino
            .get(&normalized)
            .and_then(|ino| links.inodes.get(ino))
            .and_then(|inode| {
                inode
                    .paths
                    .iter()
                    .find(|candidate| **candidate != normalized)
                    .cloned()
            }))
    }

    fn ensure_tracked_path(&self, path: &str) -> VfsResult<u64> {
        let normalized = normalize_path(path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        if let Some(ino) = links.path_to_ino.get(&normalized).copied() {
            return Ok(ino);
        }

        let ino = links.next_ino;
        links.next_ino += 1;
        links.path_to_ino.insert(normalized.clone(), ino);
        links.inodes.insert(
            ino,
            HostFilesystemLinkedInode {
                canonical_path: normalized.clone(),
                paths: BTreeSet::from([normalized]),
                metadata: HostFilesystemMetadataState::default(),
            },
        );
        Ok(ino)
    }

    fn track_link(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let ino = self.ensure_tracked_path(&normalized_old)?;
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        links.path_to_ino.insert(normalized_new.clone(), ino);
        links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist")
            .paths
            .insert(normalized_new);
        Ok(())
    }

    fn metadata_target_path(&self, path: &str) -> VfsResult<String>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        if let Some(identity) = self.tracked_identity(path)? {
            return Ok(identity.canonical_path);
        }

        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.stat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        self.realpath(&normalized)
    }

    fn update_metadata(
        &self,
        path: &str,
        update: impl FnOnce(&mut HostFilesystemMetadataState),
    ) -> VfsResult<()>
    where
        B: NativeSidecarBridge + Send + 'static,
        BridgeError<B>: fmt::Debug + Send + Sync + 'static,
    {
        let target = self.metadata_target_path(path)?;
        let ino = self.ensure_tracked_path(&target)?;
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let inode = links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist");
        update(&mut inode.metadata);
        Ok(())
    }

    fn apply_remove(&self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let Some(ino) = links.path_to_ino.remove(&normalized) else {
            return Ok(());
        };
        let remove_inode = {
            let inode = links
                .inodes
                .get_mut(&ino)
                .expect("tracked inode should exist");
            inode.paths.remove(&normalized);
            if inode.paths.is_empty() {
                true
            } else {
                if inode.canonical_path == normalized {
                    inode.canonical_path = inode
                        .paths
                        .iter()
                        .next()
                        .expect("tracked inode should retain at least one path")
                        .clone();
                }
                false
            }
        };
        if remove_inode {
            links.inodes.remove(&ino);
        }
        Ok(())
    }

    fn apply_rename(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let Some(ino) = links.path_to_ino.remove(&normalized_old) else {
            return Ok(());
        };
        links.path_to_ino.insert(normalized_new.clone(), ino);
        let inode = links
            .inodes
            .get_mut(&ino)
            .expect("tracked inode should exist");
        inode.paths.remove(&normalized_old);
        inode.paths.insert(normalized_new.clone());
        if inode.canonical_path == normalized_old {
            inode.canonical_path = normalized_new;
        }
        Ok(())
    }

    fn apply_rename_prefix(&self, old_prefix: &str, new_prefix: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_prefix);
        let normalized_new = normalize_path(new_prefix);
        let prefix = if normalized_old == "/" {
            String::from("/")
        } else {
            format!("{}/", normalized_old.trim_end_matches('/'))
        };

        let mut links = self.links.lock().map_err(|_| Self::link_state_error())?;
        let affected = links
            .path_to_ino
            .keys()
            .filter(|path| *path == &normalized_old || path.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();

        for old_path in affected {
            let suffix = old_path
                .strip_prefix(&normalized_old)
                .expect("tracked path should match renamed prefix");
            let new_path = if normalized_new == "/" {
                normalize_path(&format!("/{}", suffix.trim_start_matches('/')))
            } else if suffix.is_empty() {
                normalized_new.clone()
            } else {
                normalize_path(&format!(
                    "{}/{}",
                    normalized_new.trim_end_matches('/'),
                    suffix.trim_start_matches('/')
                ))
            };
            let ino = links
                .path_to_ino
                .remove(&old_path)
                .expect("tracked path should exist");
            links.path_to_ino.insert(new_path.clone(), ino);
            let inode = links
                .inodes
                .get_mut(&ino)
                .expect("tracked inode should exist");
            inode.paths.remove(&old_path);
            inode.paths.insert(new_path.clone());
            if inode.canonical_path == old_path {
                inode.canonical_path = new_path;
            }
        }
        Ok(())
    }
}

impl<B> VirtualFileSystem for HostFilesystem<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.read_file(ReadFileRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let normalized = normalize_path(path);
        let mut entries = self
            .bridge
            .with_mut(|bridge| {
                bridge.read_dir(ReadDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        for linked_path in links.path_to_ino.keys() {
            if dirname(linked_path) != normalized {
                continue;
            }
            let name = Path::new(linked_path)
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| linked_path.trim_start_matches('/').to_owned());
            if entries.iter().all(|entry| entry.name != name) {
                entries.push(agent_os_bridge::DirectoryEntry {
                    name,
                    kind: FileKind::File,
                });
            }
        }
        Ok(entries.into_iter().map(|entry| entry.name).collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let normalized = normalize_path(path);
        let mut entries = self
            .bridge
            .with_mut(|bridge| {
                bridge.read_dir(ReadDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        let links = self.links.lock().map_err(|_| Self::link_state_error())?;
        for linked_path in links.path_to_ino.keys() {
            if dirname(linked_path) != normalized {
                continue;
            }
            let name = Path::new(linked_path)
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| linked_path.trim_start_matches('/').to_owned());
            if entries.iter().all(|entry| entry.name != name) {
                entries.push(agent_os_bridge::DirectoryEntry {
                    name,
                    kind: FileKind::File,
                });
            }
        }
        Ok(entries
            .into_iter()
            .map(|entry| VirtualDirEntry {
                name: entry.name,
                is_directory: entry.kind == FileKind::Directory,
                is_symbolic_link: entry.kind == FileKind::SymbolicLink,
            })
            .collect())
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.write_file(WriteFileRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    contents: content.into(),
                })
            })
            .map_err(Self::vfs_error)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.create_dir(CreateDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    recursive: false,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.create_dir(CreateDirRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    recursive,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn exists(&self, path: &str) -> bool {
        if self.tracked_identity(path).ok().flatten().is_some() {
            return true;
        }
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.exists(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let identity = self.tracked_identity_for_stat(path)?;
        let normalized = identity
            .as_ref()
            .map(|identity| identity.canonical_path.clone())
            .unwrap_or_else(|| normalize_path(path));
        let metadata = match self.bridge.with_mut(|bridge| {
            bridge.stat(PathRequest {
                vm_id: self.vm_id.clone(),
                path: normalized.clone(),
            })
        }) {
            Ok(metadata) => metadata,
            Err(error) => {
                if !self.exists(&normalized) {
                    return Err(Self::bridge_path_not_found("stat", &normalized));
                }
                return Err(Self::vfs_error(error));
            }
        };
        Ok(Self::file_metadata_to_stat(metadata, identity.as_ref()))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if let Some(identity) = self.tracked_identity(&normalized)? {
            let canonical = identity.canonical_path;
            let nlink = identity.nlink;
            if canonical == normalized {
                if nlink > 1 {
                    let successor = self
                        .tracked_successor(&normalized)?
                        .expect("tracked inode should retain a successor path");
                    self.bridge
                        .with_mut(|bridge| {
                            bridge.rename(RenameRequest {
                                vm_id: self.vm_id.clone(),
                                from_path: canonical.clone(),
                                to_path: successor,
                            })
                        })
                        .map_err(Self::vfs_error)?;
                } else {
                    self.bridge
                        .with_mut(|bridge| {
                            bridge.remove_file(PathRequest {
                                vm_id: self.vm_id.clone(),
                                path: canonical,
                            })
                        })
                        .map_err(Self::vfs_error)?;
                }
            }
            self.apply_remove(&normalized)?;
            return Ok(());
        }

        self.bridge
            .with_mut(|bridge| {
                bridge.remove_file(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.remove_dir(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        let tracked = self.tracked_identity(&normalized_old)?;
        if let Some(identity) = tracked {
            let canonical = identity.canonical_path;
            if self.exists(&normalized_new) {
                return Err(VfsError::new(
                    "EEXIST",
                    format!("file already exists, rename '{new_path}'"),
                ));
            }
            if canonical == normalized_old {
                self.bridge
                    .with_mut(|bridge| {
                        bridge.rename(RenameRequest {
                            vm_id: self.vm_id.clone(),
                            from_path: canonical,
                            to_path: normalized_new.clone(),
                        })
                    })
                    .map_err(Self::vfs_error)?;
            }
            self.apply_rename(&normalized_old, &normalized_new)?;
            return Ok(());
        }

        let old_kind = self
            .bridge
            .with_mut(|bridge| {
                bridge.lstat(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized_old.clone(),
                })
            })
            .ok()
            .map(|metadata| metadata.kind);
        self.bridge
            .with_mut(|bridge| {
                bridge.rename(RenameRequest {
                    vm_id: self.vm_id.clone(),
                    from_path: normalized_old.clone(),
                    to_path: normalized_new.clone(),
                })
            })
            .map_err(Self::vfs_error)?;
        if old_kind == Some(FileKind::Directory) {
            self.apply_rename_prefix(&normalized_old, &normalized_new)?;
        }
        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let original = normalize_path(path);
        let mut normalized = original.clone();

        for _ in 0..HOST_REALPATH_MAX_SYMLINK_DEPTH {
            match self.lstat(&normalized) {
                Ok(stat) if stat.is_symbolic_link => {
                    let target = self.read_link(&normalized)?;
                    normalized = if target.starts_with('/') {
                        normalize_path(&target)
                    } else {
                        normalize_path(&format!("{}/{}", dirname(&normalized), target))
                    };
                }
                Ok(_) | Err(_) => return Ok(normalized),
            }
        }

        Err(VfsError::new(
            "ELOOP",
            format!("too many levels of symbolic links, '{original}'"),
        ))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.bridge
            .with_mut(|bridge| {
                bridge.symlink(SymlinkRequest {
                    vm_id: self.vm_id.clone(),
                    target_path: normalize_path(target),
                    link_path: normalize_path(link_path),
                })
            })
            .map_err(Self::vfs_error)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.read_link(PathRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let identity = self.tracked_identity(path)?;
        let normalized = identity
            .as_ref()
            .map(|identity| identity.canonical_path.clone())
            .unwrap_or_else(|| normalize_path(path));
        let metadata = match self.bridge.with_mut(|bridge| {
            bridge.lstat(PathRequest {
                vm_id: self.vm_id.clone(),
                path: normalized.clone(),
            })
        }) {
            Ok(metadata) => metadata,
            Err(error) => {
                let exists = self
                    .bridge
                    .with_mut(|bridge| {
                        bridge.exists(PathRequest {
                            vm_id: self.vm_id.clone(),
                            path: normalized.clone(),
                        })
                    })
                    .unwrap_or(false);
                if !exists {
                    return Err(Self::bridge_path_not_found("lstat", &normalized));
                }
                return Err(Self::vfs_error(error));
            }
        };
        Ok(Self::file_metadata_to_stat(metadata, identity.as_ref()))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let normalized_old = normalize_path(old_path);
        let normalized_new = normalize_path(new_path);
        if self.exists(&normalized_new) {
            return Err(VfsError::new(
                "EEXIST",
                format!("file already exists, link '{new_path}'"),
            ));
        }

        let old_stat = self.stat(&normalized_old)?;
        if old_stat.is_directory || old_stat.is_symbolic_link {
            return Err(VfsError::new(
                "EPERM",
                format!("operation not permitted, link '{old_path}'"),
            ));
        }
        let parent = self.lstat(&dirname(&normalized_new))?;
        if !parent.is_directory {
            return Err(VfsError::new(
                "ENOENT",
                format!("no such file or directory, link '{new_path}'"),
            ));
        }

        self.track_link(&normalized_old, &normalized_new)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let normalized = normalize_path(path);
        self.bridge
            .with_mut(|bridge| {
                bridge.chmod(ChmodRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    mode,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let now = Self::current_time_ms();
        self.update_metadata(path, |metadata| {
            metadata.uid = Some(uid);
            metadata.gid = Some(gid);
            metadata.ctime_ms = Some(now);
        })
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let now = Self::current_time_ms();
        self.update_metadata(path, |metadata| {
            metadata.atime_ms = Some(atime_ms);
            metadata.mtime_ms = Some(mtime_ms);
            metadata.ctime_ms = Some(now);
        })
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let normalized = self
            .tracked_identity(path)?
            .map(|identity| identity.canonical_path)
            .unwrap_or_else(|| normalize_path(path));
        self.bridge
            .with_mut(|bridge| {
                bridge.truncate(TruncateRequest {
                    vm_id: self.vm_id.clone(),
                    path: normalized,
                    len: length,
                })
            })
            .map_err(Self::vfs_error)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let bytes = self.read_file(path)?;
        let start = offset as usize;
        if start >= bytes.len() {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(length).min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }
}

#[derive(Clone)]
pub(crate) struct ScopedHostFilesystem<B> {
    inner: HostFilesystem<B>,
    guest_root: String,
}

impl<B> ScopedHostFilesystem<B> {
    pub(crate) fn new(inner: HostFilesystem<B>, guest_root: impl Into<String>) -> Self {
        Self {
            inner,
            guest_root: normalize_path(&guest_root.into()),
        }
    }

    fn scoped_path(&self, path: &str) -> String {
        let normalized = normalize_path(path);
        if self.guest_root == "/" {
            return normalized;
        }
        if normalized == "/" {
            return self.guest_root.clone();
        }
        format!(
            "{}/{}",
            self.guest_root.trim_end_matches('/'),
            normalized.trim_start_matches('/')
        )
    }

    fn scoped_target(&self, target: &str) -> String {
        if target.starts_with('/') {
            self.scoped_path(target)
        } else {
            target.to_owned()
        }
    }

    fn strip_guest_root_prefix<'a>(&self, target: &'a str) -> Option<&'a str> {
        if target == self.guest_root {
            Some("")
        } else {
            target
                .strip_prefix(self.guest_root.as_str())
                .filter(|stripped| stripped.starts_with('/'))
        }
    }

    pub(crate) fn unscoped_target(&self, target: String) -> String {
        if !target.starts_with('/') || self.guest_root == "/" {
            return target;
        }
        match self.strip_guest_root_prefix(&target) {
            Some(stripped) => format!("/{}", stripped.trim_start_matches('/')),
            None => target,
        }
    }
}

impl<B> VirtualFileSystem for ScopedHostFilesystem<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(&self.scoped_path(path))
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(&self.scoped_path(path))
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.read_dir_with_types(&self.scoped_path(path))
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.inner.write_file(&self.scoped_path(path), content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(&self.scoped_path(path))
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(&self.scoped_path(path), recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(&self.scoped_path(path))
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(&self.scoped_path(path))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(&self.scoped_path(path))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(&self.scoped_path(path))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner
            .rename(&self.scoped_path(old_path), &self.scoped_path(new_path))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let resolved = self.inner.realpath(&self.scoped_path(path))?;
        Ok(self.unscoped_target(resolved))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner
            .symlink(&self.scoped_target(target), &self.scoped_path(link_path))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner
            .read_link(&self.scoped_path(path))
            .map(|target| self.unscoped_target(target))
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(&self.scoped_path(path))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner
            .link(&self.scoped_path(old_path), &self.scoped_path(new_path))
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(&self.scoped_path(path), mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(&self.scoped_path(path), uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner
            .utimes(&self.scoped_path(path), atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(&self.scoped_path(path), length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(&self.scoped_path(path), offset, length)
    }
}

#[derive(Clone)]
pub(crate) struct MountPluginContext<B> {
    pub(crate) bridge: SharedBridge<B>,
    pub(crate) vm_id: String,
}

#[derive(Debug)]
struct MemoryMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for MemoryMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "memory"
    }

    fn open(
        &self,
        _request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn agent_os_kernel::mount_table::MountedFileSystem>, PluginError> {
        Ok(Box::new(MountedVirtualFileSystem::new(
            MemoryFileSystem::new(),
        )))
    }
}

#[derive(Debug)]
struct JsBridgeMountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for JsBridgeMountPlugin
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn plugin_id(&self) -> &'static str {
        "js_bridge"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn agent_os_kernel::mount_table::MountedFileSystem>, PluginError> {
        if !matches!(request.config, Value::Null | Value::Object(_)) {
            return Err(PluginError::invalid_input(
                "js_bridge mount config must be an object or null",
            ));
        }

        Ok(Box::new(MountedVirtualFileSystem::new(
            ScopedHostFilesystem::new(
                HostFilesystem::new(request.context.bridge.clone(), &request.context.vm_id),
                request.guest_path,
            ),
        )))
    }
}

pub(crate) fn bridge_permissions<B>(bridge: SharedBridge<B>, vm_id: &str) -> Permissions
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let vm_id = vm_id.to_owned();

    let filesystem_bridge = bridge.clone();
    let filesystem_vm_id = vm_id.clone();
    let network_bridge = bridge.clone();
    let network_vm_id = vm_id.clone();
    let command_bridge = bridge.clone();
    let command_vm_id = vm_id.clone();
    let environment_bridge = bridge;

    Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            let access = match request.op {
                FsOperation::Read => FilesystemAccess::Read,
                FsOperation::Write => FilesystemAccess::Write,
                FsOperation::Mkdir | FsOperation::CreateDir => FilesystemAccess::CreateDir,
                FsOperation::ReadDir => FilesystemAccess::ReadDir,
                FsOperation::Stat | FsOperation::Exists => FilesystemAccess::Stat,
                FsOperation::Remove => FilesystemAccess::Remove,
                FsOperation::Rename => FilesystemAccess::Rename,
                FsOperation::Symlink => FilesystemAccess::Symlink,
                FsOperation::ReadLink => FilesystemAccess::Read,
                FsOperation::Link => FilesystemAccess::Write,
                FsOperation::Chmod => FilesystemAccess::Write,
                FsOperation::Chown => FilesystemAccess::Write,
                FsOperation::Utimes => FilesystemAccess::Write,
                FsOperation::Truncate => FilesystemAccess::Write,
                FsOperation::MountSensitive => FilesystemAccess::Write,
            };
            let policy = if request.op == FsOperation::MountSensitive {
                "fs.mount_sensitive"
            } else {
                filesystem_permission_capability(access)
            };
            let decision = if request.op == FsOperation::MountSensitive {
                filesystem_bridge
                    .static_permission_decision(&filesystem_vm_id, policy, "fs")
                    .unwrap_or_else(PermissionDecision::allow)
            } else {
                filesystem_bridge.filesystem_decision(&filesystem_vm_id, &request.path, access)
            };

            if !decision.allow {
                emit_security_audit_event(
                    &filesystem_bridge,
                    &filesystem_vm_id,
                    "security.permission.denied",
                    audit_fields([
                        (
                            String::from("operation"),
                            filesystem_operation_label(request.op).to_owned(),
                        ),
                        (String::from("path"), request.path.clone()),
                        (String::from("policy"), String::from(policy)),
                        (
                            String::from("reason"),
                            decision
                                .reason
                                .clone()
                                .unwrap_or_else(|| String::from("permission denied")),
                        ),
                    ]),
                );
            }

            decision
        })),
        network: Some(Arc::new(move |request: &NetworkAccessRequest| {
            network_bridge.network_decision(&network_vm_id, request)
        })),
        child_process: Some(Arc::new(move |request: &CommandAccessRequest| {
            command_bridge.command_decision(&command_vm_id, request)
        })),
        environment: Some(Arc::new(move |request: &EnvAccessRequest| {
            environment_bridge.environment_decision(&vm_id, request)
        })),
    }
}

fn filesystem_operation_label(operation: FsOperation) -> &'static str {
    match operation {
        FsOperation::Read => "read",
        FsOperation::Write => "write",
        FsOperation::Mkdir => "mkdir",
        FsOperation::CreateDir => "createDir",
        FsOperation::ReadDir => "readdir",
        FsOperation::Stat => "stat",
        FsOperation::Remove => "rm",
        FsOperation::Rename => "rename",
        FsOperation::Exists => "exists",
        FsOperation::Symlink => "symlink",
        FsOperation::ReadLink => "readlink",
        FsOperation::Link => "link",
        FsOperation::Chmod => "chmod",
        FsOperation::Chown => "chown",
        FsOperation::Utimes => "utimes",
        FsOperation::Truncate => "truncate",
        FsOperation::MountSensitive => "mount",
    }
}

pub(crate) fn build_mount_plugin_registry<B>(
) -> Result<FileSystemPluginRegistry<MountPluginContext<B>>, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let mut registry = FileSystemPluginRegistry::new();
    registry.register(MemoryMountPlugin).map_err(plugin_error)?;
    registry
        .register(HostDirMountPlugin)
        .map_err(plugin_error)?;
    registry
        .register(SandboxAgentMountPlugin)
        .map_err(plugin_error)?;
    registry.register(S3MountPlugin).map_err(plugin_error)?;
    registry
        .register(GoogleDriveMountPlugin)
        .map_err(plugin_error)?;
    registry
        .register(JsBridgeMountPlugin)
        .map_err(plugin_error)?;
    Ok(registry)
}
