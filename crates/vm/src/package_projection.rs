//! agentOS package projection.
//!
//! Packages are mounted directly from their uncompressed `package.tar` files
//! when available. Transition fixtures may still project an unpacked package
//! dir as a read-only package leaf. Tar packages stay on the zero-extraction
//! fast path: the VFS indexes headers once and returns mmap-backed byte ranges.
//! The projection also serves `bin/*`, `current`, manpage aliases, and
//! `provides.files` as virtual mounts; it never writes a physical symlink farm.
//!
//! The projection is deliberately granular. Each package version is a tar leaf
//! at `/opt/agentos/pkgs/<pkg>/<version>`, and each managed command/current
//! alias is its own root-symlink leaf. The containing dirs stay writable overlay
//! dirs so user-installed commands can coexist beside managed package entries.
//!
//! This code runs during VM startup. Keep tar projection on metadata plus the
//! small manifest/package metadata entries only: do not read payloads unrelated
//! to projection, and do not reintroduce a whole-archive read or content hash.
//! If a package digest is ever required, compute it at pack time and store it in
//! a sidecar index instead of on this cold-start path.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::state::VmError;
use agentos_vfs_core::package_format::pack::{
    command_targets_from_package_json, is_projectable_command_name, manifest_json_to_v1,
    SNAPSHOT_BUNDLE_PATH,
};
use agentos_vfs_core::package_format::{generated::v1, read_manifest_chunk_from_file};
use agentos_vfs_core::posix::{normalize_path, TarFileSystem, VirtualFileSystem};

/// Root of the agentOS package tree inside the VM.
pub const OPT_AGENTOS_ROOT: &str = "/opt/agentos";
/// The symlink farm on `$PATH`.
pub const OPT_AGENTOS_BIN: &str = "/opt/agentos/bin";
pub const DEFAULT_PACKAGE_FILE_NAME: &str = "package.aospkg";
/// Back-compat alias for the packed container file name.
pub const DEFAULT_PACKAGE_TAR_NAME: &str = DEFAULT_PACKAGE_FILE_NAME;
pub const MAX_AGENTOS_PACKAGE_MOUNTS: usize = 4096;

