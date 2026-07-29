#[path = "support/mod.rs"]
mod support;

use agentos_native_sidecar::wire::{
    ConfigureVmRequest, ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestFilesystemResultResponse, GuestRuntimeKind, KillProcessRequest, MountDescriptor,
    MountPluginDescriptor, RequestPayload, ResponsePayload, RootFilesystemEntryEncoding,
};
use agentos_native_sidecar::{NativeSidecar, NativeSidecarConfig};
use filetime::{set_file_times, FileTime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use support::{
    authenticate_wire, collect_process_output_wire_with_timeout, dispose_vm_and_close_session_wire,
    execute_wire, open_session_wire, wire_request, wire_session, wire_vm, MockS3Server,
    ProcessOutputTimeout, RecordingBridge, TEST_AUTH_TOKEN,
};

struct TestRoot {
    path: PathBuf,
}

struct TempUsageBudget {
    max_bytes: u64,
    used_bytes: Mutex<u64>,
}

impl TempUsageBudget {
    fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            used_bytes: Mutex::new(0),
        }
    }

    fn observe(self: &Arc<Self>, path: &Path) -> Result<TempUsageReservation, String> {
        let bytes = directory_tree_bytes(path)
            .map_err(|error| format!("failed to measure temp backing usage: {error}"))?;
        let mut used = self.used_bytes.lock().expect("temp usage budget lock");
        let next = used.checked_add(bytes).ok_or_else(|| {
            String::from("xfstests temp backing usage overflowed the u64 accounting limit")
        })?;
        *used = next;
        drop(used);
        let reservation = TempUsageReservation {
            budget: Arc::clone(self),
            bytes,
        };
        reservation.enforce()?;
        Ok(reservation)
    }
}

struct TempUsageReservation {
    budget: Arc<TempUsageBudget>,
    bytes: u64,
}

impl TempUsageReservation {
    fn refresh(&mut self, path: &Path) -> Result<(), String> {
        let bytes = directory_tree_bytes(path)
            .map_err(|error| format!("failed to measure temp backing usage: {error}"))?;
        let mut used = self
            .budget
            .used_bytes
            .lock()
            .expect("temp usage budget lock");
        let without_reservation = used.saturating_sub(self.bytes);
        let next = without_reservation.checked_add(bytes).ok_or_else(|| {
            String::from("xfstests temp backing usage overflowed the u64 accounting limit")
        })?;
        *used = next;
        self.bytes = bytes;
        drop(used);
        self.enforce()
    }

    fn enforce(&self) -> Result<(), String> {
        let used = *self
            .budget
            .used_bytes
            .lock()
            .expect("temp usage budget lock");
        if used > self.budget.max_bytes {
            return Err(format!(
                "xfstests temp backing usage {used} bytes exceeds XFSTESTS_MAX_TEMP_BYTES={} bytes; raise XFSTESTS_MAX_TEMP_BYTES to permit a larger run",
                self.budget.max_bytes
            ));
        }
        Ok(())
    }
}

impl Drop for TempUsageReservation {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used_bytes
            .lock()
            .expect("temp usage budget lock");
        *used = used.saturating_sub(self.bytes);
    }
}

fn directory_tree_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    let mut bytes = 0u64;
    for entry in fs::read_dir(path)? {
        bytes = bytes
            .checked_add(directory_tree_bytes(&entry?.path())?)
            .ok_or_else(|| std::io::Error::other("temp backing usage exceeds u64"))?;
    }
    Ok(bytes)
}

fn refresh_xfstests_temp_roots(paths: &[&Path]) -> Result<(), String> {
    let now = FileTime::now();
    for path in paths {
        set_file_times(path, now, now).map_err(|error| {
            format!(
                "failed to refresh live xfstests temp root {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

impl TestRoot {
    fn new(name: &str) -> Self {
        let path = match env::var_os("XFSTESTS_TEMP_ROOT") {
            Some(run_root) => {
                let path = PathBuf::from(run_root).join(name);
                fs::create_dir(&path).unwrap_or_else(|error| {
                    panic!("create isolated xfstests root {}: {error}", path.display())
                });
                path
            }
            None => support::temp_dir(name),
        };
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn at(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "failed to clean xfstests verification root {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

fn test_sidecar(root: &Path) -> NativeSidecar<RecordingBridge> {
    NativeSidecar::with_config(
        RecordingBridge::default(),
        NativeSidecarConfig {
            sidecar_id: String::from("sidecar-xfstests-verify-first"),
            compile_cache_root: Some(root.join("compile-cache")),
            expected_auth_token: Some(TEST_AUTH_TOKEN.to_owned()),
            ..NativeSidecarConfig::default()
        },
    )
    .expect("create xfstests verification sidecar")
}

fn create_xfstests_vm_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    cwd: &Path,
) -> String {
    let mut payload = agentos_native_sidecar::wire::CreateVmRequest::legacy_test_config(
        GuestRuntimeKind::WebAssembly,
        HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
        agentos_native_sidecar::wire::RootFilesystemDescriptor {
            mode: agentos_native_sidecar::wire::RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: Vec::new(),
        },
        Some(support::wire_permissions_allow_all()),
    );
    let mut config: agentos_vm_config::CreateVmConfig =
        serde_json::from_str(&payload.config).expect("decode xfstests VM config");
    config.limits = Some(agentos_vm_config::VmLimitsConfig {
        resources: Some(agentos_vm_config::ResourceLimitsConfig {
            max_open_fds: Some(4096),
            max_filesystem_bytes: Some(16 * 1024 * 1024 * 1024),
            ..agentos_vm_config::ResourceLimitsConfig::default()
        }),
        wasm: Some(agentos_vm_config::WasmLimitsConfig {
            runner_cpu_time_limit_ms: Some(
                u64::try_from((xfstest_timeout() + Duration::from_secs(30)).as_millis())
                    .expect("bounded xfstests timeout fits u64 milliseconds"),
            ),
            ..agentos_vm_config::WasmLimitsConfig::default()
        }),
        js_runtime: Some(agentos_vm_config::JsRuntimeLimitsConfig {
            import_cache_materialize_timeout_ms: Some(120_000),
            ..agentos_vm_config::JsRuntimeLimitsConfig::default()
        }),
        ..agentos_vm_config::VmLimitsConfig::default()
    });
    config.user = Some(agentos_vm_config::VmUserConfig {
        uid: Some(0),
        gid: Some(0),
        username: Some(String::from("root")),
        homedir: Some(String::from("/root")),
        shell: Some(String::from("/bin/sh")),
        supplementary_gids: Some(vec![0]),
        accounts: Some(vec![
            agentos_vm_config::VmUserAccountConfig {
                uid: 1000,
                gid: 1000,
                username: String::from("fsgqa"),
                homedir: String::from("/home/fsgqa"),
                shell: String::from("/bin/bash"),
                gecos: None,
                supplementary_gids: vec![1000],
            },
            agentos_vm_config::VmUserAccountConfig {
                uid: 1001,
                gid: 1001,
                username: String::from("fsgqa2"),
                homedir: String::from("/home/fsgqa2"),
                shell: String::from("/bin/bash"),
                gecos: None,
                supplementary_gids: vec![1001],
            },
            agentos_vm_config::VmUserAccountConfig {
                uid: 123456,
                gid: 123456,
                username: String::from("123456-fsgqa"),
                homedir: String::from("/home/123456-fsgqa"),
                shell: String::from("/bin/bash"),
                gecos: None,
                supplementary_gids: vec![123456],
            },
            agentos_vm_config::VmUserAccountConfig {
                uid: 65534,
                gid: 65534,
                username: String::from("nobody"),
                homedir: String::from("/nonexistent"),
                shell: String::from("/bin/false"),
                gecos: None,
                supplementary_gids: vec![65534],
            },
            agentos_vm_config::VmUserAccountConfig {
                uid: 1,
                gid: 1,
                username: String::from("daemon"),
                homedir: String::from("/usr/sbin"),
                shell: String::from("/bin/false"),
                gecos: None,
                supplementary_gids: vec![1],
            },
        ]),
        groups: Some(vec![
            agentos_vm_config::VmGroupConfig {
                gid: 1000,
                name: String::from("fsgqa"),
                members: vec![String::from("fsgqa")],
            },
            agentos_vm_config::VmGroupConfig {
                gid: 1001,
                name: String::from("fsgqa2"),
                members: vec![String::from("fsgqa2")],
            },
            agentos_vm_config::VmGroupConfig {
                gid: 123456,
                name: String::from("123456-fsgqa"),
                members: vec![String::from("123456-fsgqa")],
            },
            agentos_vm_config::VmGroupConfig {
                gid: 65534,
                name: String::from("nobody"),
                members: vec![String::from("nobody")],
            },
            agentos_vm_config::VmGroupConfig {
                gid: 1,
                name: String::from("daemon"),
                members: vec![String::from("daemon")],
            },
        ]),
        ..agentos_vm_config::VmUserConfig::default()
    });
    payload.config = serde_json::to_string(&config).expect("encode xfstests VM config");

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_session(connection_id, session_id),
            RequestPayload::CreateVmRequest(payload),
        ))
        .expect("create root xfstests VM");
    match result.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected xfstests VM create response: {other:?}"),
    }
}

fn command_root(package: &str) -> PathBuf {
    PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"))
        .join("agentos-command-packages")
        .join(package)
        .canonicalize()
        .unwrap_or_else(|error| panic!("staged {package} WASM commands: {error}"))
}

fn c_probe_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../toolchain/c/build")
        .canonicalize()
        .expect("built xfstests C probes")
}

fn host_dir_mount(guest_path: &str, host_path: &Path, read_only: bool) -> MountDescriptor {
    MountDescriptor {
        guest_path: guest_path.to_owned(),
        guest_source: String::from("host_dir"),
        guest_fstype: String::from("host_dir"),
        read_only,
        plugin: MountPluginDescriptor {
            id: String::from("host_dir"),
            config: json!({
                "hostPath": host_path,
                "readOnly": read_only,
            })
            .to_string(),
        },
    }
}

fn chunked_local_mount(guest_path: &str, guest_source: &str, backing: &Path) -> MountDescriptor {
    MountDescriptor {
        guest_path: guest_path.to_owned(),
        guest_source: guest_source.to_owned(),
        guest_fstype: String::from("agentos"),
        read_only: false,
        plugin: MountPluginDescriptor {
            id: String::from("chunked_local"),
            config: json!({
                "metadataPath": backing.join("metadata.sqlite"),
                "blockRoot": backing.join("blocks"),
            })
            .to_string(),
        },
    }
}

fn memory_mount(guest_path: &str, guest_source: &str) -> MountDescriptor {
    MountDescriptor {
        guest_path: guest_path.to_owned(),
        guest_source: guest_source.to_owned(),
        guest_fstype: String::from("agentos"),
        read_only: false,
        plugin: MountPluginDescriptor {
            id: String::from("memory"),
            config: String::from("{}"),
        },
    }
}

fn s3_mount(
    backend: &str,
    guest_path: &str,
    guest_source: &str,
    backing: &Path,
    endpoint: &str,
) -> MountDescriptor {
    let prefix = guest_path.trim_matches('/').replace('/', "-");
    let mut config = json!({
        "bucket": "xfstests",
        "prefix": prefix,
        "region": "us-east-1",
        "endpoint": endpoint,
        "credentials": {
            "accessKeyId": "xfstests",
            "secretAccessKey": "xfstests-secret",
        },
    });
    if backend == "chunked_s3" {
        config["metadataBackend"] = json!("sqlite");
        config["metadataPath"] = json!(backing.join("metadata.sqlite"));
    }
    MountDescriptor {
        guest_path: guest_path.to_owned(),
        guest_source: guest_source.to_owned(),
        guest_fstype: String::from("agentos"),
        read_only: false,
        plugin: MountPluginDescriptor {
            id: backend.to_owned(),
            config: config.to_string(),
        },
    }
}

fn xfstests_backend_mount(
    backend: &str,
    guest_path: &str,
    guest_source: &str,
    backing: &Path,
    s3_endpoint: Option<&str>,
) -> MountDescriptor {
    match backend {
        "chunked_local" => chunked_local_mount(guest_path, guest_source, backing),
        "memory" => memory_mount(guest_path, guest_source),
        "chunked_s3" | "object_s3" => s3_mount(
            backend,
            guest_path,
            guest_source,
            backing,
            s3_endpoint.expect("S3 xfstests backend requires an isolated endpoint"),
        ),
        other => panic!("unsupported xfstests backend: {other}"),
    }
}

fn configure_verification_mounts(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    root: &Path,
    backend: &str,
    s3_endpoint: Option<&str>,
) {
    configure_mounts(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        vec![
            host_dir_mount("/__agentos/commands/0", &command_root("coreutils"), true),
            host_dir_mount("/__agentos/commands/1", &c_probe_root(), true),
            host_dir_mount("/__agentos/commands/2", &command_root("attr"), true),
            host_dir_mount("/__agentos/commands/3", &command_root("acl"), true),
            host_dir_mount("/__agentos/commands/4", &command_root("sed"), true),
            xfstests_backend_mount(
                backend,
                "/mnt/test",
                "/dev/agentos-test",
                &root.join("test"),
                s3_endpoint,
            ),
            xfstests_backend_mount(
                backend,
                "/mnt/scratch",
                "/dev/agentos-scratch",
                &root.join("scratch"),
                s3_endpoint,
            ),
        ],
    );
}

fn configure_mounts(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    mounts: Vec<MountDescriptor>,
) {
    let expected_mounts = mounts.len() as u32;
    let result = sidecar
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
        .expect("configure xfstests verification mounts");

    match result.response.payload {
        ResponsePayload::VmConfiguredResponse(response) => {
            assert_eq!(response.applied_mounts, expected_mounts);
        }
        other => panic!("unexpected configure response: {other:?}"),
    }
}

fn filesystem_request(
    operation: GuestFilesystemOperation,
    path: &str,
) -> GuestFilesystemCallRequest {
    GuestFilesystemCallRequest {
        operation,
        path: path.to_owned(),
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

fn guest_filesystem_call(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    request: GuestFilesystemCallRequest,
) -> GuestFilesystemResultResponse {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::GuestFilesystemCallRequest(request),
        ))
        .expect("dispatch verification filesystem request");

    match result.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => response,
        other => panic!("unexpected filesystem response: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_command(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    command: &str,
    args: &[&str],
) -> (String, String, i32) {
    execute_command_with_env(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        command,
        args,
        HashMap::new(),
        Duration::from_secs(20),
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_command_with_env(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    command: &str,
    args: &[&str],
    env: HashMap<String, String>,
    timeout: Duration,
) -> (String, String, i32) {
    try_execute_command_with_env(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        command,
        args,
        env,
        timeout,
    )
    .unwrap_or_else(|output| {
        panic!(
            "timed out waiting for {process_id}; stdout bytes: {}; stderr bytes: {}; stdout: {:?}; stderr: {:?}",
            output.stdout.len(),
            output.stderr.len(),
            output.stdout,
            output.stderr,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn try_execute_command_with_env(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    command: &str,
    args: &[&str],
    env: HashMap<String, String>,
    timeout: Duration,
) -> Result<(String, String, i32), ProcessOutputTimeout> {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: Some(command.to_owned()),
                runtime: None,
                entrypoint: None,
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                env,
                cwd: Some(String::from("/")),
                wasm_permission_tier: None,
            }),
        ))
        .expect("execute verification command");

    match result.response.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected execute response: {other:?}"),
    }

    support::try_collect_process_output_wire_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        timeout,
    )
}

fn assert_agentos_mount(mounts: &str, source: &str, target: &str) {
    let matching = mounts
        .lines()
        .filter(|line| line.split_whitespace().nth(1) == Some(target))
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1, "mounts for {target}:\n{mounts}");

    let fields = matching[0].split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.first(), Some(&source));
    assert_eq!(fields.get(2), Some(&"agentos"));
    assert!(
        fields
            .get(3)
            .is_some_and(|options| options.split(',').any(|option| option == "rw")),
        "mount is not writable: {}",
        matching[0]
    );
}

fn verify_first(backend: &str, include_fd_lifecycle_probe: bool) {
    support::assert_node_available();

    let root = TestRoot::new("xfstests-verify-first");
    let s3_server = matches!(backend, "chunked_s3" | "object_s3").then(MockS3Server::start);
    let s3_endpoint = s3_server.as_ref().map(MockS3Server::base_url);
    let cleanup_path = root.path().to_path_buf();
    let mut sidecar = test_sidecar(root.path());
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create verification directory");
    }

    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-verify-first");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        backend,
        s3_endpoint,
    );

    let mut write = filesystem_request(
        GuestFilesystemOperation::WriteFile,
        "/mnt/scratch/from-kernel.txt",
    );
    write.content = Some(String::from("kernel-to-guest"));
    write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
    guest_filesystem_call(&mut sidecar, 5, &connection_id, &session_id, &vm_id, write);

    let mut trace_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        trace_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    if env::var_os("XFSTESTS_TRACE_FD_FILESTAT").is_some() {
        trace_env.insert(String::from("AGENTOS_TRACE_FD_FILESTAT"), String::from("1"));
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-cat-kernel-write",
        "cat",
        &["/mnt/scratch/from-kernel.txt"],
        trace_env,
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "kernel-to-guest");

    let mut write_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        write_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-write-from-guest",
        "sh",
        &["-c", "printf guest-to-kernel > /mnt/scratch/from-guest.txt"],
        write_env,
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let read = guest_filesystem_call(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(
            GuestFilesystemOperation::ReadFile,
            "/mnt/scratch/from-guest.txt",
        ),
    );
    assert_eq!(read.content.as_deref(), Some("guest-to-kernel"));

    let (mounts, stderr, exit_code) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-read-proc-mounts",
        "cat",
        &["/proc/mounts"],
    );
    assert_eq!(exit_code, 0, "mounts:\n{mounts}\nstderr: {stderr}");
    assert_agentos_mount(&mounts, "/dev/agentos-test", "/mnt/test");
    assert_agentos_mount(&mounts, "/dev/agentos-scratch", "/mnt/scratch");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-findmnt-test-device",
        "findmnt",
        &[
            "-rncv",
            "-S",
            "/dev/agentos-test",
            "-o",
            "SOURCE,TARGET,FSTYPE,OPTIONS",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "/dev/agentos-test /mnt/test agentos rw,relatime\n");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        11,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-df-test-device",
        "df",
        &["-T", "-P", "/dev/agentos-test"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let df_row = stdout.lines().nth(1).expect("df data row");
    let df_fields = df_row.split_whitespace().collect::<Vec<_>>();
    assert_eq!(df_fields.len(), 7, "df row: {df_row}");
    assert_eq!(df_fields[0], "/dev/agentos-test");
    assert_eq!(df_fields[1], "agentos");
    let total = df_fields[2].parse::<u64>().expect("numeric df total");
    let used = df_fields[3].parse::<u64>().expect("numeric df used");
    let available = df_fields[4].parse::<u64>().expect("numeric df available");
    assert!(total > 0);
    assert!(used <= total);
    assert!(available <= total);
    assert!(df_fields[5].ends_with('%'), "df capacity: {}", df_fields[5]);
    assert_eq!(df_fields[6], "/mnt/test");

    let mut redirect_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        redirect_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        12,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-bash-legacy-redirect-both",
        "bash",
        &[
            "-c",
            "{ printf out; printf err >&2; } >& /mnt/test/redirect-both; cat /mnt/test/redirect-both",
        ],
        redirect_env,
        Duration::from_secs(20),
    );
    assert_eq!(
        exit_code, 0,
        "backend={backend}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(stdout, "outerr");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        132,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-pipeline-statuses",
        "sh",
        &[
            "-c",
            "false | true; printf '%s %s\\n' \"${PIPESTATUS[0]}\" \"${PIPESTATUS[1]}\"",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "1 0\n",
        "PIPESTATUS must preserve each child result; stderr: {stderr}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        133,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-getfattr-pipeline-status",
        "sh",
        &[
            "-c",
            "getfattr -d /mnt/test/no-such-xattr-file >/dev/null 2>&1; direct=$?; getfattr -d /mnt/test/no-such-xattr-file 2>/dev/null | sed 's/x/y/' >/dev/null; statuses=(\"${PIPESTATUS[@]}\"); getfattr_filtered() { getfattr \"$@\" | sed 's/x/y/'; return ${PIPESTATUS[0]}; }; getfattr_filtered -d /mnt/test/no-such-xattr-file >/dev/null 2>&1; wrapped=$?; printf '%s %s %s %s\\n' \"$direct\" \"${statuses[0]}\" \"${statuses[1]}\" \"$wrapped\"",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "1 1 0 1\n",
        "getfattr failures must survive a filter pipeline; stderr: {stderr}"
    );

    if include_fd_lifecycle_probe {
        let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        13,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-umask-enforcement",
        "sh",
        &[
            "-c",
            "umask; umask 077; umask; mkdir /mnt/test/umask-dir; touch /mnt/test/umask-file; stat -c '%a' /mnt/test/umask-dir /mnt/test/umask-file",
        ],
        HashMap::new(),
        Duration::from_secs(20),
    );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "0022\n0077\n700\n600\n", "umask stderr: {stderr}");

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            14,
            &connection_id,
            &session_id,
            &vm_id,
            "xfstests-nested-shell-cwd",
            "sh",
            &["-c", "cd /mnt/test && sh -c pwd"],
        );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "/mnt/test\n", "nested shell stderr: {stderr}");

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            141,
            &connection_id,
            &session_id,
            &vm_id,
            "xfstests-shell-exec",
            "sh",
            &["-c", "exec sh -c 'printf exec-ok; exit 37'; exit 0"],
        );
        assert_eq!(exit_code, 37, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "exec-ok", "exec builtin stderr: {stderr}");

        let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        143,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-inherited-pipe-reader",
        "sh",
        &[
            "-c",
            "printf 'one\\ntwo\\nthree\\n' | while read value; do dirname \"$value\" >/dev/null || exit 1; echo \"$value\"; done",
        ],
    );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            stdout, "one\ntwo\nthree\n",
            "pipe ownership stderr: {stderr}"
        );

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            144,
            &connection_id,
            &session_id,
            &vm_id,
            "xfstests-signal-traps",
            "sh",
            &["-c", "trap ':' 1 HUP TERM; trap - 1 HUP TERM; echo trap-ok"],
        );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "trap-ok\n", "signal trap stderr: {stderr}");

        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            145,
            &connection_id,
            &session_id,
            &vm_id,
            "xfstests-hostname",
            "hostname",
            &[],
        );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "agentos\n", "hostname stderr: {stderr}");

        let mut pipeline_env = HashMap::new();
        if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
            pipeline_env.insert(
                String::from("AGENTOS_TRACE_HOST_PROCESS"),
                String::from("1"),
            );
        }
        if let Ok(loops) = env::var("XFSTESTS_PIPELINE_LOOPS") {
            pipeline_env.insert(String::from("XFSTESTS_PIPELINE_LOOPS"), loops);
        }
        let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        142,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-child-pipeline-lifecycle",
        "sh",
        &[
            "-c",
            "for ((i=0; i<${XFSTESTS_PIPELINE_LOOPS:-100}; i++)); do printf x | cat >/dev/null || exit 1; done; printf stale-suffix > /mnt/test/after-pipelines; printf redirected-ok > /mnt/test/after-pipelines; cat /mnt/test/after-pipelines; echo; echo pipelines-ok",
        ],
        pipeline_env,
        Duration::from_secs(60),
    );
        assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            stdout, "redirected-ok\npipelines-ok\n",
            "repeated child pipelines leaked or retained descriptors; stderr: {stderr}"
        );
    }

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        15,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-virtual-id",
        "sh",
        &[
            "-c",
            "id -u; id -g; id -un; getent passwd fsgqa; getent group fsgqa",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "0\n0\nroot\nfsgqa:x:1000:1000::/home/fsgqa:/bin/bash\nfsgqa:x:1000:fsgqa\n",
        "identity database stderr: {stderr}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        16,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-su-dac",
        "sh",
        &[
            "-c",
            "printf secret > /mnt/test/root-only; chmod 600 /mnt/test/root-only",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let root_only = guest_filesystem_call(
        &mut sidecar,
        160,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/root-only"),
    );
    assert_eq!(root_only.content.as_deref(), Some("secret"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        161,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-runas-dac-denied",
        "sh",
        &["-c", "runas -u 1000 -g 1000 -- id -u; if runas -u 1000 -g 1000 -- cat /mnt/test/root-only; then exit 9; else echo denied; fi"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "1000\ndenied\n", "su/DAC stderr: {stderr}");

    let root_only = guest_filesystem_call(
        &mut sidecar,
        163,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/root-only"),
    );
    assert_eq!(root_only.content.as_deref(), Some("secret"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        164,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-su-dac-make-readable",
        "chmod",
        &["644", "/mnt/test/root-only"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let root_only = guest_filesystem_call(
        &mut sidecar,
        165,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/root-only"),
    );
    assert_eq!(root_only.content.as_deref(), Some("secret"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        162,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-runas-dac-readable",
        "runas",
        &[
            "-u",
            "1000",
            "-g",
            "1000",
            "--",
            "cat",
            "/mnt/test/root-only",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "secret", "su/DAC stderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        17,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chown-dac",
        "sh",
        &[
            "-c",
            "touch /mnt/test/owned-by-fsgqa /mnt/test/root-owned; chown fsgqa:fsgqa /mnt/test/owned-by-fsgqa; chmod 600 /mnt/test/owned-by-fsgqa; stat -c '%u:%g %a' /mnt/test/owned-by-fsgqa",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let owned_stat = guest_filesystem_call(
        &mut sidecar,
        170,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Stat, "/mnt/test/owned-by-fsgqa"),
    )
    .stat
    .expect("owned file stat after chown");
    assert_eq!(
        (owned_stat.uid, owned_stat.gid, owned_stat.mode & 0o7777),
        (1000, 1000, 0o600),
        "kernel ownership after root chown; guest stat stdout: {stdout}; stderr: {stderr}; s3 metadata: {:?}",
        s3_server.as_ref().map(MockS3Server::object_metadata)
    );
    assert_eq!(
        stdout, "1000:1000 600\n",
        "root chown state stderr: {stderr}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        171,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chown-dac-unprivileged",
        "runas",
        &[
            "-u",
            "1000",
            "-g",
            "1000",
            "--",
            "sh",
            "-c",
            "chmod 640 /mnt/test/owned-by-fsgqa; printf owned > /mnt/test/owned-by-fsgqa; chgrp fsgqa /mnt/test/owned-by-fsgqa; if chown fsgqa2:fsgqa2 /mnt/test/owned-by-fsgqa; then exit 8; else echo denied-owner; fi; if chown root:root /mnt/test/root-owned; then exit 9; else echo denied-root; fi; cat /mnt/test/owned-by-fsgqa",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "denied-owner\ndenied-root\nowned",
        "chown/DAC stderr: {stderr}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        18,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-c-credentials",
        "credentials_test",
        &[],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.lines().all(|line| line.ends_with(": ok")),
        "C credential probe stdout: {stdout}\nstderr: {stderr}"
    );

    let mut xattr_probe_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        xattr_probe_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        20,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-c-xattrs",
        "xattr_test",
        &[],
        xattr_probe_env,
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "xattr: ok\n", "C xattr probe stderr: {stderr}");

    let mut hardlink_probe_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        hardlink_probe_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        201,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-hardlink-tool",
        "sh",
        &[
            "-c",
            "rm -f /mnt/test/link-source /mnt/test/link-destination; touch /mnt/test/link-source; ln /mnt/test/link-source /mnt/test/link-destination; stat -c %h /mnt/test/link-source; rm /mnt/test/link-source /mnt/test/link-destination",
        ],
        hardlink_probe_env,
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "2\n", "hardlink tool stderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        203,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-sparse-accounting",
        "sh",
        &[
            "-c",
            "rm -f /mnt/test/sparse-probe; xfs_io -f -c 'pwrite -q -b 51200 -S 0x61 1638400 51200' /mnt/test/sparse-probe; stat -c 'size=%s blocks=%b' /mnt/test/sparse-probe; du -sk /mnt/test/sparse-probe; rm -f /mnt/test/sparse-probe",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let du_kib = stdout
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok());
    assert!(
        stdout
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("size=1689600 blocks="))
            && du_kib.is_some_and(|value| value <= 1024),
        "{backend} sparse file must retain bounded allocation: stdout={stdout:?} stderr={stderr:?}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        202,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-symlink-loop-errno",
        "sh",
        &[
            "-c",
            "rm -f /mnt/test/symlink-self; ln -s symlink-self /mnt/test/symlink-self; touch /mnt/test/symlink-self 2>&1; status=$?; rm -f /mnt/test/symlink-self; printf 'status=%s\n' \"$status\"",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("Too many levels of symbolic links") && stdout == "status=1\n",
        "symlink loop must surface ELOOP: stdout={stdout:?} stderr={stderr:?}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        21,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xattr-tools",
        "sh",
        &[
            "-c",
            "touch /mnt/test/xattr-tool; setfattr -n user.tool -v works /mnt/test/xattr-tool; getfattr --only-values -n user.tool /mnt/test/xattr-tool; setfattr -x user.tool /mnt/test/xattr-tool; if getfattr --only-values -n user.tool /mnt/test/xattr-tool 2>/dev/null; then exit 9; fi",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "works", "xattr tool probe stderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        22,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-acl-tools",
        "sh",
        &[
            "-c",
            "printf acl > /mnt/test/acl-tool; chmod 600 /mnt/test/acl-tool; setfacl -m u:1001:r-- /mnt/test/acl-tool",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "ACL setup stderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        221,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-getfacl-readback",
        "getfacl",
        &["-n", "--absolute-names", "/mnt/test/acl-tool"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.lines().any(|line| line == "user:1001:r--"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        222,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chacl-readback",
        "chacl",
        &["-l", "/mnt/test/acl-tool"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("u:1001:r--"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        223,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-acl-enforcement",
        "sh",
        &[
            "-c",
            "runas -u 1001 -g 1001 -- cat /mnt/test/acl-tool; setfacl -m m::--- /mnt/test/acl-tool; if runas -u 1001 -g 1001 -- cat /mnt/test/acl-tool 2>/dev/null; then exit 10; fi",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "acl", "ACL enforcement probe stderr: {stderr}");

    if include_fd_lifecycle_probe {
        let (stdout, stderr, exit_code) = execute_command_with_env(
            &mut sidecar,
            19,
            &connection_id,
            &session_id,
            &vm_id,
            "xfstests-fd-lifecycle",
            "sh",
            &[
                "-c",
                "for ((i=0; i<80; i++)); do read line < /proc/mounts || exit 1; done; cat /proc/mounts >/dev/null",
            ],
            HashMap::new(),
            Duration::from_secs(60),
        );
        assert_eq!(
            exit_code, 0,
            "repeated open/close leaked guest descriptors\nstdout: {stdout}\nstderr: {stderr}"
        );
    }

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "test root was not removed");
}

#[test]
fn xfstests_verify_first_routes_guest_io_and_reports_agentos_mounts() {
    verify_first(XFSTESTS_BACKEND, true);
}

#[test]
#[ignore = "focused xfstests packaged-df space-accounting regression"]
fn xfstests_wasi_df_reports_authoritative_space() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-df-space");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create df-space test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-df-space");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-df-space-accounting",
        "sh",
        &[
            "-c",
            "mount -o remount,size=8388608 /mnt/scratch || exit; snapshot() { df /mnt/scratch | awk 'NR == 2 { print $2, $3, $4, $5 }'; }; snapshot; dd if=/dev/zero of=/mnt/scratch/df-space bs=1024 count=1024 2>/dev/null; snapshot; rm /mnt/scratch/df-space; snapshot",
        ],
        HashMap::new(),
        Duration::from_secs(120),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(!cleanup_path.exists(), "df-space test root was not removed");
    let snapshots = stdout
        .lines()
        .map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                4,
                "df data row must have total, used, available, and capacity: {line:?}"
            );
            let parse = |field: &str| {
                field
                    .parse::<u64>()
                    .unwrap_or_else(|_| panic!("df field must be numeric: {field:?}"))
            };
            let capacity = fields[3]
                .strip_suffix('%')
                .unwrap_or_else(|| panic!("df capacity must end in %: {:?}", fields[3]))
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("df capacity must be numeric: {:?}", fields[3]));
            (
                parse(fields[0]),
                parse(fields[1]),
                parse(fields[2]),
                capacity,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 3, "stdout: {stdout}\nstderr: {stderr}");
    let (before, written, reclaimed) = (snapshots[0], snapshots[1], snapshots[2]);
    assert_eq!(written.0, before.0, "total blocks changed after write");
    assert_eq!(reclaimed.0, before.0, "total blocks changed after delete");
    assert!(
        written.1 > before.1,
        "used blocks did not increase: {snapshots:?}"
    );
    assert!(
        written.2 < before.2,
        "available blocks did not decrease: {snapshots:?}"
    );
    assert_eq!(reclaimed.1, before.1, "used blocks were not reclaimed");
    assert_eq!(reclaimed.2, before.2, "available blocks were not reclaimed");
}

#[test]
fn xfstests_wasi_mount_applies_existing_mount_policy() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-mount-policy");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create mount-policy test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-mount-policy");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-mount-policy-apply",
        "sh",
        &[
            "-c",
            "mount -t agentos -o remount,strictatime /dev/agentos-test /mnt/test && mount -t agentos -o remount,relatime /dev/agentos-scratch /mnt/scratch && cat /proc/mounts",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.contains(" /mnt/test agentos rw,strictatime ")),
        "missing strictatime mount entry; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.contains(" /mnt/scratch agentos rw,relatime ")),
        "missing relatime scratch entry; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !cleanup_path.exists(),
        "mount-policy test root was not removed"
    );
}

#[test]
fn xfstests_wasi_xfs_io_links_open_and_unnamed_files() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-unnamed-file");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create unnamed-file test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-unnamed-file");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-flink-named",
        "xfs_io",
        &[
            "-F",
            "-f",
            "-c",
            "flink /mnt/test/named-link",
            "/mnt/test/named-source",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-flink-unnamed",
        "xfs_io",
        &[
            "-T",
            "-c",
            "pwrite 0 4096",
            "-c",
            "pread 0 4096",
            "-c",
            "flink /mnt/test/unnamed-link",
            "/mnt/test",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("wrote 4096/4096 bytes at offset 0"));
    assert!(stdout.contains("read 4096/4096 bytes at offset 0"));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-flink-readback",
        "sh",
        &[
            "-c",
            "test -f /mnt/test/named-link && test -f /mnt/test/unnamed-link && test $(wc -c < /mnt/test/unnamed-link) -eq 4096 && ! ls -A /mnt/test | grep '^.agentos-tmpfile-'",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-tmpfile-read-only",
        "xfs_io",
        &["-Tr", "/mnt/test", "-c", "close"],
    );
    assert_ne!(exit_code, 0, "read-only O_TMPFILE unexpectedly succeeded");
    assert!(
        stderr.contains("Invalid argument"),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "unnamed-file test root was not removed"
    );
}

#[test]
fn xfstests_wasi_rm_unlinks_self_referential_symlink() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-unlink-self-symlink");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create unlink-symlink test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-unlink-self-symlink");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-unlink-self-symlink",
        "sh",
        &[
            "-c",
            "cd /mnt/test && ln -s symlink_self symlink_self && rm -f symlink_self && test ! -L symlink_self",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "unlink-symlink test root was not removed"
    );
}

#[test]
fn xfstests_wasi_mv_uses_kernel_rename_without_copying() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-kernel-rename");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create kernel-rename test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-kernel-rename");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let (before, setup_stderr, setup_exit) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-kernel-rename-setup",
        "sh",
        &[
            "-c",
            "printf data > /mnt/test/rename-source; stat -c '%i %X %Y %Z' /mnt/test/rename-source; sleep 1",
        ],
    );
    assert_eq!(setup_exit, 0, "stdout: {before}\nstderr: {setup_stderr}");

    let (mv_stdout, mv_stderr, mv_exit) = execute_command_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-kernel-rename-mv",
        "mv",
        &["/mnt/test/rename-source", "/mnt/test/rename-destination"],
        HashMap::new(),
        Duration::from_secs(20),
    );
    assert_eq!(mv_exit, 0, "stdout: {mv_stdout}\nstderr: {mv_stderr}");

    let (after, stat_stderr, stat_exit) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-kernel-rename-stat",
        "stat",
        &["-c", "%i %X %Y %Z", "/mnt/test/rename-destination"],
    );
    let (cross_setup_stdout, cross_setup_stderr, cross_setup_exit) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-cross-device-move-setup",
        "sh",
        &["-c", "printf cross-device > /tmp/cross-device-source"],
    );
    assert_eq!(
        cross_setup_exit, 0,
        "stdout: {cross_setup_stdout}\nstderr: {cross_setup_stderr}"
    );
    let (cross_mv_stdout, cross_mv_stderr, cross_mv_exit) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-cross-device-move",
        "mv",
        &[
            "/tmp/cross-device-source",
            "/mnt/test/cross-device-destination",
        ],
    );
    assert_eq!(
        cross_mv_exit, 0,
        "stdout: {cross_mv_stdout}\nstderr: {cross_mv_stderr}"
    );
    let (cross_stat_stdout, cross_stat_stderr, cross_stat_exit) = execute_command(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-cross-device-move-stat",
        "sh",
        &[
            "-c",
            "test ! -e /tmp/cross-device-source; cat /mnt/test/cross-device-destination",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(stat_exit, 0, "stdout: {after}\nstderr: {stat_stderr}");
    let parse = |value: &str| {
        value
            .split_whitespace()
            .map(|field| field.parse::<u64>().expect("numeric stat field"))
            .collect::<Vec<_>>()
    };
    let before = parse(&before);
    let after = parse(&after);
    assert_eq!(before.len(), 4, "unexpected before stat fields: {before:?}");
    assert_eq!(after.len(), 4, "unexpected after stat fields: {after:?}");
    assert_eq!(
        after[0], before[0],
        "mv copied the inode; stderr: {mv_stderr}"
    );
    assert_eq!(
        after[1], before[1],
        "rename changed atime; stderr: {mv_stderr}"
    );
    assert_eq!(
        after[2], before[2],
        "rename changed mtime; stderr: {mv_stderr}"
    );
    assert!(
        after[3] > before[3],
        "rename did not advance ctime; stderr: {mv_stderr}"
    );
    assert_eq!(
        cross_stat_exit, 0,
        "stdout: {cross_stat_stdout}\nstderr: {cross_stat_stderr}"
    );
    assert_eq!(cross_stat_stdout, "cross-device");
    assert!(
        !cleanup_path.exists(),
        "kernel-rename test root was not removed"
    );
}

#[test]
fn xfstests_repeated_child_pipelines_finish_with_bash_builtins() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-repeated-pipelines");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create repeated-pipeline test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-repeated-pipelines");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-repeated-pipelines",
        "sh",
        &[
            "-c",
            "type -t printf; for ((i=0; i<100; i++)); do printf x | cat >/dev/null || exit 1; done; echo pipelines-ok; printf '/bin/true\\n' | su fsgqa; echo su-stdin-ok",
        ],
        HashMap::new(),
        Duration::from_secs(60),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "builtin\npipelines-ok\nsu-stdin-ok\n",
        "stderr: {stderr}"
    );
    assert!(!cleanup_path.exists(), "pipeline test root was not removed");
}

