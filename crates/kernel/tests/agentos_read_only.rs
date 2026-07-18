use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::fd_table::{O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY};
use agentos_kernel::kernel::{KernelError, KernelResult, KernelVm, KernelVmConfig, SpawnOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::root_fs::{
    FilesystemEntry, RootFileSystem, RootFilesystemDescriptor, RootFilesystemMode,
    RootFilesystemSnapshot,
};
use agentos_kernel::vfs::{MemoryFileSystem, VirtualFileSystem, VirtualTimeSpec, VirtualUtimeSpec};
use std::fmt::Debug;

const DRIVER: &str = "shell";
const INSTRUCTIONS: &str = "/etc/agentos/instructions.md";

fn assert_erofs<T: Debug>(result: KernelResult<T>) {
    let error = result.expect_err("operation should fail on read-only agentos path");
    assert_eq!(error.code(), "EROFS");
}

fn seeded_kernel() -> KernelVm<MemoryFileSystem> {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file(INSTRUCTIONS, b"original instructions".to_vec())
        .expect("seed instructions before kernel starts");
    filesystem
        .mkdir("/etc/agentos/protected-dir", true)
        .expect("seed protected directory before kernel starts");
    filesystem
        .write_file(
            "/etc/agentos/protected-dir/sentinel",
            b"protected sentinel".to_vec(),
        )
        .expect("seed protected directory contents before kernel starts");
    filesystem.mkdir("/tmp", true).expect("seed tmp directory");

    let mut config = KernelVmConfig::new("vm-agentos-read-only");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(filesystem, config);
    kernel
        .register_driver(CommandDriver::new(DRIVER, ["sh"]))
        .expect("register shell driver");
    kernel
}

fn seeded_kernel_with_hardlink_alias() -> KernelVm<MemoryFileSystem> {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file(INSTRUCTIONS, b"original instructions".to_vec())
        .expect("seed instructions before kernel starts");
    filesystem.mkdir("/tmp", true).expect("seed tmp directory");
    filesystem
        .link(INSTRUCTIONS, "/tmp/instructions-hardlink.md")
        .expect("seed hardlink alias before kernel starts");

    let mut config = KernelVmConfig::new("vm-agentos-hardlink-read-only");
    config.permissions = Permissions::allow_all();
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

fn read_instructions(kernel: &mut KernelVm<MemoryFileSystem>) -> Result<String, KernelError> {
    let bytes = kernel.read_file(INSTRUCTIONS)?;
    Ok(String::from_utf8(bytes).expect("instructions should be utf8"))
}

#[test]
fn agentos_instructions_are_readable_but_not_writable() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);

    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions"),
        "original instructions"
    );

    assert_erofs(kernel.write_file(INSTRUCTIONS, "tampered"));
    assert_erofs(kernel.write_file_for_process(DRIVER, pid, INSTRUCTIONS, "tampered", Some(0o644)));
    assert_erofs(kernel.remove_file(INSTRUCTIONS));
    assert_erofs(kernel.rename(INSTRUCTIONS, "/tmp/instructions.md"));
    assert_erofs(kernel.rename("/tmp/replacement.md", INSTRUCTIONS));
    assert_erofs(kernel.chmod(INSTRUCTIONS, 0o777));
    assert_erofs(kernel.link(INSTRUCTIONS, "/tmp/instructions-link.md"));

    let fd = kernel
        .fd_open(DRIVER, pid, INSTRUCTIONS, O_RDONLY, None)
        .expect("open instructions read-only");
    let contents = kernel
        .fd_read(DRIVER, pid, fd, 64)
        .expect("read instructions fd");
    assert_eq!(
        String::from_utf8(contents).expect("instructions should be utf8"),
        "original instructions"
    );

    assert_erofs(kernel.fd_open(DRIVER, pid, INSTRUCTIONS, O_WRONLY, None));
    assert_erofs(kernel.fd_open(DRIVER, pid, INSTRUCTIONS, O_TRUNC, None));
    assert_erofs(kernel.fd_open(
        DRIVER,
        pid,
        "/etc/agentos/generated.md",
        O_CREAT | O_WRONLY,
        Some(0o644),
    ));
    assert_erofs(kernel.fd_write(DRIVER, pid, fd, b"tampered"));
    assert_erofs(kernel.fd_pwrite(DRIVER, pid, fd, b"tampered", 0));

    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions after failed writes"),
        "original instructions"
    );
}

