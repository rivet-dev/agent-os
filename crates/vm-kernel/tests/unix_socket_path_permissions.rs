use agentos_vm_kernel::command_registry::CommandDriver;
use agentos_vm_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agentos_vm_kernel::permissions::Permissions;
use agentos_vm_kernel::resource_accounting::{measure_filesystem_usage, ResourceLimits};
use agentos_vm_kernel::user::UserConfig;
use agentos_vm_kernel::vfs::{
    MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    MAX_PATH_LENGTH,
};

const DRIVER: &str = "shell";

fn configured_user(euid: u32, egid: u32, supplementary_gids: Vec<u32>) -> UserConfig {
    UserConfig {
        uid: Some(euid),
        gid: Some(egid),
        euid: Some(euid),
        egid: Some(egid),
        supplementary_gids,
        ..UserConfig::default()
    }
}

fn kernel_with_filesystem(
    filesystem: MemoryFileSystem,
    user: UserConfig,
) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-unix-socket-path-permissions");
    config.permissions = Permissions::allow_all();
    config.user = user;
    let mut kernel = KernelVm::new(filesystem, config);
    kernel
        .register_driver(CommandDriver::new(DRIVER, ["sh"]))
        .expect("register shell driver");
    kernel
}

fn spawn_shell(kernel: &mut KernelVm<MemoryFileSystem>) -> u32 {
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from(DRIVER)),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
        .pid()
}

fn mkdir_with_metadata(
    filesystem: &mut MemoryFileSystem,
    path: &str,
    mode: u32,
    uid: u32,
    gid: u32,
) {
    filesystem
        .mkdir(path, true)
        .expect("create fixture directory");
    filesystem
        .chmod(path, mode)
        .expect("chmod fixture directory");
    filesystem
        .chown(path, uid, gid)
        .expect("chown fixture directory");
}

struct MetadataFailureFileSystem {
    inner: MemoryFileSystem,
    fail_chown_path: String,
    fail_chown_once: bool,
}

impl MetadataFailureFileSystem {
    fn new(inner: MemoryFileSystem, fail_chown_path: impl Into<String>) -> Self {
        Self {
            inner,
            fail_chown_path: fail_chown_path.into(),
            fail_chown_once: true,
        }
    }
}

impl VirtualFileSystem for MetadataFailureFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.inner.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
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

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
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

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        if path == self.fail_chown_path && self.fail_chown_once {
            self.fail_chown_once = false;
            return Err(VfsError::new(
                "EIO",
                format!("injected chown failure for '{path}'"),
            ));
        }
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
fn unix_socket_bind_applies_umask_effective_owner_and_setgid_parent_group() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/setgid", 0o2770, 99, 900);
    mkdir_with_metadata(&mut filesystem, "/effective-gid", 0o700, 700, 999);
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(700, 701, vec![900]));
    let pid = spawn_shell(&mut kernel);
    kernel
        .umask(DRIVER, pid, Some(0o027))
        .expect("set process umask");

    let inherited = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/setgid/inherited.sock")
        .expect("bind socket under setgid parent");
    assert_eq!(inherited.canonical_path, "/setgid/inherited.sock");
    assert_eq!(inherited.stat.mode, 0o140750);
    assert_eq!(inherited.stat.uid, 700);
    assert_eq!(inherited.stat.gid, 900);
    assert_ne!(inherited.stat.ino, 0);

    let effective = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/effective-gid/effective.sock")
        .expect("bind socket under ordinary parent");
    assert_eq!(effective.stat.mode, 0o140750);
    assert_eq!(effective.stat.uid, 700);
    assert_eq!(effective.stat.gid, 701);

    let duplicate = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/setgid/inherited.sock")
        .expect_err("duplicate socket node must fail");
    assert_eq!(duplicate.code(), "EADDRINUSE");
}