#[test]
fn xfstests_nested_script_absolute_write_stays_in_mounted_vfs() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-mounted-cwd-absolute-write");
    let cleanup_path = root.path().to_path_buf();
    let case_root = root.path().join("case");
    let source = case_root.join("source");
    let cwd = case_root.join("cwd");
    for path in [
        cwd.as_path(),
        source.as_path(),
        &case_root.join("test"),
        &case_root.join("scratch"),
        &case_root.join("results"),
    ] {
        fs::create_dir_all(path).expect("create mounted-cwd regression directory");
    }
    let writer = source.join("write-absolute");
    fs::write(&writer, "#!/bin/bash\nprintf 'kernel-routed\\n'\n")
        .expect("write mounted-cwd regression script");
    fs::set_permissions(&writer, fs::Permissions::from_mode(0o755))
        .expect("make mounted-cwd regression script executable");
    let escaped = root.path().join("mnt/results/.tmp/absolute.out");

    let mut sidecar = test_sidecar(&case_root);
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-mounted-cwd-write");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, &case_root, "memory", None),
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-mounted-cwd-absolute-write",
        "bash",
        &[
            "-c",
            "mkdir -p /mnt/results/.tmp && cd /opt/xfstests && bash -c 'exec ./write-absolute' > /mnt/results/.tmp/absolute.out 2>&1",
        ],
        HashMap::new(),
        Duration::from_secs(30),
    );
    let routed = read_optional_text(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "/mnt/results/.tmp/absolute.out",
    );
    let escaped_content = fs::read_to_string(&escaped).ok();
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(routed.as_deref(), Some("kernel-routed\n"));
    assert_eq!(
        escaped_content, None,
        "absolute mounted path escaped relative to host cwd"
    );
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "mounted-cwd test root was not removed"
    );
}

#[test]
fn xfstests_blocking_pipeline_applies_backpressure_past_pipe_capacity() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-blocking-pipeline-backpressure");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create blocking-pipeline test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-blocking-pipeline");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let mut guest_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-blocking-pipeline",
        "bash",
        &[
            "-c",
            "set -o pipefail; awk 'BEGIN { for (i = 1; i <= 20000; i++) print \"user:\" i \":rwx\"; exit }' | sed -n '/:rwx$/p' | awk 'END { print NR }'",
        ],
        guest_env,
        Duration::from_secs(60),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "20000\n", "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected pipeline stderr: {stderr}");
    assert!(!cleanup_path.exists(), "pipeline test root was not removed");
}

#[test]
fn xfstests_acl_limit_filter_pipelines_do_not_leak_eagain() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-acl-limit-pipelines");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create ACL pipeline test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-acl-pipelines");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let script = r#"
set -o pipefail
[ -z "$XFSTESTS_TRACE_SHELL" ] || set -x
cd /mnt/test
touch largeaclfile
create_aces() {
    local n=$(( $1 - 4 ))
    local acl='u::rwx,g::rwx,o::rwx,m::rwx'
    while [ "$n" -ne 0 ]; do acl="$acl,u:$n:rwx"; n=$((n - 1)); done
    printf '%s\n' "$acl"
}
filter_aces() {
    local tmp_file
    tmp_file=$(mktemp /tmp/ace.XXXXXX) || return
    printf 'root:x:0:0:root:/root:/bin/bash\nfsgqa:x:1000:1000::/home/fsgqa:/bin/bash\nroot:x:0:\nfsgqa:x:1000:\n' > "$tmp_file"
    awk -v tmpfile="$tmp_file" 'BEGIN { FS=":"; while (getline <tmpfile > 0) idlist[$1]=$3 } /^user/ { if ($2 in idlist) sub($2,idlist[$2]); print; next } { print }'
    local status=$?
    rm -f "$tmp_file"
    return "$status"
}
check_acl() {
    local count=$1 acl actual
    acl=$(create_aces "$count") || return
    if [ "$count" -eq 26 ]; then
        chacl "$acl" largeaclfile >/dev/null 2>&1 && return 90
        count=25
    else
        chacl "$acl" largeaclfile || return
    fi
    getfacl --numeric largeaclfile | filter_aces >> /mnt/results/acl.full || return
    actual=$(getfacl --numeric largeaclfile | filter_aces | grep ':rwx' | wc -l) || return
    [ "$actual" -eq "$count" ] || { printf 'count=%s expected=%s\n' "$actual" "$count" >&2; return 91; }
}
for count in 24 25 26 16 17; do check_acl "$count" || exit; done

