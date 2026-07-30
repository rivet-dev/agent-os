//! Canonical `.aospkg` packer: source `package.tar` → `header + vbare manifest
//! + vbare mount index + filtered mount.tar`.
//!
//! `agentos-package.json` is a **pack-time input only**. It is parsed here to
//! build the chunk1 vbare `PackageManifest` and then stripped from the packed
//! mount tar — the vbare manifest is the single runtime manifest, and nothing
//! materializes JSON back into the guest. A root `package.json` (npm) stays in
//! the mount tar because Node module resolution may need it at runtime; its
//! `bin` field is only consulted to derive command targets.
//!
//! Packing happens once at package build time (toolchain / bench / test
//! fixtures). It is never on the VM load path.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use super::generated::v1;
use super::versioned::{encode_mount_index, encode_package_manifest};
use super::{encode_aospkg_header, AOSPKG_HEADER_LEN};
use crate::posix::vfs::{VfsError, VfsResult};

/// The toolchain-input manifest name. Pack-time input only; never shipped.
pub const MANIFEST_JSON_NAME: &str = "agentos-package.json";
/// Canonical in-package snapshot bundle path for snapshot-enabled agents.
pub const SNAPSHOT_BUNDLE_PATH: &str = "/dist/sdk-snapshot.js";
/// Pack-time mirror of the load-side index cap in `posix::tar_fs`
/// (`MAX_TAR_INDEX_ENTRIES`): fail at pack time, where the limit can be fixed,
/// instead of at every consumer's VM configure.
pub const MAX_PACK_INDEX_ENTRIES: usize = 200_000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

/// Result of packing: identity + derived command names (sorted).
#[derive(Debug, Clone)]
pub struct PackSummary {
    pub name: String,
    pub version: String,
    pub commands: Vec<String>,
}

#[derive(serde::Deserialize)]
struct SourceManifestJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    agent: Option<SourceAgentJson>,
    #[serde(default)]
    provides: Option<SourceProvidesJson>,
}

#[derive(serde::Deserialize)]
struct SourceAgentJson {
    #[serde(rename = "acpEntrypoint")]
    acp_entrypoint: String,
    #[serde(default)]
    snapshot: bool,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    #[serde(default, rename = "launchArgs")]
    launch_args: Vec<String>,
}

#[derive(serde::Deserialize)]
struct SourceProvidesJson {
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    #[serde(default)]
    files: Vec<SourceProvidesFileJson>,
}

#[derive(serde::Deserialize)]
struct SourceProvidesFileJson {
    source: String,
    target: String,
}

