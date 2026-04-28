mod sandbox_agent {
    include!("../src/plugins/sandbox_agent.rs");

    mod tests {
        use super::test_support::MockSandboxAgentServer;
        use super::{SandboxAgentFilesystem, SandboxAgentMountConfig, SandboxAgentMountPlugin};
        use agent_os_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
        use agent_os_kernel::vfs::VirtualFileSystem;
        use nix::unistd::{Gid, Uid};
        use serde_json::json;
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        #[test]
        fn filesystem_round_trips_small_files_and_uses_http_range_for_large_pread() {
            let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin", None);
            fs::write(server.root().join("hello.txt"), "hello from sandbox").expect("seed file");
            let large_file = (0..100 * 1024)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>();
            fs::write(server.root().join("large.bin"), &large_file).expect("seed large file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: None,
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            assert_eq!(
                filesystem
                    .read_text_file("/hello.txt")
                    .expect("read remote file"),
                "hello from sandbox"
            );

            filesystem
                .write_file("/nested/from-vm.txt", b"native sandbox mount".to_vec())
                .expect("write remote file");
            assert_eq!(
                fs::read_to_string(server.root().join("nested/from-vm.txt"))
                    .expect("read written file"),
                "native sandbox mount"
            );

            let chunk = filesystem
                .pread("/large.bin", 4_096, 1_024)
                .expect("pread should use a byte range");
            assert_eq!(chunk, large_file[4_096..5_120].to_vec());

            let logged_requests = server.requests();
            let pread_request = logged_requests
                .iter()
                .find(|request| {
                    request.method == "GET"
                        && request.path == "/v1/fs/file"
                        && request.query.get("path") == Some(&String::from("/large.bin"))
                })
                .expect("log pread request");
            assert_eq!(
                pread_request.headers.get("range"),
                Some(&String::from("bytes=4096-5119"))
            );
            assert_eq!(pread_request.response_status, 206);
            assert_eq!(pread_request.response_body_bytes, 1_024);
        }

        #[test]
        fn filesystem_pread_falls_back_to_full_fetch_when_remote_ignores_range() {
            let server = MockSandboxAgentServer::start_without_range_support(
                "agent-os-sandbox-plugin",
                None,
            );
            let large_file = (0..100 * 1024)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>();
            fs::write(server.root().join("large.bin"), &large_file).expect("seed large file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: None,
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            let chunk = filesystem
                .pread("/large.bin", 4_096, 1_024)
                .expect("pread should fall back to the full response");
            assert_eq!(chunk, large_file[4_096..5_120].to_vec());

            let logged_requests = server.requests();
            let pread_request = logged_requests
                .iter()
                .find(|request| {
                    request.method == "GET"
                        && request.path == "/v1/fs/file"
                        && request.query.get("path") == Some(&String::from("/large.bin"))
                })
                .expect("log pread request");
            assert_eq!(
                pread_request.headers.get("range"),
                Some(&String::from("bytes=4096-5119"))
            );
            assert_eq!(pread_request.response_status, 200);
            assert_eq!(pread_request.response_body_bytes, large_file.len());
        }

        #[test]
        fn filesystem_truncate_uses_process_api_without_full_file_buffering() {
            let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin-truncate", None);
            fs::write(server.root().join("large.bin"), vec![b'x'; 512]).expect("seed large file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: None,
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            filesystem
                .truncate("/large.bin", 3)
                .expect("truncate large file through process helper");
            assert_eq!(
                fs::read(server.root().join("large.bin")).expect("read truncated file"),
                b"xxx".to_vec()
            );

            filesystem
                .truncate("/large.bin", 6)
                .expect("extend file through process helper");
            assert_eq!(
                fs::read(server.root().join("large.bin")).expect("read extended file"),
                vec![b'x', b'x', b'x', 0, 0, 0]
            );

            filesystem
                .truncate("/large.bin", 0)
                .expect("truncate to zero through write_file path");
            assert_eq!(
                fs::metadata(server.root().join("large.bin"))
                    .expect("stat zero-length file")
                    .len(),
                0
            );

            let logged_requests = server.requests();
            assert!(
                logged_requests.iter().any(|request| {
                    request.method == "POST" && request.path == "/v1/processes/run"
                }),
                "non-zero truncate should use process helper"
            );
            assert!(
                !logged_requests.iter().any(|request| {
                    request.method == "GET"
                        && request.path == "/v1/fs/file"
                        && request.query.get("path") == Some(&String::from("/large.bin"))
                }),
                "truncate should not issue a full-file GET"
            );
            assert!(
                logged_requests.iter().any(|request| {
                    request.method == "PUT"
                        && request.path == "/v1/fs/file"
                        && request.query.get("path") == Some(&String::from("/large.bin"))
                }),
                "truncate(path, 0) should still use the write_file path"
            );
        }

        #[test]
        fn plugin_scopes_base_path_and_preserves_auth_headers() {
            let server =
                MockSandboxAgentServer::start("agent-os-sandbox-plugin-auth", Some("secret-token"));
            fs::create_dir_all(server.root().join("scoped")).expect("create scoped root");
            fs::write(server.root().join("scoped/hello.txt"), "scoped hello")
                .expect("seed scoped file");

            let plugin = SandboxAgentMountPlugin;
            let mut mounted = plugin
                .open(OpenFileSystemPluginRequest {
                    vm_id: "vm-1",
                    guest_path: "/sandbox",
                    read_only: false,
                    config: &json!({
                        "baseUrl": server.base_url(),
                        "token": "secret-token",
                        "headers": {
                            "x-sandbox-test": "enabled"
                        },
                        "basePath": "/scoped"
                    }),
                    context: &(),
                })
                .expect("open sandbox_agent mount");

            assert_eq!(
                mounted.read_file("/hello.txt").expect("read scoped file"),
                b"scoped hello".to_vec()
            );
            mounted
                .write_file("/from-plugin.txt", b"written through plugin".to_vec())
                .expect("write scoped file");
            assert_eq!(
                fs::read_to_string(server.root().join("scoped/from-plugin.txt"))
                    .expect("read plugin output"),
                "written through plugin"
            );

            let logged_requests = server.requests();
            assert!(logged_requests.iter().any(|request| {
                request.headers.get("x-sandbox-test") == Some(&String::from("enabled"))
            }));
        }

        #[test]
        fn filesystem_uses_process_api_for_symlink_and_metadata_operations() {
            let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin-process", None);
            fs::write(server.root().join("original.txt"), "hello from sandbox")
                .expect("seed original file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: None,
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            filesystem
                .symlink("/original.txt", "/alias.txt")
                .expect("create remote symlink");
            assert_eq!(
                filesystem
                    .read_link("/alias.txt")
                    .expect("read remote symlink"),
                "/original.txt"
            );
            assert_eq!(
                filesystem
                    .realpath("/alias.txt")
                    .expect("resolve remote symlink"),
                "/original.txt"
            );

            filesystem
                .link("/original.txt", "/linked.txt")
                .expect("create remote hard link");
            let original_metadata =
                fs::metadata(server.root().join("original.txt")).expect("stat original hard link");
            let linked_metadata =
                fs::metadata(server.root().join("linked.txt")).expect("stat linked hard link");
            assert_eq!(original_metadata.ino(), linked_metadata.ino());

            filesystem
                .write_file("/linked.txt", b"updated through hard link".to_vec())
                .expect("write through hard link");
            assert_eq!(
                fs::read_to_string(server.root().join("original.txt"))
                    .expect("read original after linked write"),
                "updated through hard link"
            );

            filesystem
                .chmod("/original.txt", 0o600)
                .expect("chmod remote file");
            assert_eq!(
                fs::metadata(server.root().join("original.txt"))
                    .expect("stat chmod result")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );

            let uid = Uid::current().as_raw();
            let gid = Gid::current().as_raw();
            filesystem
                .chown("/original.txt", uid, gid)
                .expect("chown remote file to current owner");
            let chown_metadata =
                fs::metadata(server.root().join("original.txt")).expect("stat chown result");
            assert_eq!(chown_metadata.uid(), uid);
            assert_eq!(chown_metadata.gid(), gid);

            let atime_ms = 1_700_000_000_000_u64;
            let mtime_ms = 1_710_000_000_000_u64;
            filesystem
                .utimes("/original.txt", atime_ms, mtime_ms)
                .expect("update remote timestamps");
            let utimes_metadata =
                fs::metadata(server.root().join("original.txt")).expect("stat utimes result");
            let observed_atime_ms =
                utimes_metadata.atime() * 1000 + utimes_metadata.atime_nsec() / 1_000_000;
            let observed_mtime_ms =
                utimes_metadata.mtime() * 1000 + utimes_metadata.mtime_nsec() / 1_000_000;
            assert_eq!(observed_atime_ms, atime_ms as i64);
            assert_eq!(observed_mtime_ms, mtime_ms as i64);

            let logged_requests = server.requests();
            assert!(logged_requests.iter().any(|request| {
                request.method == "POST" && request.path == "/v1/processes/run"
            }));
        }

        #[test]
        fn filesystem_reports_clear_error_when_process_api_is_unavailable() {
            let server = MockSandboxAgentServer::start_without_process_api(
                "agent-os-sandbox-plugin-no-proc",
                None,
            );
            fs::write(server.root().join("original.txt"), "hello from sandbox")
                .expect("seed original file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: None,
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            let error = filesystem
                .symlink("/original.txt", "/alias.txt")
                .expect_err("symlink should fail clearly without process API");
            assert_eq!(error.code(), "ENOSYS");
            assert!(
                error.to_string().contains("process API"),
                "error should mention process API availability: {error}"
            );
        }
    }
}