#[test]
fn agentos_directory_rejects_new_children_and_metadata_updates() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);

    assert_erofs(kernel.create_dir("/etc/agentos/nested"));
    assert_erofs(kernel.create_dir_for_process(DRIVER, pid, "/etc/agentos/nested", Some(0o755)));
    assert_erofs(kernel.mkdir("/etc/agentos/nested/deeper", true));
    assert_erofs(kernel.mkdir_for_process(
        DRIVER,
        pid,
        "/etc/agentos/nested/deeper",
        true,
        Some(0o755),
    ));
    assert_erofs(kernel.symlink("/tmp/source", "/etc/agentos/source-link"));
    assert_erofs(kernel.chown("/etc/agentos", 1000, 1000));
    assert_erofs(kernel.utimes("/etc/agentos", 1, 1));
    assert_erofs(kernel.truncate(INSTRUCTIONS, 0));

    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions after failed metadata updates"),
        "original instructions"
    );
}

#[test]
fn agentos_protection_follows_symlink_aliases() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);

    kernel
        .symlink(INSTRUCTIONS, "/tmp/instructions-alias")
        .expect("create writable-path symlink to instructions");
    assert_erofs(kernel.write_file("/tmp/instructions-alias", "tampered"));
    assert_erofs(kernel.write_file_for_process(
        DRIVER,
        pid,
        "/tmp/instructions-alias",
        "tampered",
        Some(0o644),
    ));
    assert_erofs(kernel.truncate("/tmp/instructions-alias", 0));
    assert_erofs(kernel.chmod("/tmp/instructions-alias", 0o777));
    assert_erofs(kernel.chown("/tmp/instructions-alias", 1000, 1000));
    assert_erofs(kernel.utimes("/tmp/instructions-alias", 1, 1));
    assert_erofs(kernel.link("/tmp/instructions-alias", "/tmp/instructions-hardlink"));

    let fd = kernel
        .fd_open(DRIVER, pid, "/tmp/instructions-alias", O_RDONLY, None)
        .expect("open instructions alias read-only");
    assert_erofs(kernel.fd_write(DRIVER, pid, fd, b"tampered"));
    assert_erofs(kernel.fd_pwrite(DRIVER, pid, fd, b"tampered", 0));
    assert_erofs(kernel.fd_open(DRIVER, pid, "/tmp/instructions-alias", O_WRONLY, None));
    assert_erofs(kernel.futimes(
        DRIVER,
        pid,
        fd,
        VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(1)),
        VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(1)),
    ));

    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions after failed alias writes"),
        "original instructions"
    );

    kernel
        .remove_file("/tmp/instructions-alias")
        .expect("outside symlink alias should remain removable");
    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions after removing alias"),
        "original instructions"
    );
}

