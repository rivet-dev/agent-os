#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use vfs::package_format::{
    encode_aospkg_header,
    generated::v1,
    parse_aospkg_header,
    versioned::{
        decode_mount_index, decode_package_manifest, encode_mount_index, encode_package_manifest,
    },
};
use vfs::posix::{TarFileSystem, VirtualFileSystem};

#[test]
fn package_format_round_trips_manifest_all_none_and_mount_index() {
    let manifest = v1::PackageManifest {
        name: String::from("empty"),
        version: String::from("1.0.0"),
        agent: None,
        provides: None,
        commands: Vec::new(),
        man_pages: Vec::new(),
        snapshot_bundle_path: None,
    };
    let decoded = decode_package_manifest(&encode_package_manifest(manifest.clone()).unwrap())
        .expect("decode manifest");
    assert_eq!(decoded, manifest);

    let index = v1::MountIndex {
        tar_entries: vec![v1::TarEntry {
            path: String::from("/"),
            kind: v1::TarEntryKind::Directory,
            offset: 0,
            size: 0,
            mode: 0o040755,
            uid: 1000,
            gid: 1234,
            mtime: 123,
            link_target: None,
        }],
    };
    let decoded = decode_mount_index(&encode_mount_index(index.clone()).unwrap()).unwrap();
    assert_eq!(decoded, index);
}

#[test]
fn package_format_round_trips_agent_metadata() {
    let manifest = v1::PackageManifest {
        name: "wasm-agent".into(),
        version: "1.0.0".into(),
        agent: Some(v1::AgentBlock {
            acp_entrypoint: "pi-acp".into(),
            snapshot: false,
            env: HashMap::new(),
            launch_args: Vec::new(),
        }),
        provides: None,
        commands: Vec::new(),
        man_pages: Vec::new(),
        snapshot_bundle_path: None,
    };
    let encoded = encode_package_manifest(manifest).expect("encode v1 manifest");
    assert_eq!(u16::from_le_bytes([encoded[0], encoded[1]]), 1);
    let decoded = decode_package_manifest(&encoded).expect("decode v1 manifest");
    assert_eq!(decoded.agent.expect("agent block").acp_entrypoint, "pi-acp");
}

#[test]
fn package_format_rejects_unknown_schema_version_and_corrupt_headers() {
    let mut bad_version = 2u16.to_le_bytes().to_vec();
    bad_version.extend_from_slice(&[]);
    let err = decode_package_manifest(&bad_version).unwrap_err();
    assert!(err.to_string().contains("decode package manifest"));

    assert!(parse_aospkg_header(b"short").is_err());

    let mut bad_magic = [0u8; 16];
    bad_magic[0..4].copy_from_slice(b"NOPE");
    bad_magic[4..6].copy_from_slice(&1u16.to_le_bytes());
    assert!(parse_aospkg_header(&bad_magic).is_err());

    let mut bad_format = [0u8; 16];
    bad_format[0..4].copy_from_slice(&[0x89, b'A', b'O', b'S']);
    bad_format[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert!(parse_aospkg_header(&bad_format).is_err());

    let mut oversized = [0u8; 16];
    oversized[0..4].copy_from_slice(&[0x89, b'A', b'O', b'S']);
    oversized[4..6].copy_from_slice(&1u16.to_le_bytes());
    oversized[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse_aospkg_header(&oversized).is_err());
}

#[test]
fn tar_filesystem_rejects_unsorted_index() {
    let path = unique_path("agentos-unsorted-aospkg");
    let manifest = encode_package_manifest(v1::PackageManifest {
        name: String::from("unsorted"),
        version: String::from("1.0.0"),
        agent: None,
        provides: None,
        commands: Vec::new(),
        man_pages: Vec::new(),
        snapshot_bundle_path: None,
    })
    .unwrap();
    let index = encode_mount_index(v1::MountIndex {
        tar_entries: vec![entry("/z"), entry("/a")],
    })
    .unwrap();
    let header = encode_aospkg_header(manifest.len(), index.len()).unwrap();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&header).unwrap();
    file.write_all(&manifest).unwrap();
    file.write_all(&index).unwrap();
    file.flush().unwrap();

    let err = match TarFileSystem::open(&path) {
        Ok(_) => panic!("unsorted index unexpectedly opened"),
        Err(error) => error,
    };
    assert!(err.to_string().contains("not sorted"), "{err}");
}

