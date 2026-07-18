use crate::ca::default_ca_snapshot;
use crate::services::default_services_snapshot;
use agentos_bridge::FilesystemSnapshot;
use agentos_kernel::mount_table::MountTable;
use agentos_kernel::root_fs::{
    decode_snapshot_with_import_limits, is_supported_root_filesystem_snapshot_format,
    FilesystemEntry, FilesystemEntryKind, RootFileSystem,
    RootFilesystemDescriptor as KernelRootFilesystemDescriptor, RootFilesystemImportLimits,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
};
use agentos_kernel::vfs::{normalize_path, VirtualFileSystem};
use agentos_sidecar_protocol::protocol::{
    RootFilesystemDescriptor as ProtocolRootFilesystemDescriptor,
    RootFilesystemEntry as ProtocolRootFilesystemEntry,
    RootFilesystemEntryEncoding as ProtocolRootFilesystemEntryEncoding,
    RootFilesystemEntryKind as ProtocolRootFilesystemEntryKind,
    RootFilesystemLowerDescriptor as ProtocolRootFilesystemLowerDescriptor,
    RootFilesystemMode as ProtocolRootFilesystemMode,
    SnapshotRootFilesystemLower as ProtocolSnapshotRootFilesystemLower,
};
use agentos_vm_config as vm_config;
use base64::Engine;
use std::error::Error;
use std::fmt;
use vfs::posix::usage::RootFilesystemResourceLimits;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarCoreError {
    code: Option<String>,
    message: String,
}

impl SidecarCoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// Construct a stable typed error for an executor-facing operation.
    ///
    /// The code is transported independently from the diagnostic message so
    /// adapters never need to recover errno by parsing `Display` output.
    pub fn typed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: Some(code.into()),
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SidecarCoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl Error for SidecarCoreError {}

pub fn root_filesystem_descriptor_from_config(
    config: &vm_config::RootFilesystemConfig,
) -> Result<KernelRootFilesystemDescriptor, SidecarCoreError> {
    root_filesystem_descriptor_from_config_with_import_limits(
        config,
        &RootFilesystemImportLimits::default(),
    )
}

