use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::fd_table::{O_CREAT, O_RDONLY, O_WRONLY};
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions, VirtualProcessOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::user::{GroupRecord, UserAccount, UserConfig};
use agentos_kernel::vfs::{MemoryFileSystem, VirtualTimeSpec, VirtualUtimeSpec};

const DRIVER: &str = "dac-driver";
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;

fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut value = 2u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in entries {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&permissions.to_le_bytes());
        value.extend_from_slice(&id.to_le_bytes());
    }
    value
}

fn extended_acl(named_user_permissions: u16, mask: u16) -> Vec<u8> {
    acl(&[
        (ACL_USER_OBJ, 0o6, u32::MAX),
        (ACL_USER, named_user_permissions, 1001),
        (ACL_GROUP_OBJ, 0, u32::MAX),
        (ACL_GROUP, 0o2, 2000),
        (ACL_MASK, mask, u32::MAX),
        (ACL_OTHER, 0, u32::MAX),
    ])
}

fn account(uid: u32, gid: u32, name: &str, supplementary_gids: Vec<u32>) -> UserAccount {
    UserAccount {
        uid,
        gid,
        username: name.to_owned(),
        homedir: format!("/home/{name}"),
        shell: String::from("/bin/sh"),
        gecos: String::new(),
        supplementary_gids,
    }
}

fn kernel() -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-dac");
    config.permissions = Permissions::allow_all();
    config.user = UserConfig {
        uid: Some(0),
        gid: Some(0),
        username: Some(String::from("root")),
        homedir: Some(String::from("/root")),
        shell: Some(String::from("/bin/sh")),
        group_name: Some(String::from("root")),
        supplementary_gids: vec![0],
        accounts: vec![
            account(1000, 1000, "alice", vec![1000, 2000]),
            account(1001, 1001, "bob", vec![1001]),
            account(1002, 1002, "carol", vec![1002, 2000]),
        ],
        groups: vec![GroupRecord {
            gid: 2000,
            name: String::from("shared"),
            members: vec![String::from("alice"), String::from("carol")],
        }],
        ..UserConfig::default()
    };
    KernelVm::new(MemoryFileSystem::new(), config)
}

fn process_as(kernel: &mut KernelVm<MemoryFileSystem>, uid: u32) -> u32 {
    let process = kernel
        .create_virtual_process(
            DRIVER,
            DRIVER,
            "dac-test",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create root process");
    kernel
        .switch_user(DRIVER, process.pid(), uid)
        .expect("switch process user");
    process.pid()
}

#[test]
fn spawn_selects_execute_bits_from_the_parent_effective_identity() {
    let mut kernel = kernel();
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell interpreter");
    kernel
        .write_file("/probe", b"#!/bin/sh\nexit 0\n".to_vec())
        .expect("write executable probe");
    kernel.chown("/probe", 1000, 2000).expect("chown probe");

    let alice = process_as(&mut kernel, 1000);
    kernel.chmod("/probe", 0o010).expect("set group execute");
    let owner_error = kernel
        .spawn_process(
            "/probe",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(DRIVER.to_owned()),
                parent_pid: Some(alice),
                ..SpawnOptions::default()
            },
        )
        .expect_err("owner class must not borrow group execute permission");
    assert_eq!(owner_error.code(), "EACCES");

    let carol = process_as(&mut kernel, 1002);
    kernel
        .spawn_process(
            "/probe",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(DRIVER.to_owned()),
                parent_pid: Some(carol),
                ..SpawnOptions::default()
            },
        )
        .expect("supplementary group execute permission should allow spawn");

    let bob = process_as(&mut kernel, 1001);
    let group_error = kernel
        .spawn_process(
            "/probe",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(DRIVER.to_owned()),
                parent_pid: Some(bob),
                ..SpawnOptions::default()
            },
        )
        .expect_err("other class must not borrow group execute permission");
    assert_eq!(group_error.code(), "EACCES");

    kernel.chmod("/probe", 0o001).expect("set other execute");
    kernel
        .spawn_process(
            "/probe",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(DRIVER.to_owned()),
                parent_pid: Some(bob),
                ..SpawnOptions::default()
            },
        )
        .expect("other execute permission should allow spawn");
}