too_many=$(create_aces 26) || exit
if chacl "$too_many" largeaclfile > /tmp/chacl.stdout 2> /tmp/chacl.stderr; then
    exit 92
fi
printf 'chacl: cannot set access acl on "largeaclfile": Argument list too long\n' > /tmp/chacl.expected
cmp /tmp/chacl.expected /tmp/chacl.stderr || exit 93

exec 3> /tmp/inherited-offset.actual
printf 'use 16 aces\n' >&3
chacl "$(create_aces 16)" largeaclfile || exit
printf 'use 17 aces\n' >&3
exec 3>&-
printf 'use 16 aces\nuse 17 aces\n' > /tmp/inherited-offset.expected
cmp /tmp/inherited-offset.expected /tmp/inherited-offset.actual || exit 94

redirected_acl_24=$(create_aces 24) || exit
redirected_acl_25=$(create_aces 25) || exit
redirected_acl_16=$(create_aces 16) || exit
redirected_acl_17=$(create_aces 17) || exit
bash -c '
    check_redirected_acl() {
        chacl "$1" largeaclfile 2>&1 | sed -e "s/Invalid argument/Argument list too long/"
        getfacl --numeric largeaclfile | awk "/:rwx$/ { print }" >> /tmp/inherited-stdout.full
        getfacl --numeric largeaclfile | awk "/:rwx$/ { print }" | wc -l >/dev/null
    }
    check_redirected_acl "$1"
    check_redirected_acl "$2"
    check_redirected_acl "$3"
    echo "use 16 aces"
    check_redirected_acl "$4"
    echo "use 17 aces"
    check_redirected_acl "$5"
' _ "$redirected_acl_24" "$redirected_acl_25" "$too_many" \
    "$redirected_acl_16" "$redirected_acl_17" > /tmp/inherited-stdout.actual
printf 'chacl: cannot set access acl on "largeaclfile": Argument list too long\nuse 16 aces\nuse 17 aces\n' > /tmp/inherited-stdout.expected
cmp /tmp/inherited-stdout.expected /tmp/inherited-stdout.actual || exit 95
rm -f /tmp/chacl.stdout /tmp/chacl.stderr /tmp/chacl.expected \
    /tmp/inherited-offset.actual /tmp/inherited-offset.expected \
    /tmp/inherited-stdout.actual /tmp/inherited-stdout.expected \
    /tmp/inherited-stdout.full
printf 'acl-pipelines-ok\n'
"#;
    let mut guest_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_SHELL").is_some() {
        guest_env.insert(String::from("XFSTESTS_TRACE_SHELL"), String::from("1"));
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-acl-limit-pipelines",
        "bash",
        &["-c", script],
        guest_env,
        Duration::from_secs(180),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "acl-pipelines-ok\n", "stderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected ACL pipeline stderr: {stderr}"
    );
    assert!(
        !cleanup_path.exists(),
        "ACL pipeline test root was not removed"
    );
}

#[test]
fn xfstests_su_consumes_inherited_stdin_and_exits() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-su-stdin");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create su stdin test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-su-stdin");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let mut guest_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-su-stdin",
        "sh",
        &[
            "-c",
            "cd /opt/xfstests; _su() { su \"$@\"; }; echo /bin/true | _su fsgqa; su -s/bin/bash - fsgqa -c 'id -u'; echo su-stdin-ok",
        ],
        guest_env,
        Duration::from_secs(30),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "1000\nsu-stdin-ok\n", "stderr: {stderr}");
    assert!(!cleanup_path.exists(), "su stdin test root was not removed");
}

#[test]
fn xfstests_generic_128_body_enforces_nosuid_permissions() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-generic-128-body");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create generic/128 body test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-generic-128-body");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let mut guest_env = HashMap::from([
        (String::from("FSTYP"), String::from("agentos")),
        (
            String::from("HOST_OPTIONS"),
            String::from("/opt/xfstests/local.config"),
        ),
    ]);
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (body_command, body_timeout) = if env::var_os("XFSTESTS_TRACE_SHELL").is_some() {
        (
            "cd /opt/xfstests && bash -x ./tests/generic/128",
            Duration::from_secs(60),
        )
    } else {
        (
            "cd /opt/xfstests && ./tests/generic/128",
            Duration::from_secs(120),
        )
    };
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-generic-128-body",
        "bash",
        &["-c", body_command],
        guest_env,
        body_timeout,
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "QA output created by 128\n", "stderr: {stderr}");
    assert!(
        !cleanup_path.exists(),
        "generic/128 body root was not removed"
    );
}

#[test]
#[ignore = "diagnostic looptest throughput probe; run explicitly with staged xfstests"]
fn xfstests_looptest_scaled_throughput_probe() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let backend =
        env::var("XFSTESTS_LOOPTEST_BACKEND").unwrap_or_else(|_| String::from(XFSTESTS_BACKEND));
    let iterations = env::var("XFSTESTS_LOOPTEST_ITERATIONS")
        .unwrap_or_else(|_| String::from("10000"))
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=100_000).contains(value))
        .expect("XFSTESTS_LOOPTEST_ITERATIONS must be in 1..=100000");
    let buffer_bytes = env::var("XFSTESTS_LOOPTEST_BUFFER_BYTES")
        .unwrap_or_else(|_| String::from("8192"))
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=16 * 1024 * 1024).contains(value))
        .expect("XFSTESTS_LOOPTEST_BUFFER_BYTES must be in 1..=16777216");
    let truncate = env::var_os("XFSTESTS_LOOPTEST_TRUNCATE").is_some();
    let root = TestRoot::new("xfstests-looptest-throughput");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create looptest throughput directory");
    }

    let s3_server =
        matches!(backend.as_str(), "chunked_s3" | "object_s3").then(MockS3Server::start);
    let s3_endpoint = s3_server.as_ref().map(MockS3Server::base_url);
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-looptest-throughput");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), &backend, s3_endpoint),
    );

    let truncate_arg = if truncate { "-t" } else { "" };
    let command = format!(
        "mkdir -p /mnt/scratch/looptest-probe && /opt/xfstests/src/looptest -i {iterations} {truncate_arg} -r -w -b {buffer_bytes} -s /mnt/scratch/looptest-probe/file && stat -c %s /mnt/scratch/looptest-probe/file"
    );
    let started = Instant::now();
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-looptest-throughput",
        "sh",
        &["-c", &command],
        HashMap::new(),
        Duration::from_secs(300),
    );
    let elapsed = started.elapsed();
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    eprintln!(
        "xfstests looptest throughput: backend={backend} iterations={iterations} buffer_bytes={buffer_bytes} truncate={truncate} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout.trim(),
        if truncate {
            String::from("0")
        } else {
            (u64::from(iterations) * u64::from(buffer_bytes)).to_string()
        },
        "stderr: {stderr}"
    );
    assert!(!cleanup_path.exists(), "looptest root was not removed");
}

#[test]
#[ignore = "full writable-backend verify-first matrix; the xfstests Makefile runs it"]
fn xfstests_verify_first_all_backends() {
    for backend in xfstests_backends() {
        verify_first(&backend, false);
    }
}

#[test]
fn xfstests_cp_copies_large_files_through_bounded_wasi_buffers() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-cp-large");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create cp test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-cp-large");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-cp-large",
        "sh",
        &[
            "-c",
            "dd if=/dev/zero of=/mnt/test/source bs=1048576 count=4 status=none; cp /mnt/test/source /mnt/test/copy; cmp /mnt/test/source /mnt/test/copy; stat -c %s /mnt/test/copy",
        ],
        HashMap::new(),
        Duration::from_secs(60),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "4194304\n", "cp stderr: {stderr}");
    assert!(!cleanup_path.exists(), "cp test root was not removed");
}

#[test]
fn xfstests_openat_directory_fd_after_readdir() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-openat-directory-fd");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create openat test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-openat");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-openat-directory-fd",
        "sh",
        &[
            "-c",
            "mkdir /mnt/test/openat-probe; printf 'openat\\n' > /mnt/test/openat-probe/payload; openat_test /mnt/test/openat-probe",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "openat: ok\n", "openat stderr: {stderr}");
    assert!(!cleanup_path.exists(), "openat test root was not removed");
}

#[test]
fn xfstests_pwritev_preserves_offset_and_vector_order() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-pwritev");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create pwritev test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-pwritev");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-pwritev",
        "pwritev_test",
        &["/mnt/test/pwritev"],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "pwritev: ok\n", "pwritev stderr: {stderr}");
    assert!(!cleanup_path.exists(), "pwritev test root was not removed");
}

#[test]
fn xfstests_sync_observes_synchronous_vfs_commit() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-sync");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        &cwd,
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create sync test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-sync");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-sync",
        "sync_test",
        &["/mnt/test/sync"],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "sync: ok\n", "sync stderr: {stderr}");
    assert!(!cleanup_path.exists(), "sync test root was not removed");
}

#[test]
fn xfstests_odirect_aligned_runtime_probe() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-odirect");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        &cwd,
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create O_DIRECT test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-odirect");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-odirect",
        "xfs_io",
        &[
            "-F",
            "-f",
            "-d",
            "-c",
            "pwrite 0 20k",
            "-c",
            "pread -v 0 512",
            "/mnt/test/direct",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("wrote 20480/20480 bytes at offset 0\n"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(
            "00000000:  cd cd cd cd cd cd cd cd cd cd cd cd cd cd cd cd  ................\n"
        ),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("\n*\nread 512/512 bytes at offset 0\n"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("read 512/512 bytes at offset 0\n"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!cleanup_path.exists(), "O_DIRECT test root was not removed");
}

#[test]
fn xfstests_flock_enforces_cross_process_exclusion() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-flock");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        &cwd,
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create flock test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-flock");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-flock-direct",
        "flock_test",
        &["hold", "/mnt/test/direct.lock", "/mnt/scratch/direct.ready"],
        HashMap::new(),
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let mut guest_env = HashMap::new();
    guest_env.insert(
        String::from("AGENTOS_TRACE_HOST_PROCESS"),
        String::from("1"),
    );
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-flock-contention",
        "flock_test",
        &["selftest", "/mnt/test/contention.lock"],
        guest_env,
        Duration::from_secs(30),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "flock=ok\n", "stderr: {stderr}");
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "flock test root was not removed");
}

#[test]
fn xfstests_t_getcwd_sigterm_does_not_write_runtime_error() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-t-getcwd-sigterm");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create t_getcwd test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-t-getcwd-sigterm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-t-getcwd-sigterm",
        "sh",
        &[
            "-c",
            "mkdir /mnt/test/getcwd-dir; /opt/xfstests/src/t_getcwd /mnt/test/getcwd-dir",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.is_empty(), "unexpected stdout: {stdout}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(!cleanup_path.exists(), "t_getcwd test root was not removed");
}

#[test]
fn xfstests_locktest_server_starts_as_background_process() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-locktest-server");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create locktest probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-locktest-server");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-locktest-server",
        "bash",
        &[
            "-c",
            "client_pid=; server_pid=; cleanup() { kill $client_pid >/dev/null 2>&1; kill $server_pid >/dev/null 2>&1; rm -rf /mnt/test/lock_file /mnt/test/server.out /mnt/test/server.port /mnt/test/client.out; }; trap cleanup EXIT; touch /mnt/test/lock_file /mnt/test/server.out /mnt/test/server.port /mnt/test/client.out; /opt/xfstests/src/locktest -n 28 /mnt/test/lock_file 2>/mnt/test/server.out 1>/mnt/test/server.port & server_pid=$!; sleep 2; port=$(cat /mnt/test/server.port | grep '^server port: ' | awk '{print $3}'); /opt/xfstests/src/locktest -n 28 -p $port -h localhost /mnt/test/lock_file 2>/mnt/test/client.out; client_status=$?; client_pid=$!; wait $server_pid; server_status=$?; printf 'port=%s\\nclient=%s\\nserver=%s\\n' \"$port\" \"$client_status\" \"$server_status\"",
        ],
        HashMap::new(),
        Duration::from_secs(30),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(
        stdout.starts_with("port=") && stdout.ends_with("client=0\nserver=0\n"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!cleanup_path.exists(), "locktest test root was not removed");
}

#[test]
fn xfstests_mmap_shared_writeback_and_private_isolation() {
    support::assert_node_available();
    let root = TestRoot::new("xfstests-mmap");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create mmap test directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-mmap");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-mmap",
        "mmap_test",
        &["/mnt/test/mmap"],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "mmap: ok\n", "mmap stderr: {stderr}");
    assert!(!cleanup_path.exists(), "mmap test root was not removed");
}

#[test]
#[ignore = "focused generic/037 concurrent xattr regression; run explicitly with staged xfstests"]
fn xfstests_generic_037_concurrent_xattr_replace_regression() {
    support::assert_node_available();

    let iterations = env::var("XFSTESTS_GENERIC_037_ITERATIONS")
        .unwrap_or_else(|_| String::from("20"))
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=1000).contains(value))
        .expect("XFSTESTS_GENERIC_037_ITERATIONS must be in 1..=1000");
    let root = TestRoot::new("xfstests-generic-037-xattr-replace");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create generic/037 regression directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-generic-037");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        "memory",
        None,
    );

    let script = format!(
        r#"set -euo pipefail
target=/mnt/scratch/generic-037-probe
touch "$target"
setfattr -n user.something -v foobar "$target"
set_xattr_loop() {{
    while :; do
        setfattr -n user.something -v foobar "$target"
        setfattr -n user.something -v rabbit_hole "$target"
    done
}}
cleanup() {{
    kill "$setter_pid" >/dev/null 2>&1 || true
    wait "$setter_pid" 2>/dev/null || true
}}
trap cleanup EXIT
set_xattr_loop &
setter_pid=$!
for iteration in $(seq 1 {iterations}); do
    value=$(getfattr --only-values -n user.something "$target")
    case "$value" in
        foobar|rabbit_hole) ;;
        *) printf 'torn xattr value at iteration %s: %s\n' "$iteration" "$value" >&2; exit 1 ;;
    esac
done
printf 'generic/037 focused: ok\n'
"#
    );
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-generic-037-xattr-replace",
        "bash",
        &["-c", &script],
        HashMap::new(),
        Duration::from_secs(300),
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "generic/037 focused: ok\n", "stderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(!cleanup_path.exists(), "generic/037 root was not removed");
}