/// A package to project, derived from chunk1 of `package.aospkg`.
#[derive(Debug, Clone)]
pub struct PackageDescriptor {
    pub name: String,
    pub version: String,
    pub dir: String,
    pub tar_path: Option<String>,
    /// `bin/` command that speaks ACP, if this is an agent package.
    pub acp_entrypoint: Option<String>,
    /// Agent launch env from the packed manifest's agent block.
    pub agent_env: HashMap<String, String>,
    /// Agent launch args from the packed manifest's agent block.
    pub agent_launch_args: Vec<String>,
    pub snapshot: bool,
    pub snapshot_bundle_path: Option<String>,
    pub provides: Option<PackageProvidesDescriptor>,
    pub commands: Vec<PackageCommandTarget>,
    pub man_pages: Vec<PackageManPageTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCommandTarget {
    pub command: String,
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManPageTarget {
    pub section: String,
    pub page: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageLeafMount {
    Tar {
        guest_path: String,
        tar_path: String,
        root: String,
    },
    HostDir {
        guest_path: String,
        host_path: String,
    },
    SingleSymlink {
        guest_path: String,
        target: String,
    },
}

#[derive(Debug, Clone)]
pub struct PackageProvidesDescriptor {
    pub env: HashMap<String, String>,
    pub files: Vec<PackageProvidesFileDescriptor>,
}

#[derive(Debug, Clone)]
pub struct PackageProvidesFileDescriptor {
    pub source: String,
    pub target: String,
}

impl PackageDescriptor {
    fn from_manifest(
        dir: String,
        tar_path: Option<String>,
        manifest: v1::PackageManifest,
    ) -> Result<Self, VmError> {
        if manifest.name.is_empty() {
            return Err(VmError::InvalidState(format!(
                "package manifest in {dir} is missing a valid \"name\""
            )));
        }
        if manifest.version.is_empty() {
            return Err(VmError::InvalidState(format!(
                "package manifest in {dir} is missing a valid \"version\""
            )));
        }
        let (acp_entrypoint, agent_env, agent_launch_args, snapshot) = match manifest.agent {
            Some(agent) => (
                Some(agent.acp_entrypoint),
                agent.env,
                agent.launch_args,
                agent.snapshot,
            ),
            None => (None, HashMap::new(), Vec::new(), false),
        };
        if acp_entrypoint
            .as_ref()
            .is_some_and(|entry| entry.is_empty())
        {
            return Err(VmError::InvalidState(format!(
                "package manifest in {dir} has an empty agent.acpEntrypoint"
            )));
        }
        Ok(Self {
            name: manifest.name,
            version: manifest.version,
            dir,
            tar_path,
            acp_entrypoint,
            agent_env,
            agent_launch_args,
            snapshot,
            snapshot_bundle_path: manifest.snapshot_bundle_path,
            provides: manifest.provides.map(convert_provides),
            commands: manifest
                .commands
                .into_iter()
                .map(|target| PackageCommandTarget {
                    command: target.command,
                    entry: target.entry,
                })
                .collect(),
            man_pages: manifest
                .man_pages
                .into_iter()
                .map(|page| PackageManPageTarget {
                    section: page.section,
                    page: page.page,
                })
                .collect(),
        })
    }

    fn tar_ref(&self) -> Option<&str> {
        self.tar_path.as_deref()
    }
}

fn convert_provides(provides: v1::ProvidesBlock) -> PackageProvidesDescriptor {
    PackageProvidesDescriptor {
        env: provides.env,
        files: provides
            .files
            .into_iter()
            .map(|file| PackageProvidesFileDescriptor {
                source: file.source,
                target: file.target,
            })
            .collect(),
    }
}

fn io_err(context: &str, error: std::io::Error) -> VmError {
    VmError::Io(format!("{context}: {error}"))
}

/// Read the sidecar-owned package manifest from `<dir>/package.aospkg`, or
/// fall back to scanning the unpacked dir for transition fixtures without a
/// packed `.aospkg` (projected as a read-only `HostDir` package leaf).
pub fn read_package_manifest(dir: &str) -> Result<PackageDescriptor, VmError> {
    match package_tar_for_dir(dir) {
        Some(package) => read_package_manifest_from_tar_with_dir(&package, dir.to_owned()),
        None => read_package_manifest_from_dir(dir),
    }
}

/// Transition path: project an unpacked package dir (no `.aospkg`) by reading
/// `agentos-package.json` (toolchain input) and scanning `bin/` and
/// `share/man/` on the host. The JSON-to-manifest conversion is shared with
/// the packer (`agentos_vfs_core::package_format::pack`), so packed and dir packages derive
/// identical manifests for the same source JSON.
fn read_package_manifest_from_dir(dir: &str) -> Result<PackageDescriptor, VmError> {
    let path = Path::new(dir).join("agentos-package.json");
    if !path.exists() {
        return Err(VmError::InvalidState(format!(
            "package dir {dir} has neither {DEFAULT_PACKAGE_FILE_NAME} nor agentos-package.json"
        )));
    }
    let manifest_json = fs::read(&path).map_err(|e| io_err("read agentos-package.json", e))?;
    let snapshot_declared = serde_json::from_slice::<serde_json::Value>(&manifest_json)
        .ok()
        .and_then(|value| {
            value
                .get("agent")
                .and_then(|agent| agent.get("snapshot"))
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false);
    let snapshot_bundle_path = snapshot_declared.then(|| SNAPSHOT_BUNDLE_PATH.to_owned());
    let commands = command_targets_from_dir(dir)?;
    let man_pages = man_pages_from_dir(dir)?;
    let manifest = manifest_json_to_v1(
        &manifest_json,
        commands
            .into_iter()
            .map(|target| v1::CommandTarget {
                command: target.command,
                entry: target.entry,
            })
            .collect(),
        man_pages
            .into_iter()
            .map(|page| v1::ManPage {
                section: page.section,
                page: page.page,
            })
            .collect(),
        snapshot_bundle_path,
    )
    .map_err(|error| {
        VmError::InvalidState(format!("invalid agentos-package.json in {dir}: {error}"))
    })?;
    PackageDescriptor::from_manifest(dir.to_owned(), None, manifest)
}

fn command_targets_from_dir(dir: &str) -> Result<Vec<PackageCommandTarget>, VmError> {
    let pkg_json = Path::new(dir).join("package.json");
    if pkg_json.exists() {
        if let Ok(text) = fs::read_to_string(&pkg_json) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(targets) = command_targets_from_package_json(&value) {
                    return Ok(targets
                        .into_iter()
                        .map(|target| PackageCommandTarget {
                            command: target.command,
                            entry: target.entry,
                        })
                        .collect());
                }
            }
        }
    }

    let bin = Path::new(dir).join("bin");
    if !bin.is_dir() {
        return Ok(Vec::new());
    }
    let mut targets = Vec::new();
    for entry in fs::read_dir(&bin).map_err(|e| io_err("read bin/", e))? {
        let entry = entry.map_err(|e| io_err("read bin/ entry", e))?;
        if let Some(name) = entry.file_name().to_str() {
            if is_projectable_command_name(name) {
                targets.push(PackageCommandTarget {
                    command: name.to_owned(),
                    entry: format!("bin/{name}"),
                });
            }
        }
    }
    targets.sort_by(|a, b| a.command.cmp(&b.command));
    Ok(targets)
}

fn man_pages_from_dir(dir: &str) -> Result<Vec<PackageManPageTarget>, VmError> {
    let man = Path::new(dir).join("share").join("man");
    if !man.is_dir() {
        return Ok(Vec::new());
    }
    let mut pages = Vec::new();
    for section in fs::read_dir(&man).map_err(|e| io_err("read man/", e))? {
        let section = section.map_err(|e| io_err("man section", e))?;
        if !section.path().is_dir() {
            continue;
        }
        let Some(section_name) = section.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        for page in fs::read_dir(section.path()).map_err(|e| io_err("man pages", e))? {
            let page = page.map_err(|e| io_err("man page", e))?;
            if let Some(page_name) = page.file_name().to_str() {
                pages.push(PackageManPageTarget {
                    section: section_name.clone(),
                    page: page_name.to_owned(),
                });
            }
        }
    }
    pages.sort_by(|a, b| (&a.section, &a.page).cmp(&(&b.section, &b.page)));
    Ok(pages)
}

/// Read the first snapshot-enabled agent package's bundled SDK snapshot source from `.aospkg`.
pub fn read_agent_snapshot_bundle(package: &PackageDescriptor) -> Result<Option<String>, VmError> {
    if !package.snapshot {
        return Ok(None);
    }
    let Some(path) = package.snapshot_bundle_path.as_deref() else {
        return Ok(None);
    };
    let Some(tar_path) = package.tar_ref() else {
        let host_path = Path::new(&package.dir).join(path.trim_start_matches('/'));
        if !host_path.exists() {
            return Ok(None);
        }
        return fs::read_to_string(&host_path)
            .map(Some)
            .map_err(|e| io_err("read agent snapshot bundle", e));
    };
    let mut fs =
        TarFileSystem::open(tar_path).map_err(|error| VmError::InvalidState(error.to_string()))?;
    match fs.read_file(path) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|error| {
            VmError::InvalidState(format!("snapshot bundle is not UTF-8: {error}"))
        }),
        Err(error) if error.code() == "ENOENT" => Ok(None),
        Err(error) => Err(VmError::InvalidState(error.to_string())),
    }
}

