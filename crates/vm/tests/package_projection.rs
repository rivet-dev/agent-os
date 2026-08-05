use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use agentos_vfs_core::package_format::pack::pack_aospkg_from_tar;
use agentos_vm::package_projection::{
    build_package_leaf_mounts, package_provides_file_mount, read_package_manifest,
    read_package_manifest_from_path, PackageLeafMount, DEFAULT_PACKAGE_TAR_NAME,
};
use tar::Builder;

const SOURCE_TAR_NAME: &str = "package.mount.tar";

fn unique_dir(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("agentos-projtest-{tag}-{nonce}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_package(root: &Path, name: &str, version: &str, commands: &[&str]) {
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(
        root.join("agentos-package.json"),
        format!("{{\"name\":\"{name}\",\"version\":\"{version}\"}}"),
    )
    .unwrap();
    for cmd in commands {
        fs::write(
            root.join("bin").join(cmd),
            format!("#!/usr/bin/env node\n// {cmd}\n"),
        )
        .unwrap();
    }
}

fn finalize_package_tar(root: &Path) {
    let tar_path = root.join(SOURCE_TAR_NAME);
    let _ = fs::remove_file(&tar_path);
    let file = fs::File::create(&tar_path).unwrap();
    let mut builder = Builder::new(file);
    append_tree(&mut builder, root, root).unwrap();
    builder.finish().unwrap();
    builder.into_inner().unwrap().flush().unwrap();
    write_aospkg(root, &tar_path);
}

fn append_tree(builder: &mut Builder<fs::File>, root: &Path, path: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let file_name = entry_path.file_name().and_then(|name| name.to_str());
        if file_name == Some(DEFAULT_PACKAGE_TAR_NAME) || file_name == Some(SOURCE_TAR_NAME) {
            continue;
        }
        let name = entry_path.strip_prefix(root).unwrap();
        if entry_path.is_dir() {
            builder.append_dir(name, &entry_path)?;
            append_tree(builder, root, &entry_path)?;
        } else {
            builder.append_path_with_name(&entry_path, name)?;
        }
    }
    Ok(())
}

fn write_aospkg(root: &Path, source_tar: &Path) {
    pack_aospkg_from_tar(source_tar, &root.join(DEFAULT_PACKAGE_TAR_NAME))
        .unwrap_or_else(|e| panic!("pack {} failed: {e}", source_tar.display()));
}

#[test]
fn reads_version_from_agentos_package_json_and_errors_when_missing() {
    let pkg = unique_dir("ver");
    write_package(&pkg, "vt", "3.1.4", &["vt"]);
    finalize_package_tar(&pkg);
    assert_eq!(
        read_package_manifest(pkg.to_str().unwrap())
            .unwrap()
            .version,
        "3.1.4"
    );

    let empty = unique_dir("ver-missing");
    assert!(read_package_manifest(empty.to_str().unwrap()).is_err());
}

#[test]
fn reads_name_agent_and_provides_from_agentos_package_json() {
    let pkg = unique_dir("manifest");
    write_package(&pkg, "package-json-name", "1.0.0", &["agent-cmd"]);
    fs::create_dir_all(pkg.join("share/config")).unwrap();
    fs::write(
        pkg.join("agentos-package.json"),
        r#"{
          "name": "manifest-name",
          "version": "1.0.0",
          "agent": { "acpEntrypoint": "agent-cmd" },
          "provides": {
            "env": { "FROM_MANIFEST": "yes" },
            "files": [{ "source": "share/config", "target": "/etc/manifest" }]
          }
        }"#,
    )
    .unwrap();
    finalize_package_tar(&pkg);

    let descriptor = read_package_manifest(pkg.to_str().unwrap()).unwrap();
    assert_eq!(descriptor.name, "manifest-name");
    assert_eq!(descriptor.version, "1.0.0");
    assert_eq!(descriptor.acp_entrypoint.as_deref(), Some("agent-cmd"));
    let provides = descriptor.provides.as_ref().expect("provides");
    assert_eq!(
        provides.env.get("FROM_MANIFEST").map(String::as_str),
        Some("yes")
    );
    assert_eq!(provides.files[0].target, "/etc/manifest");
}

#[test]
fn derives_commands_from_bin_dir() {
    let pkg = unique_dir("cmds");
    write_package(&pkg, "tool", "1.0.0", &["foo", "bar"]);
    finalize_package_tar(&pkg);
    let mut commands = read_package_manifest(pkg.to_str().unwrap())
        .unwrap()
        .commands
        .into_iter()
        .map(|target| target.command)
        .collect::<Vec<_>>();
    commands.sort();
    assert_eq!(commands, vec!["bar".to_string(), "foo".to_string()]);
}

#[test]
fn reads_manifest_and_commands_from_package_tar_without_extracting() {
    let pkg = unique_dir("tar-src");
    write_package(&pkg, "demo", "2.0.0", &["demo"]);
    finalize_package_tar(&pkg);

    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();
    assert_eq!(descriptor.name, "demo");
    assert_eq!(descriptor.version, "2.0.0");
    assert!(descriptor
        .tar_path
        .as_deref()
        .unwrap()
        .ends_with(DEFAULT_PACKAGE_TAR_NAME));
    assert_eq!(descriptor.commands[0].command, "demo");
}

#[test]
fn reads_symlink_commands_from_package_tar() {
    let pkg = unique_dir("tar-symlink-cmd");
    write_package(&pkg, "demo", "2.0.0", &[]);
    fs::write(pkg.join("adapter.mjs"), "console.log('ok');").unwrap();
    std::os::unix::fs::symlink("../adapter.mjs", pkg.join("bin/demo")).unwrap();
    finalize_package_tar(&pkg);

    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();
    assert_eq!(descriptor.commands[0].command, "demo");
    assert_eq!(descriptor.commands[0].entry, "bin/demo");
}

