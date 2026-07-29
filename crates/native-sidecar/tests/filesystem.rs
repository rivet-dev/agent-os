mod support;

mod host_dir {
    #![allow(dead_code)]
    include!("../src/plugins/host_dir.rs");

    mod tests {
        use super::HostDirFilesystem;
        use agentos_kernel::command_registry::CommandDriver;
        use agentos_kernel::fd_table::O_RDWR;
        use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
        use agentos_kernel::mount_table::{MountOptions, MountTable};
        use agentos_kernel::permissions::Permissions;
        use agentos_kernel::vfs::{
            MemoryFileSystem, VirtualFileSystem, VirtualTimeSpec, VirtualUtimeSpec,
        };
        use nix::sys::stat::{utimensat, UtimensatFlags};
        use nix::sys::time::{TimeSpec, TimeValLike};
        use std::fs;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn temp_dir(prefix: &str) -> PathBuf {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough for temp paths")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
            fs::create_dir_all(&path).expect("create temp dir");
            path
        }

        fn spawn_shell_in<F: VirtualFileSystem + 'static>(
            kernel: &mut KernelVm<F>,
        ) -> agentos_kernel::kernel::KernelProcessHandle {
            kernel
                .spawn_process(
                    "sh",
                    Vec::new(),
                    SpawnOptions {
                        requester_driver: Some(String::from("shell")),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn shell")
        }

        #[test]
        fn filesystem_host_dir_metadata_ops_reject_symlink_escape_targets() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir");
            let outside_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-outside");
            let outside_file = outside_dir.join("outside.txt");
            fs::write(&outside_file, b"outside").expect("seed outside file");
            std::os::unix::fs::symlink(&outside_file, host_dir.join("link"))
                .expect("seed escape symlink");

            let baseline = fs::metadata(&outside_file).expect("outside metadata before ops");
            let baseline_mode = baseline.permissions().mode() & 0o7777;
            let baseline_uid = baseline.uid();
            let baseline_gid = baseline.gid();
            let baseline_mtime = baseline.mtime();
            let baseline_mtime_ns = baseline.mtime_nsec();

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");

            let chmod_error = filesystem
                .chmod("/link", 0o777)
                .expect_err("chmod should reject escaped symlink target");
            assert!(matches!(chmod_error.code(), "EPERM" | "EACCES"));

            let chown_error = filesystem
                .chown("/link", baseline_uid, baseline_gid)
                .expect_err("chown should reject escaped symlink target");
            assert!(matches!(chown_error.code(), "EPERM" | "EACCES"));

            let utimes_error = filesystem
                .utimes("/link", 1_000, 2_000)
                .expect_err("utimes should reject escaped symlink target");
            assert!(matches!(utimes_error.code(), "EPERM" | "EACCES"));

            let after = fs::metadata(&outside_file).expect("outside metadata after ops");
            assert_eq!(after.permissions().mode() & 0o7777, baseline_mode);
            assert_eq!(after.uid(), baseline_uid);
            assert_eq!(after.gid(), baseline_gid);
            assert_eq!(after.mtime(), baseline_mtime);
            assert_eq!(after.mtime_nsec(), baseline_mtime_ns);

            fs::remove_dir_all(host_dir).expect("remove temp dir");
            fs::remove_dir_all(outside_dir).expect("remove outside temp dir");
        }

        #[test]
        fn filesystem_host_dir_write_file_with_mode_honors_requested_permissions() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-write-mode");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            filesystem
                .write_file_with_mode("/private.txt", b"secret".to_vec(), Some(0o600))
                .expect("write private host file");

            let metadata = fs::metadata(host_dir.join("private.txt")).expect("read file metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_host_dir_recursive_mkdir_with_mode_honors_requested_permissions() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-mkdir-mode");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            filesystem
                .mkdir_with_mode("/private/nested", true, Some(0o700))
                .expect("create private directories");

            for relative in ["private", "private/nested"] {
                let metadata =
                    fs::metadata(host_dir.join(relative)).expect("read directory metadata");
                assert_eq!(
                    metadata.permissions().mode() & 0o777,
                    0o700,
                    "unexpected mode for {relative}"
                );
            }

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_host_dir_mkdir_existing_mount_root_returns_eexist() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-mkdir-root");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            let error = filesystem
                .mkdir_with_mode("/", false, None)
                .expect_err("the mounted root already exists");

            assert_eq!(error.code(), "EEXIST");
            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_host_dir_stat_preserves_nanosecond_timestamp_precision() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-stat");
            let tracked_file = host_dir.join("tracked.txt");
            fs::write(&tracked_file, b"tracked").expect("seed tracked file");

            let atime = TimeSpec::nanoseconds(1_700_000_000_123_456_789);
            let mtime = TimeSpec::nanoseconds(1_700_000_000_987_654_321);
            utimensat(
                None,
                &tracked_file,
                &atime,
                &mtime,
                UtimensatFlags::NoFollowSymlink,
            )
            .expect("set tracked file timestamps");

            let baseline = fs::metadata(&tracked_file).expect("tracked file metadata");
            assert_ne!(
                baseline.mtime_nsec(),
                0,
                "fixture should keep non-zero mtime nsec"
            );

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            let stat = filesystem.stat("/tracked.txt").expect("stat tracked file");

            assert_eq!(
                stat.atime_ms,
                baseline.atime() as u64 * 1_000 + (baseline.atime_nsec() as u64 / 1_000_000)
            );
            assert_eq!(stat.atime_nsec, baseline.atime_nsec() as u32);
            assert_eq!(
                stat.mtime_ms,
                baseline.mtime() as u64 * 1_000 + (baseline.mtime_nsec() as u64 / 1_000_000)
            );
            assert_eq!(stat.mtime_nsec, baseline.mtime_nsec() as u32);
            assert_eq!(
                stat.ctime_ms,
                baseline.ctime() as u64 * 1_000 + (baseline.ctime_nsec() as u64 / 1_000_000)
            );
            assert_eq!(stat.ctime_nsec, baseline.ctime_nsec() as u32);

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_host_dir_utimes_spec_honors_omit_and_now_controls() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-utimes-spec");
            let tracked_file = host_dir.join("tracked.txt");
            fs::write(&tracked_file, b"tracked").expect("seed tracked file");

            let baseline_atime_sec = 1_700_000_000;
            let baseline_atime_nsec = 111_111_111;
            let baseline_mtime_sec = 1_700_000_000;
            let baseline_mtime_nsec = 222_222_222;
            let baseline_atime =
                TimeSpec::nanoseconds(baseline_atime_sec * 1_000_000_000 + baseline_atime_nsec);
            let baseline_mtime =
                TimeSpec::nanoseconds(baseline_mtime_sec * 1_000_000_000 + baseline_mtime_nsec);
            utimensat(
                None,
                &tracked_file,
                &baseline_atime,
                &baseline_mtime,
                UtimensatFlags::FollowSymlink,
            )
            .expect("seed tracked file timestamps");

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            filesystem
                .utimes_spec(
                    "/tracked.txt",
                    VirtualUtimeSpec::Set(
                        VirtualTimeSpec::new(1_700_000_123, 987_654_321)
                            .expect("valid atime timespec"),
                    ),
                    VirtualUtimeSpec::Omit,
                    true,
                )
                .expect("utimes_spec should preserve mtime");

            let after_omit = fs::metadata(&tracked_file).expect("tracked file metadata after omit");
            assert_eq!(after_omit.mtime(), baseline_mtime_sec);
            assert_eq!(after_omit.mtime_nsec(), baseline_mtime_nsec);
            assert_eq!(after_omit.atime(), 1_700_000_123);
            assert_eq!(after_omit.atime_nsec(), 987_654_321);

            filesystem
                .utimes_spec(
                    "/tracked.txt",
                    VirtualUtimeSpec::Now,
                    VirtualUtimeSpec::Omit,
                    true,
                )
                .expect("utimes_spec should accept UTIME_NOW");

            let after_now = fs::metadata(&tracked_file).expect("tracked file metadata after now");
            assert_eq!(after_now.mtime(), baseline_mtime_sec);
            assert_eq!(after_now.mtime_nsec(), baseline_mtime_nsec);
            assert!(
                after_now.atime() > after_omit.atime()
                    || (after_now.atime() == after_omit.atime()
                        && after_now.atime_nsec() >= after_omit.atime_nsec()),
                "UTIME_NOW should move atime forward"
            );

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn filesystem_host_dir_lutimes_updates_symlink_without_touching_target() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-lutimes");
            let target = host_dir.join("target.txt");
            let link = host_dir.join("link.txt");
            fs::write(&target, b"target").expect("seed target file");
            std::os::unix::fs::symlink("target.txt", &link).expect("create symlink");

            let baseline_target = fs::metadata(&target).expect("target metadata before lutimes");
            let baseline_target_mtime = baseline_target.mtime();
            let baseline_target_mtime_nsec = baseline_target.mtime_nsec();

            let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
            filesystem
                .utimes_spec(
                    "/link.txt",
                    VirtualUtimeSpec::Set(
                        VirtualTimeSpec::new(1_700_000_444, 123_456_789).expect("valid link atime"),
                    ),
                    VirtualUtimeSpec::Set(
                        VirtualTimeSpec::new(1_700_000_555, 987_654_321).expect("valid link mtime"),
                    ),
                    false,
                )
                .expect("lutimes should update the symlink itself");

            let link_metadata = fs::symlink_metadata(&link).expect("link metadata after lutimes");
            let target_metadata = fs::metadata(&target).expect("target metadata after lutimes");

            assert_eq!(link_metadata.mtime(), 1_700_000_555);
            assert_eq!(link_metadata.mtime_nsec(), 987_654_321);
            assert_eq!(link_metadata.atime(), 1_700_000_444);
            assert_eq!(link_metadata.atime_nsec(), 123_456_789);
            assert_eq!(target_metadata.mtime(), baseline_target_mtime);
            assert_eq!(target_metadata.mtime_nsec(), baseline_target_mtime_nsec);

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }

        #[test]
        fn kernel_futimes_updates_host_dir_mount_with_nanosecond_precision() {
            let host_dir = temp_dir("agentos-native-sidecar-filesystem-host-dir-futimes");
            let tracked_file = host_dir.join("tracked.txt");
            fs::write(&tracked_file, b"tracked").expect("seed tracked file");

            let mut config = KernelVmConfig::new("vm-host-dir-futimes");
            config.permissions = Permissions::allow_all();
            let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
            kernel
                .register_driver(CommandDriver::new("shell", ["sh"]))
                .expect("register shell driver");
            kernel
                .mount_filesystem(
                    "/workspace",
                    HostDirFilesystem::new(&host_dir).expect("host dir fs"),
                    MountOptions::new("host_dir"),
                )
                .expect("mount host dir");

            let process = spawn_shell_in(&mut kernel);
            let fd = kernel
                .fd_open(
                    "shell",
                    process.pid(),
                    "/workspace/tracked.txt",
                    O_RDWR,
                    None,
                )
                .expect("open tracked file");

            kernel
                .futimes(
                    "shell",
                    process.pid(),
                    fd,
                    VirtualUtimeSpec::Set(
                        VirtualTimeSpec::new(1_700_000_666, 111_222_333)
                            .expect("valid futimes atime"),
                    ),
                    VirtualUtimeSpec::Set(
                        VirtualTimeSpec::new(1_700_000_777, 444_555_666)
                            .expect("valid futimes mtime"),
                    ),
                )
                .expect("futimes should update host file");

            let metadata = fs::metadata(&tracked_file).expect("tracked metadata after futimes");
            assert_eq!(metadata.atime(), 1_700_000_666);
            assert_eq!(metadata.atime_nsec(), 111_222_333);
            assert_eq!(metadata.mtime(), 1_700_000_777);
            assert_eq!(metadata.mtime_nsec(), 444_555_666);

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }
    }
}