fn package_tar_for_dir(dir: &str) -> Option<PathBuf> {
    let tar = Path::new(dir).join(DEFAULT_PACKAGE_TAR_NAME);
    tar.is_file().then_some(tar)
}

/// Read a package manifest from the single wire `path`.
///
/// A file path is a packed `.aospkg` (the normal case). A directory path is a
/// local transition fixture: it is scanned via its `agentos-package.json`
/// (packed packages no longer ship that JSON — the vbare chunk1 manifest is
/// the only runtime manifest), or via a `package.aospkg` inside the dir.
pub fn read_package_manifest_from_path(path: &str) -> Result<PackageDescriptor, VmError> {
    if path.is_empty() {
        return Err(VmError::InvalidState(String::from(
            "package descriptor must include a package path",
        )));
    }
    let fs_path = Path::new(path);
    if fs_path.is_file() {
        let dir = fs_path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        return read_package_manifest_from_tar_with_dir(fs_path, dir);
    }
    read_package_manifest(path)
}

fn read_package_manifest_from_tar_with_dir(
    tar: &Path,
    dir: String,
) -> Result<PackageDescriptor, VmError> {
    // Projection is startup-critical and reads chunk1 only: 16-byte header,
    // then the versioned vbare PackageManifest. Do not parse tar headers, open
    // TarFileSystem, decode chunk2, or touch chunk3 here.
    let manifest = read_aospkg_manifest_chunk(tar)?;
    PackageDescriptor::from_manifest(dir, Some(tar.to_string_lossy().into_owned()), manifest)
}

fn read_aospkg_manifest_chunk(path: &Path) -> Result<v1::PackageManifest, VmError> {
    // Container framing lives in agentos_vfs_core::package_format; this is the single
    // startup-critical chunk1 read shared with every host-side consumer.
    read_manifest_chunk_from_file(path).map_err(|error| VmError::InvalidState(error.to_string()))
}