#[test]
fn traversal_and_file_modes_select_owner_group_and_other_bits() {
    let mut kernel = kernel();
    kernel.mkdir("/secure", false).unwrap();
    kernel
        .write_file("/secure/data", b"secret".to_vec())
        .unwrap();
    kernel.chown("/secure", 1000, 1000).unwrap();
    kernel.chmod("/secure", 0o710).unwrap();
    kernel.chown("/secure/data", 1000, 2000).unwrap();
    kernel.chmod("/secure/data", 0o640).unwrap();

    let alice = process_as(&mut kernel, 1000);
    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, alice, "/secure/data")
            .unwrap(),
        b"secret"
    );

    let carol = process_as(&mut kernel, 1002);
    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, carol, "/secure/data")
            .unwrap_err()
            .code(),
        "EACCES",
        "group read cannot bypass a non-searchable ancestor"
    );

    kernel.chmod("/secure", 0o711).unwrap();
    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, carol, "/secure/data")
            .unwrap(),
        b"secret"
    );
    let bob = process_as(&mut kernel, 1001);
    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, bob, "/secure/data")
            .unwrap_err()
            .code(),
        "EACCES"
    );
}

#[test]
fn creation_uses_effective_ids_umask_and_setgid_parent() {
    let mut kernel = kernel();
    kernel.mkdir("/shared", false).unwrap();
    kernel.chown("/shared", 0, 2000).unwrap();
    kernel.chmod("/shared", 0o2777).unwrap();
    let alice = process_as(&mut kernel, 1000);
    kernel.umask(DRIVER, alice, Some(0o027)).unwrap();

    kernel
        .write_file_for_process(DRIVER, alice, "/shared/file", b"x".to_vec(), Some(0o666))
        .unwrap();
    kernel
        .mkdir_for_process(DRIVER, alice, "/shared/dir", false, Some(0o777))
        .unwrap();
    let file = kernel.stat("/shared/file").unwrap();
    assert_eq!(
        (file.uid, file.gid, file.mode & 0o7777),
        (1000, 2000, 0o640)
    );
    let directory = kernel.stat("/shared/dir").unwrap();
    assert_eq!(
        (directory.uid, directory.gid, directory.mode & 0o7777),
        (1000, 2000, 0o2750)
    );
}

#[test]
fn sticky_directory_only_allows_root_directory_owner_or_file_owner() {
    let mut kernel = kernel();
    kernel.mkdir("/tmp-sticky", false).unwrap();
    kernel.chown("/tmp-sticky", 0, 0).unwrap();
    kernel.chmod("/tmp-sticky", 0o1777).unwrap();
    kernel
        .write_file("/tmp-sticky/alice", b"x".to_vec())
        .unwrap();
    kernel.chown("/tmp-sticky/alice", 1000, 1000).unwrap();

    let bob = process_as(&mut kernel, 1001);
    assert_eq!(
        kernel
            .remove_file_for_process(DRIVER, bob, "/tmp-sticky/alice")
            .unwrap_err()
            .code(),
        "EPERM"
    );
    let alice = process_as(&mut kernel, 1000);
    kernel
        .remove_file_for_process(DRIVER, alice, "/tmp-sticky/alice")
        .unwrap();
}

