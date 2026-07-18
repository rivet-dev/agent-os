use crate::posix::{
    FileExtent, MountedFileSystem, VfsError as PosixVfsError, VfsResult as PosixVfsResult,
    VirtualDirEntry, VirtualStat,
};
use agentos_runtime::{BlockingJobError, RuntimeContext};
use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_ENGINE_DEVICE_ID: AtomicU64 = AtomicU64::new(4096);

pub struct MountedEngineFileSystem<F> {
    inner: Arc<F>,
    runtime: RuntimeContext,
    device_id: u64,
}

impl<F> MountedEngineFileSystem<F> {
    pub fn with_runtime_context(inner: F, runtime: RuntimeContext) -> Self {
        Self {
            inner: Arc::new(inner),
            runtime,
            device_id: NEXT_ENGINE_DEVICE_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn run<T>(
        &self,
        reserved_bytes: usize,
        future: impl std::future::Future<Output = crate::engine::VfsResult<T>> + Send + 'static,
    ) -> PosixVfsResult<T>
    where
        T: Send + 'static,
    {
        if agentos_runtime::is_runtime_worker_thread() {
            return Err(PosixVfsError::new(
                "EDEADLK",
                "ERR_AGENTOS_VFS_RUNTIME_WORKER_WAIT: synchronous mounted filesystem calls must run outside an AgentOS Tokio worker",
            ));
        }
        let handle = self.runtime.handle().clone();
        let runtime = self.runtime.clone();
        let cancel = Arc::new(tokio::sync::Notify::new());
        let worker_cancel = Arc::clone(&cancel);
        let result = self.runtime.blocking().run_sync(
            reserved_bytes,
            self.runtime.blocking_job_timeout(),
            move || {
                handle.block_on(async move {
                    tokio::select! {
                        result = future => Some(result),
                        () = runtime.admission_closed() => None,
                        () = worker_cancel.notified() => None,
                    }
                })
            },
        );
        match result {
            Ok(Some(result)) => result.map_err(convert_error),
            Ok(None) => Err(PosixVfsError::new(
                "ECANCELED",
                "ERR_AGENTOS_VFS_CANCELLED: mounted filesystem runtime admission closed",
            )),
            Err(error) => {
                // `run_sync` stops waiting at the configured deadline. Wake the
                // still-admitted worker so the engine future is dropped and the
                // fixed blocking-executor slot cannot remain stranded.
                cancel.notify_one();
                Err(blocking_job_error(error))
            }
        }
    }
}

impl<F> MountedFileSystem for MountedEngineFileSystem<F>
where
    F: crate::engine::VirtualFileSystem + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_file(&mut self, path: &str) -> PosixVfsResult<Vec<u8>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.read_file(&path).await })
    }

    fn read_dir(&mut self, path: &str) -> PosixVfsResult<Vec<String>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.read_dir(&path).await })
    }

    fn read_dir_with_types(&mut self, path: &str) -> PosixVfsResult<Vec<VirtualDirEntry>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.read_dir_with_types(&path).await
        })
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| VirtualDirEntry {
                    name: entry.name,
                    is_directory: entry.kind == crate::engine::InodeType::Directory,
                    is_symbolic_link: entry.kind == crate::engine::InodeType::Symlink,
                })
                .collect()
        })
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len().saturating_add(content.len());
        self.run(reserved_bytes, async move {
            inner.write_file(&path, &content).await
        })
    }

    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        mode: Option<u32>,
    ) -> PosixVfsResult<()> {
        self.write_file(path, content)?;
        if let Some(mode) = mode {
            self.chmod(path, mode)?;
        }
        Ok(())
    }

    fn create_file_exclusive(&mut self, path: &str, content: Vec<u8>) -> PosixVfsResult<()> {
        if self.exists(path) {
            return Err(PosixVfsError::new(
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
    ) -> PosixVfsResult<()> {
        self.create_file_exclusive(path, content)?;
        if let Some(mode) = mode {
            self.chmod(path, mode)?;
        }
        Ok(())
    }

    fn append_file(&mut self, path: &str, content: Vec<u8>) -> PosixVfsResult<u64> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len().saturating_add(content.len());
        self.run(reserved_bytes, async move {
            inner.append(&path, &content).await
        })
    }

    fn create_dir(&mut self, path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.create_dir(&path).await })
    }

    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> PosixVfsResult<()> {
        self.create_dir(path)?;
        if let Some(mode) = mode {
            self.chmod(path, mode)?;
        }
        Ok(())
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.mkdir(&path, recursive).await
        })
    }

    fn mkdir_with_mode(
        &mut self,
        path: &str,
        recursive: bool,
        mode: Option<u32>,
    ) -> PosixVfsResult<()> {
        self.mkdir(path, recursive)?;
        if let Some(mode) = mode {
            self.chmod(path, mode)?;
        }
        Ok(())
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.mknod(&path, mode, rdev).await
        })
    }

    fn exists(&self, path: &str) -> bool {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        match self.run(reserved_bytes, async move { Ok(inner.exists(&path).await) }) {
            Ok(exists) => exists,
            Err(error) => {
                eprintln!("ERR_AGENTOS_VFS_EXISTS: {error}");
                false
            }
        }
    }

    fn stat(&mut self, path: &str) -> PosixVfsResult<VirtualStat> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        let stat = self.run(reserved_bytes, async move { inner.stat(&path).await })?;
        Ok(convert_stat(stat, self.device_id))
    }

    fn remove_file(&mut self, path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(
            reserved_bytes,
            async move { inner.remove_file(&path).await },
        )
    }

    fn remove_dir(&mut self, path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.remove_dir(&path).await })
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let old_path = old_path.to_owned();
        let new_path = new_path.to_owned();
        let reserved_bytes = old_path.len().saturating_add(new_path.len());
        self.run(reserved_bytes, async move {
            inner.rename(&old_path, &new_path).await
        })
    }

    fn realpath(&self, path: &str) -> PosixVfsResult<String> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.realpath(&path).await })
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let target = target.to_owned();
        let link_path = link_path.to_owned();
        let reserved_bytes = target.len().saturating_add(link_path.len());
        self.run(reserved_bytes, async move {
            inner.symlink(&target, &link_path).await
        })
    }

    fn read_link(&self, path: &str) -> PosixVfsResult<String> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.readlink(&path).await })
    }

    fn lstat(&self, path: &str) -> PosixVfsResult<VirtualStat> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        let stat = self.run(reserved_bytes, async move { inner.lstat(&path).await })?;
        Ok(convert_stat(stat, self.device_id))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let old_path = old_path.to_owned();
        let new_path = new_path.to_owned();
        let reserved_bytes = old_path.len().saturating_add(new_path.len());
        self.run(reserved_bytes, async move {
            inner.link(&old_path, &new_path).await
        })
    }

    fn chmod(&mut self, path: &str, mode: u32) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(
            reserved_bytes,
            async move { inner.chmod(&path, mode).await },
        )
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(
            reserved_bytes,
            async move { inner.chown(&path, uid, gid).await },
        )
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            if follow_symlinks {
                inner.chown(&path, uid, gid).await
            } else {
                inner.lchown(&path, uid, gid).await
            }
        })
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> PosixVfsResult<()> {
        self.chown_spec(path, uid, gid, false)
    }

    fn get_xattr(
        &mut self,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> PosixVfsResult<Vec<u8>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let name = name.to_owned();
        let reserved_bytes = path.len().saturating_add(name.len());
        self.run(reserved_bytes, async move {
            inner.get_xattr(&path, &name, follow_symlinks).await
        })
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> PosixVfsResult<Vec<String>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.list_xattrs(&path, follow_symlinks).await
        })
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let name = name.to_owned();
        let reserved_bytes = path
            .len()
            .saturating_add(name.len())
            .saturating_add(value.len());
        self.run(reserved_bytes, async move {
            inner
                .set_xattr(&path, &name, &value, flags, follow_symlinks)
                .await
        })
    }

    fn remove_xattr(
        &mut self,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let name = name.to_owned();
        let reserved_bytes = path.len().saturating_add(name.len());
        self.run(reserved_bytes, async move {
            inner.remove_xattr(&path, &name, follow_symlinks).await
        })
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.utimes(&path, atime_ms, mtime_ms).await
        })
    }

    fn set_atime(&mut self, path: &str, atime_ms: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.set_atime(&path, atime_ms).await
        })
    }

    fn truncate(&mut self, path: &str, length: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.truncate(&path, length).await
        })
    }

    fn sync(&mut self, path: &str) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move { inner.sync(&path).await })
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.allocate(&path, offset, length).await
        })
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.insert_range(&path, offset, length).await
        })
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.collapse_range(&path, offset, length).await
        })
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.zero_range(&path, offset, length, keep_size).await
        })
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.punch_hole(&path, offset, length).await
        })
    }

    fn allocated_ranges(&mut self, path: &str) -> PosixVfsResult<Vec<(u64, u64)>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.allocated_ranges(&path).await
        })
    }

    fn unwritten_ranges(&mut self, path: &str) -> PosixVfsResult<Vec<(u64, u64)>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.unwritten_ranges(&path).await
        })
    }

    fn extent_at(&mut self, path: &str, index: usize) -> PosixVfsResult<Option<FileExtent>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len();
        self.run(reserved_bytes, async move {
            inner.extent_at(&path, index).await
        })
        .map(|extent| {
            extent.map(|extent| FileExtent {
                start: extent.start,
                end: extent.end,
                unwritten: extent.unwritten,
            })
        })
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> PosixVfsResult<Vec<u8>> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len().saturating_add(length);
        self.run(reserved_bytes, async move {
            inner.pread(&path, offset, length).await
        })
    }

    fn pwrite(&mut self, path: &str, content: Vec<u8>, offset: u64) -> PosixVfsResult<()> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let reserved_bytes = path.len().saturating_add(content.len());
        self.run(reserved_bytes, async move {
            inner.pwrite(&path, &content, offset).await
        })
    }
}