#[test]
fn agentos_protection_rejects_preexisting_hardlink_aliases() {
    let mut kernel = seeded_kernel_with_hardlink_alias();
    let pid = spawn_shell(&mut kernel);
    let alias = "/tmp/instructions-hardlink.md";
    let symlink_alias = "/tmp/instructions-symlink-to-hardlink.md";

    assert_eq!(
        kernel
            .read_file(alias)
            .expect("read hardlink alias to instructions"),
        b"original instructions".to_vec()
    );
    assert_erofs(kernel.write_file(alias, "tampered"));
    assert_erofs(kernel.write_file_for_process(DRIVER, pid, alias, "tampered", Some(0o644)));
    assert_erofs(kernel.truncate(alias, 0));
    assert_erofs(kernel.chmod(alias, 0o777));
    assert_erofs(kernel.chown(alias, 1000, 1000));
    assert_erofs(kernel.utimes(alias, 1, 1));
    assert_erofs(kernel.remove_file(alias));
    assert_erofs(kernel.rename(alias, "/tmp/moved-hardlink.md"));

    let fd = kernel
        .fd_open(DRIVER, pid, alias, O_RDONLY, None)
        .expect("open hardlink alias read-only");
    assert_erofs(kernel.fd_write(DRIVER, pid, fd, b"tampered"));
    assert_erofs(kernel.fd_pwrite(DRIVER, pid, fd, b"tampered", 0));
    assert_erofs(kernel.fd_open(DRIVER, pid, alias, O_WRONLY, None));
    assert_erofs(kernel.futimes(
        DRIVER,
        pid,
        fd,
        VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(1)),
        VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(1)),
    ));

    kernel
        .symlink(alias, symlink_alias)
        .expect("create symlink to hardlink alias");
    assert_erofs(kernel.write_file(symlink_alias, "tampered"));
    assert_erofs(kernel.truncate(symlink_alias, 0));
    assert_erofs(kernel.fd_open(DRIVER, pid, symlink_alias, O_WRONLY, None));

    assert_eq!(
        read_instructions(&mut kernel).expect("read instructions after hardlink writes"),
        "original instructions"
    );
    assert_eq!(
        kernel
            .read_file(alias)
            .expect("hardlink alias should still exist"),
        b"original instructions".to_vec()
    );
}

#[test]
fn agentos_protection_ignores_unrelated_files_in_other_overlay_layers() {
    // Regression coverage for layered roots: the protected instructions file
    // lives in a lower snapshot layer while new files land in the writable
    // upper. Inode numbers overlap across layer filesystems, so the hardlink
    // alias check must compare per-instance device ids instead of treating
    // every equal inode number as an alias of the protected file.
    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/etc/agentos"),
                FilesystemEntry::file(
                    "/etc/agentos/instructions.md",
                    b"original instructions".to_vec(),
                ),
                FilesystemEntry::directory("/bin"),
                FilesystemEntry::directory("/tmp"),
            ],
        }],
        bootstrap_entries: vec![],
    })
    .expect("create layered root filesystem");

    let mut config = KernelVmConfig::new("vm-agentos-layered-alias");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(root, config);

    // Write enough files for the upper layer's inode counter to sweep past
    // the lower layer's inode numbers, then verify metadata updates on every
    // unrelated file still succeed.
    for index in 0..8 {
        let path = format!("/tmp/unrelated-{index}.txt");
        kernel
            .write_file(&path, "unrelated")
            .expect("write unrelated file in upper layer");
        kernel
            .chmod(&path, 0o755)
            .expect("chmod unrelated upper-layer file must not trip agentos protection");
    }

    assert_erofs(kernel.chmod("/etc/agentos/instructions.md", 0o777));
    assert_erofs(kernel.write_file("/etc/agentos/instructions.md", "tampered"));
    assert_eq!(
        kernel
            .read_file("/etc/agentos/instructions.md")
            .expect("read instructions"),
        b"original instructions".to_vec()
    );
}

#[test]
fn agentos_protection_rejects_creates_through_symlinked_parent() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);

    kernel
        .symlink("/etc/agentos", "/tmp/agentos-alias")
        .expect("create writable-path symlink to agentos directory");

    assert_erofs(kernel.write_file("/tmp/agentos-alias/generated.md", "tampered"));
    assert_erofs(kernel.create_dir("/tmp/agentos-alias/nested"));
    assert_erofs(kernel.mkdir("/tmp/agentos-alias/nested/deeper", true));
    assert_erofs(kernel.remove_file("/tmp/agentos-alias/instructions.md"));
    assert_erofs(kernel.rename(
        "/tmp/agentos-alias/instructions.md",
        "/tmp/moved-instructions.md",
    ));
    kernel
        .write_file("/tmp/replacement.md", "replacement")
        .expect("write replacement outside protected tree");
    assert_erofs(kernel.rename("/tmp/replacement.md", "/tmp/agentos-alias/replacement.md"));
    assert_erofs(kernel.symlink("/tmp/source", "/tmp/agentos-alias/source-link"));
    assert_erofs(kernel.fd_open(
        DRIVER,
        pid,
        "/tmp/agentos-alias/generated.md",
        O_CREAT | O_WRONLY,
        Some(0o644),
    ));

    assert_eq!(
        read_instructions(&mut kernel)
            .expect("read instructions after failed symlinked-parent creates"),
        "original instructions"
    );
}