/// Parse toolchain-input `agentos-package.json` bytes into a chunk1
/// `PackageManifest`, with commands/man-pages/snapshot path supplied by the
/// caller (packed: derived from the mount index; transition dir: from a dir
/// scan). One JSON schema definition serves both pipelines.
pub fn manifest_json_to_v1(
    manifest_json: &[u8],
    commands: Vec<v1::CommandTarget>,
    man_pages: Vec<v1::ManPage>,
    snapshot_bundle_path: Option<String>,
) -> VfsResult<v1::PackageManifest> {
    let source: SourceManifestJson = serde_json::from_slice(manifest_json)
        .map_err(|e| VfsError::new("EINVAL", format!("invalid {MANIFEST_JSON_NAME}: {e}")))?;
    if source.name.is_empty() {
        return Err(VfsError::new(
            "EINVAL",
            format!("{MANIFEST_JSON_NAME} is missing a valid \"name\""),
        ));
    }
    if source.version.is_empty() {
        return Err(VfsError::new(
            "EINVAL",
            format!("{MANIFEST_JSON_NAME} is missing a valid \"version\""),
        ));
    }
    Ok(v1::PackageManifest {
        name: source.name,
        version: source.version,
        agent: source.agent.map(|agent| v1::AgentBlock {
            acp_entrypoint: agent.acp_entrypoint,
            snapshot: agent.snapshot,
            env: agent.env,
            launch_args: agent.launch_args,
        }),
        provides: source.provides.map(|provides| v1::ProvidesBlock {
            env: provides.env,
            files: provides
                .files
                .into_iter()
                .map(|file| v1::ProvidesFile {
                    source: file.source,
                    target: file.target,
                })
                .collect(),
        }),
        commands,
        man_pages,
        snapshot_bundle_path,
    })
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

/// Pack the source tar at `source_tar` into a `.aospkg` at `dest`. The source
/// `agentos-package.json` must carry `name` and `version`.
pub fn pack_aospkg_from_tar(source_tar: &Path, dest: &Path) -> VfsResult<PackSummary> {
    let source_bytes = std::fs::read(source_tar).map_err(|e| {
        VfsError::new(
            "EIO",
            format!("read source tar {}: {e}", source_tar.display()),
        )
    })?;
    let (aospkg_bytes, summary) = pack_aospkg_from_tar_bytes(&source_bytes)?;
    std::fs::write(dest, aospkg_bytes)
        .map_err(|e| VfsError::new("EIO", format!("write {}: {e}", dest.display())))?;
    Ok(summary)
}

/// In-memory variant of [`pack_aospkg_from_tar`].
pub fn pack_aospkg_from_tar_bytes(source_tar: &[u8]) -> VfsResult<(Vec<u8>, PackSummary)> {
    // Pass 1: read every entry, capture the pack-time JSON inputs, and rebuild
    // the mount tar WITHOUT agentos-package.json. Index offsets must refer to
    // the rebuilt tar, so it is written first and scanned second.
    let mut manifest_json = None::<Vec<u8>>;
    let mut package_json = None::<Vec<u8>>;
    let mut builder = tar::Builder::new(Vec::<u8>::new());
    {
        let mut archive = tar::Archive::new(Cursor::new(source_tar));
        for entry in archive
            .entries()
            .map_err(|e| VfsError::new("EINVAL", format!("read source tar entries: {e}")))?
        {
            let mut entry = entry
                .map_err(|e| VfsError::new("EINVAL", format!("read source tar entry: {e}")))?;
            let path = canonical_tar_path_of(&entry)?;
            let header = entry.header().clone();
            let entry_type = header.entry_type();
            if path == "/" {
                continue;
            }
            if path == format!("/{MANIFEST_JSON_NAME}") {
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| VfsError::new("EIO", format!("read {MANIFEST_JSON_NAME}: {e}")))?;
                manifest_json = Some(bytes);
                continue; // stripped: the vbare manifest is the runtime manifest
            }
            let rel = path.trim_start_matches('/').to_owned();
            if entry_type.is_dir() {
                let mut out = header.clone();
                // Some producers record a nonzero size on dir headers; zero it
                // or the rebuilt tar frames N phantom data blocks and every
                // subsequent index offset is wrong.
                out.set_size(0);
                builder
                    .append_data(&mut out, &rel, std::io::empty())
                    .map_err(|e| VfsError::new("EIO", format!("repack dir {rel}: {e}")))?;
            } else if entry_type.is_symlink() {
                let target = entry
                    .link_name()
                    .map_err(|e| VfsError::new("EINVAL", format!("symlink target {rel}: {e}")))?
                    .ok_or_else(|| VfsError::new("EINVAL", format!("symlink {rel} has no target")))?
                    .into_owned();
                let mut out = header.clone();
                out.set_size(0);
                builder
                    .append_link(&mut out, &rel, &target)
                    .map_err(|e| VfsError::new("EIO", format!("repack symlink {rel}: {e}")))?;
            } else if entry_type.is_file() || entry_type == tar::EntryType::Continuous {
                let mut bytes = Vec::with_capacity(header.size().unwrap_or(0) as usize);
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| VfsError::new("EIO", format!("read member {rel}: {e}")))?;
                if path == "/package.json" {
                    package_json = Some(bytes.clone());
                }
                let mut out = header.clone();
                out.set_size(bytes.len() as u64);
                builder
                    .append_data(&mut out, &rel, Cursor::new(bytes))
                    .map_err(|e| VfsError::new("EIO", format!("repack file {rel}: {e}")))?;
            }
            // Other entry kinds (hardlinks, devices, fifos) are not part of the
            // package surface and are dropped, matching the index scanner.
        }
    }
    let mount_tar = builder
        .into_inner()
        .map_err(|e| VfsError::new("EIO", format!("finish repacked mount tar: {e}")))?;

    // Pass 2: index the rebuilt tar.
    let entries = scan_tar_index(&mount_tar)?;
    if entries.len() > MAX_PACK_INDEX_ENTRIES {
        return Err(VfsError::new(
            "EOVERFLOW",
            format!(
                "package mount index has {} entries > MAX_PACK_INDEX_ENTRIES ({MAX_PACK_INDEX_ENTRIES}); \
                 the load-side TarFileSystem cap would reject this package at VM configure — \
                 split the package or raise both limits together",
                entries.len()
            ),
        ));
    }

    let manifest_json = manifest_json.ok_or_else(|| {
        VfsError::new(
            "EINVAL",
            format!("source tar must contain /{MANIFEST_JSON_NAME}"),
        )
    })?;
    // Peek at the agent block for the snapshot decision; the authoritative
    // JSON-to-v1 conversion is shared with transition-directory projection.
    let source: SourceManifestJson = serde_json::from_slice(&manifest_json)
        .map_err(|e| VfsError::new("EINVAL", format!("invalid {MANIFEST_JSON_NAME}: {e}")))?;
    let commands = command_targets(&entries, package_json.as_deref());
    let man_pages = man_pages_from_index(&entries);
    let snapshot_bundle_path = source
        .agent
        .as_ref()
        .filter(|agent| agent.snapshot)
        .and_then(|_| {
            entries
                .contains_key(SNAPSHOT_BUNDLE_PATH)
                .then(|| SNAPSHOT_BUNDLE_PATH.to_owned())
        });

    let command_names = commands
        .iter()
        .map(|target| target.command.clone())
        .collect::<Vec<_>>();
    let manifest = manifest_json_to_v1(&manifest_json, commands, man_pages, snapshot_bundle_path)?;
    let (name, version) = (manifest.name.clone(), manifest.version.clone());

    let tar_entries = entries
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
        .collect();

    let manifest_bytes = encode_package_manifest(manifest)
        .map_err(|e| VfsError::new("EINVAL", format!("encode package manifest: {e}")))?;
    let index_bytes = encode_mount_index(v1::MountIndex { tar_entries })
        .map_err(|e| VfsError::new("EINVAL", format!("encode mount index: {e}")))?;
    let header = encode_aospkg_header(manifest_bytes.len(), index_bytes.len())?;

    let mut out = Vec::with_capacity(
        AOSPKG_HEADER_LEN + manifest_bytes.len() + index_bytes.len() + mount_tar.len(),
    );
    out.write_all(&header).expect("vec write");
    out.write_all(&manifest_bytes).expect("vec write");
    out.write_all(&index_bytes).expect("vec write");
    out.write_all(&mount_tar).expect("vec write");
    Ok((
        out,
        PackSummary {
            name,
            version,
            commands: command_names,
        },
    ))
}

