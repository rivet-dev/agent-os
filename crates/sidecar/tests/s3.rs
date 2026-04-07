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
                                mtime_ms: 0,
                                ctime_ms: 0,
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
                                mtime_ms: 0,
                                ctime_ms: 0,
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
