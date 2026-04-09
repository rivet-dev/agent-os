mod support;

use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::fd_table::O_RDWR;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::process_table::{
    DriverProcess, ProcessContext, ProcessExitCallback, ProcessResult, ProcessTable,
    ProcessWaitEvent, WaitPidFlags, SIGCHLD, SIGTERM,
};
use agent_os_kernel::vfs::MemoryFileSystem;
use agent_os_sidecar::protocol::{
    EventPayload, GetSignalStateRequest, GuestRuntimeKind, KillProcessRequest, OwnershipScope,
    RequestPayload, ResponsePayload, SignalDispositionAction,
};
use nix::libc;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, create_vm_with_metadata, execute, new_sidecar,
    open_session, request, temp_dir, write_fixture,
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
) -> agent_os_kernel::kernel::KernelProcessHandle {
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
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: &str,
) {
    let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process output"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutput(output)
                if output.process_id == process_id && output.chunk.contains(expected) =>
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
    on_exit: Option<ProcessExitCallback>,
}

#[derive(Default)]
struct MockDriverProcess {
    state: Mutex<MockProcessState>,
    exited: Condvar,
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

    fn exit(&self, exit_code: i32) {
        let callback = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            if state.exit_code.is_some() {
                return;
            }
            state.exit_code = Some(exit_code);
            self.exited.notify_all();
            state.on_exit.clone()
        };

        if let Some(callback) = callback {
            callback(exit_code);
        }
    }
}

impl DriverProcess for MockDriverProcess {
    fn kill(&self, signal: i32) {
        let should_exit = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            state.kills.push(signal);
            signal == SIGTERM
        };

        if should_exit {
            self.exit(128 + signal);
        }
    }

    fn wait(&self, timeout: Duration) -> Option<i32> {
        let state = self.state.lock().expect("mock process lock poisoned");
        if state.exit_code.is_some() {
            return state.exit_code;
        }

        let (state, _) = self
            .exited
            .wait_timeout(state, timeout)
            .expect("mock process wait lock poisoned");
        state.exit_code
    }

    fn set_on_exit(&self, callback: ProcessExitCallback) {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .on_exit = Some(callback);
    }
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
        (
            String::from("KERNEL_ONLY_MARKER"),
            String::from("present"),
        ),
        (
            String::from("SECOND_MARKER"),
            String::from("also-present"),
        ),
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
        null_separated_bytes(&[
            "KERNEL_ONLY_MARKER=present",
            "SECOND_MARKER=also-present",
        ]),
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

    let connection_id = authenticate(&mut sidecar, "conn-posix-sigterm");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::new(),
    );

    execute(
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

    let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let signal_state = sidecar
            .dispatch_blocking(request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalState(GetSignalStateRequest {
                    process_id: String::from("sigterm-guest"),
                }),
            ))
            .expect("query signal state");
        let ready = match signal_state.response.payload {
            ResponsePayload::SignalState(snapshot) => {
                snapshot.handlers.get(&(libc::SIGTERM as u32))
                    == Some(&agent_os_sidecar::protocol::SignalHandlerRegistration {
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
            .poll_event_blocking(&ownership, Duration::from_millis(25))
            .expect("pump signal registration");
        assert!(
            Instant::now() < registration_deadline,
            "timed out waiting for SIGTERM handler registration"
        );
    }

    sidecar
        .dispatch_blocking(request(
            6,
            ownership.clone(),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("sigterm-guest"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("deliver SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_sigterm = false;
    let mut exit_code = None;

    while exit_code.is_none() {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sigterm events");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for SIGTERM delivery"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutput(output) if output.process_id == "sigterm-guest" => {
                saw_sigterm |= output.chunk.contains("sigterm:1");
            }
            EventPayload::ProcessExited(exited) if exited.process_id == "sigterm-guest" => {
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
    let parent_pid = table.allocate_pid();
    let child_pid = table.allocate_pid();

    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    table.register(
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );

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
    let leader_pid = table.allocate_pid();
    let peer_pid = table.allocate_pid();

    table.register(
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(0),
        leader.clone(),
    );
    table.register(
        peer_pid,
        "wasmvm",
        "peer",
        Vec::new(),
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