#[test]
fn unix_socket_bind_checks_search_write_and_raw_parent_components() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/no-search", 0o600, 700, 701);
    mkdir_with_metadata(&mut filesystem, "/no-write", 0o500, 700, 701);
    mkdir_with_metadata(&mut filesystem, "/blocked", 0o700, 999, 999);
    mkdir_with_metadata(&mut filesystem, "/blocked/child", 0o777, 700, 701);
    mkdir_with_metadata(&mut filesystem, "/group", 0o730, 999, 900);
    mkdir_with_metadata(&mut filesystem, "/base", 0o700, 700, 701);
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(700, 701, vec![900]));
    let pid = spawn_shell(&mut kernel);

    for path in [
        "/no-search/denied.sock",
        "/no-write/denied.sock",
        "/blocked/child/denied.sock",
    ] {
        let error = kernel
            .bind_unix_socket_path_for_process(DRIVER, pid, "/", path)
            .expect_err("directory DAC must reject bind");
        assert_eq!(error.code(), "EACCES", "unexpected error for {path}");
    }

    kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/group/allowed.sock")
        .expect("supplementary group write/search must allow bind");

    let raw_parent_error = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/base", "missing/../must-not-exist.sock")
        .expect_err("missing raw component must not be normalized away");
    assert_eq!(raw_parent_error.code(), "ENOENT");
    assert!(!kernel
        .exists("/base/must-not-exist.sock")
        .expect("inspect failed bind destination"));

    let ownership_error = kernel
        .bind_unix_socket_path_for_process("foreign-driver", pid, "/", "/group/foreign.sock")
        .expect_err("foreign driver must not act for process");
    assert_eq!(ownership_error.code(), "EPERM");
}

#[test]
fn unix_socket_bind_follows_parent_symlinks_but_not_the_final_name() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/real", 0o777, 700, 701);
    mkdir_with_metadata(&mut filesystem, "/real/nested", 0o777, 700, 701);
    filesystem
        .symlink("/real/nested", "/alias")
        .expect("create parent alias");
    filesystem
        .symlink("/missing-target", "/real/dangling")
        .expect("create dangling final symlink");
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(700, 701, Vec::new()));
    let pid = spawn_shell(&mut kernel);

    let preflight = kernel
        .resolve_unix_socket_bind_target_for_process(DRIVER, pid, "/", "/alias/../service.sock")
        .expect("preflight bind through parent symlink");
    assert_eq!(preflight, "/real/service.sock");
    assert!(!kernel
        .exists(&preflight)
        .expect("preflight must not create the marker"));

    let node = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/alias/../service.sock")
        .expect("bind through parent symlink and raw parent component");
    assert_eq!(node.canonical_path, "/real/service.sock");

    let dangling = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/real/dangling")
        .expect_err("bind must not follow an existing final symlink");
    assert_eq!(dangling.code(), "EADDRINUSE");

    let missing_parent = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/absent/child.sock")
        .expect_err("bind must not create missing parents");
    assert_eq!(missing_parent.code(), "ENOENT");
    assert!(!kernel
        .exists("/absent")
        .expect("inspect missing parent after failed bind"));
}

#[test]
fn unix_socket_connect_follows_final_symlink_and_checks_target_write_permission() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/real", 0o777, 700, 701);
    filesystem
        .write_file("/real/plain", Vec::new())
        .expect("create regular connect target");
    filesystem
        .chmod("/real/plain", 0o222)
        .expect("make regular target writable");
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(700, 701, vec![900]));
    let pid = spawn_shell(&mut kernel);
    let bound = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/real/server.sock")
        .expect("create socket target");
    kernel
        .symlink("/real/server.sock", "/server-alias")
        .expect("create final socket symlink");
    kernel
        .symlink("/missing-socket", "/dangling-socket")
        .expect("create dangling socket symlink");
    kernel
        .symlink("/real/server.sock/", "/socket-target-with-trailing-slash")
        .expect("create socket symlink whose target has a trailing slash");

    let resolved = kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/server-alias")
        .expect("connect lookup must follow final symlink");
    assert_eq!(resolved.canonical_path, "/real/server.sock");
    assert_eq!(
        (resolved.stat.dev, resolved.stat.ino),
        (bound.stat.dev, bound.stat.ino)
    );

    kernel
        .chmod("/real/server.sock", 0o140400)
        .expect("remove socket write permission");
    let denied = kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/server-alias")
        .expect_err("socket write permission must be enforced");
    assert_eq!(denied.code(), "EACCES");

    kernel
        .chown("/real/server.sock", 999, 900)
        .expect("move socket to supplementary group");
    kernel
        .chmod("/real/server.sock", 0o140020)
        .expect("grant group-only socket write permission");
    kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/server-alias")
        .expect("supplementary group write must allow connect lookup");

    let regular = kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/real/plain")
        .expect_err("regular file is not a Unix socket target");
    assert_eq!(regular.code(), "ECONNREFUSED");

    let dangling = kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/dangling-socket")
        .expect_err("dangling final symlink must be ENOENT");
    assert_eq!(dangling.code(), "ENOENT");

    let trailing = kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/real/server.sock/")
        .expect_err("trailing slash requires a directory");
    assert_eq!(trailing.code(), "ENOTDIR");

    let symlink_target_trailing = kernel
        .resolve_unix_socket_connect_target_for_process(
            DRIVER,
            pid,
            "/",
            "/socket-target-with-trailing-slash",
        )
        .expect_err("a trailing slash in the final symlink target requires a directory");
    assert_eq!(symlink_target_trailing.code(), "ENOTDIR");
}