fn entry(path: &str) -> v1::TarEntry {
    v1::TarEntry {
        path: path.to_owned(),
        kind: v1::TarEntryKind::Directory,
        offset: 0,
        size: 0,
        mode: 0o040755,
        uid: 0,
        gid: 0,
        mtime: 0,
        link_target: None,
    }
}

fn unique_path(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("{prefix}-{nonce}.aospkg"));
    path
}

#[test]
fn pack_strips_agentos_package_json_and_uses_it_as_manifest_input() {
    use vfs::package_format::pack::pack_aospkg_from_tar_bytes;

    let mut builder = tar::Builder::new(Vec::<u8>::new());
    let manifest_json = br#"{"name":"demo","version":"2.1.0"}"#;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "agentos-package.json", &manifest_json[..])
        .unwrap();
    let tool = b"#!/bin/sh\necho demo\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(tool.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "bin/demo", &tool[..])
        .unwrap();
    let source_tar = builder.into_inner().unwrap();

    let (aospkg, summary) = pack_aospkg_from_tar_bytes(&source_tar).unwrap();
    assert_eq!(summary.name, "demo");
    assert_eq!(summary.version, "2.1.0");
    assert_eq!(summary.commands, vec![String::from("demo")]);

    let header = parse_aospkg_header(&aospkg).unwrap();
    let manifest = decode_package_manifest(&aospkg[header.manifest.clone()]).unwrap();
    assert_eq!(manifest.name, "demo");
    assert_eq!(manifest.version, "2.1.0");

    // The JSON is pack-time input only: it must not appear in the mount index
    // or the repacked mount tar.
    let index = decode_mount_index(&aospkg[header.index.clone()]).unwrap();
    assert!(index
        .tar_entries
        .iter()
        .all(|entry| entry.path != "/agentos-package.json"));
    assert!(index
        .tar_entries
        .iter()
        .any(|entry| entry.path == "/bin/demo"));
    let mut mount = tar::Archive::new(std::io::Cursor::new(&aospkg[header.mount.clone()]));
    for entry in mount.entries().unwrap() {
        let entry = entry.unwrap();
        assert_ne!(
            entry.path().unwrap().to_string_lossy(),
            "agentos-package.json"
        );
    }
}

/// Cross-language validation hook: point `AOSPKG_CROSS_CHECK` at a `.aospkg`
/// produced by the TS toolchain packer (`packages/agentos-toolchain/src/aospkg.ts`)
/// and this test decodes it with the Rust reader — both packers encode
/// `crates/vfs/package-format/v1.bare`, and this catches codec drift.
#[test]
fn cross_validates_toolchain_built_aospkg() {
    let Ok(path) = std::env::var("AOSPKG_CROSS_CHECK") else {
        eprintln!("AOSPKG_CROSS_CHECK not set; skipping cross-validation");
        return;
    };
    let bytes = std::fs::read(&path).expect("read cross-check aospkg");
    let header = parse_aospkg_header(&bytes).expect("parse header");
    let manifest = decode_package_manifest(&bytes[header.manifest.clone()])
        .expect("decode toolchain-built manifest");
    assert!(!manifest.name.is_empty());
    assert!(!manifest.version.is_empty());
    let index = decode_mount_index(&bytes[header.index.clone()])
        .expect("decode toolchain-built mount index");
    assert!(index
        .tar_entries
        .iter()
        .all(|entry| entry.path != "/agentos-package.json"));
    let mut fs = TarFileSystem::open(&path).expect("open toolchain-built aospkg");
    let root = fs.read_dir("/").expect("read_dir root");
    assert!(!root.is_empty());
    for target in &manifest.commands {
        let entry_path = format!("/{}", target.entry);
        let stat = fs
            .stat(&entry_path)
            .unwrap_or_else(|e| panic!("stat command entry {}: {e}", target.entry));
        assert!(!stat.is_directory);
        // Read actual bytes through the index offset: a wrong offset would pass
        // stat (metadata comes from the index) but return garbage content.
        let bytes = fs
            .read_file(&entry_path)
            .unwrap_or_else(|e| panic!("read command entry {}: {e}", target.entry));
        assert_eq!(bytes.len() as u64, stat.size);
        let is_wasm = bytes.starts_with(b"\0asm");
        let is_script = bytes.starts_with(b"#!");
        assert!(
            is_wasm || is_script,
            "command entry {} does not look like a wasm binary or script (first bytes: {:?})",
            target.entry,
            &bytes[..bytes.len().min(8)]
        );
    }
    eprintln!(
        "cross-validated {}@{} ({} commands, {} index entries)",
        manifest.name,
        manifest.version,
        manifest.commands.len(),
        index.tar_entries.len()
    );
}
