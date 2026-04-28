mod support;

mod s3 {
    include!("../src/plugins/s3.rs");

    mod tests {
        use super::test_support::MockS3Server;
        use super::*;

        fn test_config(server: &MockS3Server, prefix: &str) -> S3MountConfig {
            S3MountConfig {
                bucket: String::from("test-bucket"),
                prefix: Some(prefix.to_owned()),
                region: Some(String::from(DEFAULT_REGION)),
                credentials: Some(S3MountCredentials {
                    access_key_id: String::from("minioadmin"),
                    secret_access_key: String::from("minioadmin"),
                }),
                endpoint: Some(server.base_url().to_owned()),
                chunk_size: Some(8),
                inline_threshold: Some(4),
            }
        }

        #[test]
        fn s3_plugin_rejects_private_ip_endpoints() {
            let server = MockS3Server::start();
            let mut config = test_config(&server, "reject-private-endpoint");
            config.endpoint = Some(String::from("http://169.254.169.254/latest"));

            let error = match S3BackedFilesystem::from_config(config) {
                Ok(_) => panic!("private IP endpoint should fail"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("s3 mount endpoint must not target a private or local IP address"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn s3_plugin_persists_files_across_reopen_and_preserves_links() {
            let server = MockS3Server::start();

            let mut filesystem = S3BackedFilesystem::from_config(test_config(&server, "persist"))
                .expect("open s3 fs");
            filesystem
                .write_file("/workspace/original.txt", b"hello world".to_vec())
                .expect("write original");
            filesystem
                .link("/workspace/original.txt", "/workspace/linked.txt")
                .expect("link file");
            filesystem
                .symlink("/workspace/original.txt", "/workspace/alias.txt")
                .expect("symlink file");
            filesystem.shutdown().expect("flush s3 fs");

            let mut reopened = S3BackedFilesystem::from_config(test_config(&server, "persist"))
                .expect("reopen s3 fs");

            assert_eq!(
                reopened
                    .read_file("/workspace/original.txt")
                    .expect("read reopened original"),
                b"hello world".to_vec()
            );
            assert_eq!(
                reopened
                    .read_file("/workspace/linked.txt")
                    .expect("read reopened hard link"),
                b"hello world".to_vec()
            );
            assert_eq!(
                reopened
                    .read_file("/workspace/alias.txt")
                    .expect("read reopened symlink"),
                b"hello world".to_vec()
            );
            assert_eq!(
                reopened
                    .stat("/workspace/original.txt")
                    .expect("stat reopened file")
                    .nlink,
                2
            );

            let chunk_keys = server
                .object_keys()
                .into_iter()
                .filter(|key| key.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                chunk_keys.len() >= 2,
                "expected chunked storage to create multiple block objects"
            );
        }

        #[test]
        fn s3_plugin_cleans_up_stale_chunk_objects_after_truncate() {
            let server = MockS3Server::start();

            let mut filesystem = S3BackedFilesystem::from_config(test_config(&server, "truncate"))
                .expect("open s3 fs");
            filesystem
                .write_file("/large.txt", b"abcdefghijk".to_vec())
                .expect("write large file");
            filesystem.shutdown().expect("flush initial file");

            let before = server
                .object_keys()
                .into_iter()
                .filter(|key| key.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                before.len() >= 2,
                "expected multiple blocks before truncation"
            );

            filesystem
                .truncate("/large.txt", 1)
                .expect("truncate to inline size");
            filesystem.shutdown().expect("flush truncate");

            let after = server
                .object_keys()
                .into_iter()
                .filter(|key| key.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                after.is_empty(),
                "truncate should remove stale chunk objects"
            );

            let mut reopened = S3BackedFilesystem::from_config(test_config(&server, "truncate"))
                .expect("reopen truncated fs");
            assert_eq!(
                reopened
                    .read_file("/large.txt")
                    .expect("read truncated file"),
                b"a".to_vec()
            );
        }

        #[test]
        fn s3_plugin_metadata_only_flush_reuses_existing_chunks() {
            let server = MockS3Server::start();

            let mut filesystem =
                S3BackedFilesystem::from_config(test_config(&server, "chmod")).expect("open s3 fs");
            filesystem
                .write_file("/large.txt", b"abcdefghijk".to_vec())
                .expect("write large file");
            filesystem.shutdown().expect("flush initial file");
            server.clear_requests();

            for offset in 0..10 {
                filesystem
                    .chmod("/large.txt", 0o600 + offset)
                    .expect("chmod large file");
            }
            filesystem.shutdown().expect("flush chmod batch");

            let requests = server.requests();
            let chunk_uploads = requests
                .iter()
                .filter(|request| request.method == "PUT" && request.path.contains("/blocks/"))
                .count();
            assert_eq!(
                chunk_uploads, 0,
                "metadata-only flush should not re-upload file chunks"
            );
            assert!(
                requests.iter().any(|request| request.method == "PUT"
                    && request.path.contains("filesystem-manifest.json")),
                "expected metadata-only flush to update the manifest"
            );

            let mut reopened = S3BackedFilesystem::from_config(test_config(&server, "chmod"))
                .expect("reopen s3 fs");
            assert_eq!(
                reopened
                    .stat("/large.txt")
                    .expect("stat chmodded file")
                    .mode
                    & 0o777,
                0o611
            );
            assert_eq!(
                reopened
                    .read_file("/large.txt")
                    .expect("read chmodded file"),
                b"abcdefghijk".to_vec()
            );
        }

        #[test]
        fn s3_plugin_rejects_oversized_manifest_entries() {
            let server = MockS3Server::start();
            let manifest = PersistedFilesystemManifest {
                format: String::from(MANIFEST_FORMAT),
                path_index: BTreeMap::from([
                    (String::from("/"), 1),
                    (String::from("/huge.bin"), 2),
                ]),
                inodes: BTreeMap::from([
                    (
                        1,
                        PersistedFilesystemInode {
                            metadata: agent_os_kernel::vfs::MemoryFileSystemSnapshotMetadata {
                                mode: 0o040755,
                                uid: 0,
                                gid: 0,
                                nlink: 1,
                                ino: 1,
                                atime_ms: 0,
                                atime_nsec: 0,
                                mtime_ms: 0,
                                mtime_nsec: 0,
                                ctime_ms: 0,
                                ctime_nsec: 0,
                                birthtime_ms: 0,
                            },
                            kind: PersistedFilesystemInodeKind::Directory,
                        },
                    ),
                    (
                        2,
                        PersistedFilesystemInode {
                            metadata: agent_os_kernel::vfs::MemoryFileSystemSnapshotMetadata {
                                mode: 0o100644,
                                uid: 0,
                                gid: 0,
                                nlink: 1,
                                ino: 2,
                                atime_ms: 0,
                                atime_nsec: 0,
                                mtime_ms: 0,
                                mtime_nsec: 0,
                                ctime_ms: 0,
                                ctime_nsec: 0,
                                birthtime_ms: 0,
                            },
                            kind: PersistedFilesystemInodeKind::File {
                                storage: PersistedFileStorage::Chunked {
                                    size: u64::MAX,
                                    chunks: Vec::new(),
                                },
                            },
                        },
                    ),
                ]),
                next_ino: 3,
            };
            server.put_object(
                "test-bucket/oversized/filesystem-manifest.json",
                serde_json::to_vec(&manifest).expect("serialize malicious manifest"),
            );

            let error = match S3BackedFilesystem::from_config(test_config(&server, "oversized")) {
                Ok(_) => panic!("oversized manifest should be rejected"),
                Err(error) => error,
            };
            assert_eq!(error.code(), "EINVAL");
            assert!(
                error.message().contains("limit"),
                "unexpected error message: {}",
                error.message()
            );
        }
    }
}

use agent_os_bridge::StructuredEventRecord;
use agent_os_sidecar::protocol::{
    BootstrapRootFilesystemRequest, ConfigureVmRequest, DisposeReason, DisposeVmRequest,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, MountDescriptor,
    MountPluginDescriptor, OwnershipScope, RequestPayload, ResponsePayload, RootFilesystemEntry,
    RootFilesystemEntryEncoding, RootFilesystemEntryKind,
};
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};
use support::{authenticate, create_vm, open_session, request, temp_dir};

struct LocalS3EndpointEnvGuard {
    _lock: Option<MutexGuard<'static, ()>>,
    previous: Option<OsString>,
}

impl Drop for LocalS3EndpointEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var("AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS", previous),
            None => std::env::remove_var("AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS"),
        }
    }
}

