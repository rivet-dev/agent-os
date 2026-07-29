mod sandbox_agent {
    include!("../src/plugins/sandbox_agent.rs");

    mod tests {
        use super::test_support::MockSandboxAgentServer;
        use super::{
            validate_sandbox_agent_base_url, SandboxAgentFilesystem, SandboxAgentMountConfig,
            SandboxAgentMountPlugin,
        };
        use agentos_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
        use agentos_kernel::vfs::VirtualFileSystem;
        use nix::unistd::{Gid, Uid};
        use serde_json::json;
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        #[test]
        fn filesystem_round_trips_small_files_and_uses_http_range_for_large_pread() {
            let server = MockSandboxAgentServer::start("agentos-sandbox-plugin", None);
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
                max_full_read_bytes: Some(200 * 1024),
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
            let server =
                MockSandboxAgentServer::start_without_range_support("agentos-sandbox-plugin", None);
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
                max_full_read_bytes: Some(200 * 1024),
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
        fn filesystem_pread_rejects_full_fetch_fallback_above_limit() {
            let server = MockSandboxAgentServer::start_without_range_support(
                "agentos-sandbox-plugin-limit",
                None,
            );
            fs::write(server.root().join("large.bin"), vec![b'x'; 4096]).expect("seed large file");

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
                .pread("/large.bin", 0, 64)
                .expect_err("full fetch fallback should be capped");
            assert_eq!(error.code(), "EIO");
            assert!(
                error.to_string().contains("exceeded 128 byte limit"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn filesystem_pread_rejects_streamed_full_fetch_fallback_above_limit() {
            let server = MockSandboxAgentServer::start_without_range_support(
                "agentos-sandbox-plugin-stream-limit",
                None,
            );
            fs::write(server.root().join("stream-over-limit"), vec![b'x'; 4096])
                .expect("seed large file");

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
                .pread("/stream-over-limit", 0, 64)
                .expect_err("close-delimited full fetch fallback should be capped");
            assert_eq!(error.code(), "EIO");
            assert!(
                error.to_string().contains("exceeded 128 byte limit"),
                "unexpected error: {error}"
            );

            let logged_requests = server.requests();
            let pread_request = logged_requests
                .iter()
                .find(|request| {
                    request.method == "GET"
                        && request.path == "/v1/fs/file"
                        && request.query.get("path") == Some(&String::from("/stream-over-limit"))
                })
                .expect("log pread request");
            assert_eq!(pread_request.response_status, 200);
            assert_eq!(pread_request.response_body_bytes, 4096);
        }

        #[test]
        fn sandbox_agent_client_does_not_follow_redirects() {
            let server = MockSandboxAgentServer::start("agentos-sandbox-plugin-redirect", None);

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
                .read_file("/redirect-to-private")
                .expect_err("sandbox_agent client should not follow redirects");
            assert_eq!(error.code(), "EIO");
            assert!(
                error.to_string().contains("status 302"),
                "unexpected redirect error: {error}"
            );

            let logged_requests = server.requests();
            assert_eq!(logged_requests.len(), 1);
            assert_eq!(logged_requests[0].response_status, 302);
        }

        #[test]
        fn sandbox_agent_base_url_accepts_loopback_over_http() {
            for base_url in [
                "http://localhost:1234",
                "http://127.0.0.1:1234",
                "http://[::1]:1234",
            ] {
                assert_eq!(
                    validate_sandbox_agent_base_url(base_url)
                        .expect("loopback baseUrl should be accepted"),
                    base_url
                );
            }
        }

        #[test]
        fn sandbox_agent_base_url_allows_private_and_public_https_hosts() {
            // baseUrl is trusted mount config, not an SSRF surface, so private /
            // metadata hosts are accepted as long as they are well-formed https.
            for base_url in [
                "https://169.254.169.254/latest",
                "https://10.0.0.1:8080",
                "https://[2001:db8::1]:8080",
                "https://sandbox.example.com",
                "https://93.184.216.34",
            ] {
                validate_sandbox_agent_base_url(base_url).unwrap_or_else(|error| {
                    panic!("https baseUrl {base_url} should be accepted: {error}")
                });
            }
            // Trailing slash is trimmed (request building depends on this).
            assert_eq!(
                validate_sandbox_agent_base_url("https://sandbox.example.com/api/")
                    .expect("trailing slash trimmed"),
                "https://sandbox.example.com/api"
            );
        }

        #[test]
        fn sandbox_agent_base_url_requires_https_for_non_local_targets() {
            // The bearer token rides the Authorization header, so non-local http
            // is still rejected to avoid leaking it over plaintext.
            for base_url in ["http://sandbox.example.com", "http://93.184.216.34"] {
                let error = validate_sandbox_agent_base_url(base_url)
                    .expect_err("non-local http baseUrl should be rejected");
                assert!(
                    error.to_string().contains("must use https"),
                    "unexpected error for {base_url}: {error}"
                );
            }
        }

        #[test]
        fn sandbox_agent_base_url_rejects_malformed_urls() {
            for base_url in ["", "not a url", "ftp://sandbox.example.com"] {
                validate_sandbox_agent_base_url(base_url).expect_err(&format!(
                    "malformed baseUrl {base_url:?} should be rejected"
                ));
            }
        }

        #[test]
        fn filesystem_truncate_uses_process_api_without_full_file_buffering() {
            let server = MockSandboxAgentServer::start("agentos-sandbox-plugin-truncate", None);
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
                MockSandboxAgentServer::start("agentos-sandbox-plugin-auth", Some("secret-token"));
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
        fn plugin_normalizes_relative_base_path_before_scoping_requests() {
            let server = MockSandboxAgentServer::start("agentos-sandbox-plugin-base-path", None);
            fs::create_dir_all(server.root().join("scoped")).expect("create scoped root");
            fs::write(
                server.root().join("scoped/hello.txt"),
                "relative scoped hello",
            )
            .expect("seed scoped file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: Some(String::from("raw/../scoped/")),
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            assert_eq!(
                filesystem
                    .read_text_file("/hello.txt")
                    .expect("read scoped file"),
                "relative scoped hello"
            );

            let logged_requests = server.requests();
            let read_request = logged_requests
                .iter()
                .find(|request| request.method == "GET" && request.path == "/v1/fs/file")
                .expect("log read request");
            assert_eq!(
                read_request.query.get("path"),
                Some(&String::from("scoped/hello.txt"))
            );
        }

        #[test]
        fn plugin_unscopes_process_helper_targets_for_relative_base_path() {
            let server =
                MockSandboxAgentServer::start("agentos-sandbox-plugin-relative-process", None);
            fs::create_dir_all(server.root().join("scoped")).expect("create scoped root");
            fs::write(
                server.root().join("scoped/original.txt"),
                "relative symlink target",
            )
            .expect("seed scoped file");

            let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
                base_url: server.base_url().to_owned(),
                token: None,
                headers: None,
                base_path: Some(String::from("raw/../scoped/")),
                timeout_ms: Some(5_000),
                max_full_read_bytes: Some(128),
            })
            .expect("create sandbox_agent filesystem");

            filesystem
                .symlink("/original.txt", "/alias.txt")
                .expect("create scoped symlink");
            assert_eq!(
                filesystem.read_link("/alias.txt").expect("read symlink"),
                "/original.txt"
            );
            assert_eq!(
                filesystem.realpath("/alias.txt").expect("resolve symlink"),
                "/original.txt"
            );

            filesystem
                .symlink("scoped/original.txt", "/relative-alias.txt")
                .expect("create relative scoped symlink");
            assert_eq!(
                filesystem
                    .read_link("/relative-alias.txt")
                    .expect("read relative symlink"),
                "scoped/original.txt"
            );
        }

        #[test]
        fn filesystem_uses_process_api_for_symlink_and_metadata_operations() {
            let server = MockSandboxAgentServer::start("agentos-sandbox-plugin-process", None);
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
                "agentos-sandbox-plugin-no-proc",
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