#[test]
fn process_mutation_dac_denials_follow_symlinked_parents_without_mutation() {
    let mut kernel = kernel();
    kernel.mkdir("/open", false).unwrap();
    kernel.chmod("/open", 0o777).unwrap();
    kernel
        .write_file("/open/link-source", b"link source".to_vec())
        .unwrap();
    kernel
        .write_file("/open/rename-source", b"rename source".to_vec())
        .unwrap();

    kernel.mkdir("/restricted-source", false).unwrap();
    kernel
        .write_file(
            "/restricted-source/hidden-link-source",
            b"hidden source".to_vec(),
        )
        .unwrap();
    kernel.chown("/restricted-source", 1000, 1000).unwrap();
    kernel.chmod("/restricted-source", 0o700).unwrap();
    kernel
        .symlink("/restricted-source", "/source-alias")
        .unwrap();

    kernel.mkdir("/restricted-destination", false).unwrap();
    kernel
        .write_file(
            "/restricted-destination/remove-file",
            b"remove file".to_vec(),
        )
        .unwrap();
    kernel
        .write_file(
            "/restricted-destination/rename-source",
            b"restricted rename source".to_vec(),
        )
        .unwrap();
    kernel
        .write_file(
            "/restricted-destination/rename-destination",
            b"rename destination".to_vec(),
        )
        .unwrap();
    kernel
        .mkdir("/restricted-destination/remove-dir", false)
        .unwrap();
    kernel.chown("/restricted-destination", 1000, 1000).unwrap();
    kernel.chmod("/restricted-destination", 0o555).unwrap();
    kernel
        .symlink("/restricted-destination", "/destination-alias")
        .unwrap();

    let bob = process_as(&mut kernel, 1001);

    let source_traversal = kernel
        .link_for_process(
            DRIVER,
            bob,
            "/source-alias/hidden-link-source",
            "/open/hidden-hardlink",
        )
        .expect_err("hard-link source traversal should require search permission");
    assert_eq!(source_traversal.code(), "EACCES");
    assert!(!kernel.exists("/open/hidden-hardlink").unwrap());
    assert_eq!(
        kernel
            .read_file("/restricted-source/hidden-link-source")
            .unwrap(),
        b"hidden source"
    );

    let link_destination = kernel
        .link_for_process(
            DRIVER,
            bob,
            "/open/link-source",
            "/destination-alias/new-hardlink",
        )
        .expect_err("hard-link destination parent should require write permission");
    assert_eq!(link_destination.code(), "EACCES");
    assert!(!kernel
        .exists("/restricted-destination/new-hardlink")
        .unwrap());
    assert_eq!(
        kernel.read_file("/open/link-source").unwrap(),
        b"link source"
    );

    let remove_file = kernel
        .remove_file_for_process(DRIVER, bob, "/destination-alias/remove-file")
        .expect_err("unlink parent should require write permission");
    assert_eq!(remove_file.code(), "EACCES");
    assert_eq!(
        kernel
            .read_file("/restricted-destination/remove-file")
            .unwrap(),
        b"remove file"
    );

    let remove_dir = kernel
        .remove_dir_for_process(DRIVER, bob, "/destination-alias/remove-dir")
        .expect_err("rmdir parent should require write permission");
    assert_eq!(remove_dir.code(), "EACCES");
    assert!(
        kernel
            .stat("/restricted-destination/remove-dir")
            .unwrap()
            .is_directory
    );

    let rename_source = kernel
        .rename_for_process(
            DRIVER,
            bob,
            "/destination-alias/rename-source",
            "/open/moved-from-restricted",
        )
        .expect_err("rename source parent should require write permission");
    assert_eq!(rename_source.code(), "EACCES");
    assert_eq!(
        kernel
            .read_file("/restricted-destination/rename-source")
            .unwrap(),
        b"restricted rename source"
    );
    assert!(!kernel.exists("/open/moved-from-restricted").unwrap());

    let rename_destination = kernel
        .rename_for_process(
            DRIVER,
            bob,
            "/open/rename-source",
            "/destination-alias/rename-destination",
        )
        .expect_err("rename destination parent should require write permission");
    assert_eq!(rename_destination.code(), "EACCES");
    assert_eq!(
        kernel.read_file("/open/rename-source").unwrap(),
        b"rename source"
    );
    assert_eq!(
        kernel
            .read_file("/restricted-destination/rename-destination")
            .unwrap(),
        b"rename destination"
    );

    let symlink_destination = kernel
        .symlink_for_process(
            DRIVER,
            bob,
            "/target-does-not-need-to-exist",
            "/destination-alias/new-symlink",
        )
        .expect_err("symlink destination parent should require write permission");
    assert_eq!(symlink_destination.code(), "EACCES");
    assert_eq!(
        kernel
            .lstat("/restricted-destination/new-symlink")
            .expect_err("rejected symlink destination must not be created")
            .code(),
        "ENOENT"
    );

    kernel
        .symlink_for_process(
            DRIVER,
            bob,
            "/source-alias/hidden-target",
            "/open/dangling-symlink",
        )
        .expect("symlink creation must not traverse or authorize its target");
    assert_eq!(
        kernel.read_link("/open/dangling-symlink").unwrap(),
        "/source-alias/hidden-target"
    );
}

