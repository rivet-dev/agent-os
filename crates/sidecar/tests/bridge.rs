#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agent_os_bridge::{
    BridgeTypes, ClockRequest, CommandPermissionRequest, CreateJavascriptContextRequest,
    DiagnosticRecord, EnvironmentAccess, EnvironmentPermissionRequest, FilesystemAccess,
    FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    LifecycleEventRecord, LifecycleState, LoadFilesystemStateRequest, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, PathRequest, PollExecutionEventRequest,
    ReadFileRequest, StructuredEventRecord, WriteFileRequest,
};
use agent_os_sidecar::NativeSidecarBridge;
use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::fmt::Debug;

fn assert_native_sidecar_bridge<B>(bridge: &mut B)
where
    B: NativeSidecarBridge,
    <B as BridgeTypes>::Error: Debug,
{
    bridge
        .write_file(WriteFileRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/config.json"),
            contents: br#"{"ok":true}"#.to_vec(),
        })
        .expect("write file");
    assert!(bridge
        .exists(PathRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/workspace/config.json"),
        })
        .expect("exists"));
    assert_eq!(
        bridge
            .read_file(ReadFileRequest {
                vm_id: String::from("vm-1"),
                path: String::from("/workspace/config.json"),
            })
            .expect("read file"),
        br#"{"ok":true}"#.to_vec()
    );

    assert_eq!(
        bridge
            .check_filesystem_access(FilesystemPermissionRequest {
                vm_id: String::from("vm-1"),
                path: String::from("/workspace/config.json"),
                access: FilesystemAccess::Read,
            })
            .expect("filesystem permission"),
        agent_os_bridge::PermissionDecision::allow()
    );
    assert_eq!(
        bridge
            .check_network_access(NetworkPermissionRequest {
                vm_id: String::from("vm-1"),
                access: NetworkAccess::Fetch,
                resource: String::from("https://example.test"),
            })
            .expect("network permission"),
        agent_os_bridge::PermissionDecision::allow()
    );
    assert_eq!(
        bridge
            .check_command_execution(CommandPermissionRequest {
                vm_id: String::from("vm-1"),
                command: String::from("node"),
                args: vec![String::from("--version")],
                cwd: None,
                env: BTreeMap::new(),
            })
            .expect("command permission"),
        agent_os_bridge::PermissionDecision::allow()
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
        agent_os_bridge::PermissionDecision::allow()
    );

    bridge
        .flush_filesystem_state(FlushFilesystemStateRequest {
            vm_id: String::from("vm-1"),
            snapshot: FilesystemSnapshot {
                format: String::from("tar"),
                bytes: vec![1, 2, 3],
            },
        })
        .expect("flush state");
    assert_eq!(
        bridge
            .load_filesystem_state(LoadFilesystemStateRequest {
                vm_id: String::from("vm-1"),
            })
            .expect("load state")
            .expect("snapshot"),
        FilesystemSnapshot {
            format: String::from("tar"),
            bytes: vec![1, 2, 3],
        }
    );
    assert_eq!(
        bridge
            .wall_clock(ClockRequest {
                vm_id: String::from("vm-1"),
            })
            .expect("wall clock"),
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_710_000_000)
    );
    bridge
        .emit_log(LogRecord {
            vm_id: String::from("vm-1"),
            level: LogLevel::Info,
            message: String::from("native sidecar ready"),
        })
        .expect("emit log");
    bridge
        .emit_diagnostic(DiagnosticRecord {
            vm_id: String::from("vm-1"),
            message: String::from("snapshot flushed"),
            fields: BTreeMap::new(),
        })
        .expect("emit diagnostic");
    bridge
        .emit_structured_event(StructuredEventRecord {
            vm_id: String::from("vm-1"),
            name: String::from("vm.created"),
            fields: BTreeMap::new(),
        })
        .expect("emit structured event");
    bridge
        .emit_lifecycle(LifecycleEventRecord {
            vm_id: String::from("vm-1"),
            state: LifecycleState::Ready,
            detail: None,
        })
        .expect("emit lifecycle");

    let context = bridge
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm-1"),
            bootstrap_module: None,
        })
        .expect("create context");
    assert!(context.context_id.starts_with("js-context-"));
    assert!(bridge
        .poll_execution_event(PollExecutionEventRequest {
            vm_id: String::from("vm-1"),
        })
        .expect("poll event")
        .is_none());
}

#[test]
fn sidecar_crate_compiles_against_composed_host_bridge() {
    let mut bridge = RecordingBridge::default();
    assert_native_sidecar_bridge(&mut bridge);
}
