use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::mount_table::{MountOptions, MountTable};
use agent_os_kernel::permissions::{
    filter_env, EnvAccessRequest, FsAccessRequest, PermissionDecision, PermissionedFileSystem,
    Permissions,
};
use agent_os_kernel::vfs::{MemoryFileSystem, VfsResult, VirtualFileSystem};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn filesystem_fixture() -> MemoryFileSystem {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/existing.txt", b"hello".to_vec())
        .expect("seed existing file");
    filesystem
        .mkdir("/existing-dir", false)
        .expect("seed existing directory");
    filesystem
        .write_file("/existing-dir/nested.txt", b"nested".to_vec())
        .expect("seed nested file");
    filesystem
}

fn wrap_filesystem(permissions: Permissions) -> PermissionedFileSystem<MemoryFileSystem> {
    PermissionedFileSystem::new(filesystem_fixture(), "vm-permissions", permissions)
}

fn assert_fs_access_denied<T: Debug>(result: VfsResult<T>) {
    let error = result.expect_err("filesystem operation should be denied");
    assert_eq!(error.code(), "EACCES");
}

struct SwapSymlinkOnReadFile {
    inner: MemoryFileSystem,
    swap_on_read: Arc<AtomicBool>,
}

impl SwapSymlinkOnReadFile {
    fn new(inner: MemoryFileSystem, swap_on_read: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            swap_on_read,
        }
    }

    fn maybe_swap_alias(&mut self) {
        if !self.swap_on_read.swap(false, Ordering::SeqCst) {
            return;
        }

        self.inner
            .remove_file("/allowed/alias.txt")
            .expect("remove original alias");
        self.inner
            .symlink("/private/secret.txt", "/allowed/alias.txt")
            .expect("swap alias target");
    }
}

impl VirtualFileSystem for SwapSymlinkOnReadFile {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.maybe_swap_alias();
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.inner.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(
        &mut self,
        path: &str,
    ) -> VfsResult<Vec<agent_os_kernel::vfs::VirtualDirEntry>> {
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.inner.write_file(path, content)
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.inner.create_file_exclusive(path, content)
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        self.inner.append_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<agent_os_kernel::vfs::VirtualStat> {
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<agent_os_kernel::vfs::VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner.utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }
}

#[test]
fn permission_wrapped_filesystem_denies_write_with_reason() {
    let permissions = Permissions {
        filesystem: Some(Arc::new(|request: &FsAccessRequest| {
            if request.path.starts_with("/tmp") {
                PermissionDecision::allow()
            } else {
                PermissionDecision::deny("tmp-only sandbox")
            }
        })),
        ..Permissions::default()
    };

    let mut filesystem =
        PermissionedFileSystem::new(MemoryFileSystem::new(), "vm-permissions", permissions);

    let error = filesystem
        .write_file("/etc/secret.txt", b"nope".to_vec())
        .expect_err("non-/tmp writes should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error.to_string().contains("tmp-only sandbox"));
}

#[test]
fn permission_wrapped_filesystem_denies_access_by_default() {
    let mut filesystem = wrap_filesystem(Permissions::default());

    assert!(filesystem.inner().exists("/existing.txt"));
    assert_fs_access_denied(filesystem.read_file("/existing.txt"));
    assert_fs_access_denied(filesystem.write_file("/new.txt", b"hello".to_vec()));
    assert_fs_access_denied(filesystem.stat("/existing.txt"));
    assert!(
        !PermissionedFileSystem::exists(&filesystem, "/existing.txt")
            .expect("permissioned exists should fail closed")
    );
    assert_fs_access_denied(filesystem.mkdir("/created-dir", false));
    assert_fs_access_denied(filesystem.read_dir("/"));
    assert_fs_access_denied(filesystem.remove_file("/existing.txt"));
}

#[test]
fn permission_wrapped_filesystem_allows_access_with_explicit_allow_all_callback() {
    let permissions = Permissions {
        filesystem: Some(Arc::new(|_: &FsAccessRequest| PermissionDecision::allow())),
        ..Permissions::default()
    };
    let mut filesystem = wrap_filesystem(permissions);

    assert_eq!(
        filesystem
            .read_file("/existing.txt")
            .expect("read existing file"),
        b"hello".to_vec()
    );
    filesystem
        .write_file("/new.txt", b"world".to_vec())
        .expect("write new file");
    assert!(filesystem
        .exists("/existing.txt")
        .expect("existing file should be visible"));
    assert!(filesystem.stat("/existing.txt").is_ok());
    filesystem
        .mkdir("/created-dir", false)
        .expect("create directory");
    let root_entries = filesystem.read_dir("/").expect("read root directory");
    assert!(root_entries.iter().any(|entry| entry == "existing.txt"));
    assert!(root_entries.iter().any(|entry| entry == "existing-dir"));
    assert!(root_entries.iter().any(|entry| entry == "new.txt"));
    assert!(root_entries.iter().any(|entry| entry == "created-dir"));
    filesystem
        .remove_file("/existing.txt")
        .expect("remove existing file");
    assert!(!filesystem.inner().exists("/existing.txt"));
}

#[test]
fn permission_wrapped_filesystem_resolves_symlinks_before_permission_checks() {
    let mut inner = MemoryFileSystem::new();
    inner.mkdir("/allowed", true).expect("seed allowed dir");
    inner.mkdir("/private", true).expect("seed private dir");
    inner
        .write_file("/private/secret.txt", b"secret".to_vec())
        .expect("seed secret file");
    inner
        .symlink("/private/secret.txt", "/allowed/alias.txt")
        .expect("seed symlink");

    let checked_paths = Arc::new(Mutex::new(Vec::new()));
    let checked_paths_for_permission = Arc::clone(&checked_paths);
    let permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_paths_for_permission
                .lock()
                .expect("permission path lock poisoned")
                .push(request.path.clone());
            if request.path.starts_with("/allowed") {
                PermissionDecision::allow()
            } else {
                PermissionDecision::deny("allowed-only")
            }
        })),
        ..Permissions::default()
    };

    let mut filesystem = PermissionedFileSystem::new(inner, "vm-permissions", permissions);

    let error = filesystem
        .read_file("/allowed/alias.txt")
        .expect_err("symlink read should use resolved target path");
    assert_eq!(error.code(), "EACCES");
    assert_eq!(
        checked_paths
            .lock()
            .expect("permission path lock poisoned")
            .as_slice(),
        [String::from("/private/secret.txt")].as_slice()
    );
}