#[test]
fn sticky_directory_mutation_matrix_matches_linux_without_partial_changes() {
    let mut kernel = kernel();
    kernel.mkdir("/sticky", false).unwrap();
    kernel.chown("/sticky", 0, 0).unwrap();
    kernel.chmod("/sticky", 0o1777).unwrap();
    kernel.symlink("/sticky", "/sticky-alias").unwrap();
    kernel.mkdir("/open", false).unwrap();
    kernel.chmod("/open", 0o777).unwrap();

    kernel.mkdir("/sticky/alice-dir", false).unwrap();
    kernel.chown("/sticky/alice-dir", 1000, 1000).unwrap();
    kernel
        .write_file("/sticky/alice-rename-source", b"alice source".to_vec())
        .unwrap();
    kernel
        .chown("/sticky/alice-rename-source", 1000, 1000)
        .unwrap();
    kernel
        .write_file("/sticky/alice-replacement", b"alice replacement".to_vec())
        .unwrap();
    kernel
        .chown("/sticky/alice-replacement", 1000, 1000)
        .unwrap();
    kernel
        .write_file("/sticky/bob-destination", b"bob destination".to_vec())
        .unwrap();
    kernel.chown("/sticky/bob-destination", 1001, 1001).unwrap();
    kernel
        .write_file("/open/bob-link-source", b"bob source".to_vec())
        .unwrap();
    kernel.chown("/open/bob-link-source", 1001, 1001).unwrap();

    let bob = process_as(&mut kernel, 1001);
    let remove_dir = kernel
        .remove_dir_for_process(DRIVER, bob, "/sticky-alias/alice-dir")
        .expect_err("sticky directory should reject removing another user's directory");
    assert_eq!(remove_dir.code(), "EPERM");
    assert!(kernel.stat("/sticky/alice-dir").unwrap().is_directory);

    let rename_source = kernel
        .rename_for_process(
            DRIVER,
            bob,
            "/sticky-alias/alice-rename-source",
            "/sticky-alias/bob-moved-source",
        )
        .expect_err("sticky directory should reject renaming another user's source");
    assert_eq!(rename_source.code(), "EPERM");
    assert_eq!(
        kernel.read_file("/sticky/alice-rename-source").unwrap(),
        b"alice source"
    );
    assert!(!kernel.exists("/sticky/bob-moved-source").unwrap());

    kernel
        .link_for_process(
            DRIVER,
            bob,
            "/open/bob-link-source",
            "/sticky-alias/bob-hardlink",
        )
        .expect("sticky directories allow creating new hard-link names");
    assert_eq!(
        kernel.read_file("/sticky/bob-hardlink").unwrap(),
        b"bob source"
    );
    kernel
        .symlink_for_process(
            DRIVER,
            bob,
            "/target-does-not-need-to-exist",
            "/sticky-alias/bob-symlink",
        )
        .expect("sticky directories allow creating new symlink names");
    assert_eq!(
        kernel.read_link("/sticky/bob-symlink").unwrap(),
        "/target-does-not-need-to-exist"
    );

    let alice = process_as(&mut kernel, 1000);
    let replace_destination = kernel
        .rename_for_process(
            DRIVER,
            alice,
            "/sticky-alias/alice-replacement",
            "/sticky-alias/bob-destination",
        )
        .expect_err("sticky directory should reject replacing another user's destination");
    assert_eq!(replace_destination.code(), "EPERM");
    assert_eq!(
        kernel.read_file("/sticky/alice-replacement").unwrap(),
        b"alice replacement"
    );
    assert_eq!(
        kernel.read_file("/sticky/bob-destination").unwrap(),
        b"bob destination"
    );

    kernel
        .remove_dir_for_process(DRIVER, alice, "/sticky-alias/alice-dir")
        .expect("sticky entry owner should be able to remove their directory");
    kernel
        .rename_for_process(
            DRIVER,
            alice,
            "/sticky-alias/alice-rename-source",
            "/sticky-alias/alice-renamed",
        )
        .expect("sticky entry owner should be able to rename their source");

    kernel.mkdir("/alice-sticky", false).unwrap();
    kernel.chown("/alice-sticky", 1000, 1000).unwrap();
    kernel.chmod("/alice-sticky", 0o1777).unwrap();
    kernel
        .write_file("/alice-sticky/bob-file", b"owned by bob".to_vec())
        .unwrap();
    kernel.chown("/alice-sticky/bob-file", 1001, 1001).unwrap();
    kernel
        .remove_file_for_process(DRIVER, alice, "/alice-sticky/bob-file")
        .expect("sticky directory owner should be able to remove another user's entry");
    assert!(!kernel.exists("/alice-sticky/bob-file").unwrap());
    assert_eq!(kernel.read_link("/sticky-alias").unwrap(), "/sticky");
}