fn allow_local_s3_endpoints() -> LocalS3EndpointEnvGuard {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock local s3 endpoint env");
    let previous = std::env::var_os("AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS");
    std::env::set_var("AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS", "1");
    LocalS3EndpointEnvGuard {
        _lock: Some(lock),
        previous,
    }
}

fn structured_events(
    sidecar: &agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
) -> Vec<StructuredEventRecord> {
    sidecar
        .with_bridge_mut(|bridge| bridge.structured_events.clone())
        .expect("inspect structured events")
}

#[test]
fn dispose_vm_surfaces_s3_flush_failures_as_structured_events() {
    let _local_s3_guard = allow_local_s3_endpoints();
    let server = s3::test_support::MockS3Server::start();
    let mut sidecar = support::new_sidecar("s3-dispose-shutdown-failure");
    let cwd = temp_dir("s3-dispose-shutdown-failure-cwd");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                entries: vec![RootFilesystemEntry {
                    path: String::from("/data"),
                    kind: RootFilesystemEntryKind::Directory,
                    ..Default::default()
                }],
            }),
        ))
        .expect("bootstrap s3 mountpoint");

    sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/data"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("s3"),
                        config: serde_json::json!({
                            "bucket": "test-bucket",
                            "prefix": "dispose-failure",
                            "region": "us-east-1",
                            "endpoint": server.base_url(),
                            "credentials": {
                                "accessKeyId": "minioadmin",
                                "secretAccessKey": "minioadmin",
                            },
                            "chunkSize": 8,
                            "inlineThreshold": 4,
                        }),
                    },
                }],
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("configure s3 mount");

    let write = sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/data/pending.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("pending s3 flush")),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ))
        .expect("write pending s3 file");
    match write.response.payload {
        ResponsePayload::GuestFilesystemResult(_) => {}
        other => panic!("unexpected write response: {other:?}"),
    }

    drop(server);

    let dispose = sidecar
        .dispatch_blocking(request(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVm(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose vm after s3 shutdown failure");
    match dispose.response.payload {
        ResponsePayload::VmDisposed(response) => assert_eq!(response.vm_id, vm_id),
        other => panic!("unexpected dispose response: {other:?}"),
    }

    let event = structured_events(&sidecar)
        .into_iter()
        .rfind(|event| event.name == "filesystem.mount.shutdown_failed")
        .expect("expected structured shutdown failure event");
    assert_eq!(event.vm_id, vm_id);
    assert_eq!(event.fields["guest_path"], "/data");
    assert_eq!(event.fields["plugin_id"], "s3");
    assert_eq!(event.fields["read_only"], "false");
    assert_eq!(event.fields["phase"], "dispose_vm");
    assert_eq!(event.fields["error_code"], "EIO");
    assert!(
        event.fields["error"].contains("write s3 object"),
        "unexpected shutdown error: {}",
        event.fields["error"]
    );
    assert!(
        event.fields["error"].contains("dispose-failure/"),
        "unexpected shutdown error: {}",
        event.fields["error"]
    );
    event.fields["timestamp"]
        .parse::<u128>()
        .expect("structured event timestamp should be numeric");
}