mod shadow_root {
    use agentos_native_sidecar::wire::{
        ConfigureVmRequest, DisposeReason, DisposeVmRequest, EventPayload, ExecuteRequest,
        GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, MountDescriptor,
        MountPluginDescriptor, RequestPayload, ResponsePayload, RootFilesystemEntryEncoding,
        StreamChannel,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::support::{
        self, authenticate_wire, create_vm_wire, open_session_wire, temp_dir, wire_request,
        wire_vm, RecordingBridge,
    };

    const PROCESS_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024;

    fn create_test_sidecar() -> agentos_native_sidecar::NativeSidecar<RecordingBridge> {
        support::new_sidecar("filesystem-test")
    }

    fn authenticate_and_open_session(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
    ) -> (String, String) {
        let connection_id = authenticate_wire(sidecar, "conn-1");
        let session_id = open_session_wire(sidecar, 2, &connection_id);
        (connection_id, session_id)
    }

    fn create_vm_with_mounts(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        extra_mounts: Vec<MountDescriptor>,
    ) -> String {
        let cwd = temp_dir("filesystem-vm-cwd");
        let (vm_id, _) = create_vm_wire(
            sidecar,
            3,
            connection_id,
            session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let command_root = registry_command_root()
            .expect("registry WASM commands are required before mounting command root");
        let mut mounts = vec![MountDescriptor {
            guest_path: String::from("/__agentos/commands/0"),
            guest_source: String::from("host_dir"),
            guest_fstype: String::from("host_dir"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("host_dir"),
                config: serde_json::to_string(&json!({
                    "hostPath": command_root,
                    "readOnly": true,
                }))
                .expect("serialize command mount config"),
            },
        }];
        mounts.extend(extra_mounts);
        configure_vm_mounts(sidecar, connection_id, session_id, &vm_id, mounts);

        vm_id
    }

    fn configure_vm_mounts(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        mounts: Vec<MountDescriptor>,
    ) {
        sidecar
            .dispatch_wire_blocking(wire_request(
                4,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                    mounts,
                    software: Vec::new(),
                    permissions: None,
                    module_access_cwd: None,
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: HashMap::new(),
                    loopback_exempt_ports: Vec::new(),
                    packages: Vec::new(),
                    packages_mount_at: String::new(),
                    bootstrap_commands: Vec::new(),
                    binding_shim_commands: Vec::new(),
                }),
            ))
            .expect("configure command mount");
    }

    fn registry_command_root() -> Option<String> {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonicalize repo root");
        let copied = repo_root.join("software/coreutils/wasm");
        if copied.exists() {
            return Some(copied.to_string_lossy().into_owned());
        }

        let fallback = repo_root.join("toolchain/target/wasm32-wasip1/release/commands");
        if fallback.exists() {
            return Some(fallback.to_string_lossy().into_owned());
        }

        eprintln!(
            "registry WASM commands are required for filesystem tests: expected {} or {}",
            copied.display(),
            fallback.display()
        );
        None
    }