#[test]
fn xfstests_metadata_tools_round_trip() {
    support::assert_node_available();

    let root = TestRoot::new("xfstests-xattr-tools");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
    ] {
        fs::create_dir_all(path).expect("create xattr tool directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-xattr-tools");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_verification_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        root.path(),
        XFSTESTS_BACKEND,
        None,
    );

    let mut pipeline_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        pipeline_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        41,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-child-redirection-truncation",
        "bash",
        &[
            "-c",
            "printf stale-suffix > /mnt/test/redirect; printf clean > /mnt/test/redirect; touch /mnt/test/attr-file; attr -g missing /mnt/test/attr-file > /mnt/test/attr.out 2> /mnt/test/attr.err || :; printf fish | attr -s fish /mnt/test/attr-file > /mnt/test/attr.out 2> /mnt/test/attr.err; dd if=/dev/zero bs=1 count=4096 2>/dev/null | attr -q -s long /mnt/test/attr-file; dd if=/dev/zero of=/mnt/test/dd-zero bs=1 count=8 status=none; printf 'file=%s\\nerr-bytes=%s\\nzero-bytes=%s\\nzero-file-bytes=%s\\nlong-bytes=%s\\n' \"$(cat /mnt/test/redirect)\" \"$(wc -c < /mnt/test/attr.err)\" \"$(dd if=/dev/zero bs=1 count=8 2>/dev/null | wc -c)\" \"$(wc -c < /mnt/test/dd-zero)\" \"$(getfattr --only-values -n user.long /mnt/test/attr-file | wc -c)\"; cat /mnt/test/attr.out",
        ],
        pipeline_env,
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout,
        "file=clean\nerr-bytes=0\nzero-bytes=8\nzero-file-bytes=8\nlong-bytes=4096\nAttribute \"fish\" set to a 4 byte value for /mnt/test/attr-file:\nfish\n",
        "child redirection must truncate prior contents; stderr: {stderr}"
    );

    let mut write = filesystem_request(GuestFilesystemOperation::WriteFile, "/mnt/test/file");
    write.content = Some(String::from("data"));
    write.encoding = Some(RootFilesystemEntryEncoding::Utf8);
    guest_filesystem_call(&mut sidecar, 5, &connection_id, &session_id, &vm_id, write);

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-setfattr",
        "setfattr",
        &["-n", "user.tool", "-v", "works", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-getfattr",
        "getfattr",
        &["--only-values", "-n", "user.tool", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "works");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-remove-xattr",
        "setfattr",
        &["-x", "user.tool", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (stdout, _stderr, exit_code) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-get-removed-xattr",
        "getfattr",
        &["--only-values", "-n", "user.tool", "/mnt/test/file"],
    );
    assert_ne!(exit_code, 0, "removed attribute returned {stdout:?}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-setfacl",
        "setfacl",
        &["-m", "u:1001:rw-", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    let (raw_acl, stderr, exit_code) = execute_command(
        &mut sidecar,
        11,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-getfacl-wire",
        "getfattr",
        &[
            "-e",
            "hex",
            "-n",
            "system.posix_acl_access",
            "/mnt/test/file",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {raw_acl}\nstderr: {stderr}");
    assert!(
        raw_acl.contains("10000600ffffffff"),
        "ACL mask was not persisted as rw-: {raw_acl}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        12,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-getfacl",
        "getfacl",
        &["-n", "--absolute-names", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("user:1001:rw-\n"),
        "getfacl output: {stdout}"
    );
    assert!(stdout.contains("mask::rw-\n"), "getfacl output: {stdout}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        13,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chacl-list",
        "chacl",
        &["-l", "/mnt/test/file"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("u:1001:rw-"), "chacl output: {stdout}");

    let mut create_parent =
        filesystem_request(GuestFilesystemOperation::CreateDir, "/mnt/test/parent");
    create_parent.mode = Some(0o777);
    guest_filesystem_call(
        &mut sidecar,
        14,
        &connection_id,
        &session_id,
        &vm_id,
        create_parent,
    );
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        15,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-set-default-acl",
        "setfacl",
        &["-d", "--set", "u::rwx,g::rwx,o::---", "/mnt/test/parent"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        16,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-create-default-acl-child",
        "mkdir",
        &["/mnt/test/parent/child"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        17,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-get-default-acl-child",
        "getfacl",
        &["-n", "--absolute-names", "/mnt/test/parent/child"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("default:group::rwx\n") && stdout.contains("default:other::---\n"),
        "inherited default ACL output: {stdout}"
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        18,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xfs-io",
        "xfs_io",
        &[
            "-f",
            "-c",
            "pwrite -S 0x58 4 4K",
            "-c",
            "pread -q 4 4K",
            "-c",
            "fsync",
            "-c",
            "syncfs",
            "-c",
            "truncate 8K",
            "-c",
            "pwrite 1K 0 1K",
            "-c",
            "stat",
            "/mnt/test/xfs-io",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(
        stdout.contains("wrote 4096/4096 bytes at offset 4\n")
            && stdout.contains("stat.size = 8192\n"),
        "xfs_io output: {stdout}"
    );
    let stat = guest_filesystem_call(
        &mut sidecar,
        19,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Stat, "/mnt/test/xfs-io"),
    )
    .stat
    .expect("xfs_io output stat");
    assert_eq!(stat.size, 8192, "stdout: {stdout}\nstderr: {stderr}");
    let read = guest_filesystem_call(
        &mut sidecar,
        20,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/xfs-io"),
    )
    .content
    .expect("xfs_io output bytes");
    assert!(read.as_bytes()[..4].iter().all(|byte| *byte == 0));
    assert!(read.as_bytes()[4..4100].iter().all(|byte| *byte == b'X'));

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "test root was not removed");
}

#[test]
fn xfstests_xfs_io_accepts_legacy_positional_block_size() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-xfs-io-legacy-block-size");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create xfs_io probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-xfs-io-legacy");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xfs-io-legacy",
        "xfs_io",
        &[
            "-f",
            "-c",
            "pwrite -q -S 0x78 0 4K",
            "-c",
            "truncate 2K",
            "-c",
            "pwrite -q 1K 0 1K",
            "-c",
            "fpunch 512 512",
            "-c",
            "fiemap -v",
            "-c",
            "mmap 0 2K",
            "-c",
            "mread -r",
            "-c",
            "falloc 2K 1K",
            "-c",
            "truncate 2K",
            "-c",
            "finsert 512 512",
            "-c",
            "stat",
            "-c",
            "fcollapse 512 512",
            "-c",
            "stat",
            "/mnt/test/legacy-pwrite",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(stdout.contains("stat.size = 2560\n"), "stdout: {stdout}");
    assert!(stdout.contains("stat.size = 2048\n"), "stdout: {stdout}");
    assert!(
        stdout.contains("0: [0..0]: 0..0 1 0x000\n"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("1: [1..1]: hole\n"), "stdout: {stdout}");
    assert!(
        stdout.contains("2: [2..3]: 2..3 2 0x000\n"),
        "stdout: {stdout}"
    );
    let content = guest_filesystem_call(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(
            GuestFilesystemOperation::ReadFile,
            "/mnt/test/legacy-pwrite",
        ),
    )
    .content
    .expect("legacy pwrite output bytes");
    assert_eq!(content.len(), 2048);
    assert!(content.as_bytes()[..512].iter().all(|byte| *byte == 0x78));
    assert!(content.as_bytes()[512..1024].iter().all(|byte| *byte == 0));
    assert!(content.as_bytes()[1024..].iter().all(|byte| *byte == 0x78));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xfs-io-mmap-lifecycle",
        "xfs_io",
        &[
            "-f",
            "-c",
            "help mremap",
            "-c",
            "truncate 2K",
            "-c",
            "pwrite -q -S 0x58 0 2K",
            "-c",
            "mmap -rw 0 2K",
            "-c",
            "truncate 3K",
            "-c",
            "mremap -m 3K",
            "-c",
            "mwrite -S 0x57 2K 1K",
            "-c",
            "mremap 2K",
            "-c",
            "truncate 2K",
            "-c",
            "mwrite -S 0x5a 1K 1K",
            "-c",
            "truncate 3K",
            "-c",
            "mremap -m 3K",
            "-c",
            "mwrite -S 0x59 2K 1K",
            "-c",
            "msync -s 0 3K",
            "-c",
            "munmap",
            "-c",
            "close",
            "/mnt/test/mmap-lifecycle",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(
        stdout.contains("mremap [-m] len"),
        "mremap help missing: {stdout}"
    );
    let content = guest_filesystem_call(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(
            GuestFilesystemOperation::ReadFile,
            "/mnt/test/mmap-lifecycle",
        ),
    )
    .content
    .expect("mmap lifecycle output bytes");
    assert_eq!(content.len(), 3072);
    assert!(content.as_bytes()[..1024].iter().all(|byte| *byte == 0x58));
    assert!(content.as_bytes()[1024..2048]
        .iter()
        .all(|byte| *byte == 0x5a));
    assert!(content.as_bytes()[2048..].iter().all(|byte| *byte == 0x59));

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "xfs_io test root was not removed");
}

#[test]
fn xfstests_xfs_io_zero_range_preserves_bytes_extents_and_keep_size() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-xfs-io-zero-range");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create zero-range probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-xfs-io-zero-range");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xfs-io-zero-range",
        "xfs_io",
        &[
            "-f",
            "-c",
            "pwrite -q -S 0x41 0 2K",
            "-c",
            "fpunch 512 512",
            "-c",
            "fzero 384 768",
            "-c",
            "fzero -k 3K 512",
            "-c",
            "fadvise -d",
            "-c",
            "fiemap -v",
            "-c",
            "stat",
            "/mnt/test/zero-range",
        ],
        HashMap::from([(
            String::from("AGENTOS_TRACE_GUEST_FILE_ERRORS"),
            String::from("1"),
        )]),
        Duration::from_secs(20),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(stdout.contains("stat.size = 2048\n"), "stdout: {stdout}");
    assert!(
        stdout.contains("0: [0..0]: 0..0 1 0x000\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("1: [1..1]: 1..1 1 0x800\n"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("2: [2..3]: 2..3 2 0x000\n"),
        "stdout: {stdout}"
    );
    let content = guest_filesystem_call(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/zero-range"),
    )
    .content
    .expect("zero-range output bytes");
    assert_eq!(content.len(), 2048);
    assert!(content.as_bytes()[..384].iter().all(|byte| *byte == b'A'));
    assert!(content.as_bytes()[384..1152].iter().all(|byte| *byte == 0));
    assert!(content.as_bytes()[1152..].iter().all(|byte| *byte == b'A'));

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-xfs-io-zero-range-extend",
        "xfs_io",
        &["-c", "fzero 2K 512", "-c", "stat", "/mnt/test/zero-range"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert!(stdout.contains("stat.size = 2560\n"), "stdout: {stdout}");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-zero-partial-data-block",
        "xfs_io",
        &[
            "-f",
            "-c",
            "pwrite -q -S 0xcd 0 4K",
            "-c",
            "fzero 128 128",
            "-c",
            "fiemap -v",
            "/mnt/test/zero-partial-data-block",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(stdout, "0: [0..7]: 0..7 8 0x000\n");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-binary-diff-setup",
        "sh",
        &[
            "-c",
            "xfs_io -f -c 'pwrite -q -S 0xff 0 512' /mnt/test/binary-a && cp /mnt/test/binary-a /mnt/test/binary-b && diff /mnt/test/binary-a /mnt/test/binary-b && xfs_io -c 'pwrite -q -S 0xfe 0 1' /mnt/test/binary-b",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(stderr, "");

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-binary-diff-different",
        "diff",
        &["/mnt/test/binary-a", "/mnt/test/binary-b"],
    );
    assert_eq!(exit_code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "");
    assert_eq!(
        stdout,
        "Binary files /mnt/test/binary-a and /mnt/test/binary-b differ\n"
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "test root was not removed");
}

#[test]
fn xfstests_awk_formats_unsigned_integers() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-awk-unsigned");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create awk probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-awk-unsigned");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-awk-unsigned",
        "sh",
        &[
            "-c",
            concat!(
                "printf '' | awk 'BEGIN { printf \"%u %05u\\n\", 42, 7 }'; ",
                "probe() { OPTIND=1; while getopts 'dku' option; do :; done; ",
                "shift \"$((OPTIND - 1))\"; printf '%s\\n' \"$1\"; }; ",
                "probe -d sentinel; ",
                "dd if=/dev/zero bs=1 count=26 status=none | od -Ax -t x1 | tail -n 1",
            ),
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert_eq!(stdout, "42 00007\nsentinel\n00001a\n");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "awk test root was not removed");
}

#[test]
#[ignore = "focused Linux FIFO descriptor routing regression; run explicitly"]
fn xfstests_wasi_fifo_open_routes_to_kernel_pipe() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-fifo-open");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create FIFO probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-fifo-open");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &c_probe_root(),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-fifo-open",
        "sh",
        &[
            "-c",
            "cd /mnt/test && mkfifo -m 600 fifo && fifo_test fifo || exit $?; fifo_test --reader fifo > reader.out 2> reader.err & reader=$!; sleep 1; fifo_test --writer fifo > writer.out 2> writer.err; writer_status=$?; wait $reader; reader_status=$?; cat writer.out reader.out; cat writer.err reader.err >&2; printf 'writer=%s reader=%s\n' \"$writer_status\" \"$reader_status\"; test \"$writer_status\" = 0 -a \"$reader_status\" = 0",
        ],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout,
        "fifo-nonblocking-ok\nfifo-writer-ok\nfifo-reader-ok\nwriter=0 reader=0\n"
    );
    assert_eq!(stderr, "");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "FIFO probe root was not removed");
}

#[test]
#[ignore = "focused Linux renameat2 flag and atomicity regression; run explicitly"]
fn xfstests_wasi_renameat2_preserves_linux_flag_semantics() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-renameat2");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create renameat2 probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-renameat2");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-renameat2",
        "sh",
        &[
            "-c",
            concat!(
                "set -e; cd /mnt/test; mkdir renameat2-probe; cd renameat2-probe; ",
                "printf source > src; printf old > dst; ",
                "/opt/xfstests/src/renameat2 src moved || exit $?; ",
                "test ! -e src; test \"$(cat moved)\" = source; ",
                "printf source > src; printf old > dst; ",
                "if /opt/xfstests/src/renameat2 -n missing dst >missing.out 2>missing.err; then exit 20; fi; ",
                "grep -q 'No such file or directory' missing.err; test \"$(cat dst)\" = old; ",
                "if /opt/xfstests/src/renameat2 -n src dst >n.out 2>n.err; then exit 21; fi; ",
                "grep -q 'File exists' n.err; test \"$(cat src)\" = source; test \"$(cat dst)\" = old; ",
                "/opt/xfstests/src/renameat2 -x src dst; ",
                "test \"$(cat src)\" = old; test \"$(cat dst)\" = source; ",
                "if /opt/xfstests/src/renameat2 -n -x src dst >invalid.out 2>invalid.err; then exit 22; fi; ",
                "grep -q 'Invalid argument' invalid.err; ",
                "test \"$(cat src)\" = old; test \"$(cat dst)\" = source; ",
                "mkdir source-tree destination-tree; printf source > source-tree/bar; printf destination > destination-tree/bar; ",
                "if /opt/xfstests/src/renameat2 source-tree destination-tree >tree.out 2>tree.err; then exit 23; fi; ",
                "grep -q 'Directory not empty' tree.err; ",
                "test \"$(cat source-tree/bar)\" = source; test \"$(cat destination-tree/bar)\" = destination; ",
                "printf 'renameat2-ok\\n'",
            ),
        ],
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "renameat2 probe root was not removed"
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "renameat2-ok\n");
    assert_eq!(stderr, "");
}

#[test]
fn xfstests_mknod_creates_a_working_null_device() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-mknod");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create mknod probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-mknod");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &c_probe_root(),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let mut command_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        command_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-mknod",
        "sh",
        &[
            "-c",
            "printf 'No such attribute\\nOperation not permitted\\n' | sed -e 's:\\(No such attribute\\|Operation not permitted\\):normalized:'; printf '# file: b\\nx=2\\n\\n# file: a\\nx=1\\n\\n' | awk '{a[FNR]=$0}END{n = asort(a); for(i=1; i <= n; i++) print a[i]\"\\n\"}' RS=; touch /mnt/test/syscalltest && setfattr -n user.xfstests -v attr /mnt/test/syscalltest > /mnt/test/syscalltest.out 2>&1 && test ! -s /mnt/test/syscalltest.out && mknod /mnt/test/null c 1 3 && mknod -m 640 /mnt/test/block b 8 1 && mkfifo -m 600 /mnt/test/fifo && mkdir -p /mnt/test/link-target/child && ln -s link-target /mnt/test/link && test \"$(find /mnt/test | grep -c '/link/child')\" = 0 && setfattr -h -n trusted.link -v symlink /mnt/test/link && test \"$(getfattr -h --only-values -n trusted.link /mnt/test/link)\" = symlink && setfattr -h -n trusted.binary -v 0xbabe /mnt/test/link && getfattr --absolute-names -dh -m trusted.binary /mnt/test/link > /mnt/results/xattr.backup && setfattr -h -x trusted.binary /mnt/test/link && setfattr -h --restore=/mnt/results/xattr.backup && getfattr -h -e hex -n trusted.binary /mnt/test/link | grep -q 'trusted.binary=0xbabe' && setfattr -n trusted.walk -v child /mnt/test/link-target/child && getfattr -L -R -m trusted.walk /mnt/test/link | grep -q '# file: mnt/test/link/child' && if setfattr -h -n user.denied -v no /mnt/test/link 2>/dev/null; then exit 31; fi && if setfattr -n user.denied -v no /mnt/test/block 2>/dev/null; then exit 32; fi && if setfattr -n user.denied -v no /mnt/test/fifo 2>/dev/null; then exit 33; fi && stat -c '%F %t:%T' /mnt/test/null /mnt/test/block /mnt/test/fifo && printf x | sh -c 'test \"$(stat -c %F /dev/fd/0)\" = fifo; cat >/dev/null' && echo fred > /mnt/test/null && fifo_test /mnt/test/fifo && setfattr -n trusted.probe -v fifo /mnt/test/fifo && test \"$(getfattr --only-values -n trusted.probe /mnt/test/fifo)\" = fifo",
        ],
        command_env,
        Duration::from_secs(60),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.starts_with("normalized\nnormalized\n# file: a\nx=1\n\n# file: b\nx=2\n\n"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("character special file 1:3"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("block special file 8:1"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("fifo 0:0") && stdout.contains("fifo-nonblocking-ok"),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "mknod test root was not removed");
}

#[test]
fn xfstests_chattr_immutable_enforces_and_clears_write_protection() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-chattr-immutable");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create chattr probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-chattr");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-create",
        "sh",
        &["-c", "printf original >/mnt/test/immutable"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-set",
        "chattr",
        &["+i", "/mnt/test/immutable"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let (_, _, write_exit) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-denied-write",
        "sh",
        &["-c", "printf changed >/mnt/test/immutable"],
    );
    assert_ne!(write_exit, 0, "immutable write unexpectedly succeeded");
    let (_, _, unlink_exit) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-denied-unlink",
        "rm",
        &["/mnt/test/immutable"],
    );
    assert_ne!(unlink_exit, 0, "immutable unlink unexpectedly succeeded");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-clear",
        "chattr",
        &["-i", "/mnt/test/immutable"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-write-after-clear",
        "sh",
        &["-c", "printf changed >/mnt/test/immutable"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let content = guest_filesystem_call(
        &mut sidecar,
        11,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, "/mnt/test/immutable"),
    )
    .content
    .expect("read content after clearing immutable marker");
    assert_eq!(content, "changed");
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        12,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-chattr-unlink-after-clear",
        "rm",
        &["/mnt/test/immutable"],
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "chattr test root was not removed");
}

const XFSTESTS_BACKEND: &str = "chunked_local";
const XFSTESTS_DEFAULT_BACKENDS: &[&str] = &[
    "chunked_local",
    "memory",
    "chunked_s3",
    // Retained for focused return-to-service runs, but intentionally excluded
    // while the native object_s3 plugin is not exposed to users.
    // "object_s3",
];
const COMMAND_PACKAGES: &[&str] = &[
    "acl",
    "attr",
    "coreutils",
    "diffutils",
    "findutils",
    "gawk",
    "grep",
    "sed",
    "xfsprogs",
];

#[derive(Clone, Debug, Deserialize)]
struct ExceptionRecord {
    id: String,
    backend: String,
    disposition: String,
    reason: String,
    #[serde(default)]
    tracking_issue: Option<String>,
    #[serde(default)]
    notrun_reason: Option<String>,
    #[serde(default)]
    output_digest: Option<String>,
    #[serde(default)]
    classification: Option<String>,
    #[serde(default)]
    reduction: Option<String>,
    #[serde(default)]
    full_iterations: Option<u64>,
    #[serde(default)]
    reduced_iterations: Option<u64>,
    #[serde(default)]
    focused_coverage: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum OutcomeKind {
    Pass,
    Fail { output: String, digest: String },
    NotRun { reason: String },
    AllowedNotRun { reason: String },
    Harness { reason: String },
    Excluded { reason: String },
    Deferred { reason: String },
    ExpectedFailure { reason: String },
    ReducedPass { reason: String },
    UnexpectedPass { reason: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TestOutcome {
    id: String,
    backend: String,
    kind: OutcomeKind,
    stdout: String,
    stderr: String,
}

impl TestOutcome {
    fn harness(id: &str, backend: &str, reason: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            backend: backend.to_owned(),
            kind: OutcomeKind::Harness {
                reason: reason.into(),
            },
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

#[test]
fn xfstests_object_s3_directory_metadata_and_relative_fill() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-object-s3-mkdir");
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create object S3 mkdir test directory");
    }

    let server = MockS3Server::start();
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-object-s3-mkdir");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), "object_s3", Some(server.base_url())),
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-mkdir",
        "mkdir",
        &["/mnt/scratch/fill-probe"],
    );
    assert_eq!(
        exit_code, 0,
        "guest mkdir failed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        server
            .object_keys()
            .iter()
            .any(|key| key.ends_with("mnt-scratch/fill-probe/")),
        "guest mkdir did not persist an object directory marker: {:?}",
        server.object_keys()
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-nested-mkdir",
        "bash",
        &["-c", "mkdir /mnt/scratch/from-bash"],
    );
    assert_eq!(
        exit_code, 0,
        "nested guest mkdir failed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        server
            .object_keys()
            .iter()
            .any(|key| key.ends_with("mnt-scratch/from-bash/")),
        "nested guest mkdir did not persist an object directory marker: {:?}",
        server.object_keys()
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-recreate-dir",
        "bash",
        &[
            "-c",
            "rm -rf /mnt/scratch/fill-probe; mkdir /mnt/scratch/fill-probe",
        ],
    );
    assert_eq!(
        exit_code, 0,
        "guest directory recreation failed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        server
            .object_keys()
            .iter()
            .any(|key| key.ends_with("mnt-scratch/fill-probe/")),
        "recreated guest directory marker is missing: {:?}",
        server.object_keys()
    );

    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-relative-fill",
        "bash",
        &[
            "-c",
            "cd /mnt/scratch/fill-probe && /opt/xfstests/src/fill small small 10",
        ],
    );
    assert_eq!(
        exit_code,
        0,
        "relative fill under recreated object directory failed: stdout={stdout:?} stderr={stderr:?} objects={:?} metadata={:?}",
        server.object_keys(),
        server.object_metadata()
    );

    server.clear_requests();
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-special-node-cleanup",
        "bash",
        &[
            "-c",
            "mkdir /mnt/scratch/dev && mknod /mnt/scratch/dev/b b 8 1 && mknod /mnt/scratch/dev/c c 1 3 && mkfifo /mnt/scratch/dev/p && find /mnt/scratch/dev -maxdepth 1 | sort && stat -c '%F %t:%T' /mnt/scratch/dev/b /mnt/scratch/dev/c /mnt/scratch/dev/p && if setfattr -n user.denied -v no /mnt/scratch/dev/b 2>/dev/null; then exit 31; fi && if setfattr -n user.denied -v no /mnt/scratch/dev/p 2>/dev/null; then exit 32; fi",
        ],
    );
    assert_eq!(
        exit_code, 0,
        "object S3 special-node lifecycle failed: stdout={stdout:?} stderr={stderr:?} objects={:?} metadata={:?}",
        server.object_keys(),
        server.object_metadata()
    );
    let special_node_objects = server.object_keys();
    let special_node_metadata = server.object_metadata();
    let special_node_entries = guest_filesystem_call(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::ReadDir, "/mnt/scratch/dev"),
    )
    .entries
    .expect("object S3 special-node directory entries");

    let (cleanup_stdout, cleanup_stderr, cleanup_exit_code) = execute_command(
        &mut sidecar,
        11,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-special-node-cleanup",
        "bash",
        &[
            "-c",
            "rm -rf /mnt/scratch/dev && test ! -e /mnt/scratch/dev && mkdir /mnt/scratch/dev && test -d /mnt/scratch/dev",
        ],
    );
    assert_eq!(
        cleanup_exit_code, 0,
        "object S3 cleanup failed: stdout={cleanup_stdout:?} stderr={cleanup_stderr:?} objects={:?} metadata={:?}",
        server.object_keys(),
        server.object_metadata()
    );
    let listing_evidence = || {
        format!(
            "stdout={stdout:?} entries={special_node_entries:?} objects={special_node_objects:?} metadata={special_node_metadata:?}"
        )
    };
    assert!(
        stdout.contains("/mnt/scratch/dev/b\n"),
        "{}",
        listing_evidence()
    );
    assert!(
        stdout.contains("/mnt/scratch/dev/c\n"),
        "{}",
        listing_evidence()
    );
    assert!(
        stdout.contains("/mnt/scratch/dev/p\n"),
        "{}",
        listing_evidence()
    );
    assert!(
        stdout.contains("block special file 8:1"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("character special file 1:3"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("fifo 0:0"), "stdout: {stdout}");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn xfstests_object_s3_rejects_overlong_xattr_names_without_backend_io() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-object-s3-long-xattr");
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create object S3 xattr test directory");
    }

    let server = MockS3Server::start();
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-object-s3-long-xattr");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), "object_s3", Some(server.base_url())),
    );

    let (_, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-long-xattr-touch",
        "touch",
        &["/mnt/test/long-xattr"],
    );
    assert_eq!(exit_code, 0, "touch failed: {stderr}");

    let long_name = "X".repeat(300);
    for (request_id, operation) in [(6, "-s"), (7, "-g"), (8, "-r")] {
        server.clear_requests();
        let mut args = vec![operation, long_name.as_str()];
        if operation == "-s" {
            args.extend(["-V", "fish"]);
        }
        args.push("/mnt/test/long-xattr");
        let (stdout, stderr, exit_code) = execute_command(
            &mut sidecar,
            request_id,
            &connection_id,
            &session_id,
            &vm_id,
            &format!("xfstests-object-s3-long-xattr-{request_id}"),
            "attr",
            &args,
        );
        assert_ne!(
            exit_code, 0,
            "overlong {operation} unexpectedly succeeded: stdout={stdout:?} stderr={stderr:?}"
        );
        let requests = server.requests();
        assert!(
            requests
                .iter()
                .all(|request| !request.path.ends_with("/mnt-test/long-xattr")),
            "overlong {operation} accessed the target S3 object: {requests:?}"
        );
    }

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

fn xfstests_backends() -> Vec<String> {
    let configured = env::var("XFSTESTS_BACKENDS")
        .ok()
        .map(|value| {
            value
                .split([',', ' ', '\n'])
                .filter(|backend| !backend.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            XFSTESTS_DEFAULT_BACKENDS
                .iter()
                .map(|backend| (*backend).to_owned())
                .collect()
        });
    assert!(
        !configured.is_empty(),
        "XFSTESTS_BACKENDS selected zero backends"
    );
    let mut seen = HashSet::new();
    for backend in &configured {
        assert!(
            XFSTESTS_DEFAULT_BACKENDS.contains(&backend.as_str()),
            "unsupported XFSTESTS_BACKENDS entry: {backend}"
        );
        assert!(
            seen.insert(backend.clone()),
            "duplicate xfstests backend: {backend}"
        );
    }
    configured
}

fn read_optional_text(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
) -> Option<String> {
    let exists = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        filesystem_request(GuestFilesystemOperation::Exists, path),
    );
    if exists.exists != Some(true) {
        return None;
    }
    guest_filesystem_call(
        sidecar,
        request_id + 1,
        connection_id,
        session_id,
        vm_id,
        filesystem_request(GuestFilesystemOperation::ReadFile, path),
    )
    .content
}

