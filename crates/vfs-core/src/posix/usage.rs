use super::vfs::{VfsResult, VirtualFileSystem};
use std::collections::BTreeSet;

pub const DEFAULT_MAX_FILESYSTEM_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_INODE_COUNT: usize = 16_384;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileSystemUsage {
    pub total_bytes: u64,
    pub inode_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}

pub trait RootFilesystemResourceLimits {
    fn max_filesystem_bytes(&self) -> Option<u64>;
    fn max_inode_count(&self) -> Option<usize>;
}

pub fn measure_filesystem_usage<F: VirtualFileSystem>(
    filesystem: &mut F,
) -> VfsResult<FileSystemUsage> {
    let mut visited = BTreeSet::new();
    measure_path_usage(filesystem, "/", &mut visited)
}

fn measure_path_usage<F: VirtualFileSystem>(
    filesystem: &mut F,
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
        let child_usage = measure_path_usage(filesystem, &child_path, visited)?;
        usage.total_bytes = usage.total_bytes.saturating_add(child_usage.total_bytes);
        usage.inode_count = usage.inode_count.saturating_add(child_usage.inode_count);
    }

    Ok(usage)
}
