mod host_dir {
    #![allow(dead_code)]
    include!("../src/plugins/host_dir.rs");

    mod tests {
        use super::HostDirFilesystem;
        use agent_os_kernel::command_registry::CommandDriver;
        use agent_os_kernel::fd_table::O_RDWR;
        use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
        use agent_os_kernel::mount_table::{MountOptions, MountTable};
        use agent_os_kernel::permissions::Permissions;
        use agent_os_kernel::vfs::{
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
        ) -> agent_os_kernel::kernel::KernelProcessHandle {
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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir");
            let outside_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-outside");
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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-write-mode");

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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-mkdir-mode");

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
        fn filesystem_host_dir_stat_preserves_nanosecond_timestamp_precision() {
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-stat");
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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-utimes-spec");
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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-lutimes");
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
            let host_dir = temp_dir("agent-os-sidecar-filesystem-host-dir-futimes");
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
    mod bridge_support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../bridge/tests/support.rs"
        ));
    }

    use self::bridge_support::RecordingBridge;
    use agent_os_sidecar::protocol::{
        AuthenticateRequest, ConfigureVmRequest, CreateVmRequest, DisposeReason, DisposeVmRequest,
        EventPayload, ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
        GuestRuntimeKind, MountDescriptor, MountPluginDescriptor, OpenSessionRequest,
        OwnershipScope, PermissionsPolicy, RequestFrame, RequestPayload, ResponsePayload,
        RootFilesystemEntryEncoding, SidecarPlacement,
    };
    use agent_os_sidecar::{DispatchResult, NativeSidecar, NativeSidecarConfig};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs;
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    use std::sync::OnceLock;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use nix::fcntl::{flock, FlockArg};

    const TEST_AUTH_TOKEN: &str = "sidecar-test-token";

    fn request(
        request_id: i64,
        ownership: OwnershipScope,
        payload: RequestPayload,
    ) -> RequestFrame {
        RequestFrame::new(request_id, ownership, payload)
    }

    fn acquire_sidecar_runtime_test_lock() {
        static LOCK_FILE: OnceLock<std::fs::File> = OnceLock::new();
        let _ = LOCK_FILE.get_or_init(|| {
            let path = std::env::temp_dir().join("agent-os-sidecar-runtime-tests.lock");
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap_or_else(|error| {
                    panic!("open sidecar test runtime lock {}: {error}", path.display())
                });
            flock(file.as_raw_fd(), FlockArg::LockExclusive).unwrap_or_else(|error| {
                panic!("lock sidecar test runtime {}: {error}", path.display())
            });
            file
        });
    }

