#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tar::{Builder, EntryType, Header};
use vfs::package_format::{
    encode_aospkg_header,
    generated::v1,
    versioned::{encode_mount_index, encode_package_manifest},
};
use vfs::posix::{TarFileSystem, VirtualFileSystem};

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

#[test]
fn tar_filesystem_reads_files_dirs_symlinks_and_realpaths() {
    let tar_path = write_fixture_tar();
    let mut fs = TarFileSystem::open(&tar_path).expect("open tar filesystem");

    assert_eq!(
        fs.read_file("/pkg/bin/pi").expect("read file"),
        b"#!/bin/sh\necho pi\n".to_vec()
    );

    let root_entries = fs.read_dir("/pkg").expect("read package dir");
    assert!(root_entries.contains(&String::from("bin")));
    assert!(root_entries.contains(&String::from("lib")));
    assert!(!root_entries.contains(&String::from("pi")));

    let typed_entries = fs
        .read_dir_with_types("/pkg/lib")
        .expect("read typed package dir");
    assert!(typed_entries
        .iter()
        .any(|entry| entry.name == "target.txt" && !entry.is_directory));
    assert!(typed_entries
        .iter()
        .any(|entry| entry.name == "target-link.txt" && entry.is_symbolic_link));

    assert_eq!(
        fs.read_link("/pkg/lib/target-link.txt")
            .expect("read symlink"),
        "target.txt"
    );
    assert_eq!(
        fs.realpath("/pkg/lib/target-link.txt")
            .expect("resolve symlink"),
        "/pkg/lib/target.txt"
    );
    assert_eq!(
        fs.read_file("/pkg/lib/target-link.txt")
            .expect("read through symlink"),
        b"target\n".to_vec()
    );

    let stat = fs.stat("/pkg/bin/pi").expect("stat executable");
    assert_eq!(stat.mode & 0o777, 0o755);
    assert!(fs.exists("/pkg/lib/target.txt"));
    assert!(!fs.exists("/pkg/missing"));
}

#[test]
fn tar_filesystem_rejects_writes_as_read_only() {
    let tar_path = write_fixture_tar();
    let mut fs = TarFileSystem::open(&tar_path).expect("open tar filesystem");

    let error = fs
        .write_file("/pkg/new.txt", b"nope".to_vec())
        .expect_err("tar filesystem is read-only");
    assert_eq!(error.code(), "EROFS");
}

#[test]
fn tar_filesystem_cache_uses_file_identity_and_stable_guest_device() {
    let tar_path = write_fixture_tar();
    let fs_a = TarFileSystem::open(&tar_path).expect("open tar filesystem");
    let fs_b = TarFileSystem::open(&tar_path).expect("open tar filesystem again");
    assert_eq!(fs_a.archive_ptr(), fs_b.archive_ptr());

    let mut stat_a = TarFileSystem::open(&tar_path)
        .expect("reopen tar filesystem")
        .stat("/pkg/bin/pi")
        .expect("stat executable");
    let stat_b = TarFileSystem::open(&tar_path)
        .expect("reopen tar filesystem")
        .stat("/pkg/lib/target.txt")
        .expect("stat file");
    assert_ne!(stat_a.dev, 0);
    assert_eq!(stat_a.dev, stat_b.dev);

    let other_tar = write_fixture_tar();
    let other_stat = TarFileSystem::open(&other_tar)
        .expect("open other tar filesystem")
        .stat("/pkg/bin/pi")
        .expect("stat other executable");
    assert_ne!(stat_a.dev, other_stat.dev);

    let same_bytes = std::fs::read(&tar_path).expect("read fixture tar");
    let copied_tar = unique_tar_path("agentos-tar-fs-copy");
    std::fs::write(&copied_tar, same_bytes).expect("copy fixture tar bytes");
    let fs_copy = TarFileSystem::open(&copied_tar).expect("open copied tar filesystem");
    assert_ne!(fs_a.archive_ptr(), fs_copy.archive_ptr());

    stat_a = TarFileSystem::open(&tar_path)
        .expect("reopen original tar filesystem")
        .stat("/pkg/bin/pi")
        .expect("stat original executable");
    assert_ne!(stat_a.dev, other_stat.dev);
}

fn write_fixture_tar() -> PathBuf {
    let path = unique_tar_path("agentos-tar-fs-fixture");
    write_fixture_tar_at(path)
}

fn unique_tar_path(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    path.push(format!("{prefix}-{nonce}.aospkg"));
    path
}

fn write_fixture_tar_at(path: PathBuf) -> PathBuf {
    let source_tar = path.with_extension("mount.tar");
    let file = File::create(&source_tar).expect("create fixture tar");
    let mut builder = Builder::new(file);
    append_dir(&mut builder, "pkg");
    append_dir(&mut builder, "pkg/bin");
    append_file(&mut builder, "pkg/bin/pi", b"#!/bin/sh\necho pi\n", 0o755);
    append_dir(&mut builder, "pkg/lib");
    append_file(&mut builder, "pkg/lib/target.txt", b"target\n", 0o644);
    append_symlink(&mut builder, "pkg/lib/target-link.txt", "target.txt");
    builder.finish().expect("finish fixture tar");
    builder
        .into_inner()
        .expect("finish file")
        .flush()
        .expect("flush tar");

    write_aospkg(&source_tar, &path);
    path
}