#[test]
fn metadata_changes_and_descriptor_modes_enforce_linux_style_errors() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 1000).unwrap();
    kernel.chmod("/work/file", 0o6755).unwrap();
    let alice = process_as(&mut kernel, 1000);
    let bob = process_as(&mut kernel, 1001);

    assert_eq!(
        kernel
            .chmod_for_process(DRIVER, bob, "/work/file", 0o600)
            .unwrap_err()
            .code(),
        "EPERM"
    );
    kernel
        .chown_for_process(DRIVER, alice, "/work/file", 1000, 2000, true)
        .unwrap();
    assert_eq!(kernel.stat("/work/file").unwrap().mode & 0o6000, 0);
    assert_eq!(
        kernel
            .chown_for_process(DRIVER, alice, "/work/file", 1001, 2000, true)
            .unwrap_err()
            .code(),
        "EPERM"
    );

    let read_only = kernel
        .fd_open(DRIVER, alice, "/work/file", O_RDONLY, None)
        .unwrap();
    assert_eq!(
        kernel
            .fd_write(DRIVER, alice, read_only, b"no")
            .unwrap_err()
            .code(),
        "EBADF"
    );
    let write_only = kernel
        .fd_open(DRIVER, alice, "/work/new", O_CREAT | O_WRONLY, Some(0o600))
        .unwrap();
    assert_eq!(
        kernel
            .fd_read(DRIVER, alice, write_only, 1)
            .unwrap_err()
            .code(),
        "EBADF"
    );
}

#[test]
fn setting_both_timestamps_to_now_accepts_group_write_permission() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 2000).unwrap();
    kernel.chmod("/work/file", 0o660).unwrap();
    let carol = process_as(&mut kernel, 1002);

    kernel
        .utimes_spec_for_process(
            DRIVER,
            carol,
            "/work/file",
            VirtualUtimeSpec::Now,
            VirtualUtimeSpec::Now,
            true,
        )
        .expect("group write permission should allow setting both timestamps to now");

    let explicit = VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(1_000));
    assert_eq!(
        kernel
            .utimes_spec_for_process(DRIVER, carol, "/work/file", explicit, explicit, true)
            .unwrap_err()
            .code(),
        "EPERM",
        "explicit timestamps still require ownership"
    );

    kernel.chmod("/work/file", 0o640).unwrap();
    assert_eq!(
        kernel
            .utimes_spec_for_process(
                DRIVER,
                carol,
                "/work/file",
                VirtualUtimeSpec::Now,
                VirtualUtimeSpec::Now,
                true,
            )
            .unwrap_err()
            .code(),
        "EACCES"
    );
}