fn blocking_job_error(error: BlockingJobError) -> PosixVfsError {
    let code = match error {
        BlockingJobError::ResourceLimit(_) | BlockingJobError::Capacity { .. } => "EAGAIN",
        BlockingJobError::ShuttingDown => "ECANCELED",
        BlockingJobError::TimedOut { .. } => "ETIMEDOUT",
        BlockingJobError::WorkerDropped => "EIO",
    };
    PosixVfsError::new(code, error.to_string())
}

fn convert_error(error: crate::engine::VfsError) -> PosixVfsError {
    PosixVfsError::new(error.code(), error.message().to_owned())
}

fn convert_stat(stat: crate::engine::VirtualStat, device_id: u64) -> VirtualStat {
    VirtualStat {
        mode: stat.mode,
        size: stat.size,
        blocks: stat.blocks,
        dev: device_id,
        rdev: stat.rdev,
        is_directory: stat.is_directory,
        is_symbolic_link: stat.is_symbolic_link,
        atime_ms: timespec_ms(stat.atime),
        atime_nsec: stat.atime.nsec,
        mtime_ms: timespec_ms(stat.mtime),
        mtime_nsec: stat.mtime.nsec,
        ctime_ms: timespec_ms(stat.ctime),
        ctime_nsec: stat.ctime.nsec,
        birthtime_ms: timespec_ms(stat.birthtime),
        ino: stat.ino,
        nlink: stat.nlink,
        uid: stat.uid,
        gid: stat.gid,
    }
}