fn normalized_digest(output: &str) -> String {
    let normalized = output.replace("\r\n", "\n");
    format!(
        "sha256:{:x}",
        Sha256::digest(normalized.trim_end().as_bytes())
    )
}

fn xfstests_output_reports_failure(test_id: &str, stdout: &str, stderr: &str) -> bool {
    stdout.lines().any(|line| {
        (line.starts_with(test_id) && line.contains("[failed"))
            || line.starts_with("Failures:")
            || (line.starts_with("Failed ") && line.contains(" of "))
    }) || stderr
        .lines()
        .any(|line| line.contains("command not found:"))
}

fn xfstest_timeout() -> Duration {
    let configured =
        env::var("XFSTESTS_TEST_TIMEOUT_SECONDS").unwrap_or_else(|_| String::from("180"));
    let seconds = parse_xfstest_timeout_seconds(&configured)
        .expect("XFSTESTS_TEST_TIMEOUT_SECONDS must be in 1..=7200");
    Duration::from_secs(seconds)
}

fn parse_xfstest_timeout_seconds(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| (1..=7200).contains(seconds))
}

fn xfstests_mounts(
    source: &Path,
    root: &Path,
    backend: &str,
    s3_endpoint: Option<&str>,
) -> Vec<MountDescriptor> {
    let mut mounts = COMMAND_PACKAGES
        .iter()
        .enumerate()
        .map(|(index, package)| {
            host_dir_mount(
                &format!("/__agentos/commands/{index}"),
                &command_root(package),
                true,
            )
        })
        .collect::<Vec<_>>();
    mounts.extend([
        host_dir_mount("/opt/xfstests", source, true),
        xfstests_backend_mount(
            backend,
            "/mnt/test",
            "/dev/agentos-test",
            &root.join("test"),
            s3_endpoint,
        ),
        xfstests_backend_mount(
            backend,
            "/mnt/scratch",
            "/dev/agentos-scratch",
            &root.join("scratch"),
            s3_endpoint,
        ),
        chunked_local_mount(
            "/mnt/results",
            "/dev/agentos-results",
            &root.join("results"),
        ),
    ]);
    mounts
}

#[test]
fn xfstests_permname_concurrent_workers_exit() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-permname-workers");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create permname probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-permname-workers");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-permname-workers",
        "sh",
        &[
            "-c",
            "set -e; mkdir /mnt/test/permname-probe; cd /mnt/test/permname-probe; /opt/xfstests/src/permname -c 2 -l 2 -p 2; test \"$(find . -type f | wc -l)\" = 4; printf 'permname-ok\\n'",
        ],
        HashMap::new(),
        Duration::from_secs(20),
    );
    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "permname workers did not exit; stdout: {:?}; stderr: {:?}",
            output.stdout, output.stderr
        )
    });
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "permname-ok\n", "stderr: {stderr}");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("alpha size = "))
            .count(),
        1,
        "unexpected helper stderr: {stderr}"
    );

    let (nametest_stdout, nametest_stderr, nametest_exit) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-nametest-linux-sequence",
        "sh",
        &[
            "-c",
            "set -e; mkdir /mnt/test/nametest-probe; cd /mnt/test/nametest-probe; i=1; while [ $i -le 100 ]; do echo \"nametest.$i\" >> names; let i=$i+1; done; printf 'loop-i=%s\\n' \"$i\"; wc -l < names; /opt/xfstests/src/nametest -l names -s 1 -i 300 -z",
        ],
    );
    assert_eq!(
        nametest_exit, 0,
        "stdout: {nametest_stdout}\nstderr: {nametest_stderr}"
    );
    assert_eq!(
        nametest_stdout,
        concat!(
            "loop-i=101\n",
            "100\n",
            ".Seed = 1 (use \"-s 1\" to re-execute this test)\n",
            "..\n",
            "creates:     75 OK,     37 EEXIST  (   112 total, 33% EEXIST)\n",
            "removes:     44 OK,     67 ENOENT  (   111 total, 60% ENOENT)\n",
            "lookups:     31 OK,     46 ENOENT  (    77 total, 59% ENOENT)\n",
            "total  :    150 OK,    150 w/error (   300 total, 50% w/error)\n",
            "\n",
            "cleanup:     31 removes\n",
        ),
        "stderr: {nametest_stderr}"
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "permname probe root was not removed"
    );
}

#[test]
#[ignore = "focused object_s3 permname request-amplification regression"]
fn xfstests_object_s3_permname_requests_are_bounded_per_file() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-object-s3-permname-requests");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create object_s3 permname request directory");
    }

    let server = MockS3Server::start();
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-object-s3-permname-requests");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), "object_s3", Some(server.base_url())),
    );

    let (mkdir_stdout, mkdir_stderr, mkdir_exit) = execute_command(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "object-s3-permname-request-mkdir",
        "mkdir",
        &["/mnt/test/permname-requests"],
    );
    assert_eq!(
        mkdir_exit, 0,
        "probe mkdir failed: stdout={mkdir_stdout:?} stderr={mkdir_stderr:?}"
    );

    server.clear_requests();
    let (stdout, stderr, exit_code) = execute_command(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "object-s3-permname-request-probe",
        "sh",
        &[
            "-c",
            "cd /mnt/test/permname-requests && /opt/xfstests/src/permname -c 2 -l 2 -p 1",
        ],
    );
    let requests_before_flush = server
        .requests()
        .into_iter()
        .filter(|request| {
            request
                .path
                .contains("/xfstests/mnt-test/permname-requests")
        })
        .collect::<Vec<_>>();

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    let requests = server
        .requests()
        .into_iter()
        .filter(|request| {
            request
                .path
                .contains("/xfstests/mnt-test/permname-requests")
        })
        .collect::<Vec<_>>();
    let method_counts = requests.iter().fold(HashMap::new(), |mut counts, request| {
        *counts.entry(request.method.as_str()).or_insert(0usize) += 1;
        counts
    });
    let mut path_counts = requests.iter().fold(HashMap::new(), |mut counts, request| {
        *counts.entry(request.path.as_str()).or_insert(0usize) += 1;
        counts
    });
    let mut hottest_paths = path_counts.drain().collect::<Vec<_>>();
    hottest_paths.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    hottest_paths.truncate(8);
    let file_puts_before_flush = requests_before_flush
        .iter()
        .filter(|request| {
            request.method == "PUT"
                && request
                    .path
                    .strip_prefix("/xfstests/mnt-test/permname-requests/")
                    .is_some_and(|name| !name.is_empty() && !name.contains('/'))
        })
        .count();
    let file_puts_after_flush = requests
        .iter()
        .filter(|request| {
            request.method == "PUT"
                && request
                    .path
                    .strip_prefix("/xfstests/mnt-test/permname-requests/")
                    .is_some_and(|name| !name.is_empty() && !name.contains('/'))
        })
        .count();

    eprintln!(
        "object_s3 four-file probe: before_flush_requests={} before_flush_file_puts={} after_flush_requests={} after_flush_file_puts={} methods={method_counts:?} hottest_paths={hottest_paths:?}",
        requests_before_flush.len(),
        file_puts_before_flush,
        requests.len(),
        file_puts_after_flush
    );

    drop(sidecar);
    drop(root);

    assert_eq!(exit_code, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(
        file_puts_before_flush, 0,
        "dirty files were persisted before sync/unmount: {requests_before_flush:?}"
    );
    assert_eq!(
        file_puts_after_flush, 4,
        "unmount must persist each dirty file exactly once: {requests:?}"
    );
    assert!(
        requests.len() <= 320,
        "four zero-byte creates issued {} object requests (budget 320; methods={method_counts:?}; hottest_paths={hottest_paths:?}; first={:?}; last={:?})",
        requests.len(),
        requests.first(),
        requests.last()
    );
    assert!(
        !cleanup_path.exists(),
        "object_s3 permname request probe root was not removed"
    );
}

#[test]
#[ignore = "focused object_s3 post-permname command-availability regression"]
fn xfstests_object_s3_commands_survive_permname_workload() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let name_length = env::var("XFSTESTS_PERMNAME_NAME_LENGTH")
        .unwrap_or_else(|_| String::from("3"))
        .parse::<u32>()
        .ok()
        .filter(|length| (1..=6).contains(length))
        .expect("XFSTESTS_PERMNAME_NAME_LENGTH must be in 1..=6");
    let expected_files = 4_u32.pow(name_length);
    let phase_selection =
        env::var("XFSTESTS_PERMNAME_PHASES").unwrap_or_else(|_| String::from("both"));
    let phases = match phase_selection.as_str() {
        "one" => vec!["one"],
        "multi" => vec!["multi"],
        "both" => vec!["one", "multi"],
        _ => panic!("XFSTESTS_PERMNAME_PHASES must be one, multi, or both"),
    };
    let phase_words = phases.join(" ");
    let root = TestRoot::new("xfstests-object-s3-command-lifetime");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create object_s3 command-lifetime directory");
    }

    let s3_server = MockS3Server::start();
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-object-s3-command-lifetime");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(
            &source,
            root.path(),
            "object_s3",
            Some(s3_server.base_url()),
        ),
    );

    let script = format!(
        "set -e
bash -c '
set -e
for phase in {phase_words}; do
  workers=1
  test \"$phase\" = multi && workers=4
  mkdir \"/mnt/test/$phase\"
  cd \"/mnt/test/$phase\"
  /opt/xfstests/src/permname -c 4 -l {name_length} -p \"$workers\"
  test \"$(find . -type f | wc -l)\" = {expected_files}
done
'
probe=/mnt/results/command-lifetime-$$
rm -f \"$probe\" \"$probe.moved\"
date >/dev/null
printf x > \"$probe\"
sed -n 1p \"$probe\" >/dev/null
mv \"$probe\" \"$probe.moved\"
diff \"$probe.moved\" \"$probe.moved\"
head -n 0 \"$probe.moved\"
uname >/dev/null
test \"$(expr 1 + 1)\" = 2
rm -f \"$probe.moved\"
printf 'parent-commands-ok\\n'"
    );
    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-command-lifetime",
        "bash",
        &["-c", &script],
        HashMap::new(),
        xfstest_timeout(),
    );
    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "object_s3 permname command-lifetime probe timed out: stdout={:?} stderr={:?}",
            output.stdout, output.stderr
        )
    });
    let bin_rm_exists = guest_filesystem_call(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Exists, "/bin/rm"),
    )
    .exists;
    let mounted_rm_exists = guest_filesystem_call(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Exists, "/__agentos/commands/2/rm"),
    )
    .exists;
    let (fresh_stdout, fresh_stderr, fresh_exit_code) = execute_command(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-object-s3-fresh-command-check",
        "bash",
        &[
            "-c",
            "set -e; probe=/mnt/results/fresh-command-lifetime-$$; rm -f \"$probe\" \"$probe.moved\"; date >/dev/null; printf x > \"$probe\"; sed -n 1p \"$probe\" >/dev/null; mv \"$probe\" \"$probe.moved\"; diff \"$probe.moved\" \"$probe.moved\"; head -n 0 \"$probe.moved\"; uname >/dev/null; test \"$(expr 1 + 1)\" = 2; rm -f \"$probe.moved\"; printf 'fresh-commands-ok\\n'",
        ],
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    assert_eq!(
        exit_code, 0,
        "stdout: {stdout}\nstderr: {stderr}\n/bin/rm exists: {bin_rm_exists:?}\ncommand mount rm exists: {mounted_rm_exists:?}\nfresh exit: {fresh_exit_code}\nfresh stdout: {fresh_stdout}\nfresh stderr: {fresh_stderr}"
    );
    assert_eq!(stdout, "parent-commands-ok\n", "stderr: {stderr}");
    assert_eq!(bin_rm_exists, Some(true));
    assert_eq!(mounted_rm_exists, Some(true));
    assert_eq!(fresh_exit_code, 0, "fresh stderr: {fresh_stderr}");
    assert_eq!(fresh_stdout, "fresh-commands-ok\n");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("alpha size = "))
            .count(),
        phases.len(),
        "unexpected helper stderr: {stderr}"
    );
    assert!(!cleanup_path.exists(), "focused root was not removed");
}

#[test]
#[ignore = "focused pinned dirstress process matrix; run explicitly with staged xfstests"]
fn xfstests_wasi_dirstress_process_matrix() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let file_count = env::var("XFSTESTS_DIRSTRESS_FILES")
        .unwrap_or_else(|_| String::from("8"))
        .parse::<usize>()
        .expect("XFSTESTS_DIRSTRESS_FILES must be a positive integer");
    assert!(file_count > 0, "XFSTESTS_DIRSTRESS_FILES must be positive");
    let timeout = env::var("XFSTESTS_DIRSTRESS_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| String::from("120"))
        .parse::<u64>()
        .expect("XFSTESTS_DIRSTRESS_TIMEOUT_SECONDS must be a positive integer");
    assert!(
        timeout > 0,
        "XFSTESTS_DIRSTRESS_TIMEOUT_SECONDS must be positive"
    );
    let root = TestRoot::new("xfstests-dirstress-process-matrix");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create dirstress probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-dirstress");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None),
    );

    let script = format!(
        r#"
set -e
base=/mnt/test/dirstress-probe
run_case() {{
    name=$1
    procs=$2
    shared=$3
    rm -rf "$base"
    mkdir "$base"
    if ! /opt/xfstests/src/dirstress -d "$base" -f {file_count} -p "$procs" -n "$shared" -s 1 >"/mnt/results/$name.log" 2>&1; then
        cat "/mnt/results/$name.log" >&2
        exit 1
    fi
    grep -Fqx 'INFO: Dirstress complete' "/mnt/results/$name.log" || exit 1
    test ! -e "$base/stressdir" || exit 1
}}
run_case one-by-one 1 1
run_case five-by-one 5 1
run_case five-by-five 5 5
printf 'dirstress-ok\n'
"#
    );
    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-dirstress-process-matrix",
        "sh",
        &["-c", &script],
        HashMap::new(),
        Duration::from_secs(timeout),
    );
    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "dirstress process matrix did not exit; stdout: {:?}; stderr: {:?}",
            output.stdout, output.stderr
        )
    });
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "dirstress-ok\n", "stderr: {stderr}");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "dirstress probe root was not removed"
    );
}

#[test]
#[ignore = "focused Linux waitpid option and status regression; run explicitly"]
fn xfstests_wasi_waitpid_options_and_status() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-waitpid-options-status");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create waitpid probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-waitpid-status");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &c_probe_root(),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-waitpid-options-status",
        "sh",
        &[
            "-c",
            "set -e; waitpid_status; signal_tests > /mnt/results/signal-tests.log; grep -Fqx 'test_sigkill: ok' /mnt/results/signal-tests.log; grep -Fqx 'test_kill_exited: ok' /mnt/results/signal-tests.log; grep -Fqx 'test_kill_invalid: ok' /mnt/results/signal-tests.log",
        ],
        HashMap::new(),
        Duration::from_secs(30),
    );
    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "waitpid status probe did not exit; stdout: {:?}; stderr: {:?}",
            output.stdout, output.stderr
        )
    });
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "waitpid-status-ok\n", "stderr: {stderr}");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(!cleanup_path.exists(), "waitpid probe root was not removed");
}

#[test]
#[ignore = "focused Linux self-stop and parent-continue regression; run explicitly"]
fn xfstests_wasi_self_stop_and_parent_continue() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-self-stop-parent-continue");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create self-stop probe directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-self-stop");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &c_probe_root(),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-self-stop-parent-continue",
        "sh",
        &["-c", "set -e; self_stop_status"],
        HashMap::new(),
        Duration::from_secs(30),
    );
    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "self-stop probe did not exit; stdout: {:?}; stderr: {:?}",
            output.stdout, output.stderr
        )
    });
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "child-before-stop\nchild-after-continue\nself-stop-ok\n",
        "stderr: {stderr}"
    );

    let top_level_process_id = "xfstests-top-level-self-stop";
    execute_wire(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        top_level_process_id,
        GuestRuntimeKind::WebAssembly,
        &c_probe_root().join("self_stop_status"),
        vec![String::from("child")],
    );
    let stopped = support::try_collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        top_level_process_id,
        Duration::from_millis(500),
    )
    .expect_err("top-level self-stop must remain blocked before SIGCONT");
    assert_eq!(stopped.stdout, "child-before-stop\n");
    assert_eq!(stopped.stderr, "");

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: top_level_process_id.to_owned(),
                signal: String::from("SIGCONT"),
            }),
        ))
        .expect("continue top-level self-stopped process");
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        top_level_process_id,
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "child-after-continue\n", "stderr: {stderr}");

    let teardown_process_id = "xfstests-self-stop-teardown";
    execute_wire(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        teardown_process_id,
        GuestRuntimeKind::WebAssembly,
        &c_probe_root().join("self_stop_status"),
        vec![String::from("child")],
    );
    let stopped = support::try_collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        teardown_process_id,
        Duration::from_millis(500),
    )
    .expect_err("teardown probe must remain stopped before VM disposal");
    assert_eq!(stopped.stdout, "child-before-stop\n");
    assert_eq!(stopped.stderr, "");

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);
    assert!(
        !cleanup_path.exists(),
        "self-stop probe root was not removed"
    );
}

#[test]
#[ignore = "focused generic/032 process-throughput regression; run explicitly with staged xfstests"]
fn xfstests_generic_032_process_throughput_regression() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let iterations = env::var("XFSTESTS_GENERIC_032_ITERATIONS")
        .unwrap_or_else(|_| String::from("2"))
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=100).contains(value))
        .expect("XFSTESTS_GENERIC_032_ITERATIONS must be in 1..=100");
    let timeout_seconds = env::var("XFSTESTS_GENERIC_032_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| String::from("30"))
        .parse::<u64>()
        .ok()
        .filter(|value| (1..=3600).contains(value))
        .expect("XFSTESTS_GENERIC_032_TIMEOUT_SECONDS must be in 1..=3600");
    let root = TestRoot::new("xfstests-generic-032-throughput");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create generic/032 throughput directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-generic-032-throughput");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(&source, root.path(), "memory", None),
    );

    let script = format!(
        r#"set -ehu
target=/mnt/scratch/generic-032-probe
full_log=/mnt/results/generic-032.full
: > "$target"
: > "$full_log"
unrelated_heredoc() {{
    cat >/dev/null <<'ENDL'
inherited but not invoked
ENDL
    :
}}
sync_loop() {{
    trap 'wait; exit' TERM
    while :; do
        xfs_io -r -c syncfs /mnt/scratch >/dev/null 2>&1
    done
}}
cleanup() {{
    kill -9 "$syncpid" >/dev/null 2>&1 || true
    wait "$syncpid" 2>/dev/null || true
}}
trap cleanup EXIT
printf 'stage=before-background\n' > /mnt/results/generic-032-progress
printf 'stage=before-background\n'
sync_loop &
syncpid=$!
printf 'stage=after-background\n' >> /mnt/results/generic-032-progress
printf 'stage=after-background\n'

for iter in $(seq 1 {iterations}); do
    rm -f "$target"
    for pgoff in $(seq 0 0x1000 0xf000); do
        offset=$((pgoff + 0xc00))
        xfs_io -f -c "pwrite $offset 0x1" "$target" >>"$full_log" 2>&1
    done
    printf 'iteration=%s stage=small-writes\n' "$iter"

    xfs_io \
        -c 'falloc 0 0x10000' \
        -c 'pwrite 0 0x100000' \
        -c fsync \
        "$target" >>"$full_log" 2>&1
    printf 'iteration=%s stage=fsync\n' "$iter"

    if xfs_io -c 'fiemap -v' "$target" | grep -q unwritten; then
        printf 'unwritten extent in iteration %s\n' "$iter" >&2
        exit 70
    fi
    printf 'iteration=%s stage=fiemap\n' "$iter"
done

printf 'stage=before-term\n' >> /mnt/results/generic-032-progress
printf 'stage=before-term\n'
kill "$syncpid"
printf 'stage=after-term\n' >> /mnt/results/generic-032-progress
printf 'stage=after-term\n'
wait
printf 'stage=after-wait\n' >> /mnt/results/generic-032-progress
printf 'stage=after-wait\n'
trap - EXIT
printf 'generic-032-focused-ok iterations={iterations}\n'"#
    );
    let started = Instant::now();
    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-generic-032-throughput",
        "bash",
        &["-c", &script],
        HashMap::new(),
        Duration::from_secs(timeout_seconds),
    );
    let elapsed = started.elapsed();
    let progress = guest_filesystem_call(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(
            GuestFilesystemOperation::ReadFile,
            "/mnt/results/generic-032-progress",
        ),
    )
    .content;
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "generic/032 focused workload exceeded {timeout_seconds}s after {}ms; stdout: {:?}; stderr: {:?}; progress: {:?}",
            elapsed.as_millis(),
            output.stdout,
            output.stderr,
            progress
        )
    });
    eprintln!(
        "xfstests generic/032 throughput: iterations={iterations} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stderr, "", "stdout: {stdout}");
    assert_eq!(
        stdout.lines().last(),
        Some(format!("generic-032-focused-ok iterations={iterations}").as_str()),
        "stderr: {stderr}\nstdout: {stdout}"
    );
    assert!(!cleanup_path.exists(), "focused root was not removed");
}

