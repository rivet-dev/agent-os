mod support;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use agentos_native_sidecar::wire::{
    ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind,
    RequestPayload, ResponsePayload, RootFilesystemEntryEncoding, StandaloneWasmBackend,
};
use base64::Engine as _;

use support::{
    authenticate_wire, collect_process_output_wire_with_timeout, new_sidecar, open_session_wire,
    temp_dir, wire_request, wire_vm, write_fixture,
};

fn command_artifact(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let staged = root.join("packages/runtime-core/commands").join(name);
    if staged.is_file() {
        return staged;
    }
    let toolchain = root
        .join("toolchain/target/wasm32-wasip1/release/commands")
        .join(name);
    if toolchain.is_file() {
        return toolchain;
    }
    root.join("toolchain/c/build").join(name)
}

fn run_command(
    test_name: &str,
    command: &str,
    args: &[&str],
    backend: StandaloneWasmBackend,
) -> (String, String, i32) {
    run_command_with_files(test_name, command, args, &[], backend)
}

fn run_command_with_files(
    test_name: &str,
    command: &str,
    args: &[&str],
    extra_commands: &[&str],
    backend: StandaloneWasmBackend,
) -> (String, String, i32) {
    run_command_with_files_and_metadata(
        test_name,
        command,
        args,
        extra_commands,
        backend,
        HashMap::new(),
    )
}

fn run_command_with_files_and_metadata(
    test_name: &str,
    command: &str,
    args: &[&str],
    extra_commands: &[&str],
    backend: StandaloneWasmBackend,
    metadata: HashMap<String, String>,
) -> (String, String, i32) {
    let artifact = command_artifact(command);
    let module = std::fs::read(&artifact).unwrap_or_else(|error| {
        panic!(
            "generated command artifact {} is required: {error}",
            artifact.display()
        )
    });
    let mut sidecar = new_sidecar(test_name);
    let cwd = temp_dir(&format!("{test_name}-cwd"));
    let entrypoint = cwd.join(command);
    write_fixture(&entrypoint, &module);
    make_executable(&entrypoint);
    for extra in extra_commands {
        let path = cwd.join(extra);
        write_fixture(
            &path,
            std::fs::read(command_artifact(extra))
                .unwrap_or_else(|error| panic!("generated {extra} command is required: {error}")),
        );
        make_executable(&path);
    }

    let connection_id = authenticate_wire(&mut sidecar, "conn-software-parity");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        metadata,
    );
    support::write_guest_file_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/fixture.txt",
        "fixture\n",
    );
    for (index, extra) in extra_commands.iter().enumerate() {
        let guest_path = if *extra == "sh" {
            String::from("/bin/sh")
        } else {
            format!("/workspace/{extra}")
        };
        write_guest_binary(
            &mut sidecar,
            10 + index as i64,
            &connection_id,
            &session_id,
            &vm_id,
            &guest_path,
            &std::fs::read(command_artifact(extra)).expect("read generated child command"),
        );
    }
    let process_id = format!("process-{test_name}");
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            100,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.clone(),
                command: None,
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: Some(backend),
            }),
        ))
        .expect("start generated software command through sidecar");
    assert!(
        matches!(
            started.response.payload,
            ResponsePayload::ProcessStartedResponse(_)
        ),
        "unexpected software start response: {:?}",
        started.response.payload
    );

    collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(60),
    )
}

fn write_guest_binary(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
    contents: &[u8],
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: path.to_owned(),
                destination_path: None,
                target: None,
                content: Some(base64::engine::general_purpose::STANDARD.encode(contents)),
                encoding: Some(RootFilesystemEntryEncoding::Base64),
                recursive: false,
                max_depth: None,
                mode: Some(0o755),
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        ))
        .expect("write generated child command into guest VFS");
    assert!(
        matches!(
            result.response.payload,
            ResponsePayload::GuestFilesystemResultResponse(_)
        ),
        "unexpected guest binary write response: {:?}",
        result.response.payload
    );
    let chmod = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id + 1_000,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Chmod,
                path: path.to_owned(),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                max_depth: None,
                mode: Some(0o755),
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        ))
        .expect("mark generated child command executable in guest VFS");
    assert!(
        matches!(
            chmod.response.payload,
            ResponsePayload::GuestFilesystemResultResponse(_)
        ),
        "unexpected guest chmod response: {:?}",
        chmod.response.payload
    );
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("command metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("mark command executable");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn assert_command_parity(command: &str, args: &[&str]) -> (String, String, i32) {
    let v8 = run_command(
        &format!("software-{command}-v8"),
        command,
        args,
        StandaloneWasmBackend::V8,
    );
    let wasmtime = run_command(
        &format!("software-{command}-wasmtime"),
        command,
        args,
        StandaloneWasmBackend::Wasmtime,
    );
    assert_eq!(wasmtime, v8, "software command {command} diverged");
    wasmtime
}

fn assert_command_parity_with_files(
    command: &str,
    args: &[&str],
    extra_commands: &[&str],
) -> (String, String, i32) {
    let v8 = run_command_with_files(
        &format!("software-{command}-v8"),
        command,
        args,
        extra_commands,
        StandaloneWasmBackend::V8,
    );
    let wasmtime = run_command_with_files(
        &format!("software-{command}-wasmtime"),
        command,
        args,
        extra_commands,
        StandaloneWasmBackend::Wasmtime,
    );
    assert_eq!(wasmtime, v8, "software command {command} diverged");
    wasmtime
}

