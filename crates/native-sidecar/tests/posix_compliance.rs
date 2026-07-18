mod support;

use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::fd_table::O_RDWR;
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::process_runtime::{
    ProcessControlRequest, ProcessExit, ProcessRuntimeEndpoint, ProcessRuntimeEndpointError,
    ProcessRuntimeIdentity, ProcessTermination,
};
use agentos_kernel::process_table::{
    ProcessContext, ProcessEntry, ProcessResult, ProcessTable, ProcessWaitEvent, SignalAction,
    SignalDisposition, WaitPidFlags, SIGCHLD, SIGTERM,
};
use agentos_kernel::vfs::MemoryFileSystem;
use agentos_native_sidecar::wire::{
    EventPayload, GetSignalStateRequest, GuestRuntimeKind, KillProcessRequest, RequestPayload,
    ResponsePayload, SignalDispositionAction, SignalHandlerRegistration,
};
use nix::libc;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, execute_wire, new_sidecar,
    open_session_wire, temp_dir, wire_request, wire_vm, write_fixture,
};

fn assert_process_error_code<T: Debug>(result: ProcessResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn assert_not_trivial_pattern(bytes: &[u8]) {
    assert!(bytes.iter().any(|byte| *byte != 0));
    assert!(
        bytes.windows(2).any(|window| window[0] != window[1]),
        "random data should not collapse to a repeated byte"
    );
}

fn null_separated_bytes(parts: &[&str]) -> Vec<u8> {
    if parts.is_empty() {
        return Vec::new();
    }

    let mut bytes = parts.join("\0").into_bytes();
    bytes.push(0);
    bytes
}

fn chunk_contains(chunk: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    chunk.windows(needle.len()).any(|window| window == needle)
}

fn new_kernel(name: &str) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new(name);
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell driver");
    kernel
}