fn root_filesystem_descriptor_from_config_with_import_limits(
    config: &vm_config::RootFilesystemConfig,
    import_limits: &RootFilesystemImportLimits,
) -> Result<KernelRootFilesystemDescriptor, SidecarCoreError> {
    Ok(KernelRootFilesystemDescriptor {
        mode: root_filesystem_mode_from_config(config.mode),
        disable_default_base_layer: config.disable_default_base_layer,
        lowers: config
            .lowers
            .iter()
            .map(|lower| root_filesystem_lower_from_config(lower, import_limits))
            .collect::<Result<Vec<_>, _>>()?,
        bootstrap_entries: config
            .bootstrap_entries
            .iter()
            .map(root_filesystem_entry_from_config)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub fn root_filesystem_protocol_descriptor_from_config(
    config: &vm_config::RootFilesystemConfig,
) -> ProtocolRootFilesystemDescriptor {
    ProtocolRootFilesystemDescriptor {
        mode: match config.mode {
            vm_config::RootFilesystemMode::Ephemeral => ProtocolRootFilesystemMode::Ephemeral,
            vm_config::RootFilesystemMode::ReadOnly => ProtocolRootFilesystemMode::ReadOnly,
        },
        disable_default_base_layer: config.disable_default_base_layer,
        lowers: config
            .lowers
            .iter()
            .map(root_filesystem_protocol_lower_from_config)
            .collect(),
        bootstrap_entries: config
            .bootstrap_entries
            .iter()
            .map(root_filesystem_protocol_entry_from_config)
            .collect(),
    }
}

pub fn build_root_filesystem(
    config: &vm_config::RootFilesystemConfig,
    limits: &impl RootFilesystemResourceLimits,
) -> Result<RootFileSystem, SidecarCoreError> {
    build_root_filesystem_with_loaded_snapshot(config, None, limits)
}

pub fn build_root_filesystem_with_loaded_snapshot(
    config: &vm_config::RootFilesystemConfig,
    loaded_snapshot: Option<&FilesystemSnapshot>,
    limits: &impl RootFilesystemResourceLimits,
) -> Result<RootFileSystem, SidecarCoreError> {
    let import_limits = RootFilesystemImportLimits::from_resource_limits(limits);
    let mut descriptor = if let Some(restored) = supported_loaded_snapshot(loaded_snapshot) {
        KernelRootFilesystemDescriptor {
            mode: root_filesystem_mode_from_config(config.mode),
            disable_default_base_layer: true,
            lowers: vec![
                decode_snapshot_with_import_limits(&restored.bytes, &import_limits).map_err(
                    |error| {
                        SidecarCoreError::new(format!("decode restored root filesystem: {error}"))
                    },
                )?,
            ],
            bootstrap_entries: config
                .bootstrap_entries
                .iter()
                .map(root_filesystem_entry_from_config)
                .collect::<Result<Vec<_>, _>>()?,
        }
    } else {
        root_filesystem_descriptor_from_config_with_import_limits(config, &import_limits)?
    };
    // Adding any explicit lower disables the kernel's automatic minimal lower.
    // Preserve that scaffold when the CA is the only explicit layer.
    if descriptor.disable_default_base_layer && descriptor.lowers.is_empty() {
        let mut minimal_root = RootFileSystem::from_descriptor_with_import_limits(
            KernelRootFilesystemDescriptor {
                mode: KernelRootFilesystemMode::Ephemeral,
                disable_default_base_layer: true,
                lowers: Vec::new(),
                bootstrap_entries: Vec::new(),
            },
            &import_limits,
        )
        .map_err(|error| {
            SidecarCoreError::new(format!("build minimal root filesystem: {error}"))
        })?;
        descriptor.lowers.push(
            minimal_root.snapshot().map_err(|error| {
                SidecarCoreError::new(format!("snapshot minimal root: {error}"))
            })?,
        );
    }

    // This is the final (lowest-precedence) AgentOS lower. Overlay lookup checks
    // user lowers first, and bootstrap entries are applied to the upper, so an
    // explicit trust store always replaces these defaults.
    let bootstrap_replaces_bundle = descriptor
        .bootstrap_entries
        .iter()
        .any(|entry| normalize_path(&entry.path) == crate::ca::CA_CERTIFICATES_GUEST_PATH);
    let bootstrap_replaces_cert_pem = descriptor
        .bootstrap_entries
        .iter()
        .any(|entry| normalize_path(&entry.path) == crate::ca::CA_CERTIFICATES_SYMLINK_PATH);
    // `write_file` follows a lower-layer symlink during copy-up. When an
    // explicit bootstrap node replaces one of the trust paths, remove that
    // exact lower entry first so a regular file replaces the symlink itself
    // instead of overwriting its target. This also handles restored snapshots
    // that captured an older default CA layout.
    for lower in &mut descriptor.lowers {
        lower.entries.retain(|entry| {
            let path = normalize_path(&entry.path);
            !(bootstrap_replaces_bundle && path == crate::ca::CA_CERTIFICATES_GUEST_PATH)
                && !(bootstrap_replaces_cert_pem && path == crate::ca::CA_CERTIFICATES_SYMLINK_PATH)
        });
    }
    descriptor.lowers.push(default_services_snapshot());
    descriptor.lowers.push(default_ca_snapshot(
        bootstrap_replaces_bundle,
        bootstrap_replaces_cert_pem,
    ));
    RootFileSystem::from_descriptor_with_import_limits(descriptor, &import_limits)
        .map_err(|error| SidecarCoreError::new(format!("build root filesystem: {error}")))
}

pub fn build_root_mount_table(
    config: &vm_config::RootFilesystemConfig,
    limits: &impl RootFilesystemResourceLimits,
) -> Result<MountTable, SidecarCoreError> {
    Ok(MountTable::new(build_root_filesystem(config, limits)?))
}

pub fn build_root_mount_table_with_loaded_snapshot(
    config: &vm_config::RootFilesystemConfig,
    loaded_snapshot: Option<&FilesystemSnapshot>,
    limits: &impl RootFilesystemResourceLimits,
) -> Result<MountTable, SidecarCoreError> {
    Ok(MountTable::new(build_root_filesystem_with_loaded_snapshot(
        config,
        loaded_snapshot,
        limits,
    )?))
}

pub fn root_filesystem_mode_from_config(
    mode: vm_config::RootFilesystemMode,
) -> KernelRootFilesystemMode {
    match mode {
        vm_config::RootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
        vm_config::RootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
    }
}

pub fn protocol_root_filesystem_mode(mode: ProtocolRootFilesystemMode) -> KernelRootFilesystemMode {
    match mode {
        ProtocolRootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
        ProtocolRootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
    }
}

fn supported_loaded_snapshot(snapshot: Option<&FilesystemSnapshot>) -> Option<&FilesystemSnapshot> {
    snapshot.filter(|snapshot| is_supported_root_filesystem_snapshot_format(&snapshot.format))
}

fn root_filesystem_lower_from_config(
    lower: &vm_config::RootFilesystemLowerDescriptor,
    import_limits: &RootFilesystemImportLimits,
) -> Result<RootFilesystemSnapshot, SidecarCoreError> {
    match lower {
        vm_config::RootFilesystemLowerDescriptor::Snapshot { entries } => {
            Ok(RootFilesystemSnapshot {
                entries: entries
                    .iter()
                    .map(root_filesystem_entry_from_config)
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
        vm_config::RootFilesystemLowerDescriptor::BundledBaseFilesystem => Ok(
            agentos_kernel::root_fs::load_bundled_base_snapshot_with_limits(import_limits)
                .map_err(|error| {
                    SidecarCoreError::new(format!("load bundled base filesystem lower: {error}"))
                })?,
        ),
    }
}

fn root_filesystem_entry_from_config(
    entry: &vm_config::RootFilesystemEntry,
) -> Result<FilesystemEntry, SidecarCoreError> {
    let mode = entry.mode.unwrap_or(match entry.kind {
        vm_config::RootFilesystemEntryKind::File => {
            if entry.executable {
                0o755
            } else {
                0o644
            }
        }
        vm_config::RootFilesystemEntryKind::Directory => 0o755,
        vm_config::RootFilesystemEntryKind::Symlink => 0o777,
    });

    let content = match entry.content.as_ref() {
        Some(content) => match entry.encoding {
            Some(vm_config::RootFilesystemEntryEncoding::Base64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|error| {
                        SidecarCoreError::new(format!(
                            "invalid base64 root filesystem content for {}: {error}",
                            entry.path
                        ))
                    })?,
            ),
            Some(vm_config::RootFilesystemEntryEncoding::Utf8) | None => {
                Some(content.as_bytes().to_vec())
            }
        },
        None => None,
    };

    Ok(FilesystemEntry {
        path: normalize_path(&entry.path),
        kind: match entry.kind {
            vm_config::RootFilesystemEntryKind::File => FilesystemEntryKind::File,
            vm_config::RootFilesystemEntryKind::Directory => FilesystemEntryKind::Directory,
            vm_config::RootFilesystemEntryKind::Symlink => FilesystemEntryKind::Symlink,
        },
        mode,
        uid: entry.uid.unwrap_or(0),
        gid: entry.gid.unwrap_or(0),
        content,
        target: entry.target.clone(),
    })
}

fn root_filesystem_protocol_lower_from_config(
    lower: &vm_config::RootFilesystemLowerDescriptor,
) -> ProtocolRootFilesystemLowerDescriptor {
    match lower {
        vm_config::RootFilesystemLowerDescriptor::Snapshot { entries } => {
            ProtocolRootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                ProtocolSnapshotRootFilesystemLower {
                    entries: entries
                        .iter()
                        .map(root_filesystem_protocol_entry_from_config)
                        .collect(),
                },
            )
        }
        vm_config::RootFilesystemLowerDescriptor::BundledBaseFilesystem => {
            ProtocolRootFilesystemLowerDescriptor::BundledBaseFilesystemLower
        }
    }
}

fn root_filesystem_protocol_entry_from_config(
    entry: &vm_config::RootFilesystemEntry,
) -> ProtocolRootFilesystemEntry {
    ProtocolRootFilesystemEntry {
        path: entry.path.clone(),
        kind: match entry.kind {
            vm_config::RootFilesystemEntryKind::File => ProtocolRootFilesystemEntryKind::File,
            vm_config::RootFilesystemEntryKind::Directory => {
                ProtocolRootFilesystemEntryKind::Directory
            }
            vm_config::RootFilesystemEntryKind::Symlink => ProtocolRootFilesystemEntryKind::Symlink,
        },
        mode: entry.mode,
        uid: entry.uid,
        gid: entry.gid,
        content: entry.content.clone(),
        encoding: entry.encoding.map(|encoding| match encoding {
            vm_config::RootFilesystemEntryEncoding::Utf8 => {
                ProtocolRootFilesystemEntryEncoding::Utf8
            }
            vm_config::RootFilesystemEntryEncoding::Base64 => {
                ProtocolRootFilesystemEntryEncoding::Base64
            }
        }),
        target: entry.target.clone(),
        executable: entry.executable,
    }
}

pub fn root_snapshot_entry(entry: &FilesystemEntry) -> ProtocolRootFilesystemEntry {
    let (content, encoding) = entry
        .content
        .clone()
        .map(snapshot_entry_content)
        .map(|(content, encoding)| (Some(content), Some(encoding)))
        .unwrap_or((None, None));

    ProtocolRootFilesystemEntry {
        path: entry.path.clone(),
        kind: match entry.kind {
            FilesystemEntryKind::File => ProtocolRootFilesystemEntryKind::File,
            FilesystemEntryKind::Directory => ProtocolRootFilesystemEntryKind::Directory,
            FilesystemEntryKind::Symlink => ProtocolRootFilesystemEntryKind::Symlink,
        },
        mode: Some(entry.mode),
        uid: Some(entry.uid),
        gid: Some(entry.gid),
        content,
        encoding,
        target: entry.target.clone(),
        executable: entry.mode & 0o111 != 0,
    }
}

/// Convert a protocol root-filesystem entry into a kernel `FilesystemEntry`, decoding
/// content (utf8/base64) and applying per-kind mode defaults. Shared by native and
/// browser bootstrap + snapshot paths.
pub fn convert_root_filesystem_entry(
    entry: &ProtocolRootFilesystemEntry,
) -> Result<FilesystemEntry, SidecarCoreError> {
    let mode = entry.mode.unwrap_or(match entry.kind {
        ProtocolRootFilesystemEntryKind::File => {
            if entry.executable {
                0o755
            } else {
                0o644
            }
        }
        ProtocolRootFilesystemEntryKind::Directory => 0o755,
        ProtocolRootFilesystemEntryKind::Symlink => 0o777,
    });

    let content = match entry.content.as_ref() {
        Some(content) => match entry.encoding {
            Some(ProtocolRootFilesystemEntryEncoding::Base64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|error| {
                        SidecarCoreError::new(format!(
                            "invalid base64 root filesystem content for {}: {error}",
                            entry.path
                        ))
                    })?,
            ),
            Some(ProtocolRootFilesystemEntryEncoding::Utf8) | None => {
                Some(content.as_bytes().to_vec())
            }
        },
        None => None,
    };

    Ok(FilesystemEntry {
        path: normalize_path(&entry.path),
        kind: match entry.kind {
            ProtocolRootFilesystemEntryKind::File => FilesystemEntryKind::File,
            ProtocolRootFilesystemEntryKind::Directory => FilesystemEntryKind::Directory,
            ProtocolRootFilesystemEntryKind::Symlink => FilesystemEntryKind::Symlink,
        },
        mode,
        uid: entry.uid.unwrap_or(0),
        gid: entry.gid.unwrap_or(0),
        content,
        target: entry.target.clone(),
    })
}

/// Build a kernel snapshot from protocol entries (shared by native + browser).
pub fn root_snapshot_from_entries(
    entries: &[ProtocolRootFilesystemEntry],
) -> Result<RootFilesystemSnapshot, SidecarCoreError> {
    Ok(RootFilesystemSnapshot {
        entries: entries
            .iter()
            .map(convert_root_filesystem_entry)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// Write one root-filesystem bootstrap entry (file/dir/symlink) into a VFS, creating
/// parent dirs and applying deterministic mode/uid/gid defaults (chmod/chown for
/// non-symlinks). Shared by native (raw root FS) and browser (kernel `filesystem_mut`).
pub fn apply_root_filesystem_entry<F: VirtualFileSystem>(
    filesystem: &mut F,
    entry: &ProtocolRootFilesystemEntry,
) -> Result<(), SidecarCoreError> {
    let kernel_entry = convert_root_filesystem_entry(entry)?;

    let parent = parent_directory(&kernel_entry.path);
    if parent != "/" && !filesystem.exists(&parent) {
        filesystem.mkdir(&parent, true).map_err(vfs_err)?;
    }

    match kernel_entry.kind {
        FilesystemEntryKind::Directory => filesystem
            .mkdir(&kernel_entry.path, true)
            .map_err(vfs_err)?,
        FilesystemEntryKind::File => filesystem
            .write_file(&kernel_entry.path, kernel_entry.content.unwrap_or_default())
            .map_err(vfs_err)?,
        FilesystemEntryKind::Symlink => filesystem
            .symlink(
                kernel_entry.target.as_deref().ok_or_else(|| {
                    SidecarCoreError::new(format!(
                        "root filesystem bootstrap for symlink {} requires a target",
                        entry.path
                    ))
                })?,
                &kernel_entry.path,
            )
            .map_err(vfs_err)?,
    }

    if !matches!(kernel_entry.kind, FilesystemEntryKind::Symlink) {
        filesystem
            .chmod(&kernel_entry.path, kernel_entry.mode)
            .map_err(vfs_err)?;
        filesystem
            .chown(&kernel_entry.path, kernel_entry.uid, kernel_entry.gid)
            .map_err(vfs_err)?;
    }

    Ok(())
}

fn vfs_err<E: fmt::Display>(error: E) -> SidecarCoreError {
    SidecarCoreError::new(error.to_string())
}

fn parent_directory(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => path[..index].to_string(),
    }
}

fn snapshot_entry_content(content: Vec<u8>) -> (String, ProtocolRootFilesystemEntryEncoding) {
    match String::from_utf8(content) {
        Ok(text) => (text, ProtocolRootFilesystemEntryEncoding::Utf8),
        Err(error) => (
            base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            ProtocolRootFilesystemEntryEncoding::Base64,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca::{
        CA_CERTIFICATES_BUNDLE, CA_CERTIFICATES_GUEST_PATH, CA_CERTIFICATES_SYMLINK_PATH,
        CA_CERTIFICATES_SYMLINK_TARGET,
    };
    use crate::services::{BASELINE_SERVICES, SERVICES_GUEST_PATH};
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::vfs::VirtualFileSystem;

    #[test]
    fn builds_root_filesystem_from_snapshot_lower_and_bootstrap_upper() {
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                lowers: vec![vm_config::RootFilesystemLowerDescriptor::Snapshot {
                    entries: vec![vm_config::RootFilesystemEntry {
                        path: String::from("/workspace/value.txt"),
                        kind: vm_config::RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(String::from("lower")),
                        encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                }],
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: String::from("/workspace/value.txt"),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(String::from("upper")),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                }],
                ..vm_config::RootFilesystemConfig::default()
            },
            &ResourceLimits::default(),
        )
        .expect("build root filesystem");

        assert_eq!(
            root.read_file("/workspace/value.txt")
                .expect("read merged value"),
            b"upper".to_vec()
        );
    }

    #[test]
    fn installs_default_linux_services_database() {
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                ..vm_config::RootFilesystemConfig::default()
            },
            &ResourceLimits::default(),
        )
        .expect("build root filesystem");

        assert_eq!(
            root.read_file(SERVICES_GUEST_PATH)
                .expect("read default services"),
            BASELINE_SERVICES.as_bytes()
        );
    }

    #[test]
    fn decodes_base64_root_filesystem_entries() {
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: String::from("/bin/tool"),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(String::from("dG9vbA==")),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Base64),
                    target: None,
                    executable: true,
                }],
                ..vm_config::RootFilesystemConfig::default()
            },
            &ResourceLimits::default(),
        )
        .expect("build root filesystem");

        assert_eq!(root.read_file("/bin/tool").expect("read file"), b"tool");
        assert_eq!(
            root.stat("/bin/tool").expect("stat file").mode & 0o777,
            0o755
        );
    }

    #[test]
    fn serializes_root_snapshot_entries_as_utf8_or_base64() {
        let text = root_snapshot_entry(&FilesystemEntry {
            path: String::from("/workspace/text.txt"),
            kind: FilesystemEntryKind::File,
            mode: 0o755,
            uid: 501,
            gid: 20,
            content: Some(b"hello".to_vec()),
            target: None,
        });

        assert_eq!(text.path, "/workspace/text.txt");
        assert_eq!(text.kind, ProtocolRootFilesystemEntryKind::File);
        assert_eq!(text.mode, Some(0o755));
        assert_eq!(text.uid, Some(501));
        assert_eq!(text.gid, Some(20));
        assert_eq!(text.content.as_deref(), Some("hello"));
        assert_eq!(
            text.encoding,
            Some(ProtocolRootFilesystemEntryEncoding::Utf8)
        );
        assert!(text.executable);

        let binary = root_snapshot_entry(&FilesystemEntry {
            path: String::from("/workspace/binary.bin"),
            kind: FilesystemEntryKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            content: Some(vec![0xff, 0x00]),
            target: None,
        });

        assert_eq!(binary.content.as_deref(), Some("/wA="));
        assert_eq!(
            binary.encoding,
            Some(ProtocolRootFilesystemEntryEncoding::Base64)
        );
        assert!(!binary.executable);
    }

    #[test]
    fn builds_protocol_descriptor_from_config_without_normalizing_optional_fields() {
        let descriptor =
            root_filesystem_protocol_descriptor_from_config(&vm_config::RootFilesystemConfig {
                mode: vm_config::RootFilesystemMode::ReadOnly,
                disable_default_base_layer: true,
                lowers: vec![
                    vm_config::RootFilesystemLowerDescriptor::BundledBaseFilesystem,
                    vm_config::RootFilesystemLowerDescriptor::Snapshot {
                        entries: vec![vm_config::RootFilesystemEntry {
                            path: String::from("relative/lower.txt"),
                            kind: vm_config::RootFilesystemEntryKind::File,
                            mode: None,
                            uid: None,
                            gid: None,
                            content: Some(String::from("lower")),
                            encoding: None,
                            target: None,
                            executable: false,
                        }],
                    },
                ],
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: String::from("/bin/tool"),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: Some(0o700),
                    uid: Some(1000),
                    gid: Some(1000),
                    content: Some(String::from("dG9vbA==")),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Base64),
                    target: None,
                    executable: true,
                }],
            });

        assert_eq!(descriptor.mode, ProtocolRootFilesystemMode::ReadOnly);
        assert!(descriptor.disable_default_base_layer);
        assert_eq!(descriptor.lowers.len(), 2);
        assert!(matches!(
            descriptor.lowers[0],
            ProtocolRootFilesystemLowerDescriptor::BundledBaseFilesystemLower
        ));
        let ProtocolRootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(snapshot) =
            &descriptor.lowers[1]
        else {
            panic!("expected snapshot lower");
        };
        assert_eq!(snapshot.entries[0].path, "relative/lower.txt");
        assert_eq!(snapshot.entries[0].mode, None);
        assert_eq!(snapshot.entries[0].encoding, None);

        let entry = &descriptor.bootstrap_entries[0];
        assert_eq!(entry.path, "/bin/tool");
        assert_eq!(entry.kind, ProtocolRootFilesystemEntryKind::File);
        assert_eq!(entry.mode, Some(0o700));
        assert_eq!(entry.uid, Some(1000));
        assert_eq!(entry.gid, Some(1000));
        assert_eq!(entry.content.as_deref(), Some("dG9vbA=="));
        assert_eq!(
            entry.encoding,
            Some(ProtocolRootFilesystemEntryEncoding::Base64)
        );
        assert!(entry.executable);
    }

    #[test]
    fn maps_root_filesystem_modes_to_kernel_modes() {
        assert_eq!(
            root_filesystem_mode_from_config(vm_config::RootFilesystemMode::Ephemeral),
            KernelRootFilesystemMode::Ephemeral
        );
        assert_eq!(
            root_filesystem_mode_from_config(vm_config::RootFilesystemMode::ReadOnly),
            KernelRootFilesystemMode::ReadOnly
        );
        assert_eq!(
            protocol_root_filesystem_mode(ProtocolRootFilesystemMode::Ephemeral),
            KernelRootFilesystemMode::Ephemeral
        );
        assert_eq!(
            protocol_root_filesystem_mode(ProtocolRootFilesystemMode::ReadOnly),
            KernelRootFilesystemMode::ReadOnly
        );
    }

    #[test]
    fn restored_snapshot_replaces_config_lowers_but_preserves_bootstrap_entries() {
        let restored = RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::file(
                "/workspace/restored.txt",
                b"restored",
            )],
        };
        let loaded_snapshot = FilesystemSnapshot {
            format: String::from(agentos_kernel::root_fs::ROOT_FILESYSTEM_SNAPSHOT_FORMAT),
            bytes: agentos_kernel::root_fs::encode_snapshot(&restored)
                .expect("encode restored snapshot"),
        };
        let mut root = build_root_filesystem_with_loaded_snapshot(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                lowers: vec![vm_config::RootFilesystemLowerDescriptor::Snapshot {
                    entries: vec![vm_config::RootFilesystemEntry {
                        path: String::from("/workspace/ignored-lower.txt"),
                        kind: vm_config::RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(String::from("ignored")),
                        encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                }],
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: String::from("/workspace/bootstrap.txt"),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(String::from("bootstrap")),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                }],
                ..vm_config::RootFilesystemConfig::default()
            },
            Some(&loaded_snapshot),
            &ResourceLimits::default(),
        )
        .expect("build root filesystem from restored snapshot");

        assert_eq!(
            root.read_file("/workspace/restored.txt")
                .expect("read restored file"),
            b"restored".to_vec()
        );
        assert_eq!(
            root.read_file("/workspace/bootstrap.txt")
                .expect("read bootstrap file"),
            b"bootstrap".to_vec()
        );
        assert!(
            root.read_file("/workspace/ignored-lower.txt").is_err(),
            "restored snapshots should replace configured lowers"
        );
    }

    #[test]
    fn installs_default_ca_in_read_only_roots_before_bootstrap_finishes() {
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig {
                mode: vm_config::RootFilesystemMode::ReadOnly,
                disable_default_base_layer: true,
                ..vm_config::RootFilesystemConfig::default()
            },
            &ResourceLimits::default(),
        )
        .expect("build read-only root filesystem");

        assert_eq!(
            root.read_file(CA_CERTIFICATES_GUEST_PATH)
                .expect("read default CA bundle"),
            CA_CERTIFICATES_BUNDLE
        );
        assert_eq!(
            root.read_link(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read default CA symlink"),
            CA_CERTIFICATES_SYMLINK_TARGET
        );
        assert!(
            root.exists("/usr/bin/env"),
            "adding the CA lower must preserve the kernel's minimal root scaffold"
        );

        root.finish_bootstrap();
        assert_eq!(
            root.read_file(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read CA through symlink after read-only lock"),
            CA_CERTIFICATES_BUNDLE
        );
        assert!(
            root.write_file(CA_CERTIFICATES_GUEST_PATH, b"replacement")
                .is_err(),
            "read-only root must lock the seeded trust store with all other writes"
        );
    }

    #[test]
    fn installs_default_ca_in_ephemeral_bundled_roots() {
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig::default(),
            &ResourceLimits::default(),
        )
        .expect("build default ephemeral root filesystem");

        assert_eq!(
            root.read_file(CA_CERTIFICATES_GUEST_PATH)
                .expect("read default CA bundle from bundled root"),
            CA_CERTIFICATES_BUNDLE
        );
        assert_eq!(
            root.read_file(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read default CA through bundled-root symlink"),
            CA_CERTIFICATES_BUNDLE
        );
    }

    #[test]
    fn explicit_ca_entries_replace_defaults_across_lower_and_bootstrap_layers() {
        let custom_bundle = "custom lower bundle\n";
        let custom_cert_pem = "custom bootstrap cert.pem\n";
        let mut root = build_root_filesystem(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                lowers: vec![vm_config::RootFilesystemLowerDescriptor::Snapshot {
                    entries: vec![vm_config::RootFilesystemEntry {
                        path: CA_CERTIFICATES_GUEST_PATH.to_string(),
                        kind: vm_config::RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(custom_bundle.to_string()),
                        encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                }],
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(custom_cert_pem.to_string()),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                }],
                ..vm_config::RootFilesystemConfig::default()
            },
            &ResourceLimits::default(),
        )
        .expect("build root filesystem with custom trust");

        assert_eq!(
            root.read_file(CA_CERTIFICATES_GUEST_PATH)
                .expect("read custom lower bundle"),
            custom_bundle.as_bytes()
        );
        assert_eq!(
            root.read_file(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read regular custom cert.pem"),
            custom_cert_pem.as_bytes()
        );
        assert!(
            !root
                .lstat(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("lstat custom cert.pem")
                .is_symbolic_link,
            "a regular custom cert.pem must replace the default symlink"
        );
    }

    #[test]
    fn restored_snapshot_ca_replaces_default_ca() {
        let restored_bundle = b"restored trust bundle\n";
        let restored = RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::file(CA_CERTIFICATES_GUEST_PATH, restored_bundle),
                FilesystemEntry::symlink(
                    CA_CERTIFICATES_SYMLINK_PATH,
                    CA_CERTIFICATES_SYMLINK_TARGET,
                ),
            ],
        };
        let loaded_snapshot = FilesystemSnapshot {
            format: String::from(agentos_kernel::root_fs::ROOT_FILESYSTEM_SNAPSHOT_FORMAT),
            bytes: agentos_kernel::root_fs::encode_snapshot(&restored)
                .expect("encode restored snapshot"),
        };
        let mut root = build_root_filesystem_with_loaded_snapshot(
            &vm_config::RootFilesystemConfig {
                disable_default_base_layer: true,
                bootstrap_entries: vec![vm_config::RootFilesystemEntry {
                    path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
                    kind: vm_config::RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some("restored custom cert.pem\n".to_string()),
                    encoding: Some(vm_config::RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                }],
                ..vm_config::RootFilesystemConfig::default()
            },
            Some(&loaded_snapshot),
            &ResourceLimits::default(),
        )
        .expect("build root filesystem from restored snapshot");

        assert_eq!(
            root.read_file(CA_CERTIFICATES_GUEST_PATH)
                .expect("read restored CA bundle"),
            restored_bundle
        );
        assert_eq!(
            root.read_file(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("read restored custom regular cert.pem"),
            b"restored custom cert.pem\n"
        );
        assert!(
            !root
                .lstat(CA_CERTIFICATES_SYMLINK_PATH)
                .expect("lstat restored custom cert.pem")
                .is_symbolic_link
        );
    }
}