    fn create_test_sidecar() -> NativeSidecar<RecordingBridge> {
        acquire_sidecar_runtime_test_lock();
        NativeSidecar::with_config(
            RecordingBridge::default(),
            NativeSidecarConfig {
                sidecar_id: String::from("sidecar-filesystem-test"),
                compile_cache_root: Some(temp_dir("agent-os-sidecar-filesystem-test-cache")),
                expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                ..NativeSidecarConfig::default()
            },
        )
        .expect("create sidecar")
    }

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for temp paths")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn authenticated_connection_id(auth: DispatchResult) -> String {
        match auth.response.payload {
            ResponsePayload::Authenticated(response) => response.connection_id,
            other => panic!("expected authenticated response, got {other:?}"),
        }
    }

    fn opened_session_id(session: DispatchResult) -> String {
        match session.response.payload {
            ResponsePayload::SessionOpened(response) => response.session_id,
            other => panic!("expected session_opened response, got {other:?}"),
        }
    }

    fn created_vm_id(response: DispatchResult) -> String {
        match response.response.payload {
            ResponsePayload::VmCreated(response) => response.vm_id,
            other => panic!("expected vm_created response, got {other:?}"),
        }
    }

    fn authenticate_and_open_session(
        sidecar: &mut NativeSidecar<RecordingBridge>,
    ) -> (String, String) {
        let auth = sidecar
            .dispatch_blocking(request(
                1,
                OwnershipScope::connection("conn-1"),
                RequestPayload::Authenticate(AuthenticateRequest {
                    client_name: String::from("filesystem-tests"),
                    auth_token: String::from(TEST_AUTH_TOKEN),
                    bridge_version: agent_os_bridge::bridge_contract().version,
                }),
            ))
            .expect("authenticate");
        let connection_id = authenticated_connection_id(auth);

        let session = sidecar
            .dispatch_blocking(request(
                2,
                OwnershipScope::connection(&connection_id),
                RequestPayload::OpenSession(OpenSessionRequest {
                    placement: SidecarPlacement::Shared { pool: None },
                    metadata: BTreeMap::new(),
                }),
            ))
            .expect("open session");
        let session_id = opened_session_id(session);
        (connection_id, session_id)
    }

    fn create_vm_with_mounts(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        extra_mounts: Vec<MountDescriptor>,
    ) -> String {
        let response = sidecar
            .dispatch_blocking(request(
                3,
                OwnershipScope::session(connection_id, session_id),
                RequestPayload::CreateVm(CreateVmRequest {
                    runtime: GuestRuntimeKind::JavaScript,
                    metadata: BTreeMap::new(),
                    root_filesystem: Default::default(),
                    permissions: Some(PermissionsPolicy::allow_all()),
                }),
            ))
            .expect("create vm");

        let vm_id = created_vm_id(response);
        let mut mounts = vec![MountDescriptor {
            guest_path: String::from("/__agentos/commands/0"),
            read_only: true,
            plugin: MountPluginDescriptor {
                id: String::from("host_dir"),
                config: json!({
                    "hostPath": registry_command_root(),
                    "readOnly": true,
                }),
            },
        }];
        mounts.extend(extra_mounts);
        sidecar
            .dispatch_blocking(request(
                4,
                OwnershipScope::vm(connection_id, session_id, &vm_id),
                RequestPayload::ConfigureVm(ConfigureVmRequest {
                    mounts,
                    software: Vec::new(),
                    permissions: None,
                    module_access_cwd: None,
                    instructions: Vec::new(),
                    projected_modules: Vec::new(),
                    command_permissions: BTreeMap::new(),
                    allowed_node_builtins: Vec::new(),
                    loopback_exempt_ports: Vec::new(),
                }),
            ))
            .expect("configure command mount");

        vm_id
    }

    fn registry_command_root() -> String {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonicalize repo root");
        let copied = repo_root.join("registry/software/coreutils/wasm");
        if copied.exists() {
            return copied.to_string_lossy().into_owned();
        }

        let fallback = repo_root.join("registry/native/target/wasm32-wasip1/release/commands");
        if fallback.exists() {
            return fallback.to_string_lossy().into_owned();
        }

        panic!(
            "registry WASM commands are required for filesystem tests: expected {} or {}",
            copied.display(),
            fallback.display()
        );
    }

    fn guest_filesystem_call(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        payload: GuestFilesystemCallRequest,
    ) {
        let response = sidecar
            .dispatch_blocking(request(
                request_id,
                OwnershipScope::vm(connection_id, session_id, vm_id),
                RequestPayload::GuestFilesystemCall(payload),
            ))
            .expect("dispatch guest filesystem call");
        match response.response.payload {
            ResponsePayload::GuestFilesystemResult(_) => {}
            other => panic!("expected guest_filesystem_result response, got {other:?}"),
        }
    }

    fn execute_command(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        process_id: &str,
        command: &str,
        args: Vec<String>,
    ) -> (String, String, Option<i32>) {
        let response = sidecar
            .dispatch_blocking(request(
                request_id,
                OwnershipScope::vm(connection_id, session_id, vm_id),
                RequestPayload::Execute(ExecuteRequest {
                    process_id: String::from(process_id),
                    command: Some(String::from(command)),
                    runtime: None,
                    entrypoint: None,
                    args,
                    env: BTreeMap::new(),
                    cwd: Some(String::from("/workspace")),
                    wasm_permission_tier: None,
                }),
            ))
            .expect("dispatch execute");

        match response.response.payload {
            ResponsePayload::ProcessStarted(started) => {
                assert_eq!(started.process_id, process_id);
            }
            other => panic!("unexpected execute response: {other:?}"),
        }

        let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
        drain_process_output(sidecar, &ownership, process_id)
    }

    fn execute_javascript_entrypoint(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        request_id: i64,
        process_id: &str,
        entrypoint: &str,
    ) -> (String, String, Option<i32>) {
        let response = sidecar
            .dispatch_blocking(request(
                request_id,
                OwnershipScope::vm(connection_id, session_id, vm_id),
                RequestPayload::Execute(ExecuteRequest {
                    process_id: String::from(process_id),
                    command: None,
                    runtime: Some(GuestRuntimeKind::JavaScript),
                    entrypoint: Some(String::from(entrypoint)),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    cwd: Some(String::from("/workspace")),
                    wasm_permission_tier: None,
                }),
            ))
            .expect("dispatch execute");

        match response.response.payload {
            ResponsePayload::ProcessStarted(started) => {
                assert_eq!(started.process_id, process_id);
            }
            other => panic!("unexpected execute response: {other:?}"),
        }

        let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
        drain_process_output(sidecar, &ownership, process_id)
    }

    fn drain_process_output(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        ownership: &OwnershipScope,
        process_id: &str,
    ) -> (String, String, Option<i32>) {
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = None;

        for _ in 0..64 {
            let Some(event) = sidecar
                .poll_event_blocking(ownership, Duration::from_secs(5))
                .expect("poll process event")
            else {
                if exit_code.is_some() {
                    break;
                }
                panic!("timed out waiting for process {process_id} to exit");
            };

            match event.payload {
                EventPayload::ProcessOutput(output) if output.process_id == process_id => {
                    match output.channel {
                        agent_os_sidecar::protocol::StreamChannel::Stdout => {
                            stdout.push_str(&output.chunk);
                        }
                        agent_os_sidecar::protocol::StreamChannel::Stderr => {
                            stderr.push_str(&output.chunk);
                        }
                    }
                }
                EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                    exit_code = Some(exited.exit_code);
                    break;
                }
                _ => {}
            }
        }

        (stdout, stderr, exit_code)
    }

    fn dispose_vm_and_close_session(
        sidecar: &mut NativeSidecar<RecordingBridge>,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
    ) {
        sidecar
            .dispatch_blocking(request(
                90,
                OwnershipScope::vm(connection_id, session_id, vm_id),
                RequestPayload::DisposeVm(DisposeVmRequest {
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
        let host_dir = temp_dir("agent-os-sidecar-cross-mount-rename-js");
        fs::write(host_dir.join("source.txt"), "mapped-source\n").expect("seed mapped file");

        let mut sidecar = create_test_sidecar();
        let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar);
        let vm_id = create_vm_with_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            vec![MountDescriptor {
                guest_path: String::from("/mapped"),
                read_only: false,
                plugin: MountPluginDescriptor {
                    id: String::from("host_dir"),
                    config: json!({
                        "hostPath": host_dir.to_string_lossy().into_owned(),
                        "readOnly": false,
                    }),
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
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
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
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
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
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
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
                ..GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::Mkdir,
                    path: String::new(),
                    destination_path: None,
                    target: None,
                    content: None,
                    encoding: None,
                    recursive: false,
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
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
                    mode: None,
                    uid: None,
                    gid: None,
                    atime_ms: None,
                    mtime_ms: None,
                    len: None,
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
