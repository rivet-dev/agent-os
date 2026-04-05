mod bridge_support;

use agent_os_kernel::bridge::{
    ClockRequest, CommandPermissionRequest, CreateDirRequest, CreateJavascriptContextRequest,
    CreateWasmContextRequest, DiagnosticRecord, DirectoryEntry, EnvironmentAccess,
    EnvironmentPermissionRequest, ExecutionEvent, ExecutionHandleRequest, ExecutionSignal,
    FilesystemAccess, FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    GuestKernelCall, GuestRuntime, HostBridge, LifecycleEventRecord, LifecycleState,
    LoadFilesystemStateRequest, LogLevel, LogRecord, NetworkAccess, NetworkPermissionRequest,
    PathRequest, PollExecutionEventRequest, RandomBytesRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, ScheduleTimerRequest, StructuredEventRecord, SymlinkRequest, TruncateRequest,
    WriteExecutionStdinRequest, WriteFileRequest,
};
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::{Duration, SystemTime};

fn assert_host_bridge<B>(bridge: &mut B)
where
    B: HostBridge,
    <B as agent_os_kernel::bridge::BridgeTypes>::Error: Debug,
{
    let contents = bridge
        .read_file(ReadFileRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/input.txt"),
        })
        .expect("read file");
    assert_eq!(contents, b"hello".to_vec());

    bridge
        .write_file(WriteFileRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/output.txt"),
            contents: b"world".to_vec(),
        })
        .expect("write file");
    assert!(bridge
        .exists(PathRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/output.txt"),
        })
        .expect("exists after write"));

    let directory = bridge
        .read_dir(ReadDirRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace"),
        })
        .expect("read dir");
    assert_eq!(directory.len(), 1);

    let metadata = bridge
        .stat(PathRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/input.txt"),
        })
        .expect("stat");
    assert_eq!(metadata.kind, agent_os_kernel::bridge::FileKind::File);
    assert_eq!(metadata.size, 5);

    bridge
        .create_dir(CreateDirRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/tmp"),
            recursive: true,
        })
        .expect("create dir");
    bridge
        .rename(RenameRequest {
            vm_id: String::from("vm-1"),
            from_path: String::from("/workspace/output.txt"),
            to_path: String::from("/workspace/output-renamed.txt"),
        })
        .expect("rename");
    bridge
        .symlink(SymlinkRequest {
            vm_id: String::from("vm-1"),
            target_path: String::from("/workspace/input.txt"),
            link_path: String::from("/workspace/input-link.txt"),
        })
        .expect("symlink");
    assert_eq!(
        bridge
            .read_link(PathRequest {
                vm_id: String::from("vm-1"),
                path: String::from("/workspace/input-link.txt"),
            })
            .expect("readlink"),
        "/workspace/input.txt"
    );
    bridge
        .truncate(TruncateRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/input.txt"),
            len: 2,
        })
        .expect("truncate");
    assert_eq!(
        bridge
            .read_file(ReadFileRequest {
                vm_id: String::from("vm-1"),
                path: String::from("/workspace/input.txt"),
            })
            .expect("read after truncate"),
        b"he".to_vec()
    );

    assert_eq!(
        bridge
            .check_filesystem_access(FilesystemPermissionRequest {
                vm_id: String::from("vm-1"),
                path: String::from("/workspace/input.txt"),
                access: FilesystemAccess::Read,
            })
            .expect("filesystem permission"),
        agent_os_kernel::bridge::PermissionDecision::allow()
    );
    assert_eq!(
        bridge
            .check_network_access(NetworkPermissionRequest {
                vm_id: String::from("vm-1"),
                access: NetworkAccess::Fetch,
                resource: String::from("https://example.test"),
            })
            .expect("network permission"),
        agent_os_kernel::bridge::PermissionDecision::allow()
    );
    assert_eq!(
        bridge
            .check_command_execution(CommandPermissionRequest {
                vm_id: String::from("vm-1"),
                command: String::from("node"),
                args: vec![String::from("--version")],
                cwd: Some(String::from("/workspace")),
                env: BTreeMap::new(),
            })
            .expect("command permission"),
        agent_os_kernel::bridge::PermissionDecision::allow()
    );
    assert_eq!(
        bridge
            .check_environment_access(EnvironmentPermissionRequest {
                vm_id: String::from("vm-1"),
                access: EnvironmentAccess::Read,
                key: String::from("PATH"),
                value: None,
            })
            .expect("env permission"),
        agent_os_kernel::bridge::PermissionDecision::allow()
    );

    assert_eq!(
        bridge
            .load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: String::from("vm-1"),
            })
            .expect("load snapshot")
            .expect("snapshot present")
            .format,
        "tar"
    );
    bridge
        .flush_filesystem_state(FlushFilesystemStateRequest {
            vm_id: String::from("vm-2"),
            snapshot: FilesystemSnapshot {
                format: String::from("tar"),
                bytes: vec![9, 9, 9],
            },
        })
        .expect("flush snapshot");
    assert_eq!(
        bridge
            .load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: String::from("vm-2"),
            })
            .expect("load flushed snapshot")
            .expect("flushed snapshot present")
            .bytes,
        vec![9, 9, 9]
    );

    assert_eq!(
        bridge
            .wall_clock(ClockRequest {
                vm_id: String::from("vm-1"),
            })
            .expect("wall clock"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_710_000_000)
    );
    assert_eq!(
        bridge
            .monotonic_clock(ClockRequest {
                vm_id: String::from("vm-1"),
            })
            .expect("monotonic clock"),
        Duration::from_millis(42)
    );
    assert_eq!(
        bridge
            .schedule_timer(ScheduleTimerRequest {
                vm_id: String::from("vm-1"),
                delay: Duration::from_millis(5),
            })
            .expect("schedule timer")
            .timer_id,
        "timer-1"
    );
    assert_eq!(
        bridge
            .fill_random_bytes(RandomBytesRequest {
                vm_id: String::from("vm-1"),
                len: 4,
            })
            .expect("random bytes"),
        vec![0xA5; 4]
    );

    bridge
        .emit_log(LogRecord {
            vm_id: String::from("vm-1"),
            level: LogLevel::Info,
            message: String::from("started"),
        })
        .expect("emit log");
    bridge
        .emit_diagnostic(DiagnosticRecord {
            vm_id: String::from("vm-1"),
            message: String::from("healthy"),
            fields: BTreeMap::from([(String::from("uptime_ms"), String::from("10"))]),
        })
        .expect("emit diagnostic");
    bridge
        .emit_structured_event(StructuredEventRecord {
            vm_id: String::from("vm-1"),
            name: String::from("process.stdout"),
            fields: BTreeMap::from([(String::from("fd"), String::from("1"))]),
        })
        .expect("emit structured event");
    bridge
        .emit_lifecycle(LifecycleEventRecord {
            vm_id: String::from("vm-1"),
            state: LifecycleState::Ready,
            detail: Some(String::from("booted")),
        })
        .expect("emit lifecycle");

    let js_context = bridge
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-1"),
            bootstrap_module: Some(String::from("@rivet-dev/agent-os/bootstrap")),
        })
        .expect("create js context");
    assert_eq!(js_context.runtime, GuestRuntime::JavaScript);

    let wasm_context = bridge
        .create_wasm_context(CreateWasmContextRequest {
            vm_id: String::from("vm-1"),
            module_path: Some(String::from("/workspace/module.wasm")),
        })
        .expect("create wasm context");
    assert_eq!(wasm_context.runtime, GuestRuntime::WebAssembly);

    let execution = bridge
        .start_execution(agent_os_kernel::bridge::StartExecutionRequest {
            vm_id: String::from("vm-1"),
            context_id: js_context.context_id,
            argv: vec![String::from("index.js")],
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
        })
        .expect("start execution");
    assert_eq!(execution.execution_id, "exec-1");

    bridge
        .write_stdin(WriteExecutionStdinRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id.clone(),
            chunk: b"input".to_vec(),
        })
        .expect("write stdin");
    bridge
        .close_stdin(ExecutionHandleRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id.clone(),
        })
        .expect("close stdin");
    bridge
        .kill_execution(agent_os_kernel::bridge::KillExecutionRequest {
            vm_id: String::from("vm-1"),
            execution_id: execution.execution_id,
            signal: ExecutionSignal::Terminate,
        })
        .expect("kill execution");

    match bridge
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-1"),
        })
        .expect("poll execution event")
    {
        Some(ExecutionEvent::GuestRequest(event)) => {
            assert_eq!(event.operation, "fs.read");
        }
        other => panic!("unexpected execution event: {other:?}"),
    }

    let _ = wasm_context;
}

#[test]
fn host_bridge_traits_are_method_oriented_and_composable() {
    let mut bridge = RecordingBridge::default();
    bridge.seed_file("/workspace/input.txt", b"hello".to_vec());
    bridge.seed_directory(
        "/workspace",
        vec![DirectoryEntry {
            name: String::from("input.txt"),
            kind: agent_os_kernel::bridge::FileKind::File,
        }],
    );
    bridge.seed_snapshot(
        "vm-1",
        FilesystemSnapshot {
            format: String::from("tar"),
            bytes: vec![1, 2, 3],
        },
    );
    bridge.push_execution_event(ExecutionEvent::GuestRequest(GuestKernelCall {
        vm_id: String::from("vm-1"),
        execution_id: String::from("exec-seeded"),
        operation: String::from("fs.read"),
        payload: b"{}".to_vec(),
    }));

    assert_host_bridge(&mut bridge);
}