#[derive(Clone)]
struct IndexedEntry {
    kind: v1::TarEntryKind,
    offset: u64,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    link_target: Option<String>,
}

fn write_aospkg(source_tar: &PathBuf, dest: &PathBuf) {
    let source_bytes = std::fs::read(source_tar).expect("read source tar");
    let index = scan_tar_index(source_tar);
    let manifest = v1::PackageManifest {
        name: String::from("fixture"),
        version: String::from("1.0.0"),
        agent: None,
        provides: None,
        commands: Vec::new(),
        man_pages: Vec::new(),
        snapshot_bundle_path: None,
    };
    let manifest_bytes = encode_package_manifest(manifest).expect("encode manifest");
    let index_bytes = encode_mount_index(index).expect("encode index");
    let header = encode_aospkg_header(manifest_bytes.len(), index_bytes.len()).expect("header");
    let mut file = File::create(dest).expect("create aospkg");
    file.write_all(&header).expect("write header");
    file.write_all(&manifest_bytes).expect("write manifest");
    file.write_all(&index_bytes).expect("write index");
    file.write_all(&source_bytes).expect("write mount tar");
    file.flush().expect("flush aospkg");
}

fn scan_tar_index(source_tar: &PathBuf) -> v1::MountIndex {
    let file = File::open(source_tar).expect("open source tar");
    let mut archive = tar::Archive::new(file);
    let mut entries = BTreeMap::<String, IndexedEntry>::new();
    entries.insert(
        String::from("/"),
        IndexedEntry {
            kind: v1::TarEntryKind::Directory,
            offset: 0,
            size: 0,
            mode: S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            mtime: 0,
            link_target: None,
        },
    );
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = canonical_tar_path(&entry);
        let header = entry.header().clone();
        let entry_type = header.entry_type();
        let mode = header.mode().unwrap_or(0o755) & 0o7777;
        let uid = header.uid().unwrap_or(0) as u32;
        let gid = header.gid().unwrap_or(0) as u32;
        let mtime = header.mtime().unwrap_or(0) as i64;
        let indexed = if entry_type.is_dir() {
            Some(IndexedEntry {
                kind: v1::TarEntryKind::Directory,
                offset: 0,
                size: 0,
                mode: S_IFDIR | mode,
                uid,
                gid,
                mtime,
                link_target: None,
            })
        } else if entry_type.is_symlink() {
            Some(IndexedEntry {
                kind: v1::TarEntryKind::Symlink,
                offset: 0,
                size: 0,
                mode: S_IFLNK | mode.max(0o777),
                uid,
                gid,
                mtime,
                link_target: Some(
                    entry
                        .link_name()
                        .unwrap()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ),
            })
        } else if entry_type.is_file() || entry_type == EntryType::Continuous {
            let offset = entry.raw_file_position();
            let size = header.size().unwrap_or(0);
            let mut drain = Vec::new();
            let _ = entry.read_to_end(&mut drain);
            Some(IndexedEntry {
                kind: v1::TarEntryKind::File,
                offset,
                size,
                mode: S_IFREG | mode,
                uid,
                gid,
                mtime,
                link_target: None,
            })
        } else {
            None
        };
        if let Some(indexed) = indexed {
            synthesize_parent_dirs(&path, &mut entries);
            entries.insert(path, indexed);
        }
    }
    v1::MountIndex {
        tar_entries: entries
            .into_iter()
            .map(|(path, entry)| v1::TarEntry {
                path,
                kind: entry.kind,
                offset: entry.offset,
                size: entry.size,
                mode: entry.mode,
                uid: entry.uid,
                gid: entry.gid,
                mtime: entry.mtime,
                link_target: entry.link_target,
            })
            .collect(),
    }
}

fn canonical_tar_path(entry: &tar::Entry<'_, File>) -> String {
    let path = entry.path().expect("path");
    let parts = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            std::path::Component::CurDir => None,
            _ => panic!("tar path escapes root: {}", path.display()),
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::from("/")
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn synthesize_parent_dirs(path: &str, entries: &mut BTreeMap<String, IndexedEntry>) {
    let components = path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut current = String::from("/");
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current = if current == "/" {
            format!("/{component}")
        } else {
            format!("{current}/{component}")
        };
        entries.entry(current.clone()).or_insert(IndexedEntry {
            kind: v1::TarEntryKind::Directory,
            offset: 0,
            size: 0,
            mode: S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            mtime: 0,
            link_target: None,
        });
    }
}

fn append_dir(builder: &mut Builder<File>, path: &str) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::empty())
        .expect("append directory");
}

fn append_file(builder: &mut Builder<File>, path: &str, content: &[u8], mode: u32) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(content.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, path, content)
        .expect("append file");
}

fn append_symlink(builder: &mut Builder<File>, path: &str, target: &str) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Symlink);
    header.set_mode(0o777);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(0);
    header.set_link_name(target).expect("set link target");
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::empty())
        .expect("append symlink");
}