fn spawn_shell(
    kernel: &mut KernelVm<MemoryFileSystem>,
    args: Vec<String>,
    cwd: &str,
    env: BTreeMap<String, String>,
) -> agentos_kernel::kernel::KernelProcessHandle {
    kernel
        .spawn_process(
            "sh",
            args,
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                cwd: Some(String::from(cwd)),
                env,
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
}

fn wait_for_process_output(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: &str,
) {
    let ownership = wire_vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process output"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        let Some(event) = event else { continue };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == process_id && chunk_contains(&output.chunk, expected) =>
            {
                return;
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct MockProcessState {
    kills: Vec<i32>,
    exit_code: Option<i32>,
    binding: Option<(ProcessTable, u32)>,
}

#[derive(Default)]
struct MockDriverProcess {
    state: Mutex<MockProcessState>,
}

impl MockDriverProcess {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn kills(&self) -> Vec<i32> {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .kills
            .clone()
    }

    fn bind(&self, table: &ProcessTable, pid: u32) {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .binding = Some((table.clone(), pid));
    }

    fn exit(&self, exit_code: i32) {
        let binding = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            if state.exit_code.is_some() {
                return;
            }
            state.exit_code = Some(exit_code);
            state.binding.clone()
        };

        if let Some((table, pid)) = binding {
            table
                .report_exit(pid, ProcessExit::Exited(exit_code))
                .expect("mock process exit must reach the bound kernel process");
        }
    }
}

impl ProcessRuntimeEndpoint for MockDriverProcess {
    fn identity(&self) -> Option<ProcessRuntimeIdentity> {
        None
    }

    fn request_control(
        &self,
        request: ProcessControlRequest,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        let (binding, termination) = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            let signal = match request {
                ProcessControlRequest::Checkpoint => state
                    .binding
                    .as_ref()
                    .and_then(|(table, pid)| table.sigpending(*pid).ok())
                    .and_then(|pending| pending.signals().into_iter().next()),
                ProcessControlRequest::Terminate(ProcessTermination::Signal { signal, .. }) => {
                    Some(signal)
                }
                _ => None,
            };
            if let Some(signal) = signal {
                state.kills.push(signal);
            }
            let termination = match request {
                ProcessControlRequest::Terminate(ProcessTermination::Signal { signal, .. }) => {
                    Some(ProcessExit::Signaled {
                        signal,
                        core_dumped: false,
                    })
                }
                ProcessControlRequest::Terminate(ProcessTermination::RuntimeFault)
                | ProcessControlRequest::Cancel(_) => Some(ProcessExit::Exited(1)),
                _ => None,
            };
            (state.binding.clone(), termination)
        };

        if let (Some((table, pid)), Some(termination)) = (binding, termination) {
            table
                .report_exit(pid, termination)
                .expect("mock termination must reach the bound kernel process");
        }
        Ok(())
    }
}

fn register_mock_process(
    table: &ProcessTable,
    pid: u32,
    command: &str,
    context: ProcessContext,
    process: Arc<MockDriverProcess>,
) -> ProcessEntry {
    let entry = table.register(pid, "wasmvm", command, Vec::new(), context, process.clone());
    process.bind(table, pid);
    entry
}

fn create_context(ppid: u32) -> ProcessContext {
    ProcessContext {
        pid: 0,
        ppid,
        env: BTreeMap::new(),
        cwd: String::from("/"),
        ..ProcessContext::default()
    }
}

fn allocate_pid(table: &ProcessTable) -> u32 {
    table.allocate_pid().expect("allocate pid")
}

#[test]
fn proc_filesystem_reports_kernel_identity_and_sanitized_process_metadata() {
    let mut kernel = new_kernel("vm-posix-procfs");
    kernel
        .mkdir("/guest/work", true)
        .expect("create guest working directory");
    kernel
        .write_file("/guest/work/data.txt", b"hello".to_vec())
        .expect("seed guest file");

    let env = BTreeMap::from([
        (String::from("KERNEL_ONLY_MARKER"), String::from("present")),
        (String::from("SECOND_MARKER"), String::from("also-present")),
    ]);
    let process = spawn_shell(
        &mut kernel,
        vec![String::from("-lc"), String::from("echo ok")],
        "/guest/work",
        env,
    );
    let pid = process.pid();
    let data_fd = kernel
        .fd_open("shell", pid, "/guest/work/data.txt", O_RDWR, None)
        .expect("open extra guest fd");

    let self_link = kernel
        .read_link_for_process("shell", pid, "/proc/self")
        .expect("resolve /proc/self");
    assert_eq!(self_link, format!("/proc/{pid}"));

    let stat_text = String::from_utf8(
        kernel
            .read_file_for_process("shell", pid, "/proc/self/stat")
            .expect("read /proc/self/stat"),
    )
    .expect("proc stat should be utf8");
    let reported_pid = stat_text
        .split_whitespace()
        .next()
        .expect("proc stat should include pid")
        .parse::<u32>()
        .expect("proc stat pid should be numeric");
    assert_eq!(reported_pid, pid, "proc identity should use kernel pid");

    let cmdline = kernel
        .read_file_for_process("shell", pid, &format!("/proc/{pid}/cmdline"))
        .expect("read cmdline");
    assert_eq!(cmdline, null_separated_bytes(&["sh", "-lc", "echo ok"]));

    let environ = kernel
        .read_file_for_process("shell", pid, &format!("/proc/{pid}/environ"))
        .expect("read environ");
    assert_eq!(
        environ,
        null_separated_bytes(&["KERNEL_ONLY_MARKER=present", "SECOND_MARKER=also-present",]),
        "proc environ should only expose kernel-managed env entries"
    );

    let cwd = kernel
        .read_link_for_process("shell", pid, &format!("/proc/{pid}/cwd"))
        .expect("read cwd");
    assert_eq!(cwd, "/guest/work");

    let fd_entries = kernel
        .read_dir_for_process("shell", pid, &format!("/proc/{pid}/fd"))
        .expect("read fd directory");
    assert!(fd_entries.contains(&String::from("0")));
    assert!(fd_entries.contains(&String::from("1")));
    assert!(fd_entries.contains(&String::from("2")));
    assert!(fd_entries.contains(&data_fd.to_string()));
    assert_eq!(
        kernel
            .read_link_for_process("shell", pid, &format!("/proc/{pid}/fd/{data_fd}"))
            .expect("read proc fd link"),
        String::from("/guest/work/data.txt")
    );

    process.finish(0);
    kernel.waitpid(pid).expect("wait procfs shell");
}

#[test]
fn device_nodes_match_posix_special_file_semantics() {
    let mut kernel = new_kernel("vm-posix-devices");
    let process = spawn_shell(&mut kernel, Vec::new(), "/", BTreeMap::new());
    let pid = process.pid();

    let null_fd = kernel
        .fd_open("shell", pid, "/dev/null", O_RDWR, None)
        .expect("open /dev/null");
    let written = kernel
        .fd_write("shell", pid, null_fd, b"discard-me")
        .expect("write /dev/null");
    assert_eq!(written, b"discard-me".len());
    let null_bytes = kernel
        .fd_read("shell", pid, null_fd, 32)
        .expect("read /dev/null");
    assert!(null_bytes.is_empty(), "/dev/null should always read as EOF");

    let zero_fd = kernel
        .fd_open("shell", pid, "/dev/zero", O_RDWR, None)
        .expect("open /dev/zero");
    let zeroes = kernel
        .fd_read("shell", pid, zero_fd, 64)
        .expect("read /dev/zero");
    assert_eq!(zeroes.len(), 64);
    assert!(zeroes.iter().all(|byte| *byte == 0));

    let random_fd = kernel
        .fd_open("shell", pid, "/dev/urandom", O_RDWR, None)
        .expect("open /dev/urandom");
    let first = kernel
        .fd_read("shell", pid, random_fd, 1024)
        .expect("read first urandom chunk");
    let second = kernel
        .fd_read("shell", pid, random_fd, 1024)
        .expect("read second urandom chunk");
    assert_eq!(first.len(), 1024);
    assert_eq!(second.len(), 1024);
    assert_not_trivial_pattern(&first);
    assert_not_trivial_pattern(&second);
    assert_ne!(first, second, "urandom reads should vary");

    process.finish(0);
    kernel.waitpid(pid).expect("wait device shell");
}

#[test]
fn v8_guest_process_receives_sigterm_delivery() {
    assert_node_available();

    let mut sidecar = new_sidecar("posix-v8-sigterm");
    let cwd = temp_dir("posix-v8-sigterm-cwd");
    let entry = cwd.join("sigterm.mjs");

    write_fixture(
        &entry,
        [
            "let deliveries = 0;",
            "process.on('SIGTERM', () => {",
            "  deliveries += 1;",
            "  console.log(`sigterm:${deliveries}`);",
            "  process.exit(0);",
            "});",
            "console.log('ready');",
            "setInterval(() => {}, 25);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-posix-sigterm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "sigterm-guest",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "sigterm-guest",
        "ready",
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let signal_state = sidecar
            .dispatch_wire_blocking(wire_request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                    process_id: String::from("sigterm-guest"),
                }),
            ))
            .expect("query signal state");
        let ready = match signal_state.response.payload {
            ResponsePayload::SignalStateResponse(snapshot) => {
                snapshot.handlers.get(&(libc::SIGTERM as u32))
                    == Some(&SignalHandlerRegistration {
                        action: SignalDispositionAction::User,
                        mask: vec![],
                        flags: 0,
                    })
            }
            other => panic!("unexpected signal state response: {other:?}"),
        };
        if ready {
            break;
        }

        let _ = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(25))
            .expect("pump signal registration");
        assert!(
            Instant::now() < registration_deadline,
            "timed out waiting for SIGTERM handler registration"
        );
    }

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("sigterm-guest"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("deliver SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_sigterm = false;
    let mut exit_code = None;

    while exit_code.is_none() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for SIGTERM delivery"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sigterm events");
        let Some(event) = event else { continue };

        match event.payload {
            EventPayload::ProcessOutputEvent(output) if output.process_id == "sigterm-guest" => {
                saw_sigterm |= chunk_contains(&output.chunk, "sigterm:1");
            }
            EventPayload::ProcessExitedEvent(exited) if exited.process_id == "sigterm-guest" => {
                exit_code = Some(exited.exit_code);
            }
            _ => {}
        }
    }

    assert!(saw_sigterm, "guest should observe SIGTERM");
    assert_eq!(exit_code, Some(0));
}