#[test]
fn process_mutation_matrix_rejects_read_only_sources_and_destinations_without_mutation() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);
    kernel
        .write_file("/tmp/link-source", "link source")
        .expect("seed writable hard-link source");
    kernel
        .write_file("/tmp/rename-source", "rename source")
        .expect("seed writable rename source");
    kernel
        .write_file("/tmp/replacement", "replacement")
        .expect("seed writable replacement");

    assert_erofs(kernel.link_for_process(
        DRIVER,
        pid,
        INSTRUCTIONS,
        "/tmp/protected-source-hardlink",
    ));
    assert!(!kernel
        .exists("/tmp/protected-source-hardlink")
        .expect("check rejected hard-link destination"));
    assert_eq!(
        read_instructions(&mut kernel).expect("read protected hard-link source"),
        "original instructions"
    );

    assert_erofs(kernel.link_for_process(
        DRIVER,
        pid,
        "/tmp/link-source",
        "/etc/agentos/new-hardlink",
    ));
    assert_eq!(
        kernel
            .read_file("/tmp/link-source")
            .expect("read writable hard-link source after denial"),
        b"link source"
    );
    assert!(!kernel
        .exists("/etc/agentos/new-hardlink")
        .expect("check rejected protected hard-link destination"));

    assert_erofs(kernel.remove_file_for_process(DRIVER, pid, INSTRUCTIONS));
    assert_eq!(
        read_instructions(&mut kernel).expect("read protected file after rejected unlink"),
        "original instructions"
    );

    assert_erofs(kernel.remove_dir_for_process(DRIVER, pid, "/etc/agentos/protected-dir"));
    assert_eq!(
        kernel
            .read_file("/etc/agentos/protected-dir/sentinel")
            .expect("read protected directory contents after rejected removal"),
        b"protected sentinel"
    );

    assert_erofs(kernel.rename_for_process(DRIVER, pid, INSTRUCTIONS, "/tmp/moved-instructions"));
    assert_eq!(
        read_instructions(&mut kernel).expect("read protected rename source"),
        "original instructions"
    );
    assert!(!kernel
        .exists("/tmp/moved-instructions")
        .expect("check rejected rename destination"));

    assert_erofs(kernel.rename_for_process(
        DRIVER,
        pid,
        "/tmp/rename-source",
        "/etc/agentos/new-name",
    ));
    assert_eq!(
        kernel
            .read_file("/tmp/rename-source")
            .expect("read writable rename source after denial"),
        b"rename source"
    );
    assert!(!kernel
        .exists("/etc/agentos/new-name")
        .expect("check rejected protected rename destination"));

    assert_erofs(kernel.rename_for_process(DRIVER, pid, "/tmp/replacement", INSTRUCTIONS));
    assert_eq!(
        kernel
            .read_file("/tmp/replacement")
            .expect("read rejected replacement source"),
        b"replacement"
    );
    assert_eq!(
        read_instructions(&mut kernel).expect("read rejected replacement destination"),
        "original instructions"
    );

    assert_erofs(kernel.symlink_for_process(
        DRIVER,
        pid,
        "/tmp/target-does-not-need-to-exist",
        "/etc/agentos/new-symlink",
    ));
    assert_eq!(
        kernel
            .lstat("/etc/agentos/new-symlink")
            .expect_err("rejected protected symlink destination must not be created")
            .code(),
        "ENOENT"
    );
}