#[test]
fn permission_wrapped_filesystem_uses_resolved_path_after_permission_check() {
    let mut inner = MemoryFileSystem::new();
    inner.mkdir("/allowed", true).expect("seed allowed dir");
    inner.mkdir("/private", true).expect("seed private dir");
    inner
        .write_file("/allowed/public.txt", b"public".to_vec())
        .expect("seed public file");
    inner
        .write_file("/private/secret.txt", b"secret".to_vec())
        .expect("seed secret file");
    inner
        .symlink("/allowed/public.txt", "/allowed/alias.txt")
        .expect("seed alias");

    let swap_on_read = Arc::new(AtomicBool::new(false));
    let swap_on_read_for_permission = Arc::clone(&swap_on_read);
    let permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            if request.path == "/allowed/public.txt" {
                swap_on_read_for_permission.store(true, Ordering::SeqCst);
                PermissionDecision::allow()
            } else {
                PermissionDecision::deny("allowed-only")
            }
        })),
        ..Permissions::default()
    };

    let mut filesystem = PermissionedFileSystem::new(
        SwapSymlinkOnReadFile::new(inner, swap_on_read),
        "vm-permissions",
        permissions,
    );

    assert_eq!(
        filesystem
            .read_file("/allowed/alias.txt")
            .expect("read should stay pinned to the resolved target"),
        b"public".to_vec()
    );
}

#[test]
fn permission_wrapped_filesystem_link_checks_source_and_destination_permissions() {
    let mut inner = MemoryFileSystem::new();
    inner.mkdir("/allowed", true).expect("seed allowed dir");
    inner.mkdir("/private", true).expect("seed private dir");
    inner
        .write_file("/private/source.txt", b"source".to_vec())
        .expect("seed source file");

    let checked_paths = Arc::new(Mutex::new(Vec::new()));
    let checked_paths_for_permission = Arc::clone(&checked_paths);
    let permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_paths_for_permission
                .lock()
                .expect("permission path lock poisoned")
                .push(request.path.clone());
            PermissionDecision::allow()
        })),
        ..Permissions::default()
    };

    let mut filesystem = PermissionedFileSystem::new(inner, "vm-permissions", permissions);
    filesystem
        .link("/private/source.txt", "/allowed/linked.txt")
        .expect("hardlink should succeed");

    assert_eq!(
        checked_paths
            .lock()
            .expect("permission path lock poisoned")
            .as_slice(),
        [
            String::from("/private/source.txt"),
            String::from("/allowed/linked.txt"),
        ]
        .as_slice()
    );
}

