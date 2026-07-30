//! The versioned default trust store installed in every AgentOS VM root.

use agentos_vm_kernel::root_fs::{FilesystemEntry, FilesystemEntryKind, RootFilesystemSnapshot};

/// Mozilla trust-store snapshot generated from the exact-pinned build dependency.
pub const CA_CERTIFICATES_BUNDLE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/ca-certificates.crt"));
pub const CA_CERTIFICATES_GUEST_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";
pub const CA_CERTIFICATES_SYMLINK_PATH: &str = "/etc/ssl/cert.pem";
pub const CA_CERTIFICATES_SYMLINK_TARGET: &str = "certs/ca-certificates.crt";

/// Return the lowest-precedence CA layer. User lowers and bootstrap entries
/// remain authoritative when they provide either conventional trust path.
pub(crate) fn default_ca_snapshot(
    omit_bundle: bool,
    omit_cert_pem_symlink: bool,
) -> RootFilesystemSnapshot {
    assert!(
        !CA_CERTIFICATES_BUNDLE.is_empty(),
        "embedded Mozilla CA certificate bundle must not be empty"
    );

    let mut entries = vec![
        FilesystemEntry::directory("/etc/ssl"),
        FilesystemEntry::directory("/etc/ssl/certs"),
    ];
    if !omit_bundle {
        entries.push(FilesystemEntry {
            path: CA_CERTIFICATES_GUEST_PATH.to_string(),
            kind: FilesystemEntryKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            content: Some(CA_CERTIFICATES_BUNDLE.to_vec()),
            target: None,
        });
    }
    if !omit_cert_pem_symlink {
        entries.push(FilesystemEntry {
            path: CA_CERTIFICATES_SYMLINK_PATH.to_string(),
            kind: FilesystemEntryKind::Symlink,
            mode: 0o777,
            uid: 0,
            gid: 0,
            content: None,
            target: Some(CA_CERTIFICATES_SYMLINK_TARGET.to_string()),
        });
    }

    RootFilesystemSnapshot { entries }
}