#[test]
fn xattrs_enforce_dac_namespaces_and_linux_flags() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 1000).unwrap();
    kernel.chmod("/work/file", 0o644).unwrap();
    let alice = process_as(&mut kernel, 1000);
    let bob = process_as(&mut kernel, 1001);

    kernel
        .set_xattr_for_process(
            DRIVER,
            alice,
            "/work/file",
            "user.note",
            b"hello".to_vec(),
            1,
            true,
        )
        .unwrap();
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, bob, "/work/file", "user.note", true)
            .unwrap(),
        b"hello"
    );
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                bob,
                "/work/file",
                "user.note",
                b"no".to_vec(),
                2,
                true,
            )
            .unwrap_err()
            .code(),
        "EACCES"
    );
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, alice, "/work/file", "trusted.note", true)
            .unwrap_err()
            .code(),
        "EPERM"
    );
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                alice,
                "/work/file",
                "user.note",
                b"again".to_vec(),
                1,
                true,
            )
            .unwrap_err()
            .code(),
        "EEXIST"
    );
    kernel
        .remove_xattr_for_process(DRIVER, alice, "/work/file", "user.note", true)
        .unwrap();
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, alice, "/work/file", "user.note", true)
            .unwrap_err()
            .code(),
        "ENODATA"
    );

    let oversized_name = format!("user.{}", "x".repeat(251));
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                alice,
                "/work/file",
                &oversized_name,
                b"value".to_vec(),
                0,
                true,
            )
            .unwrap_err()
            .code(),
        "EINVAL"
    );
}

#[test]
fn xattr_value_limit_accepts_linux_boundary_and_rejects_plus_one_transactionally() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 1000).unwrap();
    let alice = process_as(&mut kernel, 1000);
    let exact = vec![b'x'; 64 * 1024];
    let oversized = vec![b'y'; 64 * 1024 + 1];

    kernel
        .set_xattr_for_process(
            DRIVER,
            alice,
            "/work/file",
            "user.path-limit",
            exact.clone(),
            1,
            true,
        )
        .expect("path xattr exact Linux boundary");
    let path_error = kernel
        .set_xattr_for_process(
            DRIVER,
            alice,
            "/work/file",
            "user.path-limit",
            oversized.clone(),
            2,
            true,
        )
        .expect_err("path xattr limit +1");
    assert_eq!(path_error.code(), "E2BIG");
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, alice, "/work/file", "user.path-limit", true)
            .expect("path xattr survives rejected replacement"),
        exact,
        "oversized path replacement must not partially mutate the prior value"
    );

    let fd = kernel
        .fd_open(DRIVER, alice, "/work/file", O_RDONLY, None)
        .expect("open xattr target");
    kernel
        .fd_set_xattr_for_process(DRIVER, alice, fd, "user.fd-limit", exact.clone(), 1)
        .expect("fd xattr exact Linux boundary");
    let fd_error = kernel
        .fd_set_xattr_for_process(DRIVER, alice, fd, "user.fd-limit", oversized, 2)
        .expect_err("fd xattr limit +1");
    assert_eq!(fd_error.code(), "E2BIG");
    assert_eq!(
        kernel
            .fd_get_xattr_for_process(DRIVER, alice, fd, "user.fd-limit")
            .expect("fd xattr survives rejected replacement"),
        exact,
        "oversized fd replacement must not partially mutate the prior value"
    );
}

#[test]
fn fd_xattrs_follow_the_open_description_after_unlink() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 1000).unwrap();
    let alice = process_as(&mut kernel, 1000);
    kernel
        .set_xattr_for_process(
            DRIVER,
            alice,
            "/work/file",
            "user.note",
            b"before".to_vec(),
            0,
            true,
        )
        .unwrap();
    let fd = kernel
        .fd_open(DRIVER, alice, "/work/file", O_RDONLY, None)
        .unwrap();

    kernel
        .remove_file_for_process(DRIVER, alice, "/work/file")
        .unwrap();
    assert_eq!(
        kernel
            .fd_get_xattr_for_process(DRIVER, alice, fd, "user.note")
            .unwrap(),
        b"before"
    );
    assert_eq!(
        kernel
            .fd_list_xattrs_for_process(DRIVER, alice, fd)
            .unwrap(),
        vec![String::from("user.note")]
    );
    kernel
        .fd_set_xattr_for_process(DRIVER, alice, fd, "user.note", b"after".to_vec(), 2)
        .unwrap();
    assert_eq!(
        kernel
            .fd_get_xattr_for_process(DRIVER, alice, fd, "user.note")
            .unwrap(),
        b"after"
    );
    kernel
        .fd_remove_xattr_for_process(DRIVER, alice, fd, "user.note")
        .unwrap();
    assert_eq!(
        kernel
            .fd_get_xattr_for_process(DRIVER, alice, fd, "user.note")
            .unwrap_err()
            .code(),
        "ENODATA"
    );
}