#[test]
fn process_table_delivers_sigchld_and_reaps_zombies_via_waitpid() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);

    register_mock_process(
        &table,
        parent_pid,
        "parent",
        create_context(0),
        parent.clone(),
    );
    register_mock_process(
        &table,
        child_pid,
        "child",
        create_context(parent_pid),
        child.clone(),
    );
    table
        .signal_action(
            parent_pid,
            SIGCHLD,
            Some(SignalAction {
                disposition: SignalDisposition::User,
                ..SignalAction::DEFAULT
            }),
        )
        .expect("catch SIGCHLD");

    assert_eq!(
        table
            .waitpid_for(parent_pid, -1, WaitPidFlags::WNOHANG)
            .expect("initial waitpid should succeed"),
        None
    );

    table
        .kill(child_pid as i32, SIGTERM)
        .expect("send SIGTERM to child");
    assert_eq!(child.kills(), vec![SIGTERM]);
    assert_eq!(parent.kills(), vec![SIGCHLD]);

    let waited = table
        .waitpid_for(parent_pid, -1, WaitPidFlags::empty())
        .expect("waitpid should succeed")
        .expect("waitpid should report exited child");
    assert_eq!(waited.pid, child_pid);
    assert_eq!(waited.status, 128 + SIGTERM);
    assert_eq!(waited.event, ProcessWaitEvent::Exited);
    assert!(
        table.get(child_pid).is_none(),
        "waitpid should clean up child zombies"
    );

    assert_process_error_code(table.waitpid(child_pid), "ESRCH");
}

#[test]
fn process_table_negative_pid_kill_targets_entire_process_groups() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let leader = MockDriverProcess::new();
    let peer = MockDriverProcess::new();
    let leader_pid = allocate_pid(&table);
    let peer_pid = allocate_pid(&table);

    register_mock_process(
        &table,
        leader_pid,
        "leader",
        create_context(0),
        leader.clone(),
    );
    register_mock_process(
        &table,
        peer_pid,
        "peer",
        create_context(leader_pid),
        peer.clone(),
    );
    table
        .setpgid(peer_pid, leader_pid)
        .expect("peer should join leader process group");

    table
        .kill(-(leader_pid as i32), SIGTERM)
        .expect("group kill should succeed");

    assert_eq!(leader.kills(), vec![SIGTERM]);
    assert_eq!(peer.kills(), vec![SIGTERM]);
}
