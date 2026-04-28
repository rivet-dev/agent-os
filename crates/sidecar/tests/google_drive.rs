mod google_drive {
    include!("../src/plugins/google_drive.rs");

    mod tests {
        use super::test_support::MockGoogleDriveServer;
        use super::*;

        const TEST_PRIVATE_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEpAIBAAKCAQEAyRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTL\n\
UTv4l4sggh5/CYYi/cvI+SXVT9kPWSKXxJXBXd/4LkvcPuUakBoAkfh+eiFVMh2V\n\
rUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8H\n\
oGfG/AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBI\n\
Mc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi+yUod+j8MtvIj812dkS4QMiRVN/\n\
by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQIDAQABAoIBAHREk0I0O9DvECKd\n\
WUpAmF3mY7oY9PNQiu44Yaf+AoSuyRpRUGTMIgc3u3eivOE8ALX0BmYUO5JtuRNZ\n\
Dpvt4SAwqCnVUinIf6C+eH/wSurCpapSM0BAHp4aOA7igptyOMgMPYBHNA1e9A7j\n\
E0dCxKWMl3DSWNyjQTk4zeRGEAEfbNjHrq6YCtjHSZSLmWiG80hnfnYos9hOr5Jn\n\
LnyS7ZmFE/5P3XVrxLc/tQ5zum0R4cbrgzHiQP5RgfxGJaEi7XcgherCCOgurJSS\n\
bYH29Gz8u5fFbS+Yg8s+OiCss3cs1rSgJ9/eHZuzGEdUZVARH6hVMjSuwvqVTFaE\n\
8AgtleECgYEA+uLMn4kNqHlJS2A5uAnCkj90ZxEtNm3E8hAxUrhssktY5XSOAPBl\n\
xyf5RuRGIImGtUVIr4HuJSa5TX48n3Vdt9MYCprO/iYl6moNRSPt5qowIIOJmIjY\n\
2mqPDfDt/zw+fcDD3lmCJrFlzcnh0uea1CohxEbQnL3cypeLt+WbU6kCgYEAzSp1\n\
9m1ajieFkqgoB0YTpt/OroDx38vvI5unInJlEeOjQ+oIAQdN2wpxBvTrRorMU6P0\n\
7mFUbt1j+Co6CbNiw+X8HcCaqYLR5clbJOOWNR36PuzOpQLkfK8woupBxzW9B8gZ\n\
mY8rB1mbJ+/WTPrEJy6YGmIEBkWylQ2VpW8O4O0CgYEApdbvvfFBlwD9YxbrcGz7\n\
MeNCFbMz+MucqQntIKoKJ91ImPxvtc0y6e/Rhnv0oyNlaUOwJVu0yNgNG117w0g4\n\
t/+Q38mvVC5xV7/cn7x9UMFk6MkqVir3dYGEqIl/OP1grY2Tq9HtB5iyG9L8NIam\n\
QOLMyUqqMUILxdthHyFmiGkCgYEAn9+PjpjGMPHxL0gj8Q8VbzsFtou6b1deIRRA\n\
2CHmSltltR1gYVTMwXxQeUhPMmgkMqUXzs4/WijgpthY44hK1TaZEKIuoxrS70nJ\n\
4WQLf5a9k1065fDsFZD6yGjdGxvwEmlGMZgTwqV7t1I4X0Ilqhav5hcs5apYL7gn\n\
PYPeRz0CgYALHCj/Ji8XSsDoF/MhVhnGdIs2P99NNdmo3R2Pv0CuZbDKMU559LJH\n\
UvrKS8WkuWRDuKrz1W/EQKApFjDGpdqToZqriUFQzwy7mR3ayIiogzNtHcvbDHx8\n\
oFnGY0OFksX/ye0/XGpy2SFxYRwGU98HPYeBvAQQrVjdkzfy7BmXQQ==\n\
-----END RSA PRIVATE KEY-----";

        fn test_config(server: &MockGoogleDriveServer, prefix: &str) -> GoogleDriveMountConfig {
            GoogleDriveMountConfig {
                credentials: GoogleDriveMountCredentials {
                    client_email: String::from("test-service-account@example.com"),
                    private_key: String::from(TEST_PRIVATE_KEY),
                },
                folder_id: String::from("folder-123"),
                key_prefix: Some(String::from(prefix)),
                chunk_size: Some(8),
                inline_threshold: Some(4),
                token_url: Some(format!("{}/token", server.base_url())),
                api_base_url: Some(String::from(server.base_url())),
            }
        }

        #[test]
        fn google_drive_plugin_rejects_untrusted_token_hosts() {
            let server = MockGoogleDriveServer::start();
            let mut config = test_config(&server, "reject-token-host");
            config.token_url = Some(String::from("https://evil.example/token"));

            let error = match GoogleDriveBackedFilesystem::from_config(config) {
                Ok(_) => panic!("untrusted token host should be rejected"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("google_drive mount tokenUrl host must be one of"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn google_drive_plugin_rejects_untrusted_api_base_hosts() {
            let server = MockGoogleDriveServer::start();
            let mut config = test_config(&server, "reject-api-host");
            config.api_base_url = Some(String::from("https://metadata.google.internal"));

            let error = match GoogleDriveBackedFilesystem::from_config(config) {
                Ok(_) => panic!("untrusted api base host should be rejected"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("google_drive mount apiBaseUrl host must be one of"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn google_drive_query_literals_escape_backslashes_before_quotes() {
            assert_eq!(escape_query_literal(r#"plain-text"#), r#"plain-text"#);
            assert_eq!(
                escape_query_literal(r#"with\backslash"#),
                r#"with\\backslash"#
            );
            assert_eq!(escape_query_literal("with'quote"), "with\\'quote");
            assert_eq!(
                escape_query_literal(r#"path\with'quote"#),
                r#"path\\with\'quote"#
            );
        }

        #[test]
        fn google_drive_plugin_persists_files_across_reopen_and_preserves_links() {
            let server = MockGoogleDriveServer::start();

            let mut filesystem =
                GoogleDriveBackedFilesystem::from_config(test_config(&server, "persist"))
                    .expect("open google drive fs");
            filesystem
                .write_file("/workspace/original.txt", b"hello world".to_vec())
                .expect("write original");
            filesystem
                .link("/workspace/original.txt", "/workspace/linked.txt")
                .expect("link file");
            filesystem
                .symlink("/workspace/original.txt", "/workspace/alias.txt")
                .expect("symlink file");

            let mut reopened =
                GoogleDriveBackedFilesystem::from_config(test_config(&server, "persist"))
                    .expect("reopen google drive fs");

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

            let chunk_files = server
                .file_names()
                .into_iter()
                .filter(|name| name.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                chunk_files.len() >= 2,
                "expected chunked storage to create multiple google drive block files"
            );
            assert!(
                server
                    .requests()
                    .iter()
                    .any(|request| request.method == "POST" && request.path == "/token"),
                "expected oauth token requests during google drive persistence"
            );
        }

        #[test]
        fn google_drive_plugin_cleans_up_stale_chunk_objects_after_truncate() {
            let server = MockGoogleDriveServer::start();

            let mut filesystem =
                GoogleDriveBackedFilesystem::from_config(test_config(&server, "truncate"))
                    .expect("open google drive fs");
            filesystem
                .write_file("/large.txt", b"abcdefghijk".to_vec())
                .expect("write large file");

            let before = server
                .file_names()
                .into_iter()
                .filter(|name| name.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                before.len() >= 2,
                "expected multiple google drive blocks before truncation"
            );

            filesystem
                .truncate("/large.txt", 1)
                .expect("truncate to inline size");

            let after = server
                .file_names()
                .into_iter()
                .filter(|name| name.contains("/blocks/"))
                .collect::<Vec<_>>();
            assert!(
                after.is_empty(),
                "truncate should remove stale google drive block files"
            );

            let mut reopened =
                GoogleDriveBackedFilesystem::from_config(test_config(&server, "truncate"))
                    .expect("reopen truncated fs");
            assert_eq!(
                reopened
                    .read_file("/large.txt")
                    .expect("read truncated file"),
                b"a".to_vec()
            );
        }

        #[test]
        fn google_drive_plugin_rejects_oversized_manifest_entries() {
            let server = MockGoogleDriveServer::start();
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
            server.insert_file(
                "oversized/filesystem-manifest.json",
                "folder-123",
                serde_json::to_vec(&manifest).expect("serialize malicious manifest"),
            );

            let error =
                match GoogleDriveBackedFilesystem::from_config(test_config(&server, "oversized")) {
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