    fn guest_filesystem_call(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        payload: GuestFilesystemCallRequest,
    ) -> agentos_native_sidecar::wire::GuestFilesystemResultResponse {
        let response = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::GuestFilesystemCallRequest(payload),
            ))
            .expect("dispatch guest filesystem call");
        match response.response.payload {
            ResponsePayload::GuestFilesystemResultResponse(result) => result,
            other => panic!("expected guest_filesystem_result response, got {other:?}"),
        }
    }

    fn base_guest_filesystem_request(
        operation: GuestFilesystemOperation,
        path: &str,
    ) -> GuestFilesystemCallRequest {
        GuestFilesystemCallRequest {
            operation,
            path: String::from(path),
            destination_path: None,
            target: None,
            content: None,
            encoding: None,
            recursive: false,
            max_depth: None,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
            offset: None,
        }
    }

    fn guest_path_exists(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        path: &str,
    ) -> bool {
        let response = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::GuestFilesystemCallRequest(base_guest_filesystem_request(
                    GuestFilesystemOperation::Exists,
                    path,
                )),
            ))
            .expect("dispatch guest filesystem exists");
        match response.response.payload {
            ResponsePayload::GuestFilesystemResultResponse(result) => {
                result.exists.unwrap_or(false)
            }
            other => panic!("expected guest_filesystem_result response, got {other:?}"),
        }
    }

    fn guest_lstat(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        path: &str,
    ) -> agentos_native_sidecar::wire::GuestFilesystemStat {
        guest_filesystem_call(
            sidecar,
            connection_id,
            session_id,
            vm_id,
            request_id,
            base_guest_filesystem_request(GuestFilesystemOperation::Lstat, path),
        )
        .stat
        .expect("guest lstat response should include stat")
    }

    fn guest_read_text(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        path: &str,
    ) -> String {
        let response = guest_filesystem_call(
            sidecar,
            connection_id,
            session_id,
            vm_id,
            request_id,
            base_guest_filesystem_request(GuestFilesystemOperation::ReadFile, path),
        );
        assert_eq!(
            response.encoding,
            Some(RootFilesystemEntryEncoding::Utf8),
            "test fixture should remain UTF-8"
        );
        response
            .content
            .expect("read response should include content")
    }

    fn locate_shadow_root(marker_guest_path: &str) -> PathBuf {
        let marker_relative = marker_guest_path.trim_start_matches('/');
        fs::read_dir(std::env::temp_dir())
            .expect("list temp dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("agentos-native-sidecar-shadow-"))
                    && fs::symlink_metadata(candidate.join(marker_relative)).is_ok()
            })
            .expect("locate VM shadow root through unique marker")
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestNodeType {
        File,
        Symlink,
        Directory,
    }

    fn create_guest_test_node(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: &mut i64,
        path: &str,
        node_type: TestNodeType,
    ) {
        let request = match node_type {
            TestNodeType::File => {
                let mut request =
                    base_guest_filesystem_request(GuestFilesystemOperation::WriteFile, path);
                request.content = Some(String::from("replacement file\n"));
                request.encoding = Some(RootFilesystemEntryEncoding::Utf8);
                request
            }
            TestNodeType::Symlink => {
                let mut request =
                    base_guest_filesystem_request(GuestFilesystemOperation::Symlink, path);
                request.target = Some(String::from("target.txt"));
                request
            }
            TestNodeType::Directory => {
                let mut request =
                    base_guest_filesystem_request(GuestFilesystemOperation::Mkdir, path);
                request.recursive = true;
                request
            }
        };
        guest_filesystem_call(
            sidecar,
            connection_id,
            session_id,
            vm_id,
            *request_id,
            request,
        );
        *request_id += 1;
    }

    fn replace_shadow_test_node(path: &Path, node_type: TestNodeType) {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(path).expect("remove old shadow directory");
            } else {
                fs::remove_file(path).expect("remove old shadow file or symlink");
            }
        }
        match node_type {
            TestNodeType::File => {
                fs::write(path, b"replacement file\n").expect("write replacement shadow file")
            }
            TestNodeType::Symlink => std::os::unix::fs::symlink("target.txt", path)
                .expect("create replacement shadow symlink"),
            TestNodeType::Directory => {
                fs::create_dir(path).expect("create replacement shadow directory")
            }
        }
    }

    /// Deleting a path directly from the VM shadow root (the way host-side
    /// guest runtimes delete files, without a kernel-direct unlink) must
    /// propagate into the kernel VFS on the next shadow sync walk instead of
    /// being resurrected by the additive copy-in.
    #[test]
    fn shadow_direct_deletions_before_first_sync_propagate_into_kernel_vfs() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        // Guest filesystem calls need no command mounts; create the VM bare so
        // this regression test runs even without built registry commands.
        let cwd = temp_dir("filesystem-shadow-reconcile-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let doomed_root = format!("/workspace/reconcile-{nonce}");
        let doomed_dir = format!("{doomed_root}/nested");
        let doomed_file = format!("{doomed_dir}/probe.txt");
        let survivor_file = format!("/workspace/reconcile-survivor-{nonce}.txt");

        let mut mkdir =
            base_guest_filesystem_request(GuestFilesystemOperation::CreateDir, &doomed_dir);
        mkdir.recursive = true;
        guest_filesystem_call(&mut sidecar, &connection_id, &session_id, &vm_id, 20, mkdir);

        let mut write =
            base_guest_filesystem_request(GuestFilesystemOperation::WriteFile, &doomed_file);
        write.content = Some(String::from("doomed\n"));
        write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        guest_filesystem_call(&mut sidecar, &connection_id, &session_id, &vm_id, 21, write);

        let mut survivor =
            base_guest_filesystem_request(GuestFilesystemOperation::WriteFile, &survivor_file);
        survivor.content = Some(String::from("survivor\n"));
        survivor.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            22,
            survivor,
        );

        // Locate this VM's shadow root through the mirrored unique file, then
        // delete the subtree before any read-side sync primes the inventory.
        // This is exactly how a short-lived host-side runtime file can be
        // created and deleted between reconciliation walks.
        let marker_rel = format!("workspace/reconcile-{nonce}/nested/probe.txt");
        let shadow_root = std::fs::read_dir(std::env::temp_dir())
            .expect("list temp dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("agentos-native-sidecar-shadow-"))
                    && candidate.join(&marker_rel).is_file()
            })
            .expect("locate VM shadow root through mirrored probe file");
        fs::remove_dir_all(shadow_root.join(format!("workspace/reconcile-{nonce}")))
            .expect("delete subtree from the shadow root");

        // The next host filesystem call re-walks the shadow; the kernel must
        // drop the deleted subtree instead of resurrecting it forever.
        assert!(
            !guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                24,
                &doomed_file,
            ),
            "kernel resurrected a file deleted from the shadow root"
        );
        assert!(
            !guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                25,
                &doomed_root,
            ),
            "kernel resurrected a directory deleted from the shadow root"
        );
        assert!(
            guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                26,
                &survivor_file,
            ),
            "deletion reconcile must not remove shadow-backed paths that still exist"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn shadow_type_replacements_cover_file_symlink_directory_matrix() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-replacement-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let base = format!("/workspace/replacement-matrix-{nonce}");
        let target = format!("{base}/target.txt");
        let mut request_id = 20;
        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &target,
            TestNodeType::File,
        );
        let shadow_root = locate_shadow_root(&target);
        let transitions = [
            (TestNodeType::File, TestNodeType::Symlink),
            (TestNodeType::File, TestNodeType::Directory),
            (TestNodeType::Symlink, TestNodeType::File),
            (TestNodeType::Symlink, TestNodeType::Directory),
            (TestNodeType::Directory, TestNodeType::File),
            (TestNodeType::Directory, TestNodeType::Symlink),
        ];

        for (index, (initial, replacement)) in transitions.into_iter().enumerate() {
            let path = format!("{base}/node-{index}");
            create_guest_test_node(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                &mut request_id,
                &path,
                initial,
            );
            // Prime this node's initial type so reconciliation must explicitly
            // unlink the stale kernel node before copying the replacement.
            guest_lstat(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                &path,
            );
            request_id += 1;

            replace_shadow_test_node(&shadow_root.join(path.trim_start_matches('/')), replacement);
            let stat = guest_lstat(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                &path,
            );
            request_id += 1;
            match replacement {
                TestNodeType::File => {
                    assert!(!stat.is_directory && !stat.is_symbolic_link)
                }
                TestNodeType::Symlink => assert!(stat.is_symbolic_link),
                TestNodeType::Directory => {
                    assert!(stat.is_directory && !stat.is_symbolic_link)
                }
            }
            assert_eq!(
                guest_read_text(
                    &mut sidecar,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    request_id,
                    &target,
                ),
                "replacement file\n",
                "replacing {initial:?} with {replacement:?} followed and overwrote the stale symlink target"
            );
            request_id += 1;
        }

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn guest_rename_moves_a_broken_symlink_without_resurrecting_its_source() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-broken-symlink-rename-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let source = format!("/workspace/broken-link-{nonce}");
        let destination = format!("/workspace/renamed-broken-link-{nonce}");
        let target = format!("missing-target-{nonce}");
        let mut symlink_request =
            base_guest_filesystem_request(GuestFilesystemOperation::Symlink, &source);
        symlink_request.target = Some(target.clone());
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            20,
            symlink_request,
        );

        let shadow_root = locate_shadow_root(&source);
        let shadow_source = shadow_root.join(source.trim_start_matches('/'));
        assert!(fs::symlink_metadata(&shadow_source)
            .expect("lstat broken shadow symlink")
            .file_type()
            .is_symlink());
        assert!(
            fs::metadata(&shadow_source).is_err(),
            "test symlink must remain dangling"
        );

        let mut rename_request =
            base_guest_filesystem_request(GuestFilesystemOperation::Rename, &source);
        rename_request.destination_path = Some(destination.clone());
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            21,
            rename_request,
        );

        assert!(
            !guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                22,
                &source,
            ),
            "reconciliation resurrected the renamed broken symlink at its source"
        );
        assert!(
            guest_lstat(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                23,
                &destination,
            )
            .is_symbolic_link
        );
        assert_eq!(
            guest_filesystem_call(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                24,
                base_guest_filesystem_request(GuestFilesystemOperation::ReadLink, &destination,),
            )
            .target
            .as_deref(),
            Some(target.as_str())
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn shadow_mode_change_updates_an_existing_kernel_directory() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-existing-directory-mode-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = format!("/workspace/mode-change-{nonce}");
        let mut request_id = 20;
        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &directory,
            TestNodeType::Directory,
        );
        let shadow_root = locate_shadow_root(&directory);
        let shadow_directory = shadow_root.join(directory.trim_start_matches('/'));
        fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(0o710))
            .expect("change existing shadow directory mode");

        let stat = guest_lstat(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            &directory,
        );
        assert_eq!(
            stat.mode & 0o7777,
            0o710,
            "shadow reconciliation did not import the existing directory mode"
        );

        fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(0o755))
            .expect("restore shadow directory mode");
        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn guest_chmod_zero_preserves_descendant_deletion_inventory() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-chmod-zero-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = format!("/workspace/chmod-zero-{nonce}");
        let child = format!("{directory}/tracked-child.txt");
        let mut request_id = 20;
        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &child,
            TestNodeType::File,
        );
        let shadow_root = locate_shadow_root(&child);
        let shadow_directory = shadow_root.join(directory.trim_start_matches('/'));
        let shadow_child = shadow_root.join(child.trim_start_matches('/'));

        let mut chmod_request =
            base_guest_filesystem_request(GuestFilesystemOperation::Chmod, &directory);
        chmod_request.mode = Some(0);
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            chmod_request,
        );
        request_id += 1;

        fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(0o755))
            .expect("restore shadow directory access for direct deletion");
        fs::remove_file(&shadow_child).expect("delete tracked child from shadow");
        assert!(
            !guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                &child,
            ),
            "chmod 000 discarded descendant inventory and resurrected a deleted child"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn unreadable_shadow_subtree_does_not_delete_kernel_children() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-unreadable-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = format!("/workspace/unreadable-{nonce}");
        let child = format!("{directory}/preserved.txt");
        let mut request_id = 20;
        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &child,
            TestNodeType::File,
        );
        assert!(guest_path_exists(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            &child,
        ));

        let shadow_root = locate_shadow_root(&child);
        let shadow_directory = shadow_root.join(directory.trim_start_matches('/'));
        let original_mode = fs::metadata(&shadow_directory)
            .expect("stat shadow directory")
            .permissions()
            .mode();
        fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(0o000))
            .expect("make shadow directory unreadable");
        if fs::read_dir(&shadow_directory).is_ok() {
            fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(original_mode))
                .expect("restore readable shadow directory");
            dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
            return;
        }

        let preserved = guest_path_exists(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id + 1,
            &child,
        );
        fs::set_permissions(&shadow_directory, fs::Permissions::from_mode(original_mode))
            .expect("restore readable shadow directory");
        assert!(
            preserved,
            "an unreadable shadow subtree was mistaken for a deleted subtree"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[test]
    fn shadow_sync_skips_every_live_normalized_mount_boundary() {
        let host_mount = temp_dir("filesystem-shadow-mount-boundary-host");
        fs::write(host_mount.join("value.txt"), b"host plugin\n").expect("seed host mount file");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-mount-boundary-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let locator = format!("/workspace/mount-boundary-locator-{nonce}.txt");
        let mut request_id = 20;
        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &locator,
            TestNodeType::File,
        );
        let shadow_root = locate_shadow_root(&locator);

        configure_vm_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            vec![
                MountDescriptor {
                    guest_path: String::from("/mount-boundaries/./memory//"),
                    guest_source: String::from("memory"),
                    guest_fstype: String::from("memory"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("memory"),
                        config: String::from("{}"),
                    },
                },
                MountDescriptor {
                    guest_path: String::from("/mount-boundaries/host/../host//"),
                    guest_source: String::from("host_dir"),
                    guest_fstype: String::from("host_dir"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: serde_json::to_string(&json!({
                            "hostPath": host_mount.to_string_lossy().into_owned(),
                            "readOnly": false,
                        }))
                        .expect("serialize host mount config"),
                    },
                },
            ],
        );

        let memory_path = "/mount-boundaries/memory/value.txt";
        let mut write_memory =
            base_guest_filesystem_request(GuestFilesystemOperation::WriteFile, memory_path);
        write_memory.content = Some(String::from("memory plugin\n"));
        write_memory.encoding = Some(RootFilesystemEntryEncoding::Utf8);
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            write_memory,
        );
        request_id += 1;

        let shadow_memory = shadow_root.join("mount-boundaries/memory/value.txt");
        let shadow_host = shadow_root.join("mount-boundaries/host/value.txt");
        fs::create_dir_all(shadow_host.parent().expect("shadow host parent"))
            .expect("create stale host mount shadow");
        fs::write(&shadow_memory, b"stale shadow\n").expect("overwrite memory mount shadow");
        fs::write(&shadow_host, b"stale shadow\n").expect("write host mount shadow");

        assert_eq!(
            guest_read_text(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                memory_path,
            ),
            "memory plugin\n"
        );
        request_id += 1;
        assert_eq!(
            guest_read_text(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                "/mount-boundaries/host/value.txt",
            ),
            "host plugin\n"
        );
        request_id += 1;

        fs::remove_dir_all(shadow_root.join("mount-boundaries/memory"))
            .expect("delete memory mount shadow subtree");
        fs::remove_dir_all(shadow_root.join("mount-boundaries/host"))
            .expect("delete host mount shadow subtree");
        assert_eq!(
            guest_read_text(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                memory_path,
            ),
            "memory plugin\n",
            "shadow deletion crossed the normalized memory mount boundary"
        );
        request_id += 1;
        assert_eq!(
            guest_read_text(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                "/mount-boundaries/host/value.txt",
            ),
            "host plugin\n",
            "shadow deletion crossed the normalized host_dir mount boundary"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
        fs::remove_dir_all(host_mount).expect("remove host mount temp dir");
    }

    #[test]
    fn failed_shadow_directory_deletion_retries_after_unmounted_mountpoint_is_removed() {
        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let cwd = temp_dir("filesystem-shadow-delete-retry-cwd");
        let (vm_id, _) = create_vm_wire(
            &mut sidecar,
            3,
            &connection_id,
            &session_id,
            GuestRuntimeKind::JavaScript,
            &cwd,
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = format!("/workspace/delete-retry-{nonce}");
        let mountpoint = format!("{directory}/mounted");
        let mut request_id = 20;

        create_guest_test_node(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &mut request_id,
            &directory,
            TestNodeType::Directory,
        );
        assert!(guest_path_exists(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            &directory,
        ));
        request_id += 1;

        // Mounting creates the mountpoint in the kernel VFS but not in the host
        // shadow. After unmounting, that kernel-only directory makes the first
        // tracked parent removal fail ENOTEMPTY.
        configure_vm_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            vec![MountDescriptor {
                guest_path: mountpoint.clone(),
                guest_source: String::from("memory"),
                guest_fstype: String::from("memory"),
                read_only: false,
                plugin: MountPluginDescriptor {
                    id: String::from("memory"),
                    config: String::from("{}"),
                },
            }],
        );
        configure_vm_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            Vec::new(),
        );
        let shadow_root = locate_shadow_root(&directory);
        fs::remove_dir_all(shadow_root.join(directory.trim_start_matches('/')))
            .expect("remove retry directory from shadow");
        assert!(
            guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                &directory,
            ),
            "first non-empty directory deletion should leave the kernel directory for retry"
        );
        request_id += 1;

        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            request_id,
            base_guest_filesystem_request(GuestFilesystemOperation::RemoveDir, &mountpoint),
        );
        request_id += 1;
        assert!(
            !guest_path_exists(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                request_id,
                &directory,
            ),
            "pending directory deletion was not retried after its blocker disappeared"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_command(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        process_id: &str,
        command: &str,
        args: Vec<String>,
    ) -> (String, String, Option<i32>) {
        let response = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::ExecuteRequest(ExecuteRequest {
                    process_id: String::from(process_id),
                    command: Some(String::from(command)),
                    runtime: None,
                    entrypoint: None,
                    args,
                    env: HashMap::new(),
                    cwd: Some(String::from("/workspace")),
                    wasm_permission_tier: None,
                }),
            ))
            .expect("dispatch execute");

        match response.response.payload {
            ResponsePayload::ProcessStartedResponse(started) => {
                assert_eq!(started.process_id, process_id);
            }
            other => panic!("unexpected execute response: {other:?}"),
        }

        drain_process_output(sidecar, connection_id, session_id, vm_id, process_id)
    }

    fn execute_javascript_entrypoint(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        process_id: &str,
        entrypoint: &str,
    ) -> (String, String, Option<i32>) {
        let response = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::ExecuteRequest(ExecuteRequest {
                    process_id: String::from(process_id),
                    command: None,
                    runtime: Some(GuestRuntimeKind::JavaScript),
                    entrypoint: Some(String::from(entrypoint)),
                    args: Vec::new(),
                    env: HashMap::new(),
                    cwd: Some(String::from("/workspace")),
                    wasm_permission_tier: None,
                }),
            ))
            .expect("dispatch execute");

        match response.response.payload {
            ResponsePayload::ProcessStartedResponse(started) => {
                assert_eq!(started.process_id, process_id);
            }
            other => panic!("unexpected execute response: {other:?}"),
        }

        drain_process_output(sidecar, connection_id, session_id, vm_id, process_id)
    }

    fn drain_process_output(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        process_id: &str,
    ) -> (String, String, Option<i32>) {
        let ownership = wire_vm(connection_id, session_id, vm_id);
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;

        for _ in 0..64 {
            let Some(event) = sidecar
                .poll_event_wire_blocking(&ownership, Duration::from_secs(5))
                .expect("poll wire process event")
            else {
                if exit_code.is_some() {
                    break;
                }
                panic!("timed out waiting for process {process_id} to exit");
            };

            match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => {
                            append_process_output(
                                &mut stdout,
                                &output.chunk,
                                &output.process_id,
                                "stdout",
                            );
                        }
                        StreamChannel::Stderr => {
                            append_process_output(
                                &mut stderr,
                                &output.chunk,
                                &output.process_id,
                                "stderr",
                            );
                        }
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    exit_code = Some(exited.exit_code);
                    break;
                }
                _ => {}
            }
        }

        (stdout, stderr, exit_code)
    }

    fn append_process_output(buffer: &mut String, chunk: &[u8], process_id: &str, channel: &str) {
        let text = String::from_utf8_lossy(chunk);
        assert!(
            buffer.len().saturating_add(text.len()) <= PROCESS_OUTPUT_BYTE_LIMIT,
            "filesystem process {process_id} exceeded {PROCESS_OUTPUT_BYTE_LIMIT} bytes on {channel}"
        );
        buffer.push_str(&text);
    }

    fn dispose_vm_and_close_session(
        sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
    ) {
        sidecar
            .dispatch_wire_blocking(wire_request(
                90,
                wire_vm(connection_id, session_id, vm_id),
                RequestPayload::DisposeVmRequest(DisposeVmRequest {
                    reason: DisposeReason::Requested,
                }),
            ))
            .expect("dispose vm");
        sidecar
            .close_session_blocking(connection_id, session_id)
            .expect("close session");
        sidecar
            .remove_connection_blocking(connection_id)
            .expect("remove connection");
    }

    #[test]
    fn filesystem_cross_mount_rename_reports_exdev_to_js_and_falls_back_in_shell() {
        if registry_command_root().is_none() {
            return;
        }

        let host_dir = temp_dir("agentos-native-sidecar-cross-mount-rename-js");
        fs::write(host_dir.join("source.txt"), "mapped-source\n").expect("seed mapped file");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let vm_id = create_vm_with_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            vec![MountDescriptor {
                guest_path: String::from("/mapped"),
                guest_source: String::from("host_dir"),
                guest_fstype: String::from("host_dir"),
                read_only: false,
                plugin: MountPluginDescriptor {
                    id: String::from("host_dir"),
                    config: serde_json::to_string(&json!({
                        "hostPath": host_dir.to_string_lossy().into_owned(),
                        "readOnly": false,
                    }))
                    .expect("serialize mapped mount config"),
                },
            }],
        );

        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            4,
            GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/original.txt"),
                content: Some(String::from("original\n")),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::WriteFile,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    max_depth: None,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
                    offset: None,
                }
            },
        );
        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            5,
            GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Symlink,
                path: String::from("/workspace/alias.txt"),
                target: Some(String::from("/workspace/original.txt")),
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::Symlink,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    max_depth: None,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
                    offset: None,
                }
            },
        );

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            6,
            "proc-ls-symlink",
            "/bin/ls",
            vec![String::from("-l"), String::from("/workspace")],
        );
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        assert!(
            stdout.contains("alias.txt"),
            "stdout did not render mirrored symlink:\n{stdout}"
        );

        let (cat_stdout, cat_stderr, cat_exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            7,
            "proc-cat-symlink",
            "/bin/cat",
            vec![String::from("/workspace/alias.txt")],
        );
        assert_eq!(cat_exit_code, Some(0), "stderr: {cat_stderr}");
        assert_eq!(cat_stdout, "original\n");

        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            8,
            GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Link,
                path: String::from("/workspace/original.txt"),
                destination_path: Some(String::from("/workspace/linked.txt")),
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::Link,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    max_depth: None,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
                    offset: None,
                }
            },
        );

        let (ls_stdout, ls_stderr, ls_exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            9,
            "proc-ls-link",
            "/bin/ls",
            vec![String::from("-l"), String::from("/workspace")],
        );
        assert_eq!(ls_exit_code, Some(0), "stderr: {ls_stderr}");
        assert!(
            ls_stdout.contains("linked.txt"),
            "stdout did not render mirrored hard link:\n{ls_stdout}"
        );

        let (cat_stdout, cat_stderr, cat_exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            10,
            "proc-cat-link",
            "/bin/cat",
            vec![String::from("/workspace/linked.txt")],
        );
        assert_eq!(cat_exit_code, Some(0), "stderr: {cat_stderr}");
        assert_eq!(cat_stdout, "original\n");

        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            11,
            GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Mkdir,
                path: String::from("/kernel"),
                recursive: false,
                max_depth: None,
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::Mkdir,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    max_depth: None,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
                    offset: None,
                }
            },
        );

        guest_filesystem_call(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            12,
            GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/rename-check.js"),
                content: Some(String::from(
                    r#"const fs = require("node:fs");

try {
  fs.renameSync("/mapped/source.txt", "/kernel/dest.txt");
  console.log(JSON.stringify({ ok: true }));
} catch (error) {
  console.log(JSON.stringify({ ok: false, code: error.code, message: error.message }));
}
"#,
                )),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::WriteFile,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    max_depth: None,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
                    offset: None,
                }
            },
        );

        let (stdout, stderr, exit_code) = execute_javascript_entrypoint(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            13,
            "proc-js-rename-exdev",
            "/workspace/rename-check.js",
        );
        assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        let result: serde_json::Value =
            serde_json::from_str(stdout.trim()).expect("parse renameSync result");
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "EXDEV");
        assert!(
            !host_dir.join("dest.txt").exists(),
            "renameSync should not create a host destination during EXDEV failure"
        );
        assert!(
            host_dir.join("source.txt").exists(),
            "renameSync should leave the mapped source in place on EXDEV"
        );

        fs::write(host_dir.join("source.txt"), "mv-fallback\n").expect("reset mapped file for mv");

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            14,
            "proc-mv-cross-mount",
            "/bin/mv",
            vec![
                String::from("/mapped/source.txt"),
                String::from("/kernel/copied.txt"),
            ],
        );
        assert_eq!(exit_code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stderr, "");

        let (cat_stdout, cat_stderr, cat_exit_code) = execute_command(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            15,
            "proc-cat-cross-mount",
            "/bin/cat",
            vec![String::from("/kernel/copied.txt")],
        );
        assert_eq!(cat_exit_code, Some(0), "stderr: {cat_stderr}");
        assert_eq!(cat_stdout, "mv-fallback\n");
        assert!(
            !host_dir.join("source.txt").exists(),
            "mv should unlink the mapped source after copying"
        );

        dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
        fs::remove_dir_all(host_dir).expect("remove temp dir");
    }
}