fn run_curl_http_backend(backend: StandaloneWasmBackend) -> (String, String, i32) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind parity HTTP listener");
    listener
        .set_nonblocking(true)
        .expect("make parity HTTP listener deadline-aware");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "curl did not reach the parity HTTP listener before its deadline"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept parity HTTP request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set request deadline");
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).expect("read parity HTTP request");
            assert_ne!(read, 0, "curl closed before sending request headers");
            request.extend_from_slice(&chunk[..read]);
            assert!(
                request.len() <= 16 * 1024,
                "curl request headers are bounded"
            );
        }
        assert!(
            request.starts_with(b"GET /wasmtime-parity HTTP/1."),
            "unexpected curl request: {:?}",
            String::from_utf8_lossy(&request)
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\nagentos-network-path\n",
            )
            .expect("write parity HTTP response");
    });
    let result = run_command_with_files_and_metadata(
        &format!("software-curl-http-{backend:?}"),
        "curl",
        &["-fsS", &format!("http://127.0.0.1:{port}/wasmtime-parity")],
        &[],
        backend,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            format!("[{port}]"),
        )]),
    );
    server.join().expect("parity HTTP server");
    result
}

#[test]
#[ignore = "requires the generated packages/runtime-core/commands corpus"]
fn coreutils_ls_matches_v8_wasm() {
    let (stdout, stderr, exit_code) = assert_command_parity("ls", &["-1", "/workspace"]);
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stdout.contains("fixture.txt"), "stdout: {stdout}");
}

#[test]
#[ignore = "requires the generated packages/runtime-core/commands corpus"]
fn direct_software_corpus_matches_v8_wasm() {
    for (command, args, expected) in [
        ("grep", vec!["fixture", "/workspace/fixture.txt"], "fixture"),
        ("sqlite3", vec![":memory:", "select 1;"], "1"),
        ("git", vec!["--version"], "git version"),
        (
            "tar",
            vec![
                "-cf",
                "/workspace/fixture.tar",
                "-C",
                "/workspace",
                "fixture.txt",
            ],
            "",
        ),
        ("gzip", vec!["-c", "/workspace/fixture.txt"], ""),
        ("curl", vec!["--version"], "curl"),
        ("stat", vec!["-c", "%s", "/workspace/fixture.txt"], "8"),
    ] {
        let (stdout, stderr, exit_code) = assert_command_parity(command, &args);
        assert_eq!(exit_code, 0, "{command} stderr: {stderr}");
        assert!(
            stdout.contains(expected),
            "{command} output omitted {expected:?}: {stdout:?}"
        );
    }
}

#[test]
#[ignore = "requires the generated packages/runtime-core/commands corpus"]
fn shell_pipeline_and_child_backend_affinity_match_v8_wasm() {
    let script = "printf 'alpha\\nbeta\\n' | /workspace/grep beta";
    let (stdout, stderr, exit_code) =
        assert_command_parity_with_files("sh", &["-c", script], &["grep"]);
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "beta\n");
}

#[test]
#[ignore = "requires the generated toolchain/c/build/exec_variants fixture"]
fn exec_variants_match_linux_behavior_on_both_wasm_backends() {
    for (mode, marker) in [
        ("execle", "execle: ok"),
        ("execvpe", "execvpe: ok"),
        ("execve-shebang", "execve_shebang: ok"),
        ("shell-fallback", "shell_fallback: ok"),
        ("fexecve", "fexecve_unlinked_cloexec: ok"),
        ("fexecve-script", "fexecve_script_unlinked: ok"),
        (
            "fexecve-script-cloexec",
            "fexecve_script_cloexec_enoent: ok",
        ),
    ] {
        // Launch the fixture through the projected guest path. The trusted
        // initial entrypoint is a host-only image source and intentionally is
        // not mirrored into the kernel VFS, whereas a process that self-execs
        // must name the live kernel-owned executable.
        let script = format!("/workspace/exec_variants {mode}");
        let (stdout, stderr, exit_code) =
            assert_command_parity_with_files("sh", &["-c", &script], &["exec_variants", "sh"]);
        assert_eq!(exit_code, 0, "{mode} stderr: {stderr}");
        assert!(
            stdout.contains(marker),
            "{mode} output omitted {marker:?}: {stdout:?}"
        );
    }
}

#[test]
#[ignore = "requires the focused generated vim command artifact"]
fn vim_batch_edit_matches_v8_wasm() {
    let (stdout, stderr, exit_code) = assert_command_parity(
        "vim",
        &[
            "-Nu",
            "NONE",
            "-n",
            "-e",
            "-c",
            "%s/fixture/edited/",
            "-c",
            "%print",
            "-c",
            "q!",
            "/workspace/fixture.txt",
        ],
    );
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stdout.contains("edited"), "stdout: {stdout:?}");
}

#[test]
#[ignore = "requires the generated curl command artifact"]
fn curl_network_path_matches_v8_wasm() {
    let v8 = run_curl_http_backend(StandaloneWasmBackend::V8);
    let wasmtime = run_curl_http_backend(StandaloneWasmBackend::Wasmtime);
    assert_eq!(wasmtime, v8, "curl HTTP behavior diverged");
    assert_eq!(wasmtime.2, 0, "stderr: {}", wasmtime.1);
    assert_eq!(wasmtime.0, "agentos-network-path\n");
}