#[test]
fn builds_tar_current_bin_and_manpage_leaf_mounts() {
    let pkg = unique_dir("mounts-src");
    write_package(&pkg, "demo", "2.0.0", &["demo"]);
    fs::create_dir_all(pkg.join("share/man/man1")).unwrap();
    fs::write(pkg.join("share/man/man1/demo.1"), "manual").unwrap();
    finalize_package_tar(&pkg);
    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();

    let mounts = build_package_leaf_mounts(&[descriptor], "/opt/agentos").unwrap();
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::Tar { guest_path, root, .. }
            if guest_path == "/opt/agentos/pkgs/demo/2.0.0" && root == "/"
    )));
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::SingleSymlink { guest_path, target }
            if guest_path == "/opt/agentos/pkgs/demo/current" && target == "2.0.0"
    )));
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::SingleSymlink { guest_path, target }
            if guest_path == "/opt/agentos/bin/demo"
                && target == "../pkgs/demo/current/bin/demo"
    )));
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::SingleSymlink { guest_path, target }
            if guest_path == "/opt/agentos/share/man/man1/demo.1"
                && target == "../../../pkgs/demo/current/share/man/man1/demo.1"
    )));
}

#[test]
fn builds_host_dir_leaf_mount_for_dir_only_transition_packages() {
    let pkg = unique_dir("dir-only");
    write_package(&pkg, "demo", "2.0.0", &["demo"]);
    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();

    let mounts = build_package_leaf_mounts(&[descriptor], "/opt/agentos").unwrap();
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::HostDir { guest_path, host_path }
            if guest_path == "/opt/agentos/pkgs/demo/2.0.0"
                && host_path == pkg.to_str().unwrap()
    )));
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::SingleSymlink { guest_path, target }
            if guest_path == "/opt/agentos/pkgs/demo/current" && target == "2.0.0"
    )));
    assert!(mounts.iter().any(|mount| matches!(
        mount,
        PackageLeafMount::SingleSymlink { guest_path, target }
            if guest_path == "/opt/agentos/bin/demo"
                && target == "../pkgs/demo/current/bin/demo"
    )));
}

#[test]
fn duplicate_commands_are_rejected_before_mounting() {
    let pkg_a = unique_dir("dup-a");
    write_package(&pkg_a, "a", "1.0.0", &["tool"]);
    finalize_package_tar(&pkg_a);
    let pkg_b = unique_dir("dup-b");
    write_package(&pkg_b, "b", "1.0.0", &["tool"]);
    finalize_package_tar(&pkg_b);

    let a = read_package_manifest_from_path(pkg_a.to_str().unwrap()).unwrap();
    let b = read_package_manifest_from_path(pkg_b.to_str().unwrap()).unwrap();
    let err = build_package_leaf_mounts(&[a, b], "/opt/agentos").unwrap_err();
    assert!(err.to_string().contains("already provided"), "{err}");
}

#[test]
fn invalid_agent_entrypoint_is_rejected() {
    let pkg = unique_dir("bad-agent");
    write_package(&pkg, "agent", "1.0.0", &["real"]);
    fs::write(
        pkg.join("agentos-package.json"),
        r#"{"name":"agent","version":"1.0.0","agent":{"acpEntrypoint":"missing"}}"#,
    )
    .unwrap();
    finalize_package_tar(&pkg);

    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();
    let err = build_package_leaf_mounts(&[descriptor], "/opt/agentos").unwrap_err();
    assert!(err.to_string().contains("acpEntrypoint"), "{err}");
}

#[test]
fn provides_files_mounts_tar_subtree() {
    let pkg = unique_dir("provides");
    write_package(&pkg, "provider", "1.0.0", &["provider"]);
    fs::create_dir_all(pkg.join("share/config")).unwrap();
    fs::write(pkg.join("share/config/settings.json"), "{}").unwrap();
    fs::write(
        pkg.join("agentos-package.json"),
        r#"{
          "name": "provider",
          "version": "1.0.0",
          "provides": {
            "files": [{ "source": "share/config", "target": "/etc/provider" }]
          }
        }"#,
    )
    .unwrap();
    finalize_package_tar(&pkg);

    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();
    let mount = package_provides_file_mount(&descriptor, "share/config", "/etc/provider")
        .unwrap()
        .expect("provides dir mount");
    assert!(matches!(
        mount,
        PackageLeafMount::Tar { guest_path, root, .. }
            if guest_path == "/etc/provider" && root == "/share/config"
    ));
}

#[test]
fn provides_files_mounts_host_dir_subtree_for_dir_only_packages() {
    let pkg = unique_dir("provides-dir");
    write_package(&pkg, "provider", "1.0.0", &["provider"]);
    fs::create_dir_all(pkg.join("share/config")).unwrap();
    fs::write(pkg.join("share/config/settings.json"), "{}").unwrap();

    let descriptor = read_package_manifest_from_path(pkg.to_str().unwrap()).unwrap();
    let mount = package_provides_file_mount(&descriptor, "share/config", "/etc/provider")
        .unwrap()
        .expect("provides dir mount");
    assert!(matches!(
        mount,
        PackageLeafMount::HostDir { guest_path, host_path }
            if guest_path == "/etc/provider"
                && host_path == pkg.join("share/config").to_string_lossy()
    ));
}