#[test]
fn xattrs_enforce_linux_inode_type_and_symlink_rules() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.write_file("/work/target", b"x".to_vec()).unwrap();
    kernel.symlink("target", "/work/link").unwrap();
    let root = process_as(&mut kernel, 0);

    kernel
        .set_xattr_for_process(
            DRIVER,
            root,
            "/work/link",
            "trusted.note",
            b"link".to_vec(),
            0,
            false,
        )
        .unwrap();
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, root, "/work/link", "trusted.note", false)
            .unwrap(),
        b"link"
    );
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, root, "/work/target", "trusted.note", true)
            .unwrap_err()
            .code(),
        "ENODATA"
    );
    assert_eq!(
        kernel
            .get_xattr_for_process(DRIVER, root, "/work/link", "user.missing", false)
            .unwrap_err()
            .code(),
        "ENODATA"
    );
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                root,
                "/work/link",
                "user.note",
                b"no".to_vec(),
                0,
                false,
            )
            .unwrap_err()
            .code(),
        "EPERM"
    );

    kernel
        .mknod_for_process(DRIVER, root, "/work/fifo", 0o010600, 0)
        .unwrap();
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                root,
                "/work/fifo",
                "user.note",
                b"no".to_vec(),
                0,
                true,
            )
            .unwrap_err()
            .code(),
        "EPERM"
    );
    kernel
        .set_xattr_for_process(
            DRIVER,
            root,
            "/work/fifo",
            "trusted.note",
            b"fifo".to_vec(),
            0,
            true,
        )
        .unwrap();

    kernel.mkdir("/work/target-dir", false).unwrap();
    kernel
        .write_file("/work/target-dir/child", b"child".to_vec())
        .unwrap();
    kernel.symlink("target-dir", "/work/parent-link").unwrap();
    kernel
        .set_xattr_for_process(
            DRIVER,
            root,
            "/work/parent-link/child",
            "trusted.parent",
            b"resolved".to_vec(),
            0,
            true,
        )
        .unwrap();
    assert_eq!(
        kernel
            .get_xattr_for_process(
                DRIVER,
                root,
                "/work/parent-link/child",
                "trusted.parent",
                true,
            )
            .unwrap(),
        b"resolved"
    );
}

#[test]
fn access_acl_enforces_named_entries_mask_and_chmod_synchronization() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"secret".to_vec()).unwrap();
    kernel.chown("/work/file", 1000, 1000).unwrap();
    kernel.chmod("/work/file", 0o600).unwrap();
    let alice = process_as(&mut kernel, 1000);
    let bob = process_as(&mut kernel, 1001);
    let carol = process_as(&mut kernel, 1002);

    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, bob, "/work/file")
            .unwrap_err()
            .code(),
        "EACCES",
        "a cached negative ACL lookup must be invalidated when an ACL is added",
    );

    kernel
        .set_xattr_for_process(
            DRIVER,
            alice,
            "/work/file",
            "system.posix_acl_access",
            extended_acl(0o4, 0o6),
            0,
            true,
        )
        .unwrap();
    assert_eq!(kernel.stat("/work/file").unwrap().mode & 0o777, 0o660);
    assert_eq!(
        kernel
            .read_file_for_process(DRIVER, bob, "/work/file")
            .unwrap(),
        b"secret"
    );
    assert_eq!(
        kernel
            .write_file_for_process(DRIVER, bob, "/work/file", b"no".to_vec(), None)
            .unwrap_err()
            .code(),
        "EACCES"
    );
    kernel
        .write_file_for_process(DRIVER, carol, "/work/file", b"group".to_vec(), None)
        .unwrap();

    kernel
        .chmod_for_process(DRIVER, alice, "/work/file", 0o640)
        .unwrap();
    assert_eq!(
        kernel
            .write_file_for_process(DRIVER, carol, "/work/file", b"no".to_vec(), None)
            .unwrap_err()
            .code(),
        "EACCES"
    );
    let stored = kernel
        .get_xattr("/work/file", "system.posix_acl_access", true)
        .unwrap();
    assert_eq!(stored, extended_acl(0o4, 0o4));
}