pub fn build_package_leaf_mounts(
    packages: &[PackageDescriptor],
    mount_at: &str,
) -> Result<Vec<PackageLeafMount>, VmError> {
    let mount_at = normalize_mount_root(mount_at);
    let mut mounts = Vec::new();
    let mut command_paths = HashSet::new();

    for package in packages {
        let commands = package
            .commands
            .iter()
            .map(|target| target.command.clone())
            .collect::<Vec<_>>();
        if let Some(acp) = &package.acp_entrypoint {
            if !commands.contains(acp) {
                return Err(VmError::InvalidState(format!(
                    "agent acpEntrypoint {acp:?} is not one of {}'s commands",
                    package.name
                )));
            }
        }

        let package_root = package_guest_root(&mount_at, &package.name);
        let version_path = normalize_path(&format!("{package_root}/{}", package.version));
        if let Some(tar_path) = package.tar_ref() {
            push_mount(
                &mut mounts,
                PackageLeafMount::Tar {
                    guest_path: version_path,
                    tar_path: tar_path.to_owned(),
                    root: String::from("/"),
                },
            )?;
        } else {
            push_mount(
                &mut mounts,
                PackageLeafMount::HostDir {
                    guest_path: version_path,
                    host_path: package.dir.clone(),
                },
            )?;
        }
        push_mount(
            &mut mounts,
            PackageLeafMount::SingleSymlink {
                guest_path: normalize_path(&format!("{package_root}/current")),
                target: package.version.clone(),
            },
        )?;

        for target in &package.commands {
            let guest_path = normalize_path(&format!("{mount_at}/bin/{}", target.command));
            if !command_paths.insert(guest_path.clone()) {
                return Err(VmError::InvalidState(format!(
                    "command {:?} is already provided by another package",
                    target.command
                )));
            }
            push_mount(
                &mut mounts,
                PackageLeafMount::SingleSymlink {
                    guest_path,
                    target: format!("../pkgs/{}/current/{}", package.name, target.entry),
                },
            )?;
        }

        for page in &package.man_pages {
            push_mount(
                &mut mounts,
                PackageLeafMount::SingleSymlink {
                    guest_path: normalize_path(&format!(
                        "{mount_at}/share/man/{}/{}",
                        page.section, page.page
                    )),
                    target: format!(
                        "../../../pkgs/{}/current/share/man/{}/{}",
                        package.name, page.section, page.page
                    ),
                },
            )?;
        }
    }

    Ok(mounts)
}

pub fn package_provides_file_mount(
    package: &PackageDescriptor,
    source: &str,
    target: &str,
) -> Result<Option<PackageLeafMount>, VmError> {
    if let Some(tar_path) = package.tar_ref() {
        let root = normalize_package_source(source);
        let mut fs = TarFileSystem::open_at(tar_path, &root)
            .map_err(|error| VmError::InvalidState(error.to_string()))?;
        match fs.stat("/") {
            Ok(stat) if stat.is_directory => Ok(Some(PackageLeafMount::Tar {
                guest_path: normalize_path(target),
                tar_path: tar_path.to_owned(),
                root,
            })),
            Ok(_) => Ok(None),
            Err(error) if error.code() == "ENOENT" => Err(VmError::InvalidState(format!(
                "package provides file source is missing: package `{}` source `{source}` target `{target}`",
                package.name
            ))),
            Err(error) => Err(VmError::InvalidState(error.to_string())),
        }
    } else {
        let host_path =
            Path::new(&package.dir).join(normalize_package_source(source).trim_start_matches('/'));
        match fs::metadata(&host_path) {
            Ok(metadata) if metadata.is_dir() => Ok(Some(PackageLeafMount::HostDir {
                guest_path: normalize_path(target),
                host_path: host_path.to_string_lossy().into_owned(),
            })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(VmError::InvalidState(format!(
                    "package provides file source is missing: package `{}` source `{source}` target `{target}`",
                    package.name
                )))
            }
            Err(error) => Err(io_err("stat package provides file source", error)),
        }
    }
}

fn push_mount(mounts: &mut Vec<PackageLeafMount>, mount: PackageLeafMount) -> Result<(), VmError> {
    let observed = mounts.len() + 1;
    if observed > MAX_AGENTOS_PACKAGE_MOUNTS {
        return Err(VmError::InvalidState(format!(
            "agentos package mount count exceeded: {observed} mounts > {MAX_AGENTOS_PACKAGE_MOUNTS} mounts (raise via limits.agentosPackages.maxMounts)"
        )));
    }
    if observed * 100 / MAX_AGENTOS_PACKAGE_MOUNTS >= 80 {
        tracing::warn!(
            limit = "agentos_package_mounts",
            observed,
            capacity = MAX_AGENTOS_PACKAGE_MOUNTS,
            fill_percent = observed * 100 / MAX_AGENTOS_PACKAGE_MOUNTS,
            wired = "limits.agentosPackages.maxMounts",
            "agentos package mount count approaching configured limit"
        );
    }
    mounts.push(mount);
    Ok(())
}

fn normalize_mount_root(mount_at: &str) -> String {
    if mount_at.is_empty() {
        String::from(OPT_AGENTOS_ROOT)
    } else {
        normalize_path(mount_at)
    }
}

fn package_guest_root(mount_at: &str, name: &str) -> String {
    normalize_path(&format!("{mount_at}/pkgs/{name}"))
}

fn normalize_package_source(source: &str) -> String {
    if source.trim().is_empty() {
        String::from("/")
    } else {
        normalize_path(source)
    }
}