#[test]
fn unix_socket_path_walk_is_bounded_and_root_bypasses_dac() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/locked", 0o000, 999, 999);
    for index in 0..=40 {
        let target = if index == 40 {
            String::from("/locked")
        } else {
            format!("/link-{}", index + 1)
        };
        filesystem
            .symlink(&target, &format!("/link-{index}"))
            .expect("create bounded symlink chain");
    }
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(0, 0, Vec::new()));
    let pid = spawn_shell(&mut kernel);

    let loop_error = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/link-0/too-deep.sock")
        .expect_err("more than 40 followed symlinks must fail");
    assert_eq!(loop_error.code(), "ELOOP");

    let node = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/locked/root.sock")
        .expect("root effective uid must bypass directory DAC");
    assert_eq!(node.stat.uid, 0);
    assert_eq!(node.stat.gid, 0);
    kernel
        .chmod("/locked/root.sock", 0o140000)
        .expect("remove every socket permission bit");
    kernel
        .resolve_unix_socket_connect_target_for_process(DRIVER, pid, "/", "/locked/root.sock")
        .expect("root effective uid must bypass socket write DAC");
}

#[test]
fn unix_socket_path_validation_uses_raw_linux_length_and_allows_control_bytes() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/real", 0o700, 700, 701);
    let mut kernel = kernel_with_filesystem(filesystem, configured_user(700, 701, Vec::new()));
    let pid = spawn_shell(&mut kernel);

    let control_path = "/real/control\nbyte.sock";
    let control_node = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", control_path)
        .expect("Linux permits non-NUL control bytes in Unix socket pathnames");
    assert_eq!(control_node.canonical_path, control_path);

    let raw_too_long = format!("/real/{}socket", "a/../".repeat(MAX_PATH_LENGTH));
    assert!(raw_too_long.len() >= MAX_PATH_LENGTH);
    let length_error = kernel
        .resolve_unix_socket_bind_target_for_process(DRIVER, pid, "/", &raw_too_long)
        .expect_err("raw pathname length must be checked before resolving dot components");
    assert_eq!(length_error.code(), "ENAMETOOLONG");

    let nul_error = kernel
        .resolve_unix_socket_bind_target_for_process(DRIVER, pid, "/", "/real/nul\0byte.sock")
        .expect_err("embedded NUL must remain invalid");
    assert_eq!(nul_error.code(), "EINVAL");
}

#[test]
fn unix_socket_bind_rolls_back_metadata_failure_without_charging_inode_quota() {
    let mut filesystem = MemoryFileSystem::new();
    mkdir_with_metadata(&mut filesystem, "/sockets", 0o700, 700, 701);
    // Preseed the driver's projected command so registration does not change
    // the inode baseline after the quota is configured.
    filesystem
        .write_file("/bin/sh", Vec::new())
        .expect("preseed projected shell command");
    let initial_usage = measure_filesystem_usage(&mut filesystem).expect("measure fixture usage");

    let mut config = KernelVmConfig::new("vm-unix-socket-bind-rollback");
    config.permissions = Permissions::allow_all();
    config.user = configured_user(700, 701, Vec::new());
    config.resources = ResourceLimits {
        max_inode_count: Some(initial_usage.inode_count + 2),
        ..ResourceLimits::default()
    };
    let mut kernel = KernelVm::new(
        MetadataFailureFileSystem::new(filesystem, "/sockets/fail.sock"),
        config,
    );
    kernel
        .register_driver(CommandDriver::new(DRIVER, ["sh"]))
        .expect("register shell driver");
    let pid = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from(DRIVER)),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
        .pid();

    let metadata_error = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/sockets/fail.sock")
        .expect_err("injected metadata failure must propagate");
    assert_eq!(
        metadata_error.code(),
        "EIO",
        "unexpected bind failure: {metadata_error}"
    );
    assert!(!kernel
        .exists("/sockets/fail.sock")
        .expect("inspect rolled-back marker"));

    kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/sockets/ok.sock")
        .expect("rolled-back bind must not consume the remaining inode quota");
    let second_node = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/sockets/last.sock")
        .expect("the second available inode remains usable after rollback");
    assert_eq!(second_node.canonical_path, "/sockets/last.sock");
    let quota_error = kernel
        .bind_unix_socket_path_for_process(DRIVER, pid, "/", "/sockets/over-limit.sock")
        .expect_err("the successful markers must consume the remaining inode quota");
    assert_eq!(quota_error.code(), "ENOSPC");
}