#[test]
fn permission_wrapped_filesystem_exists_fails_closed_on_permission_denied() {
    let permissions = Permissions {
        filesystem: Some(Arc::new(|_: &FsAccessRequest| {
            PermissionDecision::deny("hidden")
        })),
        ..Permissions::default()
    };
    let filesystem = wrap_filesystem(permissions);

    assert!(
        !PermissionedFileSystem::exists(&filesystem, "/existing.txt")
            .expect("permissioned exists should fail closed")
    );
    assert!(!VirtualFileSystem::exists(&filesystem, "/existing.txt"));
}

#[test]
fn filter_env_only_keeps_allowed_keys() {
    let permissions = Permissions {
        environment: Some(Arc::new(|request: &EnvAccessRequest| PermissionDecision {
            allow: request.key != "SECRET_KEY",
            reason: None,
        })),
        ..Permissions::default()
    };

    let env = BTreeMap::from([
        (String::from("HOME"), String::from("/home/user")),
        (String::from("PATH"), String::from("/usr/bin")),
        (String::from("SECRET_KEY"), String::from("hidden")),
    ]);

    let filtered = filter_env("vm-permissions", &env, &permissions);
    assert_eq!(filtered.get("HOME"), Some(&String::from("/home/user")));
    assert_eq!(filtered.get("PATH"), Some(&String::from("/usr/bin")));
    assert!(!filtered.contains_key("SECRET_KEY"));
}

#[test]
fn child_process_permissions_block_spawn() {
    let mut config = KernelVmConfig::new("vm-permissions");
    config.permissions = Permissions {
        child_process: Some(Arc::new(|request| {
            if request.command == "blocked" {
                PermissionDecision::deny("blocked by policy")
            } else {
                PermissionDecision::allow()
            }
        })),
        ..Permissions::allow_all()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("alpha", ["blocked"]))
        .expect("register driver");

    let error = kernel
        .spawn_process("blocked", Vec::new(), SpawnOptions::default())
        .expect_err("spawn should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error.to_string().contains("blocked by policy"));
}

#[test]
fn kernel_vm_config_defaults_to_deny_all_permissions() {
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), KernelVmConfig::new("vm-defaults"));

    let error = kernel
        .write_file("/tmp/denied.txt", b"nope".to_vec())
        .expect_err("default config should deny filesystem writes");
    assert_eq!(error.code(), "EACCES");
}

#[test]
fn kernel_default_spawn_cwd_matches_home_user() {
    let captured_cwd = Arc::new(Mutex::new(None));
    let captured_cwd_for_permission = Arc::clone(&captured_cwd);

    let mut config = KernelVmConfig::new("vm-default-cwd");
    config.permissions = Permissions {
        child_process: Some(Arc::new(move |request| {
            *captured_cwd_for_permission
                .lock()
                .expect("captured cwd lock poisoned") = request.cwd.clone();
            PermissionDecision::allow()
        })),
        ..Permissions::allow_all()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("alpha", ["echo"]))
        .expect("register driver");

    let process = kernel
        .spawn_process("echo", Vec::new(), SpawnOptions::default())
        .expect("spawn should succeed");

    assert_eq!(
        captured_cwd
            .lock()
            .expect("captured cwd lock poisoned")
            .as_deref(),
        Some("/home/user")
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn driver_pid_ownership_is_enforced_across_kernel_operations() {
    let mut config = KernelVmConfig::new("vm-auth");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("alpha", ["alpha-cmd"]))
        .expect("register alpha");
    kernel
        .register_driver(CommandDriver::new("beta", ["beta-cmd"]))
        .expect("register beta");

    let alpha = kernel
        .spawn_process(
            "alpha-cmd",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("alpha")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn alpha");
    let beta = kernel
        .spawn_process(
            "beta-cmd",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("beta")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn beta");

    let error = kernel
        .open_pipe("alpha", beta.pid())
        .expect_err("alpha should not open a pipe for beta");
    assert_eq!(error.code(), "EPERM");
    assert!(error.to_string().contains("does not own PID"));

    let error = kernel
        .kill_process("beta", alpha.pid(), 15)
        .expect_err("beta should not kill alpha");
    assert_eq!(error.code(), "EPERM");

    alpha.finish(0);
    beta.finish(0);
    kernel.wait_and_reap(alpha.pid()).expect("reap alpha");
    kernel.wait_and_reap(beta.pid()).expect("reap beta");
}

#[test]
fn kernel_mounts_require_write_permission_on_the_mount_path() {
    let checked = Arc::new(Mutex::new(Vec::new()));
    let checked_for_permission = Arc::clone(&checked);
    let mut config = KernelVmConfig::new("vm-mount-permissions");
    config.permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_for_permission
                .lock()
                .expect("checked mount paths lock poisoned")
                .push((request.op, request.path.clone()));
            PermissionDecision::deny("mounts disabled")
        })),
        ..Permissions::default()
    };

    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    let error = kernel
        .mount_filesystem(
            "/workspace",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .expect_err("mount should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error.to_string().contains("mounts disabled"));
    assert_eq!(
        checked
            .lock()
            .expect("checked mount paths lock poisoned")
            .as_slice(),
        [(
            agent_os_kernel::permissions::FsOperation::Write,
            String::from("/workspace")
        )]
        .as_slice()
    );
}

#[test]
fn kernel_sensitive_mounts_require_explicit_sensitive_permission() {
    let checked = Arc::new(Mutex::new(Vec::new()));
    let checked_for_permission = Arc::clone(&checked);
    let mut config = KernelVmConfig::new("vm-sensitive-mounts");
    config.permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_for_permission
                .lock()
                .expect("checked mount paths lock poisoned")
                .push((request.op, request.path.clone()));
            match request.op {
                agent_os_kernel::permissions::FsOperation::Write => PermissionDecision::allow(),
                agent_os_kernel::permissions::FsOperation::MountSensitive => {
                    PermissionDecision::deny("sensitive mounts require elevation")
                }
                other => panic!("unexpected filesystem permission probe: {other:?}"),
            }
        })),
        ..Permissions::default()
    };

    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    let error = kernel
        .mount_filesystem("/etc", MemoryFileSystem::new(), MountOptions::new("memory"))
        .expect_err("sensitive mount should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error
        .to_string()
        .contains("sensitive mounts require elevation"));
    assert_eq!(
        checked
            .lock()
            .expect("checked mount paths lock poisoned")
            .as_slice(),
        [
            (
                agent_os_kernel::permissions::FsOperation::Write,
                String::from("/etc"),
            ),
            (
                agent_os_kernel::permissions::FsOperation::MountSensitive,
                String::from("/etc"),
            ),
        ]
        .as_slice()
    );
}