#[test]
#[ignore = "focused scaled generic/014 workload; run explicitly with staged xfstests"]
fn xfstests_truncfile_scaled_throughput_regression() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let backend = env::var("XFSTESTS_TRUNCFILE_BACKEND").unwrap_or_else(|_| String::from("memory"));
    let iterations = env::var("XFSTESTS_TRUNCFILE_ITERATIONS")
        .unwrap_or_else(|_| String::from("1000"))
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=10_000).contains(value))
        .expect("XFSTESTS_TRUNCFILE_ITERATIONS must be in 1..=10000");
    let timeout_seconds = env::var("XFSTESTS_TRUNCFILE_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| String::from("30"))
        .parse::<u64>()
        .ok()
        .filter(|value| (1..=300).contains(value))
        .expect("XFSTESTS_TRUNCFILE_TIMEOUT_SECONDS must be in 1..=300");
    let seed = env::var("XFSTESTS_TRUNCFILE_SEED")
        .unwrap_or_else(|_| String::from("1"))
        .parse::<u32>()
        .ok()
        .filter(|value| *value <= i32::MAX as u32)
        .expect("XFSTESTS_TRUNCFILE_SEED must be in 0..=2147483647");
    let mount = env::var("XFSTESTS_TRUNCFILE_MOUNT").unwrap_or_else(|_| String::from("scratch"));
    assert!(
        matches!(mount.as_str(), "test" | "scratch"),
        "XFSTESTS_TRUNCFILE_MOUNT must be test or scratch"
    );
    let root = TestRoot::new("xfstests-truncfile-throughput");
    let cleanup_path = root.path().to_path_buf();
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create truncfile throughput directory");
    }

    let s3_server =
        matches!(backend.as_str(), "chunked_s3" | "object_s3").then(MockS3Server::start);
    let s3_endpoint = s3_server.as_ref().map(MockS3Server::base_url);
    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-truncfile-throughput");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), &backend, s3_endpoint);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &source.join("src"),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let iterations_arg = iterations.to_string();
    let seed_arg = seed.to_string();
    let target_path = format!("/mnt/{mount}/truncfile-probe");
    let started = Instant::now();
    let output = try_execute_command_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-truncfile-throughput",
        "truncfile",
        &["-s", &seed_arg, "-c", &iterations_arg, &target_path],
        HashMap::new(),
        Duration::from_secs(timeout_seconds),
    );
    let elapsed = started.elapsed();
    let stat = guest_filesystem_call(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Stat, &target_path),
    )
    .stat;
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    drop(sidecar);
    drop(root);

    let (stdout, stderr, exit_code) = output.unwrap_or_else(|output| {
        panic!(
            "scaled truncfile exceeded {timeout_seconds}s after {}ms; stdout: {:?}; stderr: {:?}",
            elapsed.as_millis(),
            output.stdout,
            output.stderr
        )
    });
    eprintln!(
        "xfstests truncfile throughput: backend={backend} mount={mount} seed={seed} iterations={iterations} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout,
        format!("Seed = {seed} (use \"-s {seed}\" to re-execute this test)\n"),
        "stderr: {stderr}"
    );
    let stat = stat.expect("truncfile output must exist");
    assert_eq!(stat.mode & 0o170000, 0o100000);
    assert!(stat.size <= 256 * 1024 * 1024);
    assert!(
        !cleanup_path.exists(),
        "truncfile throughput root was not removed"
    );
}

#[test]
#[ignore = "WASI ports for pinned xfstests C helpers; run through the xfstests Makefile"]
fn xfstests_wasi_helper_ports() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let root = TestRoot::new("xfstests-helper-ports");
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create helper probe backing directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, "conn-xfstests-helper-ports");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    let mut mounts = xfstests_mounts(&source, root.path(), XFSTESTS_BACKEND, None);
    mounts.push(host_dir_mount(
        &format!("/__agentos/commands/{}", COMMAND_PACKAGES.len()),
        &c_probe_root(),
        true,
    ));
    configure_mounts(&mut sidecar, &connection_id, &session_id, &vm_id, mounts);

    let (setup_stdout, setup_stderr, setup_exit) = execute_command(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-helper-setup",
        "sh",
        &[
            "-c",
            "set -e; rm -rf /mnt/test/helper-probe; mkdir /mnt/test/helper-probe; chmod 0777 /mnt/test/helper-probe",
        ],
    );
    assert_eq!(
        setup_exit, 0,
        "helper setup stdout: {setup_stdout}\nstderr: {setup_stderr}"
    );
    let setup_stat = guest_filesystem_call(
        &mut sidecar,
        40,
        &connection_id,
        &session_id,
        &vm_id,
        filesystem_request(GuestFilesystemOperation::Stat, "/mnt/test/helper-probe"),
    )
    .stat
    .expect("guest mkdir must create helper directory in kernel backing");
    assert!(setup_stat.is_directory);

    let script = r#"
set -e
cd /mnt/test/helper-probe
/opt/xfstests/src/permname -c 2 -l 2 -p 2
test "$(find . -type f | wc -l)" = 4
/opt/xfstests/src/runas -u 1000 -g 1000 -- sh -c 'test "$(id -u)" = 1000; touch owned'
test "$(stat -c %u owned)" = 1000
touch -d @946684800 owned
test "$(stat -c %Y owned)" = 946684800
/opt/xfstests/src/lstat64 owned >/dev/null
cp /opt/xfstests/src/testx testx.file
/opt/xfstests/src/fs_perms 600 99 99 100 99 t 0 >/dev/null
/opt/xfstests/src/fs_perms 001 99 99 12 100 x 1 >/dev/null
if /opt/xfstests/src/fs_perms 000 99 99 99 99 x 1 >/dev/null; then exit 20; fi
printf '#!/bin/sh\necho acl-exec-ok\n' > acl-script
chmod 755 "$PWD/acl-script"
chown 1000:1000 acl-script
chacl u::r-x,g::---,o::--- acl-script
/opt/xfstests/src/runas -u 1000 -g 1000 -- ./acl-script | grep -qx acl-exec-ok
chacl u::---,g::---,o::--- acl-script
if /opt/xfstests/src/runas -u 1000 -g 1000 -- ./acl-script >/dev/null 2>&1; then exit 21; fi
mkdir acl-default
chacl -b u::rwx,g::rwx,o::rwx u::r-x,g::r--,o::--- acl-default
touch acl-default/inherited
test "$(stat -c %a acl-default/inherited)" = 440
test "$(ls -ldn acl-default/inherited | awk '{print $3 ":" $4}')" = 0:0
mkdir -p acl-recursive/child
touch acl-recursive/child/file
chacl -r u::rwx,g::-w-,o::--x acl-recursive
chacl -l acl-recursive/child/file | grep -q '\[u::rwx,g::-w-,o::--x\]'
if chacl u acl-script 2>acl-invalid; then exit 22; fi
grep -qx 'chacl: u - Invalid argument' acl-invalid
printf 'set -x\ntrue\nset +x\n' > xtrace-script
sh xtrace-script 2>xtrace.out
grep -Fqx '+ true' xtrace.out
grep -Fqx '+ set +x' xtrace.out
/opt/xfstests/src/truncfile -l 4096 -b 512 -c 4 truncated >/dev/null
test -f truncated
/opt/xfstests/src/writev_on_pagefault writev-file | grep -qx 'wrote 3 bytes'
test "$(stat -c %s writev-file)" = 3
printf 'one\ntwo\nthree\n' > names
/opt/xfstests/src/nametest -l names -s 1 -i 300 -z >/dev/null
/opt/xfstests/src/fill fill-copy fill-copy 10
cp fill-copy fill-copy.0
diff fill-copy fill-copy.0
cmp fill-copy fill-copy.0
cmp -s fill-copy fill-copy.0
cmp -n 4 fill-copy fill-copy.0 1 1
dd if=/dev/zero of=cmp-large bs=1048576 count=1 status=none
cp cmp-large cmp-large.0
cmp cmp-large cmp-large.0
printf 'cp fill-copy fill-copy.pipe\ndiff fill-copy fill-copy.pipe\n' | tee /mnt/results/copy.commands | sh
printf 'cmp fill-copy fill-copy.pipe\n' | tee /mnt/results/cmp.commands | sh
/opt/xfstests/src/t_access_root access-root 0 1000 1000 >/dev/null
printf old > rename-old
printf new > rename-new
/opt/xfstests/src/t_rename_overwrite rename-old rename-new
touch rename-empty-old rename-empty-new
/opt/xfstests/src/t_rename_overwrite rename-empty-old rename-empty-new
mkdir rename-dir-old rename-dir-new
/opt/xfstests/src/t_rename_overwrite rename-dir-old rename-dir-new
rename_dir=/mnt/test/$$
mkdir -p "$rename_dir"
touch "$rename_dir/file1" "$rename_dir/file2"
/opt/xfstests/src/t_rename_overwrite "$rename_dir/file1" "$rename_dir/file2"
rm "$rename_dir/file2"
mkdir "$rename_dir/dir1" "$rename_dir/dir2"
/opt/xfstests/src/t_rename_overwrite "$rename_dir/dir1" "$rename_dir/dir2"
rmdir "$rename_dir/dir2" "$rename_dir"
bash -c '
set -e
rename_dir=/mnt/test/nested-$$
mkdir -p "$rename_dir"
touch "$rename_dir/file1" "$rename_dir/file2"
/opt/xfstests/src/t_rename_overwrite "$rename_dir/file1" "$rename_dir/file2"
rm "$rename_dir/file2"
mkdir "$rename_dir/dir1" "$rename_dir/dir2"
/opt/xfstests/src/t_rename_overwrite "$rename_dir/dir1" "$rename_dir/dir2"
rmdir "$rename_dir/dir2" "$rename_dir"
'
mkdir sum-dir
printf payload > sum-dir/file
/opt/xfstests/src/fssum -f -w /mnt/results/fssum.manifest sum-dir
/opt/xfstests/src/fssum -r /mnt/results/fssum.manifest sum-dir >/dev/null
test "$PWD" = /mnt/test/helper-probe
test -d .
touch multi-open-cwd-probe
rm multi-open-cwd-probe
/opt/xfstests/src/multi_open_unlink -f unlinked -n 2 -s 0 -F -S
test ! -e unlinked.1
test ! -e unlinked.2
/opt/xfstests/src/looptest -i 4 -r -w -b 256 -s looptest.file
test "$(stat -c %s looptest.file)" = 1024
/opt/xfstests/src/devzero -c -t -b 1 -n 2 devzero.file >/dev/null
test "$(stat -c %s devzero.file)" = 1024
printf cccccccccc > mmap-source
/opt/xfstests/src/t_mmap_writev mmap-source mmap-copy
printf 'aaaaaaaaaabbbbbbbbbbcccccccccc' | cmp - mmap-copy
/opt/xfstests/src/pwrite_mmap_blocked mmap-pwrite | grep -qx 'pwrite 1 bytes from 2 to 3'
test "$(cat mmap-pwrite)" = 01224
mkdir getcwd-dir
/opt/xfstests/src/t_getcwd "$PWD/getcwd-dir"
touch feature-chown
/opt/xfstests/src/feature -c feature-chown
test "$(stat -c %u:%g feature-chown)" = 98789:98789
test "$(/opt/xfstests/src/feature -s)" -gt 0
test "$(/opt/xfstests/src/feature -w)" = 32
test "$(/opt/xfstests/src/feature -o)" -gt 0
if /opt/xfstests/src/feature -A; then exit 41; fi
mkdir dtype
touch dtype/file
mkdir dtype/dir
ln -s file dtype/link
/opt/xfstests/src/t_dir_type dtype > dtype.out
grep -qx 'file f' dtype.out
grep -qx 'dir d' dtype.out
grep -qx 'link l' dtype.out
printf 'permname=4\nrunas_uid=1000\nlstat64=ok\nfs_perms=ok\nacl_tools=ok\nbrush_xtrace=ok\ntruncfile=ok\nnametest=ok\nt_access_root=ok\nt_rename_overwrite=ok\nfssum=ok\nmulti_open_unlink=ok\nlooptest=ok\ndevzero=ok\nt_mmap_writev=ok\npwrite_mmap_blocked=ok\nt_getcwd=ok\nfeature=ok\nt_dir_type=ok\n'
flock_test selftest flock.file
"#;
    let mut guest_env = HashMap::new();
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    let trace_shell = env::var_os("XFSTESTS_TRACE_SHELL").is_some();
    let shell_args = if trace_shell {
        vec!["-x", "-c", script]
    } else {
        vec!["-c", script]
    };
    let helper_timeout_seconds = env::var("XFSTESTS_HELPER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| String::from("120"))
        .parse::<u64>()
        .ok()
        .filter(|seconds| (1..=300).contains(seconds))
        .expect("XFSTESTS_HELPER_TIMEOUT_SECONDS must be in 1..=300");
    let (stdout, stderr, exit_code) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "xfstests-wasi-helper-ports",
        "sh",
        &shell_args,
        guest_env,
        Duration::from_secs(helper_timeout_seconds),
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout,
        "permname=4\nrunas_uid=1000\nlstat64=ok\nfs_perms=ok\nacl_tools=ok\nbrush_xtrace=ok\ntruncfile=ok\nnametest=ok\nt_access_root=ok\nt_rename_overwrite=ok\nfssum=ok\nmulti_open_unlink=ok\nlooptest=ok\ndevzero=ok\nt_mmap_writev=ok\npwrite_mmap_blocked=ok\nt_getcwd=ok\nfeature=ok\nt_dir_type=ok\nflock=ok\n",
        "stderr: {stderr}"
    );
    if !trace_shell {
        assert!(
            stderr.lines().all(|line| line.starts_with("alpha size = ")),
            "unexpected helper stderr: {stderr}"
        );
    }
}

fn run_xfstest(source: &Path, test_id: &str, backend: &str, root_path: PathBuf) -> TestOutcome {
    let root = TestRoot::at(root_path);
    let s3_server = matches!(backend, "chunked_s3" | "object_s3").then(MockS3Server::start);
    let s3_endpoint = s3_server.as_ref().map(MockS3Server::base_url);
    let cwd = root.path().join("cwd");
    for path in [
        cwd.as_path(),
        &root.path().join("test"),
        &root.path().join("scratch"),
        &root.path().join("results"),
    ] {
        fs::create_dir_all(path).expect("create per-test backing directory");
    }

    let mut sidecar = test_sidecar(root.path());
    let connection_id = authenticate_wire(&mut sidecar, &format!("conn-{test_id}"));
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_xfstests_vm_wire(&mut sidecar, 3, &connection_id, &session_id, &cwd);
    configure_mounts(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        xfstests_mounts(source, root.path(), backend, s3_endpoint),
    );

    let probe = "for c in bash awk sed grep diff df find mount; do command -v \"$c\" >/dev/null || { echo missing:$c >&2; exit 127; }; done";
    let (probe_stdout, probe_stderr, probe_exit) = execute_command_with_env(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("xfstests-probe-{}", test_id.replace('/', "-")),
        "bash",
        &["-c", probe],
        HashMap::new(),
        Duration::from_secs(30),
    );
    if probe_exit != 0 {
        dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
        return TestOutcome::harness(
            test_id,
            backend,
            format!(
                "bootstrap probe exited {probe_exit}: stdout={probe_stdout:?} stderr={probe_stderr:?}"
            ),
        );
    }

    let (pipeline_stdout, pipeline_stderr, pipeline_exit) = execute_command_with_env(
        &mut sidecar,
        51,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("xfstests-pipeline-probe-{}", test_id.replace('/', "-")),
        "bash",
        &[
            "-c",
            "printf '# skip\\nsmall\\nbig\\nsub/small\\n' | sed -e '/^#/d' | while read value; do printf '<%s>\\n' \"$value\"; done",
        ],
        HashMap::new(),
        Duration::from_secs(30),
    );
    if pipeline_exit != 0 || pipeline_stdout != "<small>\n<big>\n<sub/small>\n" {
        dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
        return TestOutcome::harness(
            test_id,
            backend,
            format!(
                "shell pipeline probe exited {pipeline_exit}: stdout={pipeline_stdout:?} stderr={pipeline_stderr:?}"
            ),
        );
    }

    let check_command = if env::var_os("XFSTESTS_TRACE_SHELL").is_some() {
        "bash -x ./check"
    } else {
        "./check"
    };
    let check_options = if env::var_os("XFSTESTS_DUMP_OUTPUT").is_some() {
        "-d "
    } else {
        ""
    };
    let command = format!(
        "mkdir -p /mnt/results/.tmp && cd /opt/xfstests && {check_command} {check_options}{test_id}"
    );
    let mut guest_env = HashMap::new();
    guest_env.insert(String::from("FSTYP"), String::from("agentos"));
    guest_env.insert(String::from("TMPDIR"), String::from("/mnt/results/.tmp"));
    guest_env.insert(
        String::from("HOST_OPTIONS"),
        String::from("/opt/xfstests/local.config"),
    );
    if let Some(iterations) = env::var_os("XFSTESTS_WORKER_REDUCED_ITERATIONS") {
        assert_eq!(test_id, "generic/014");
        assert_eq!(backend, "object_s3");
        guest_env.insert(
            String::from("XFSTESTS_GENERIC_014_ITERATIONS"),
            iterations
                .into_string()
                .expect("reduced iteration count must be UTF-8"),
        );
    }
    if env::var_os("XFSTESTS_TRACE_HOST_PROCESS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_HOST_PROCESS"),
            String::from("1"),
        );
    }
    if env::var_os("XFSTESTS_TRACE_FD_FILESTAT").is_some() {
        guest_env.insert(String::from("AGENTOS_TRACE_FD_FILESTAT"), String::from("1"));
    }
    if env::var_os("XFSTESTS_TRACE_FD_SEEK").is_some() {
        guest_env.insert(String::from("AGENTOS_TRACE_FD_SEEK"), String::from("1"));
    }
    if env::var_os("XFSTESTS_TRACE_GUEST_FILE_ERRORS").is_some() {
        guest_env.insert(
            String::from("AGENTOS_TRACE_GUEST_FILE_ERRORS"),
            String::from("1"),
        );
    }
    let timeout = xfstest_timeout();
    let run_result = try_execute_command_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("xfstests-run-{}", test_id.replace('/', "-")),
        "bash",
        &["-c", &command],
        guest_env,
        timeout,
    );
    let (stdout, mut stderr, exit_code) = match run_result {
        Ok(output) => output,
        Err(output) => {
            let mut timeout_stderr = output.stderr;
            let relative = test_id
                .strip_prefix("generic/")
                .expect("validated generic test id");
            let full_path = format!("/mnt/results/generic/{relative}.full");
            let full_tail_command = format!("tail -c 65536 -- {full_path} 2>/dev/null || true");
            let (full_tail, full_tail_stderr, _) = execute_command_with_env(
                &mut sidecar,
                13,
                &connection_id,
                &session_id,
                &vm_id,
                &format!("xfstests-timeout-full-tail-{}", test_id.replace('/', "-")),
                "bash",
                &["-c", &full_tail_command],
                HashMap::new(),
                Duration::from_secs(30),
            );
            if !full_tail.is_empty() {
                timeout_stderr
                    .push_str("\n--- xfstests .full tail at timeout (max 65536 bytes) ---\n");
                timeout_stderr.push_str(&full_tail);
            }
            if !full_tail_stderr.is_empty() {
                timeout_stderr.push_str("\n--- xfstests timeout collection stderr ---\n");
                timeout_stderr.push_str(&full_tail_stderr);
            }
            let private_output_command =
                "for file in /tmp/*.out; do [ -f \"$file\" ] || continue; tail -c 65536 -- \"$file\"; break; done";
            let (private_output, private_output_stderr, _) = execute_command_with_env(
                &mut sidecar,
                14,
                &connection_id,
                &session_id,
                &vm_id,
                &format!(
                    "xfstests-timeout-private-output-{}",
                    test_id.replace('/', "-")
                ),
                "bash",
                &["-c", private_output_command],
                HashMap::new(),
                Duration::from_secs(30),
            );
            if !private_output.is_empty() {
                timeout_stderr.push_str(
                    "\n--- xfstests private output tail at timeout (max 65536 bytes) ---\n",
                );
                timeout_stderr.push_str(&private_output);
            }
            if !private_output_stderr.is_empty() {
                timeout_stderr.push_str("\n--- xfstests private-output collection stderr ---\n");
                timeout_stderr.push_str(&private_output_stderr);
            }
            let progress = (test_id == "generic/129").then(|| {
                (1..=4)
                    .map(|index| {
                        let path = format!("/mnt/scratch/looptest/looptest{index}.tst");
                        let exists = guest_filesystem_call(
                            &mut sidecar,
                            60 + i64::from(index),
                            &connection_id,
                            &session_id,
                            &vm_id,
                            filesystem_request(GuestFilesystemOperation::Exists, &path),
                        )
                        .exists
                            == Some(true);
                        let size = exists.then(|| {
                            guest_filesystem_call(
                                &mut sidecar,
                                70 + i64::from(index),
                                &connection_id,
                                &session_id,
                                &vm_id,
                                filesystem_request(GuestFilesystemOperation::Stat, &path),
                            )
                            .stat
                            .map(|stat| stat.size)
                        });
                        (path, size.flatten())
                    })
                    .collect::<Vec<_>>()
            });
            let s3_request_summary = s3_server.as_ref().map(|server| {
                let requests = server.requests();
                let tail = requests.iter().rev().take(16).cloned().collect::<Vec<_>>();
                (requests.len(), tail)
            });
            dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
            return TestOutcome {
                id: test_id.to_owned(),
                backend: backend.to_owned(),
                kind: OutcomeKind::Harness {
                    reason: format!(
                        "check timed out after {} seconds; stdout bytes: {}; stderr bytes: {}; progress: {:?}; s3 requests (count, newest first): {:?}",
                        timeout.as_secs(),
                        output.stdout.len(),
                        timeout_stderr.len(),
                        progress,
                        s3_request_summary
                    ),
                },
                stdout: output.stdout,
                stderr: timeout_stderr,
            };
        }
    };

    let relative = test_id
        .strip_prefix("generic/")
        .expect("validated generic test id");
    if exit_code != 0 {
        let full_path = format!("/mnt/results/generic/{relative}.full");
        let full_tail_command = format!("tail -c 65536 -- {full_path} 2>/dev/null || true");
        let (full_tail, full_tail_stderr, _) = execute_command_with_env(
            &mut sidecar,
            13,
            &connection_id,
            &session_id,
            &vm_id,
            &format!("xfstests-full-tail-{}", test_id.replace('/', "-")),
            "bash",
            &["-c", &full_tail_command],
            HashMap::new(),
            Duration::from_secs(30),
        );
        if !full_tail.is_empty() {
            stderr.push_str("\n--- xfstests .full tail (max 65536 bytes) ---\n");
            stderr.push_str(&full_tail);
        }
        if !full_tail_stderr.is_empty() {
            stderr.push_str("\n--- xfstests .full collection stderr ---\n");
            stderr.push_str(&full_tail_stderr);
        }
    }
    let notrun = read_optional_text(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("/mnt/results/generic/{relative}.notrun"),
    );
    let bad = read_optional_text(
        &mut sidecar,
        9,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("/mnt/results/generic/{relative}.out.bad"),
    );
    let diagnostics = if exit_code != 0 && notrun.is_none() && bad.is_none() {
        let (diag_stdout, diag_stderr, diag_exit) = execute_command_with_env(
            &mut sidecar,
            11,
            &connection_id,
            &session_id,
            &vm_id,
            &format!("xfstests-diag-{}", test_id.replace('/', "-")),
            "bash",
            &[
                "-c",
                "cd /opt/xfstests && stat -c '%f %a %F' ./check && head -1 ./check && bash -n ./check",
            ],
            HashMap::new(),
            Duration::from_secs(30),
        );
        Some(format!(
            "diagnostic_exit={diag_exit} stdout={diag_stdout:?} stderr={diag_stderr:?}"
        ))
    } else {
        None
    };
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);

    let kind = if let Some(reason) = notrun {
        OutcomeKind::NotRun {
            reason: reason.trim().to_owned(),
        }
    } else if let Some(output) = bad {
        OutcomeKind::Fail {
            digest: normalized_digest(&output),
            output,
        }
    } else if exit_code != 0 {
        OutcomeKind::Harness {
            reason: format!(
                "check exited {exit_code} without a notrun or out.bad artifact; {}",
                diagnostics.as_deref().unwrap_or("diagnostics unavailable")
            ),
        }
    } else if xfstests_output_reports_failure(test_id, &stdout, &stderr) {
        OutcomeKind::Harness {
            reason: format!(
                "check reported a failure but produced no readable out.bad artifact: stdout={stdout:?} stderr={stderr:?}"
            ),
        }
    } else if !stdout.contains(test_id) {
        OutcomeKind::Harness {
            reason: format!(
                "check produced no result row for the selected test: stdout={stdout:?} stderr={stderr:?}"
            ),
        }
    } else {
        OutcomeKind::Pass
    };
    TestOutcome {
        id: test_id.to_owned(),
        backend: backend.to_owned(),
        kind,
        stdout,
        stderr,
    }
}