fn timespec_ms(time: crate::engine::Timespec) -> u64 {
    if time.sec < 0 {
        return 0;
    }
    (time.sec as u64).saturating_mul(1_000) + u64::from(time.nsec / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::engines::{ChunkedFs, ChunkedFsOptions};
    use crate::engine::mem::{InMemoryMetadataStore, MemoryBlockStore};
    use crate::posix::S_IFREG;

    #[test]
    fn mounted_engine_filesystem_bridges_sync_posix_calls() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create test runtime");
        let fs = ChunkedFs::with_options(
            InMemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            ChunkedFsOptions {
                inline_threshold: 2,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );
        let mut mounted = MountedEngineFileSystem::with_runtime_context(fs, runtime.context());

        mounted
            .mkdir("/work/nested", true)
            .expect("create nested dir");
        mounted
            .write_file_with_mode("/work/nested/file.txt", b"hello".to_vec(), Some(0o600))
            .expect("write file");
        assert_eq!(
            mounted
                .pread("/work/nested/file.txt", 1, 3)
                .expect("pread file"),
            b"ell"
        );
        let entries = mounted
            .read_dir_with_types("/work/nested")
            .expect("read typed dir");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
        assert!(!entries[0].is_directory);

        let stat = mounted.stat("/work/nested/file.txt").expect("stat file");
        assert_eq!(stat.mode & 0o777, 0o600);
        assert_eq!(stat.mode & S_IFREG, S_IFREG);
        assert_eq!(stat.size, 5);
        assert_eq!(
            mounted
                .extent_at("/work/nested/file.txt", 0)
                .expect("query first extent"),
            Some(FileExtent {
                start: 0,
                end: 5,
                unwritten: false,
            })
        );
        assert_eq!(
            mounted
                .extent_at("/work/nested/file.txt", 1)
                .expect("query past final extent"),
            None
        );
    }

    #[test]
    fn mounted_engine_filesystem_rejects_waits_on_agentos_runtime_workers() {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("create test runtime");
        let context = runtime.context();
        let task_context = context.clone();
        let task = context
            .spawn(agentos_runtime::TaskClass::Plugin, async move {
                let fs = ChunkedFs::with_options(
                    InMemoryMetadataStore::new(),
                    MemoryBlockStore::new(),
                    ChunkedFsOptions::default(),
                );
                let mut mounted = MountedEngineFileSystem::with_runtime_context(fs, task_context);
                mounted
                    .mkdir("/must-not-block", true)
                    .expect_err("runtime workers must not synchronously wait")
            })
            .expect("spawn worker regression");
        let error = runtime.block_on(task).expect("worker regression join");
        assert_eq!(error.code(), "EDEADLK");
        assert!(error
            .message()
            .contains("ERR_AGENTOS_VFS_RUNTIME_WORKER_WAIT"));
    }
}