fn scan_tar_index(mount_tar: &[u8]) -> VfsResult<BTreeMap<String, IndexedEntry>> {
    let mut archive = tar::Archive::new(Cursor::new(mount_tar));
    let mut entries = BTreeMap::<String, IndexedEntry>::new();
    for entry in archive
        .entries()
        .map_err(|e| VfsError::new("EINVAL", format!("scan repacked tar: {e}")))?
    {
        let entry =
            entry.map_err(|e| VfsError::new("EINVAL", format!("scan repacked tar entry: {e}")))?;
        let path = canonical_tar_path_of(&entry)?;
        if path == "/" {
            continue;
        }
        let header = entry.header();
        let entry_type = header.entry_type();
        let mode = header.mode().unwrap_or(0o755) & 0o7777;
        let uid = header.uid().unwrap_or(0) as u32;
        let gid = header.gid().unwrap_or(0) as u32;
        let mtime = header.mtime().unwrap_or(0) as i64;
        let size = header.size().unwrap_or(0);
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
            let target = entry
                .link_name()
                .map_err(|e| VfsError::new("EINVAL", format!("symlink target {path}: {e}")))?
                .ok_or_else(|| VfsError::new("EINVAL", format!("symlink {path} has no target")))?
                .to_string_lossy()
                .into_owned();
            Some(IndexedEntry {
                kind: v1::TarEntryKind::Symlink,
                offset: 0,
                size: 0,
                mode: S_IFLNK | mode.max(0o777),
                uid,
                gid,
                mtime,
                link_target: Some(target),
            })
        } else if entry_type.is_file() || entry_type == tar::EntryType::Continuous {
            Some(IndexedEntry {
                kind: v1::TarEntryKind::File,
                offset: entry.raw_file_position(),
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
    entries.entry(String::from("/")).or_insert(IndexedEntry {
        kind: v1::TarEntryKind::Directory,
        offset: 0,
        size: 0,
        mode: S_IFDIR | 0o755,
        uid: 0,
        gid: 0,
        mtime: 0,
        link_target: None,
    });
    Ok(entries)
}

fn canonical_tar_path_of<R: Read>(entry: &tar::Entry<'_, R>) -> VfsResult<String> {
    let path = entry
        .path()
        .map_err(|e| VfsError::new("EINVAL", format!("read tar member path: {e}")))?;
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            _ => {
                return Err(VfsError::new(
                    "EINVAL",
                    format!("tar member path escapes root: {}", path.display()),
                ))
            }
        }
    }
    if parts.is_empty() {
        Ok(String::from("/"))
    } else {
        Ok(format!("/{}", parts.join("/")))
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

fn command_targets(
    entries: &BTreeMap<String, IndexedEntry>,
    package_json: Option<&[u8]>,
) -> Vec<v1::CommandTarget> {
    if let Some(bytes) = package_json {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
            if let Some(targets) = command_targets_from_package_json(&value) {
                return targets;
            }
        }
    }
    let mut commands = entries
        .keys()
        .filter_map(|path| {
            let name = path.strip_prefix("/bin/")?;
            (!name.contains('/') && is_projectable_command_name(name)).then(|| v1::CommandTarget {
                command: name.to_owned(),
                entry: format!("bin/{name}"),
            })
        })
        .collect::<Vec<_>>();
    commands.sort_by(|a, b| a.command.cmp(&b.command));
    commands
}

/// Derive command targets from a root npm `package.json` `bin` field. Shared
/// with the sidecar's transition-dir projection so packed and dir packages
/// derive identical command sets.
pub fn command_targets_from_package_json(
    value: &serde_json::Value,
) -> Option<Vec<v1::CommandTarget>> {
    match value.get("bin") {
        Some(serde_json::Value::String(path)) => {
            let name = value.get("name").and_then(|v| v.as_str())?;
            let unscoped = name.rsplit('/').next().unwrap_or(name).to_owned();
            Some(
                is_projectable_command_name(&unscoped)
                    .then(|| v1::CommandTarget {
                        command: unscoped,
                        entry: normalize_rel(path),
                    })
                    .into_iter()
                    .collect(),
            )
        }
        Some(serde_json::Value::Object(map)) => {
            let mut targets = map
                .iter()
                .filter_map(|(name, path)| {
                    is_projectable_command_name(name)
                        .then(|| path.as_str())
                        .flatten()
                        .map(|path| v1::CommandTarget {
                            command: name.clone(),
                            entry: normalize_rel(path),
                        })
                })
                .collect::<Vec<_>>();
            targets.sort_by(|a, b| a.command.cmp(&b.command));
            Some(targets)
        }
        _ => None,
    }
}

fn man_pages_from_index(entries: &BTreeMap<String, IndexedEntry>) -> Vec<v1::ManPage> {
    let mut pages = entries
        .keys()
        .filter_map(|path| {
            let suffix = path.strip_prefix("/share/man/")?;
            let (section, page) = suffix.split_once('/')?;
            (!page.contains('/')).then(|| v1::ManPage {
                section: section.to_owned(),
                page: page.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    pages.sort_by(|a, b| (&a.section, &a.page).cmp(&(&b.section, &b.page)));
    pages
}

pub fn is_projectable_command_name(name: &str) -> bool {
    !name.starts_with('_') && !name.starts_with('.')
}

fn normalize_rel(path: &str) -> String {
    path.strip_prefix("./").unwrap_or(path).to_owned()
}