fn bounded_stream_tail<R: Read>(mut reader: R, max_bytes: usize) -> String {
    let mut tail = VecDeque::with_capacity(max_bytes);
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let message = format!("\n[worker stream read failed: {error}]\n");
                for byte in message.bytes() {
                    if tail.len() == max_bytes {
                        tail.pop_front();
                        truncated = true;
                    }
                    tail.push_back(byte);
                }
                break;
            }
        };
        for byte in &chunk[..count] {
            if tail.len() == max_bytes {
                tail.pop_front();
                truncated = true;
            }
            tail.push_back(*byte);
        }
    }
    let bytes = tail.into_iter().collect::<Vec<_>>();
    format!(
        "{}{}",
        if truncated {
            "[earlier worker output truncated]\n"
        } else {
            ""
        },
        String::from_utf8_lossy(&bytes)
    )
}

fn collect_worker_output(
    stdout: thread::JoinHandle<String>,
    stderr: thread::JoinHandle<String>,
) -> String {
    let stdout = stdout
        .join()
        .unwrap_or_else(|_| String::from("[worker stdout reader panicked]\n"));
    let stderr = stderr
        .join()
        .unwrap_or_else(|_| String::from("[worker stderr reader panicked]\n"));
    format!("--- worker stdout ---\n{stdout}\n--- worker stderr ---\n{stderr}")
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    format!("non-string panic payload ({:?})", payload.type_id())
}

fn run_xfstest_subprocess(
    source: &Path,
    test_id: &str,
    backend: &str,
    temp_budget: &Arc<TempUsageBudget>,
    reduction: Option<&ExceptionRecord>,
) -> TestOutcome {
    const WORKER_GRACE: Duration = Duration::from_secs(45);
    const TEMP_USAGE_POLL: Duration = Duration::from_secs(1);
    const MAX_RESULT_BYTES: u64 = 32 * 1024 * 1024;
    const MAX_WORKER_STREAM_BYTES: usize = 32 * 1024;

    let workspace = TestRoot::new(&format!(
        "xfstests-worker-{backend}-{}",
        test_id.replace('/', "-")
    ));
    let mut temp_reservation = match temp_budget.observe(workspace.path()) {
        Ok(reservation) => reservation,
        Err(error) => return TestOutcome::harness(test_id, backend, error),
    };
    let root = workspace.path().join("test-root");
    fs::create_dir_all(&root).expect("create xfstests worker root");
    let result_workspace_name = format!(
        "xfstests-worker-result-{backend}-{}",
        test_id.replace('/', "-")
    );
    let configured_run_root = env::var_os("XFSTESTS_TEMP_ROOT").map(PathBuf::from);
    let run_root = configured_run_root
        .clone()
        .unwrap_or_else(|| workspace.path().to_path_buf());
    let result_workspace_path = configured_run_root
        .map(|root| root.join(&result_workspace_name))
        .unwrap_or_else(|| support::temp_dir(&result_workspace_name));
    if !result_workspace_path.exists() {
        fs::create_dir(&result_workspace_path).expect("create xfstests worker result root");
    }
    let result_workspace = TestRoot::at(result_workspace_path);
    let result_path = result_workspace.path().join("outcome.json");
    if let Err(error) =
        refresh_xfstests_temp_roots(&[&run_root, workspace.path(), result_workspace.path()])
    {
        return TestOutcome::harness(test_id, backend, error);
    }
    let mut command = Command::new(env::current_exe().expect("xfstests test executable"));
    command
        .args([
            "--ignored",
            "--exact",
            "xfstests_single_case_worker",
            "--nocapture",
        ])
        .env("XFSTESTS_ROOT", source)
        .env("XFSTESTS_WORKER_TEST_ID", test_id)
        .env("XFSTESTS_WORKER_BACKEND", backend)
        .env("XFSTESTS_WORKER_ROOT", &root)
        .env("XFSTESTS_WORKER_RESULT", &result_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(reduction) = reduction {
        assert_eq!(reduction.disposition, "reduced");
        assert_eq!(reduction.reduction.as_deref(), Some("truncfile-iterations"));
        assert_eq!(test_id, "generic/014");
        command.env(
            "XFSTESTS_WORKER_REDUCED_ITERATIONS",
            reduction
                .reduced_iterations
                .expect("validated reduced_iterations")
                .to_string(),
        );
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return TestOutcome::harness(
                test_id,
                backend,
                format!("failed to spawn test worker: {error}"),
            );
        }
    };
    let stdout_reader = child.stdout.take().expect("piped xfstests worker stdout");
    let stderr_reader = child.stderr.take().expect("piped xfstests worker stderr");
    let stdout_log =
        thread::spawn(move || bounded_stream_tail(stdout_reader, MAX_WORKER_STREAM_BYTES));
    let stderr_log =
        thread::spawn(move || bounded_stream_tail(stderr_reader, MAX_WORKER_STREAM_BYTES));

    let deadline = Instant::now() + xfstest_timeout() + WORKER_GRACE;
    let mut next_temp_usage_poll = Instant::now();
    let status = loop {
        if Instant::now() >= next_temp_usage_poll {
            if let Err(error) =
                refresh_xfstests_temp_roots(&[&run_root, workspace.path(), result_workspace.path()])
            {
                let _ = child.kill();
                let _ = child.wait();
                return TestOutcome {
                    id: test_id.to_owned(),
                    backend: backend.to_owned(),
                    kind: OutcomeKind::Harness { reason: error },
                    stdout: String::new(),
                    stderr: collect_worker_output(stdout_log, stderr_log),
                };
            }
            if let Err(error) = temp_reservation.refresh(workspace.path()) {
                let _ = child.kill();
                let _ = child.wait();
                return TestOutcome {
                    id: test_id.to_owned(),
                    backend: backend.to_owned(),
                    kind: OutcomeKind::Harness { reason: error },
                    stdout: String::new(),
                    stderr: collect_worker_output(stdout_log, stderr_log),
                };
            }
            next_temp_usage_poll = Instant::now() + TEMP_USAGE_POLL;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return TestOutcome {
                    id: test_id.to_owned(),
                    backend: backend.to_owned(),
                    kind: OutcomeKind::Harness {
                        reason: format!(
                            "OS worker watchdog expired after {} seconds",
                            (xfstest_timeout() + WORKER_GRACE).as_secs()
                        ),
                    },
                    stdout: String::new(),
                    stderr: collect_worker_output(stdout_log, stderr_log),
                };
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return TestOutcome {
                    id: test_id.to_owned(),
                    backend: backend.to_owned(),
                    kind: OutcomeKind::Harness {
                        reason: format!("failed to poll test worker: {error}"),
                    },
                    stdout: String::new(),
                    stderr: collect_worker_output(stdout_log, stderr_log),
                };
            }
        }
    };

    let Some(status) = status else {
        unreachable!("worker loop only exits with a status")
    };
    let worker_output = collect_worker_output(stdout_log, stderr_log);
    let result_length = fs::metadata(&result_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if !status.success() || result_length == 0 || result_length > MAX_RESULT_BYTES {
        return TestOutcome {
            id: test_id.to_owned(),
            backend: backend.to_owned(),
            kind: OutcomeKind::Harness {
                reason: format!(
                    "worker exited with {status}; result bytes={result_length}, maximum={MAX_RESULT_BYTES}"
                ),
            },
            stdout: String::new(),
            stderr: worker_output,
        };
    }
    match serde_json::from_slice::<TestOutcome>(
        &fs::read(&result_path).expect("read bounded xfstests worker result"),
    ) {
        Ok(outcome) => outcome,
        Err(error) => TestOutcome {
            id: test_id.to_owned(),
            backend: backend.to_owned(),
            kind: OutcomeKind::Harness {
                reason: format!("worker returned invalid outcome JSON: {error}"),
            },
            stdout: String::new(),
            stderr: worker_output,
        },
    }
}

#[test]
#[ignore = "internal per-case subprocess; matrix owns invocation"]
fn xfstests_single_case_worker() {
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from parent"));
    let test_id = env::var("XFSTESTS_WORKER_TEST_ID").expect("worker test id from parent");
    let backend = env::var("XFSTESTS_WORKER_BACKEND").expect("worker backend from parent");
    let root = PathBuf::from(env::var("XFSTESTS_WORKER_ROOT").expect("worker root from parent"));
    let result_path =
        PathBuf::from(env::var("XFSTESTS_WORKER_RESULT").expect("worker result from parent"));
    if env::var_os("XFSTESTS_WORKER_FORCE_HANG").is_some() {
        loop {
            thread::park();
        }
    }
    let outcome = match catch_unwind(AssertUnwindSafe(|| {
        if env::var_os("XFSTESTS_WORKER_FORCE_WORKSPACE_LOSS").is_some() {
            fs::remove_dir_all(root.parent().expect("worker root has workspace parent"))
                .expect("remove forced-loss worker workspace");
            panic!("forced xfstests worker workspace loss");
        }
        run_xfstest(&source, &test_id, &backend, root)
    })) {
        Ok(outcome) => outcome,
        Err(payload) => TestOutcome::harness(
            &test_id,
            &backend,
            format!(
                "single-case worker panicked: {}",
                panic_payload_message(payload.as_ref())
            ),
        ),
    };
    fs::write(
        result_path,
        serde_json::to_vec(&outcome).expect("serialize xfstests worker outcome"),
    )
    .expect("write xfstests worker outcome");
}

fn quick_tests(source: &Path) -> Vec<String> {
    let group_list = fs::read_to_string(source.join("tests/generic/group.list"))
        .expect("staged generic/group.list");
    let mut selected = group_list
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields
                .contains(&"quick")
                .then(|| format!("generic/{}", fields[0]))
        })
        .collect::<Vec<_>>();
    if let Ok(filter) = env::var("XFSTESTS_TESTS") {
        let requested = filter
            .split([',', ' ', '\n'])
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        selected.retain(|test_id| requested.contains(test_id));
        assert_eq!(
            selected.len(),
            requested.len(),
            "requested tests must be in generic/quick"
        );
    }
    selected
}

fn load_exception_records(source: &Path) -> Vec<ExceptionRecord> {
    serde_json::from_slice(
        &fs::read(source.join("agentos-exceptions.json")).expect("generated exception JSON"),
    )
    .expect("validated exception JSON")
}

fn apply_exception(outcome: &mut TestOutcome, exception: &ExceptionRecord) -> bool {
    match (&outcome.kind, exception.disposition.as_str()) {
        (OutcomeKind::NotRun { reason }, "allowed-notrun")
            if exception.notrun_reason.as_deref() == Some(reason) =>
        {
            outcome.kind = OutcomeKind::AllowedNotRun {
                reason: exception_detail(exception),
            };
            true
        }
        (OutcomeKind::Fail { digest, .. }, "expected-failure")
            if exception.output_digest.as_deref() == Some(digest) =>
        {
            outcome.kind = OutcomeKind::ExpectedFailure {
                reason: exception_detail(exception),
            };
            true
        }
        (OutcomeKind::Pass, "reduced") => {
            outcome.kind = OutcomeKind::ReducedPass {
                reason: exception_detail(exception),
            };
            true
        }
        (OutcomeKind::Pass, _) => {
            outcome.kind = OutcomeKind::UnexpectedPass {
                reason: exception_detail(exception),
            };
            true
        }
        _ => false,
    }
}

fn exception_detail(exception: &ExceptionRecord) -> String {
    let mut detail = exception.reason.clone();
    if let Some(classification) = &exception.classification {
        detail.push_str(&format!("; classification={classification}"));
    }
    if let Some(issue) = &exception.tracking_issue {
        detail.push_str(&format!("; tracking={issue}"));
    }
    if exception.disposition == "reduced" {
        detail.push_str(&format!(
            "; reduction={}; iterations={}/{}; focused_coverage={}",
            exception.reduction.as_deref().unwrap_or("missing"),
            exception.reduced_iterations.unwrap_or(0),
            exception.full_iterations.unwrap_or(0),
            exception.focused_coverage.as_deref().unwrap_or("missing")
        ));
    }
    detail
}

fn outcome_satisfies_strict_policy(kind: &OutcomeKind) -> bool {
    matches!(
        kind,
        OutcomeKind::Pass
            | OutcomeKind::AllowedNotRun { .. }
            | OutcomeKind::Deferred { .. }
            | OutcomeKind::Excluded { .. }
            | OutcomeKind::ExpectedFailure { .. }
            | OutcomeKind::ReducedPass { .. }
    )
}

#[cfg(test)]
mod exception_policy_tests {
    use super::*;

    #[test]
    fn worker_stream_tail_is_bounded_and_marks_truncation() {
        let output = bounded_stream_tail(std::io::Cursor::new(b"abcdef"), 4);
        assert_eq!(output, "[earlier worker output truncated]\ncdef");
    }

    #[test]
    fn worker_panic_payload_preserves_string_messages() {
        let borrowed: Box<dyn Any + Send> = Box::new("borrowed panic");
        let owned: Box<dyn Any + Send> = Box::new(String::from("owned panic"));

        assert_eq!(panic_payload_message(borrowed.as_ref()), "borrowed panic");
        assert_eq!(panic_payload_message(owned.as_ref()), "owned panic");
    }

    fn allowed_notrun(reason: &str) -> ExceptionRecord {
        ExceptionRecord {
            id: String::from("generic/001"),
            backend: String::from(XFSTESTS_BACKEND),
            disposition: String::from("allowed-notrun"),
            reason: String::from("missing helper is tracked"),
            tracking_issue: Some(String::from("agentos#fs-helper")),
            notrun_reason: Some(reason.to_owned()),
            output_digest: None,
            classification: None,
            reduction: None,
            full_iterations: None,
            reduced_iterations: None,
            focused_coverage: None,
        }
    }