#[test]
fn kernel_unmounts_require_write_permission_on_the_mount_path() {
    let checked = Arc::new(Mutex::new(Vec::new()));
    let checked_for_permission = Arc::clone(&checked);
    let mut config = KernelVmConfig::new("vm-unmount-permissions");
    config.permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_for_permission
                .lock()
                .expect("checked unmount paths lock poisoned")
                .push((request.op, request.path.clone()));
            PermissionDecision::deny("unmounts disabled")
        })),
        ..Permissions::default()
    };

    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    kernel
        .filesystem_mut()
        .inner_mut()
        .inner_mut()
        .mount(
            "/workspace",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .expect("seed mount");

    let error = kernel
        .unmount_filesystem("/workspace")
        .expect_err("unmount should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error.to_string().contains("unmounts disabled"));
    assert_eq!(
        checked
            .lock()
            .expect("checked unmount paths lock poisoned")
            .as_slice(),
        [(
            agent_os_kernel::permissions::FsOperation::Write,
            String::from("/workspace")
        )]
        .as_slice()
    );
}

#[test]
fn kernel_sensitive_unmounts_require_explicit_sensitive_permission() {
    let checked = Arc::new(Mutex::new(Vec::new()));
    let checked_for_permission = Arc::clone(&checked);
    let mut config = KernelVmConfig::new("vm-sensitive-unmounts");
    config.permissions = Permissions {
        filesystem: Some(Arc::new(move |request: &FsAccessRequest| {
            checked_for_permission
                .lock()
                .expect("checked sensitive unmount paths lock poisoned")
                .push((request.op, request.path.clone()));
            match request.op {
                agent_os_kernel::permissions::FsOperation::Write => PermissionDecision::allow(),
                agent_os_kernel::permissions::FsOperation::MountSensitive => {
                    PermissionDecision::deny("sensitive mounts require elevation")
                }
                other => panic!("unexpected filesystem permission probe: {other:?}"),
            }
        })),
        ..Permissions::default()
    };

    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    kernel
        .filesystem_mut()
        .inner_mut()
        .inner_mut()
        .mount("/etc", MemoryFileSystem::new(), MountOptions::new("memory"))
        .expect("seed sensitive mount");

    let error = kernel
        .unmount_filesystem("/etc")
        .expect_err("sensitive unmount should be denied");
    assert_eq!(error.code(), "EACCES");
    assert!(error
        .to_string()
        .contains("sensitive mounts require elevation"));
    assert_eq!(
        checked
            .lock()
            .expect("checked sensitive unmount paths lock poisoned")
            .as_slice(),
        [
            (
                agent_os_kernel::permissions::FsOperation::Write,
                String::from("/etc"),
            ),
            (
                agent_os_kernel::permissions::FsOperation::MountSensitive,
                String::from("/etc"),
            ),
        ]
        .as_slice()
    );
}