#[test]
fn default_acl_is_inherited_and_restricts_requested_mode_instead_of_using_umask() {
    let mut kernel = kernel();
    kernel.mkdir("/parent", false).unwrap();
    kernel.chown("/parent", 1000, 1000).unwrap();
    kernel.chmod("/parent", 0o770).unwrap();
    let alice = process_as(&mut kernel, 1000);
    let bob = process_as(&mut kernel, 1001);
    let parent_acl = acl(&[
        (ACL_USER_OBJ, 0o7, u32::MAX),
        (ACL_USER, 0o7, 1001),
        (ACL_GROUP_OBJ, 0, u32::MAX),
        (ACL_MASK, 0o7, u32::MAX),
        (ACL_OTHER, 0, u32::MAX),
    ]);
    for name in ["system.posix_acl_access", "system.posix_acl_default"] {
        kernel
            .set_xattr_for_process(DRIVER, alice, "/parent", name, parent_acl.clone(), 0, true)
            .unwrap();
    }
    kernel.umask(DRIVER, alice, Some(0o077)).unwrap();

    kernel
        .write_file_for_process(DRIVER, alice, "/parent/file", b"data".to_vec(), Some(0o666))
        .unwrap();
    assert_eq!(kernel.stat("/parent/file").unwrap().mode & 0o777, 0o660);
    kernel
        .write_file_for_process(DRIVER, bob, "/parent/file", b"bob".to_vec(), None)
        .unwrap();

    kernel
        .mkdir_for_process(DRIVER, alice, "/parent/dir", false, Some(0o777))
        .unwrap();
    assert_eq!(kernel.stat("/parent/dir").unwrap().mode & 0o777, 0o770);
    assert_eq!(
        kernel
            .get_xattr("/parent/dir", "system.posix_acl_default", true)
            .unwrap(),
        parent_acl
    );
}

#[test]
fn malformed_acls_and_symlink_mutation_are_rejected() {
    let mut kernel = kernel();
    kernel.mkdir("/work", false).unwrap();
    kernel.chmod("/work", 0o777).unwrap();
    kernel.write_file("/work/file", b"x".to_vec()).unwrap();
    kernel.symlink("/work/file", "/work/link").unwrap();
    let root = process_as(&mut kernel, 0);

    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                root,
                "/work/file",
                "system.posix_acl_access",
                vec![2, 0, 0, 0, 1],
                0,
                true,
            )
            .unwrap_err()
            .code(),
        "EINVAL"
    );

    let mut oversized_entries = vec![(ACL_USER_OBJ, 0o7, u32::MAX)];
    oversized_entries.extend((1..=22).map(|id| (ACL_USER, 0o7, id)));
    oversized_entries.extend([
        (ACL_GROUP_OBJ, 0o7, u32::MAX),
        (ACL_MASK, 0o7, u32::MAX),
        (ACL_OTHER, 0o7, u32::MAX),
    ]);
    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                root,
                "/work/file",
                "system.posix_acl_access",
                acl(&oversized_entries),
                0,
                true,
            )
            .unwrap_err()
            .code(),
        "E2BIG"
    );

    assert_eq!(
        kernel
            .set_xattr_for_process(
                DRIVER,
                root,
                "/work/link",
                "user.note",
                b"no".to_vec(),
                0,
                false,
            )
            .unwrap_err()
            .code(),
        "EPERM"
    );
}