#[test]
fn process_mutation_read_only_checks_follow_symlinked_parents_without_mutation() {
    let mut kernel = seeded_kernel();
    let pid = spawn_shell(&mut kernel);
    kernel
        .symlink("/etc/agentos", "/tmp/agentos-alias")
        .expect("create writable-path alias to protected directory");
    kernel
        .write_file("/tmp/link-source", "link source")
        .expect("seed writable hard-link source");
    kernel
        .write_file("/tmp/rename-source", "rename source")
        .expect("seed writable rename source");
    kernel
        .write_file("/tmp/replacement", "replacement")
        .expect("seed writable replacement");

    assert_erofs(kernel.link_for_process(
        DRIVER,
        pid,
        "/tmp/agentos-alias/instructions.md",
        "/tmp/aliased-source-hardlink",
    ));
    assert!(!kernel
        .exists("/tmp/aliased-source-hardlink")
        .expect("check rejected aliased-source hard link"));
    assert_eq!(
        read_instructions(&mut kernel).expect("read aliased hard-link source after denial"),
        "original instructions"
    );

    assert_erofs(kernel.link_for_process(
        DRIVER,
        pid,
        "/tmp/link-source",
        "/tmp/agentos-alias/new-hardlink",
    ));
    assert_eq!(
        kernel
            .read_file("/tmp/link-source")
            .expect("read hard-link source after aliased destination denial"),
        b"link source"
    );
    assert!(!kernel
        .exists("/etc/agentos/new-hardlink")
        .expect("check protected hard-link destination"));

    assert_erofs(kernel.remove_file_for_process(DRIVER, pid, "/tmp/agentos-alias/instructions.md"));
    assert_eq!(
        read_instructions(&mut kernel).expect("read aliased unlink target after denial"),
        "original instructions"
    );

    assert_erofs(kernel.remove_dir_for_process(DRIVER, pid, "/tmp/agentos-alias/protected-dir"));
    assert_eq!(
        kernel
            .read_file("/etc/agentos/protected-dir/sentinel")
            .expect("read aliased protected directory after denial"),
        b"protected sentinel"
    );

    assert_erofs(kernel.rename_for_process(
        DRIVER,
        pid,
        "/tmp/agentos-alias/instructions.md",
        "/tmp/moved-aliased-instructions",
    ));
    assert_eq!(
        read_instructions(&mut kernel).expect("read aliased rename source after denial"),
        "original instructions"
    );
    assert!(!kernel
        .exists("/tmp/moved-aliased-instructions")
        .expect("check rejected writable rename destination"));

    assert_erofs(kernel.rename_for_process(
        DRIVER,
        pid,
        "/tmp/rename-source",
        "/tmp/agentos-alias/new-name",
    ));
    assert_eq!(
        kernel
            .read_file("/tmp/rename-source")
            .expect("read rename source after aliased destination denial"),
        b"rename source"
    );
    assert!(!kernel
        .exists("/etc/agentos/new-name")
        .expect("check rejected protected rename destination"));

    assert_erofs(kernel.rename_for_process(
        DRIVER,
        pid,
        "/tmp/replacement",
        "/tmp/agentos-alias/instructions.md",
    ));
    assert_eq!(
        kernel
            .read_file("/tmp/replacement")
            .expect("read replacement after aliased destination denial"),
        b"replacement"
    );
    assert_eq!(
        read_instructions(&mut kernel).expect("read aliased replacement destination"),
        "original instructions"
    );

    assert_erofs(kernel.symlink_for_process(
        DRIVER,
        pid,
        "/tmp/target-does-not-need-to-exist",
        "/tmp/agentos-alias/new-symlink",
    ));
    assert_eq!(
        kernel
            .lstat("/etc/agentos/new-symlink")
            .expect_err("rejected aliased symlink destination must not be created")
            .code(),
        "ENOENT"
    );
    assert_eq!(
        kernel
            .read_link("/tmp/agentos-alias")
            .expect("read parent alias after rejected mutations"),
        "/etc/agentos"
    );
}