    #[test]
    fn exact_allowed_notrun_satisfies_strict_policy() {
        let mut outcome = TestOutcome {
            id: String::from("generic/001"),
            backend: String::from(XFSTESTS_BACKEND),
            kind: OutcomeKind::NotRun {
                reason: String::from("missing verify_fill"),
            },
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(apply_exception(
            &mut outcome,
            &allowed_notrun("missing verify_fill")
        ));
        assert!(outcome_satisfies_strict_policy(&outcome.kind));
    }

    #[test]
    fn mismatched_notrun_reason_remains_strict_failure() {
        let mut outcome = TestOutcome {
            id: String::from("generic/001"),
            backend: String::from(XFSTESTS_BACKEND),
            kind: OutcomeKind::NotRun {
                reason: String::from("different regression"),
            },
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!apply_exception(
            &mut outcome,
            &allowed_notrun("missing verify_fill")
        ));
        assert!(!outcome_satisfies_strict_policy(&outcome.kind));
    }

    #[test]
    fn reduced_test_must_pass_before_it_satisfies_strict_policy() {
        let reduction = ExceptionRecord {
            id: String::from("generic/014"),
            backend: String::from("object_s3"),
            disposition: String::from("reduced"),
            reason: String::from("whole-object repetition reduction"),
            tracking_issue: Some(String::from("ISSUES.md#f-014")),
            notrun_reason: None,
            output_digest: None,
            classification: None,
            reduction: Some(String::from("truncfile-iterations")),
            full_iterations: Some(10_000),
            reduced_iterations: Some(1_000),
            focused_coverage: Some(String::from("object sparse semantics")),
        };
        let mut passing = TestOutcome {
            id: reduction.id.clone(),
            backend: reduction.backend.clone(),
            kind: OutcomeKind::Pass,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(apply_exception(&mut passing, &reduction));
        assert!(matches!(passing.kind, OutcomeKind::ReducedPass { .. }));
        assert!(outcome_satisfies_strict_policy(&passing.kind));

        let mut failing = TestOutcome::harness(
            &reduction.id,
            &reduction.backend,
            "reduced execution failed",
        );
        assert!(!apply_exception(&mut failing, &reduction));
        assert!(!outcome_satisfies_strict_policy(&failing.kind));
    }
}

#[cfg(test)]
mod harness_bound_tests {
    use super::*;

    #[test]
    fn upstream_failure_markers_cannot_be_classified_as_pass() {
        let stdout = "generic/006       [failed, exit status 127]- output mismatch\n\
Failures: generic/006\n\
Failed 1 of 1 tests\n";
        let stderr = "error: command not found: rm\n";

        assert!(xfstests_output_reports_failure("generic/006", stdout, ""));
        assert!(xfstests_output_reports_failure(
            "generic/006",
            "generic/006 42s\n",
            stderr
        ));
        assert!(!xfstests_output_reports_failure(
            "generic/006",
            "generic/006 42s\nRan: generic/006\nPassed all 1 tests\n",
            ""
        ));
    }

    #[test]
    fn live_temp_root_refresh_renews_run_case_and_result_directories() {
        let run_root = TestRoot::at(support::temp_dir("xfstests-keepalive-run"));
        let case_root = run_root.path().join("case");
        fs::create_dir(&case_root).expect("create keepalive case root");
        let result_root = TestRoot::at(support::temp_dir("xfstests-keepalive-result"));
        let old = FileTime::from_unix_time(1, 0);
        for path in [run_root.path(), case_root.as_path(), result_root.path()] {
            set_file_times(path, old, old).expect("age xfstests temp root");
        }

        refresh_xfstests_temp_roots(&[run_root.path(), &case_root, result_root.path()])
            .expect("refresh live xfstests temp roots");

        for path in [run_root.path(), case_root.as_path(), result_root.path()] {
            let refreshed = FileTime::from_last_modification_time(
                &fs::metadata(path).expect("stat refreshed xfstests temp root"),
            );
            assert!(
                refreshed > old,
                "temp root was not refreshed: {}",
                path.display()
            );
        }
    }

    #[test]
    fn whole_object_case_timeout_remains_bounded() {
        assert_eq!(parse_xfstest_timeout_seconds("1"), Some(1));
        assert_eq!(parse_xfstest_timeout_seconds("7200"), Some(7200));
        assert_eq!(parse_xfstest_timeout_seconds("0"), None);
        assert_eq!(parse_xfstest_timeout_seconds("7201"), None);
        assert_eq!(parse_xfstest_timeout_seconds("invalid"), None);
    }

    #[test]
    fn worker_outcome_survives_case_workspace_loss() {
        let workspace = TestRoot::at(support::temp_dir("xfstests-worker-loss-case"));
        let root = workspace.path().join("test-root");
        fs::create_dir_all(&root).expect("create forced-loss worker root");
        let result_workspace = TestRoot::at(support::temp_dir("xfstests-worker-loss-result"));
        let result_path = result_workspace.path().join("outcome.json");

        let output = Command::new(env::current_exe().expect("xfstests test executable"))
            .args([
                "--ignored",
                "--exact",
                "xfstests_single_case_worker",
                "--nocapture",
            ])
            .env("XFSTESTS_ROOT", ".")
            .env("XFSTESTS_WORKER_TEST_ID", "generic/006")
            .env("XFSTESTS_WORKER_BACKEND", "object_s3")
            .env("XFSTESTS_WORKER_ROOT", &root)
            .env("XFSTESTS_WORKER_RESULT", &result_path)
            .env("XFSTESTS_WORKER_FORCE_WORKSPACE_LOSS", "1")
            .output()
            .expect("run forced-loss xfstests worker");

        assert!(
            output.status.success(),
            "worker failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let outcome: TestOutcome =
            serde_json::from_slice(&fs::read(&result_path).expect("read preserved worker outcome"))
                .expect("decode preserved worker outcome");
        assert!(matches!(
            outcome.kind,
            OutcomeKind::Harness { ref reason }
                if reason.contains("forced xfstests worker workspace loss")
        ));
        assert!(
            !workspace.path().exists(),
            "worker should have removed the disposable case workspace"
        );
    }

    #[test]
    fn backend_mounts_preserve_agentos_identity() {
        let backing = Path::new("/tmp/xfstests-backend-test");
        for backend in XFSTESTS_DEFAULT_BACKENDS {
            let mount = xfstests_backend_mount(
                backend,
                "/mnt/test",
                "/dev/agentos-test",
                backing,
                Some("http://127.0.0.1:1"),
            );
            assert_eq!(mount.guest_fstype, "agentos");
            assert_eq!(mount.guest_source, "/dev/agentos-test");
            assert_eq!(mount.plugin.id, *backend);
        }
    }

    #[test]
    fn temp_usage_budget_enforces_aggregate_usage_and_releases_reservations() {
        let first_root = TestRoot::new("xfstests-budget-first");
        let second_root = TestRoot::new("xfstests-budget-second");
        fs::write(first_root.path().join("first"), b"1234").expect("write first backing");
        fs::write(second_root.path().join("second"), b"12").expect("write second backing");

        let budget = Arc::new(TempUsageBudget::new(6));
        let first = budget
            .observe(first_root.path())
            .expect("reserve four bytes");
        let mut second = budget
            .observe(second_root.path())
            .expect("reserve aggregate six bytes");
        assert_eq!(*budget.used_bytes.lock().expect("budget lock"), 6);

        fs::write(second_root.path().join("second"), b"123").expect("grow second backing");
        let error = second
            .refresh(second_root.path())
            .expect_err("aggregate usage above six bytes must fail");
        assert!(error.contains("XFSTESTS_MAX_TEMP_BYTES=6"), "{error}");
        assert!(error.contains("raise XFSTESTS_MAX_TEMP_BYTES"), "{error}");

        drop(first);
        second
            .refresh(second_root.path())
            .expect("released reservation makes three bytes valid");
        drop(second);
        assert_eq!(*budget.used_bytes.lock().expect("budget lock"), 0);
    }

    #[test]
    fn rejected_temp_usage_reservation_does_not_leak_accounting() {
        let root = TestRoot::new("xfstests-budget-rejected");
        fs::write(root.path().join("backing"), b"12").expect("write backing");
        let budget = Arc::new(TempUsageBudget::new(1));

        assert!(
            budget.observe(root.path()).is_err(),
            "oversized initial reservation must fail"
        );
        assert_eq!(*budget.used_bytes.lock().expect("budget lock"), 0);
    }

    #[test]
    fn reports_bound_inline_diagnostics_and_preserve_raw_logs() {
        let root = TestRoot::new("xfstests-bounded-report");
        let payload = "diagnostic-line\n".repeat(1_000);
        let outcomes = [TestOutcome {
            id: String::from("generic/999"),
            backend: String::from(XFSTESTS_BACKEND),
            kind: OutcomeKind::Harness {
                reason: payload.clone(),
            },
            stdout: payload.clone(),
            stderr: payload.clone(),
        }];
        let log_stem = outcome_log_stem(&outcomes[0]);

        fs::write(
            root.path().join("agentos-surface-audit.md"),
            "# Test surface audit\n",
        )
        .expect("write test surface audit");
        write_reports(root.path(), root.path(), "test-pin", &outcomes, false);

        let results = fs::read_to_string(root.path().join("results.md")).expect("results report");
        let gaps = fs::read_to_string(root.path().join("agentos-gaps.md")).expect("gaps report");
        assert!(results.len() < 5_000, "results report was not bounded");
        assert!(gaps.len() < 5_000, "gaps report was not bounded");
        assert!(results.contains("detail truncated; see raw logs"));
        assert!(gaps.contains("full detail: [detail]"));
        assert!(gaps.contains("stderr: [stderr]"));
        let backend_results = fs::read_to_string(
            root.path()
                .join("backends")
                .join(XFSTESTS_BACKEND)
                .join("results.md"),
        )
        .expect("per-backend results report");
        assert!(backend_results.contains("Backend: `chunked_local`"));
        assert!(backend_results.contains("Strict status: **FAIL**"));

        let log_dir = root.path().join("logs");
        assert_eq!(
            fs::read_to_string(log_dir.join(format!("{log_stem}.detail.log"))).expect("raw detail"),
            payload
        );
        assert_eq!(
            fs::read_to_string(log_dir.join(format!("{log_stem}.stdout.log"))).expect("raw stdout"),
            payload
        );
        assert_eq!(
            fs::read_to_string(log_dir.join(format!("{log_stem}.stderr.log"))).expect("raw stderr"),
            payload
        );
    }

    #[test]
    fn reports_preserve_duplicate_test_backend_diagnostics() {
        let root = TestRoot::new("xfstests-duplicate-report");
        let outcomes = [
            TestOutcome::harness("generic/999", XFSTESTS_BACKEND, "primary failure"),
            TestOutcome::harness("generic/999", XFSTESTS_BACKEND, "secondary failure"),
        ];
        fs::write(
            root.path().join("agentos-surface-audit.md"),
            "# Test surface audit\n",
        )
        .expect("write test surface audit");

        write_reports(root.path(), root.path(), "test-pin", &outcomes, false);

        let stems = outcome_log_stems(&outcomes);
        assert_eq!(stems[1], format!("{}-2", stems[0]));
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("logs")
                    .join(format!("{}.detail.log", stems[0]))
            )
            .expect("primary detail"),
            "primary failure"
        );
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("logs")
                    .join(format!("{}.detail.log", stems[1]))
            )
            .expect("secondary detail"),
            "secondary failure"
        );
    }
}

fn outcome_label(kind: &OutcomeKind) -> &'static str {
    match kind {
        OutcomeKind::Pass => "pass",
        OutcomeKind::Fail { .. } => "fail",
        OutcomeKind::NotRun { .. } => "notrun",
        OutcomeKind::AllowedNotRun { .. } => "allowed-notrun",
        OutcomeKind::Harness { .. } => "harness",
        OutcomeKind::Excluded { .. } => "excluded",
        OutcomeKind::Deferred { .. } => "deferred",
        OutcomeKind::ExpectedFailure { .. } => "expected-failure",
        OutcomeKind::ReducedPass { .. } => "reduced-pass",
        OutcomeKind::UnexpectedPass { .. } => "unexpected-pass",
    }
}

fn outcome_detail(kind: &OutcomeKind) -> &str {
    match kind {
        OutcomeKind::Pass => "",
        OutcomeKind::Fail { output, .. } => output,
        OutcomeKind::NotRun { reason }
        | OutcomeKind::AllowedNotRun { reason }
        | OutcomeKind::Harness { reason }
        | OutcomeKind::Excluded { reason }
        | OutcomeKind::Deferred { reason }
        | OutcomeKind::ExpectedFailure { reason }
        | OutcomeKind::ReducedPass { reason }
        | OutcomeKind::UnexpectedPass { reason } => reason,
    }
}

fn bounded_inline(value: &str, max_chars: usize) -> String {
    let normalized = value.replace(['\n', '\r'], " ");
    let mut chars = normalized.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.push_str(" [detail truncated; see raw logs]");
    }
    bounded
}

fn outcome_log_stem(outcome: &TestOutcome) -> String {
    format!(
        "{}-{}",
        outcome.backend.replace(['/', '\\'], "-"),
        outcome.id.replace(['/', '\\'], "-")
    )
}

fn outcome_log_stems(outcomes: &[TestOutcome]) -> Vec<String> {
    let mut occurrences = HashMap::<String, usize>::new();
    outcomes
        .iter()
        .map(|outcome| {
            let base = outcome_log_stem(outcome);
            let occurrence = occurrences.entry(base.clone()).or_default();
            *occurrence += 1;
            if *occurrence == 1 {
                base
            } else {
                format!("{base}-{}", *occurrence)
            }
        })
        .collect()
}

fn write_reports(
    report_dir: &Path,
    source: &Path,
    pin: &str,
    outcomes: &[TestOutcome],
    strict_ok: bool,
) {
    const MAX_INLINE_DETAIL_CHARS: usize = 2_048;

    fs::create_dir_all(report_dir).expect("create xfstests report directory");
    let log_dir = report_dir.join("logs");
    if log_dir.exists() {
        fs::remove_dir_all(&log_dir).expect("remove stale xfstests raw logs");
    }
    let backend_report_dir = report_dir.join("backends");
    if backend_report_dir.exists() {
        fs::remove_dir_all(&backend_report_dir).expect("remove stale per-backend reports");
    }
    fs::create_dir_all(&log_dir).expect("create xfstests raw log directory");
    let log_stems = outcome_log_stems(outcomes);
    let mut counts = HashMap::<&str, usize>::new();
    for outcome in outcomes {
        *counts.entry(outcome_label(&outcome.kind)).or_default() += 1;
    }
    let labels = [
        "pass",
        "fail",
        "expected-failure",
        "reduced-pass",
        "notrun",
        "allowed-notrun",
        "deferred",
        "excluded",
        "unexpected-pass",
        "harness",
    ];
    let headline = labels
        .iter()
        .map(|label| format!("{label}={}", counts.get(label).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut results = format!(
        "# xfstests results\n\nPinned SHA: `{pin}`\n\nStrict status: **{}**\n\n{headline}\n\n| Test | Backend | Result | Detail | Raw output |\n|---|---|---|---|---|\n",
        if strict_ok { "PASS" } else { "FAIL" }
    );
    for (outcome, log_stem) in outcomes.iter().zip(&log_stems) {
        let mut log_links = Vec::new();
        let full_detail = outcome_detail(&outcome.kind);
        if !full_detail.is_empty() {
            let name = format!("{log_stem}.detail.log");
            fs::write(log_dir.join(&name), full_detail).expect("write per-test detail log");
            log_links.push(format!("[detail](logs/{name})"));
        }
        if !outcome.stdout.is_empty() {
            let name = format!("{log_stem}.stdout.log");
            fs::write(log_dir.join(&name), &outcome.stdout).expect("write per-test stdout log");
            log_links.push(format!("[stdout](logs/{name})"));
        }
        if !outcome.stderr.is_empty() {
            let name = format!("{log_stem}.stderr.log");
            fs::write(log_dir.join(&name), &outcome.stderr).expect("write per-test stderr log");
            log_links.push(format!("[stderr](logs/{name})"));
        }
        let detail = bounded_inline(full_detail, MAX_INLINE_DETAIL_CHARS).replace('|', "\\|");
        results.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} |\n",
            outcome.id,
            outcome.backend,
            outcome_label(&outcome.kind),
            detail,
            log_links.join(", ")
        ));
    }
    fs::write(report_dir.join("results.md"), results).expect("write results report");

    let mut gaps = String::from("# AgentOS filesystem gaps\n\n## Ranked fixes\n\n");
    for (index, outcome) in outcomes.iter().enumerate().filter(|(_, outcome)| {
        !matches!(
            outcome.kind,
            OutcomeKind::Pass | OutcomeKind::ReducedPass { .. } | OutcomeKind::Excluded { .. }
        )
    }) {
        let log_stem = &log_stems[index];
        gaps.push_str(&format!(
            "- `{}` on `{}`: **{}** - {}\n",
            outcome.id,
            outcome.backend,
            outcome_label(&outcome.kind),
            bounded_inline(outcome_detail(&outcome.kind), MAX_INLINE_DETAIL_CHARS)
        ));
        if !outcome_detail(&outcome.kind).is_empty() {
            gaps.push_str(&format!(
                "  full detail: [detail](logs/{log_stem}.detail.log) ({} bytes)\n",
                outcome_detail(&outcome.kind).len()
            ));
        }
        if !outcome.stderr.is_empty() {
            gaps.push_str(&format!(
                "  stderr: [stderr](logs/{log_stem}.stderr.log) ({} bytes)\n",
                outcome.stderr.len()
            ));
        }
    }
    fs::write(report_dir.join("agentos-gaps.md"), gaps).expect("write gaps report");

    let mut backends = outcomes
        .iter()
        .map(|outcome| outcome.backend.as_str())
        .collect::<Vec<_>>();
    backends.sort_unstable();
    backends.dedup();
    for backend in &backends {
        let backend_outcomes = outcomes
            .iter()
            .enumerate()
            .filter(|(_, outcome)| outcome.backend == *backend)
            .collect::<Vec<_>>();
        let backend_strict = backend_outcomes
            .iter()
            .all(|(_, outcome)| outcome_satisfies_strict_policy(&outcome.kind));
        let mut backend_counts = HashMap::<&str, usize>::new();
        for (_, outcome) in &backend_outcomes {
            *backend_counts
                .entry(outcome_label(&outcome.kind))
                .or_default() += 1;
        }
        let backend_headline = labels
            .iter()
            .map(|label| {
                format!(
                    "{label}={}",
                    backend_counts.get(label).copied().unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut backend_results = format!(
            "# xfstests backend results\n\nPinned SHA: `{pin}`\n\nBackend: `{backend}`\n\nStrict status: **{}**\n\n{backend_headline}\n\n| Test | Result | Detail | Raw output |\n|---|---|---|---|\n",
            if backend_strict { "PASS" } else { "FAIL" }
        );
        for (index, outcome) in backend_outcomes {
            let log_stem = &log_stems[index];
            let mut log_links = Vec::new();
            if !outcome_detail(&outcome.kind).is_empty() {
                log_links.push(format!("[detail](../../logs/{log_stem}.detail.log)"));
            }
            if !outcome.stdout.is_empty() {
                log_links.push(format!("[stdout](../../logs/{log_stem}.stdout.log)"));
            }
            if !outcome.stderr.is_empty() {
                log_links.push(format!("[stderr](../../logs/{log_stem}.stderr.log)"));
            }
            backend_results.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                outcome.id,
                outcome_label(&outcome.kind),
                bounded_inline(outcome_detail(&outcome.kind), MAX_INLINE_DETAIL_CHARS)
                    .replace('|', "\\|"),
                log_links.join(", ")
            ));
        }
        let backend_dir = report_dir.join("backends").join(backend);
        fs::create_dir_all(&backend_dir).expect("create per-backend report directory");
        fs::write(backend_dir.join("results.md"), backend_results)
            .expect("write per-backend results report");
    }

    let mut audit = fs::read_to_string(source.join("agentos-surface-audit.md"))
        .expect("generated pinned surface audit");
    let backend_summary = backends.join(", ");
    audit.push_str(&format!(
        "\n## Runtime gates\n\n| Surface | Status | Evidence |\n|---|---|---|\n| VFS routing and mount discovery | supported+tested | verify-first gate, backends `{backend_summary}` |\n| multi-user credentials and DAC | supported+tested | Rust and C credential/DAC probes |\n| WASI helper process ports | supported+tested | `xfstests_wasi_helper_ports` |\n"
    ));
    fs::write(report_dir.join("surface-audit.md"), audit).expect("write surface audit");
}

#[test]
#[ignore = "expensive pinned xfstests matrix; run with make -C tests/xfstests run"]
fn xfstests_generic_quick_matrix() {
    support::assert_node_available();
    let source = PathBuf::from(env::var("XFSTESTS_ROOT").expect("XFSTESTS_ROOT from Makefile"));
    let report_dir =
        PathBuf::from(env::var("XFSTESTS_REPORT_DIR").expect("XFSTESTS_REPORT_DIR from Makefile"));
    let pin = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/xfstests/xfstests.pin"),
    )
    .expect("xfstests pin");
    let pin = pin.trim();
    let staged_head = Command::new("git")
        .args([
            "-C",
            source.to_str().expect("UTF-8 xfstests root"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("read staged xfstests HEAD");
    assert!(
        staged_head.status.success(),
        "staged xfstests must be a git checkout"
    );
    assert_eq!(
        String::from_utf8_lossy(&staged_head.stdout).trim(),
        pin,
        "staged xfstests HEAD must exactly match xfstests.pin"
    );
    let backends = xfstests_backends();
    for backend in &backends {
        verify_first(backend, false);
    }
    let selected = quick_tests(&source);
    assert!(!selected.is_empty(), "zero generic/quick tests selected");
    let exceptions = load_exception_records(&source);
    let exception_map = exceptions
        .iter()
        .filter(|record| backends.contains(&record.backend) && selected.contains(&record.id))
        .map(|record| ((record.id.clone(), record.backend.clone()), record.clone()))
        .collect::<HashMap<_, _>>();
    let mut outcomes = Vec::new();
    let mut runnable = VecDeque::new();
    let mut used_exceptions = HashSet::new();
    for backend in &backends {
        for test_id in &selected {
            let key = (test_id.clone(), backend.clone());
            match exception_map.get(&key) {
                Some(record) if record.disposition == "excluded" => {
                    used_exceptions.insert(key);
                    outcomes.push(TestOutcome {
                        id: test_id.clone(),
                        backend: backend.clone(),
                        kind: OutcomeKind::Excluded {
                            reason: record.reason.clone(),
                        },
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                Some(record) if record.disposition == "deferred" => {
                    used_exceptions.insert(key);
                    outcomes.push(TestOutcome {
                        id: test_id.clone(),
                        backend: backend.clone(),
                        kind: OutcomeKind::Deferred {
                            reason: exception_detail(record),
                        },
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                _ => runnable.push_back((
                    test_id.clone(),
                    backend.clone(),
                    exception_map
                        .get(&key)
                        .filter(|record| record.disposition == "reduced")
                        .cloned(),
                )),
            }
        }
    }

    let selected_attempts = runnable.len();
    assert!(
        selected_attempts > 0 || !outcomes.is_empty(),
        "zero tests classified"
    );
    let concurrency = env::var("XFSTESTS_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0 && *value <= 16)
        .expect("XFSTESTS_CONCURRENCY must be in 1..=16");
    let max_temp_bytes = env::var("XFSTESTS_MAX_TEMP_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 1024 * 1024 && *value <= 1024 * 1024 * 1024 * 1024)
        .expect("XFSTESTS_MAX_TEMP_BYTES must be in 1048576..=1099511627776");
    let temp_budget = Arc::new(TempUsageBudget::new(max_temp_bytes));
    let queue = Arc::new(Mutex::new(runnable));
    let stop_scheduling = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let mut attempted_keys = HashSet::new();
    let mut completed_attempts = 0usize;
    let mut fail_fast_triggered = false;
    thread::scope(|scope| {
        for _ in 0..concurrency.min(selected_attempts) {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            let source = source.clone();
            let temp_budget = Arc::clone(&temp_budget);
            let stop_scheduling = Arc::clone(&stop_scheduling);
            scope.spawn(move || loop {
                if stop_scheduling.load(Ordering::Acquire) {
                    break;
                }
                let case = queue.lock().expect("xfstests queue lock").pop_front();
                let Some((test_id, backend, reduction)) = case else {
                    break;
                };
                let outcome = match catch_unwind(AssertUnwindSafe(|| {
                    run_xfstest_subprocess(
                        &source,
                        &test_id,
                        &backend,
                        &temp_budget,
                        reduction.as_ref(),
                    )
                })) {
                    Ok(outcome) => outcome,
                    Err(_) => TestOutcome::harness(
                        &test_id,
                        &backend,
                        "runner panicked; per-test root was cleaned",
                    ),
                };
                sender.send(outcome).expect("send xfstests outcome");
            });
        }
        drop(sender);
        for mut outcome in receiver {
            completed_attempts += 1;
            let key = (outcome.id.clone(), outcome.backend.clone());
            attempted_keys.insert(key.clone());
            if let Some(exception) = exception_map.get(&key) {
                if apply_exception(&mut outcome, exception) {
                    used_exceptions.insert(key);
                }
            }
            let strict = outcome_satisfies_strict_policy(&outcome.kind);
            outcomes.push(outcome);
            if !strict {
                fail_fast_triggered = true;
                stop_scheduling.store(true, Ordering::Release);
                queue.lock().expect("xfstests queue lock").clear();
            }
        }
    });
    outcomes.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.backend.cmp(&right.backend))
    });

    let stale = exception_map
        .keys()
        .filter(|key| attempted_keys.contains(*key) && !used_exceptions.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    for (id, backend) in &stale {
        outcomes.push(TestOutcome::harness(
            id,
            backend,
            format!("stale or mismatched exception for backend {backend}"),
        ));
    }
    let strict_ok = !fail_fast_triggered
        && completed_attempts == selected_attempts
        && stale.is_empty()
        && outcomes
            .iter()
            .all(|outcome| outcome_satisfies_strict_policy(&outcome.kind));
    write_reports(&report_dir, &source, pin, &outcomes, strict_ok);
    eprintln!(
        "xfstests: attempted={completed_attempts}/{selected_attempts} strict_status={} report={}",
        if strict_ok { "pass" } else { "fail" },
        report_dir.display()
    );
    assert!(
        strict_ok,
        "xfstests strict result failed; see report/results.md"
    );
}
