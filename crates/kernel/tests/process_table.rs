use agent_os_kernel::process_table::{
    DriverProcess, ProcessContext, ProcessExitCallback, ProcessResult, ProcessStatus, ProcessTable,
    ProcessWaitEvent, WaitPidFlags, SIGCHLD, SIGCONT, SIGHUP, SIGSTOP, SIGTSTP,
};
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn assert_error_code<T: Debug>(result: ProcessResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

#[derive(Default)]
struct MockProcessState {
    kills: Vec<i32>,
    exit_code: Option<i32>,
    on_exit: Option<ProcessExitCallback>,
    ignore_sigterm: bool,
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

    fn stubborn() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(MockProcessState {
                ignore_sigterm: true,
                ..MockProcessState::default()
            }),
            exited: Condvar::new(),
        })
    }

    fn schedule_exit(self: &Arc<Self>, delay: Duration, exit_code: i32) {
        let process = Arc::clone(self);
        thread::spawn(move || {
            thread::sleep(delay);
            process.exit(exit_code);
        });
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

    fn kills(&self) -> Vec<i32> {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .kills
            .clone()
    }
}

impl DriverProcess for MockDriverProcess {
    fn kill(&self, signal: i32) {
        let should_exit = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            state.kills.push(signal);
            signal == 9 || (signal == 15 && !state.ignore_sigterm)
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

fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }

    assert!(predicate(), "condition should become true before timeout");
}

#[test]
fn register_allocates_expected_process_metadata_and_parent_groups() {
    let table = ProcessTable::new();
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();

    let parent_pid = table.allocate_pid();
    let child_pid = table.allocate_pid();

    let parent_entry = table.register(
        parent_pid,
        "wasmvm",
        "grep",
        vec![String::from("-r"), String::from("foo")],
        create_context(0),
        parent,
    );
    let child_entry = table.register(
        child_pid,
        "node",
        "node",
        vec![String::from("-e"), String::from("1+1")],
        create_context(parent_pid),
        child,
    );

    assert_eq!(parent_entry.pid, 1);
    assert_eq!(child_entry.pid, 2);
    assert_eq!(parent_entry.pgid, 1);
    assert_eq!(parent_entry.sid, 1);
    assert_eq!(child_entry.pgid, 1);
    assert_eq!(child_entry.sid, 1);
    assert_eq!(child_entry.driver, "node");
}

#[test]
fn waitpid_resolves_for_exiting_and_already_exited_processes() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let process = MockDriverProcess::new();
    let pid = table.allocate_pid();
    table.register(
        pid,
        "wasmvm",
        "echo",
        vec![String::from("hello")],
        create_context(0),
        process.clone(),
    );

    process.schedule_exit(Duration::from_millis(10), 0);
    assert_eq!(
        table.waitpid(pid).expect("waitpid should resolve"),
        (pid, 0)
    );
    assert_eq!(table.zombie_timer_count(), 0);
    assert!(
        table.get(pid).is_none(),
        "waitpid should reap exited processes"
    );

    let exited_pid = table.allocate_pid();
    table.register(
        exited_pid,
        "wasmvm",
        "true",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    table.mark_exited(exited_pid, 42);

    assert_eq!(
        table
            .waitpid(exited_pid)
            .expect("waitpid should resolve immediately"),
        (exited_pid, 42)
    );
    assert_eq!(table.zombie_timer_count(), 0);
    assert!(
        table.get(exited_pid).is_none(),
        "waitpid should reap already exited processes"
    );
}

#[test]
fn waitpid_for_supports_wnohang_and_waiting_for_any_child() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child_a = MockDriverProcess::new();
    let child_b = MockDriverProcess::new();

    let parent_pid = table.allocate_pid();
    let child_a_pid = table.allocate_pid();
    let child_b_pid = table.allocate_pid();

    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    table.register(
        child_a_pid,
        "wasmvm",
        "child-a",
        Vec::new(),
        create_context(parent_pid),
        child_a,
    );
    table.register(
        child_b_pid,
        "wasmvm",
        "child-b",
        Vec::new(),
        create_context(parent_pid),
        child_b.clone(),
    );

    assert_eq!(
        table
            .waitpid_for(parent_pid, -1, WaitPidFlags::WNOHANG)
            .expect("wnohang wait should succeed"),
        None
    );

    child_b.exit(27);
    assert_eq!(
        table
            .waitpid_for(parent_pid, -1, WaitPidFlags::empty())
            .expect("wait for any child should succeed"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: child_b_pid,
            status: 27,
            event: ProcessWaitEvent::Exited,
        })
    );
    assert!(
        table.get(child_b_pid).is_none(),
        "waited child should be reaped"
    );
    assert!(
        table.get(child_a_pid).is_some(),
        "other matching children should remain"
    );
}

#[test]
fn on_process_exit_runs_before_waitpid_waiters_are_notified() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let process = MockDriverProcess::new();
    let pid = table.allocate_pid();
    table.register(
        pid,
        "wasmvm",
        "sleep",
        vec![String::from("1")],
        create_context(0),
        process.clone(),
    );

    let (callback_entered_tx, callback_entered_rx) = mpsc::channel();
    let callback_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let callback_gate_for_exit = Arc::clone(&callback_gate);
    table.set_on_process_exit(Some(Arc::new(move |_| {
        callback_entered_tx
            .send(())
            .expect("callback should report entry");
        let (released, wake) = &*callback_gate_for_exit;
        let mut released = released.lock().expect("callback gate lock poisoned");
        while !*released {
            released = wake
                .wait(released)
                .expect("callback gate wait lock poisoned");
        }
    })));

    let waiter_table = table.clone();
    let (wait_result_tx, wait_result_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let result = waiter_table.waitpid(pid).expect("waitpid should resolve");
        wait_result_tx
            .send(result)
            .expect("waiter should report exit result");
    });

    thread::sleep(Duration::from_millis(10));
    let process_for_exit = process.clone();
    let exit_handle = thread::spawn(move || {
        process_for_exit.exit(0);
    });

    callback_entered_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("exit callback should run");
    assert!(wait_result_rx.try_recv().is_err());

    let (released, wake) = &*callback_gate;
    *released.lock().expect("callback gate lock poisoned") = true;
    wake.notify_all();
    assert_eq!(
        wait_result_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("waitpid should resolve after callback"),
        (pid, 0)
    );
    exit_handle.join().expect("exit thread should finish");
    waiter.join().expect("waiter thread should finish");
}

#[test]
fn waitpid_for_reports_stopped_and_continued_children_once() {
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
        child,
    );

    table.mark_stopped(child_pid, SIGSTOP);
    assert_eq!(
        table
            .waitpid_for(parent_pid, child_pid as i32, WaitPidFlags::WNOHANG)
            .expect("stopped child lookup should succeed"),
        None
    );
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                child_pid as i32,
                WaitPidFlags::WNOHANG | WaitPidFlags::WUNTRACED,
            )
            .expect("wuntraced wait should succeed"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: child_pid,
            status: SIGSTOP,
            event: ProcessWaitEvent::Stopped,
        })
    );
    assert_eq!(
        table
            .get(child_pid)
            .expect("child remains registered")
            .status,
        ProcessStatus::Stopped
    );

    table.mark_continued(child_pid);
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                child_pid as i32,
                WaitPidFlags::WNOHANG | WaitPidFlags::WCONTINUED,
            )
            .expect("wcontinued wait should succeed"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: child_pid,
            status: SIGCONT,
            event: ProcessWaitEvent::Continued,
        })
    );
    assert_eq!(
        table
            .get(child_pid)
            .expect("child remains registered")
            .status,
        ProcessStatus::Running
    );
    assert_eq!(parent.kills(), vec![SIGCHLD, SIGCHLD]);
}

#[test]
fn kill_routes_signals_and_validates_process_existence() {
    let table = ProcessTable::new();
    let process = MockDriverProcess::new();
    let pid = table.allocate_pid();
    table.register(
        pid,
        "wasmvm",
        "sleep",
        vec![String::from("1")],
        create_context(0),
        process.clone(),
    );

    table
        .kill(pid as i32, 0)
        .expect("signal 0 is an existence check");
    assert!(process.kills().is_empty());

    table
        .kill(pid as i32, 15)
        .expect("signal should be delivered");
    assert_eq!(process.kills(), vec![15]);

    assert_error_code(table.kill(999, 15), "ESRCH");
    assert_error_code(table.kill(pid as i32, -1), "EINVAL");
    assert_error_code(table.kill(pid as i32, 100), "EINVAL");
}

#[test]
fn kill_updates_job_control_state_for_stop_and_continue_signals() {
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

    table
        .kill(child_pid as i32, SIGTSTP)
        .expect("SIGTSTP should stop the child");
    assert_eq!(child.kills(), vec![SIGTSTP]);
    assert_eq!(
        table
            .get(child_pid)
            .expect("child remains registered")
            .status,
        ProcessStatus::Stopped
    );
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                child_pid as i32,
                WaitPidFlags::WNOHANG | WaitPidFlags::WUNTRACED,
            )
            .expect("stopped child wait should succeed"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: child_pid,
            status: SIGTSTP,
            event: ProcessWaitEvent::Stopped,
        })
    );

    table
        .kill(child_pid as i32, SIGCONT)
        .expect("SIGCONT should continue the child");
    assert_eq!(child.kills(), vec![SIGTSTP, SIGCONT]);
    assert_eq!(
        table
            .get(child_pid)
            .expect("child remains registered")
            .status,
        ProcessStatus::Running
    );
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                child_pid as i32,
                WaitPidFlags::WNOHANG | WaitPidFlags::WCONTINUED,
            )
            .expect("continued child wait should succeed"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: child_pid,
            status: SIGCONT,
            event: ProcessWaitEvent::Continued,
        })
    );
    assert_eq!(parent.kills(), vec![SIGCHLD, SIGCHLD]);
}

#[test]
fn exiting_child_delivers_sigchld_to_living_parent() {
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

    child.exit(0);

    wait_for(
        || parent.kills() == vec![SIGCHLD],
        Duration::from_millis(100),
    );
    assert_eq!(
        table.waitpid(child_pid).expect("reap child"),
        (child_pid, 0)
    );
}

#[test]
fn killed_child_delivers_sigchld_to_living_parent() {
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

    table
        .kill(child_pid as i32, 15)
        .expect("deliver SIGTERM to child");

    wait_for(
        || parent.kills() == vec![SIGCHLD],
        Duration::from_millis(100),
    );
    assert_eq!(
        table.waitpid(child_pid).expect("reap killed child"),
        (child_pid, 143)
    );
}

#[test]
fn process_groups_and_sessions_follow_legacy_rules() {
    let table = ProcessTable::new();

    let p1 = table.allocate_pid();
    let p2 = table.allocate_pid();
    let p3 = table.allocate_pid();
    let p4 = table.allocate_pid();

    table.register(
        p1,
        "wasmvm",
        "sh",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    table.register(
        p2,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(p1),
        MockDriverProcess::new(),
    );
    table.register(
        p3,
        "wasmvm",
        "peer",
        Vec::new(),
        create_context(p1),
        MockDriverProcess::new(),
    );
    table.register(
        p4,
        "wasmvm",
        "other",
        Vec::new(),
        create_context(p1),
        MockDriverProcess::new(),
    );

    table
        .setpgid(p2, 0)
        .expect("process can create its own group");
    table
        .setpgid(p3, p2)
        .expect("peer can join an existing group in the same session");
    assert_eq!(table.getpgid(p2).expect("pgid"), p2);
    assert_eq!(table.getpgid(p3).expect("pgid"), p2);
    assert!(table.has_process_group(p2));

    table.setsid(p4).expect("child can become a session leader");
    assert_eq!(table.getsid(p4).expect("sid"), p4);
    assert_error_code(table.setpgid(p3, p4), "EPERM");
}

#[test]
fn negative_pid_kill_targets_entire_process_groups() {
    let table = ProcessTable::new();
    let leader = MockDriverProcess::new();
    let peer = MockDriverProcess::new();
    let pid1 = table.allocate_pid();
    let pid2 = table.allocate_pid();

    table.register(
        pid1,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(0),
        leader.clone(),
    );
    table.register(
        pid2,
        "wasmvm",
        "peer",
        Vec::new(),
        create_context(pid1),
        peer.clone(),
    );
    table.setpgid(pid2, pid1).expect("peer joins leader group");

    table
        .kill(-(pid1 as i32), 15)
        .expect("group kill should succeed");

    assert_eq!(leader.kills(), vec![15]);
    assert_eq!(peer.kills(), vec![15]);
}

#[test]
fn negative_pid_kill_reaches_stopped_and_exited_group_members() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let init = MockDriverProcess::new();
    let parent = MockDriverProcess::new();
    let leader = MockDriverProcess::stubborn();
    let stopped = MockDriverProcess::stubborn();
    let zombie = MockDriverProcess::stubborn();
    let init_pid = table.allocate_pid();
    let parent_pid = table.allocate_pid();
    let leader_pid = table.allocate_pid();
    let stopped_pid = table.allocate_pid();
    let zombie_pid = table.allocate_pid();

    table.register(
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent,
    );
    table.register(
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(parent_pid),
        leader.clone(),
    );
    table.register(
        stopped_pid,
        "wasmvm",
        "stopped",
        Vec::new(),
        create_context(parent_pid),
        stopped.clone(),
    );
    table.register(
        zombie_pid,
        "wasmvm",
        "zombie",
        Vec::new(),
        create_context(parent_pid),
        zombie.clone(),
    );
    table
        .setpgid(leader_pid, 0)
        .expect("leader becomes process-group leader");
    table
        .setpgid(stopped_pid, leader_pid)
        .expect("stopped peer joins leader group");
    table
        .setpgid(zombie_pid, leader_pid)
        .expect("zombie peer joins leader group");
    table.mark_stopped(stopped_pid, SIGSTOP);
    zombie.exit(23);

    table
        .kill(-(leader_pid as i32), 15)
        .expect("group kill should include stopped and zombie members");

    assert_eq!(leader.kills(), vec![15]);
    assert_eq!(stopped.kills(), vec![15]);
    assert_eq!(zombie.kills(), vec![15]);
}

#[test]
fn exiting_parent_reparents_children_to_pid_one_when_available() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let init = MockDriverProcess::new();
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let init_pid = table.allocate_pid();
    let parent_pid = table.allocate_pid();
    let child_pid = table.allocate_pid();

    table.register(
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent.clone(),
    );
    table.register(
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child,
    );

    parent.exit(0);

    assert_eq!(
        table
            .getppid(child_pid)
            .expect("child should be reparented"),
        1
    );
}

#[test]
fn orphaned_stopped_process_groups_receive_sighup_and_sigcont() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let init = MockDriverProcess::new();
    let parent = MockDriverProcess::new();
    let leader = MockDriverProcess::new();
    let stopped = MockDriverProcess::new();
    let init_pid = table.allocate_pid();
    let parent_pid = table.allocate_pid();
    let leader_pid = table.allocate_pid();
    let stopped_pid = table.allocate_pid();

    table.register(
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent.clone(),
    );
    table.register(
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(parent_pid),
        leader.clone(),
    );
    table.register(
        stopped_pid,
        "wasmvm",
        "stopped",
        Vec::new(),
        create_context(parent_pid),
        stopped.clone(),
    );
    table
        .setpgid(leader_pid, 0)
        .expect("leader becomes process-group leader");
    table
        .setpgid(stopped_pid, leader_pid)
        .expect("stopped peer joins leader group");
    table.mark_stopped(stopped_pid, SIGSTOP);

    parent.exit(0);

    assert_eq!(leader.kills(), vec![SIGHUP, SIGCONT]);
    assert_eq!(stopped.kills(), vec![SIGHUP, SIGCONT]);
}

#[test]
fn terminate_all_escalates_from_sigterm_to_sigkill_for_survivors() {
    let table = ProcessTable::new();
    let graceful = MockDriverProcess::new();
    let stubborn = MockDriverProcess::stubborn();

    let pid1 = table.allocate_pid();
    let pid2 = table.allocate_pid();
    table.register(
        pid1,
        "wasmvm",
        "graceful",
        Vec::new(),
        create_context(0),
        graceful.clone(),
    );
    table.register(
        pid2,
        "wasmvm",
        "stubborn",
        Vec::new(),
        create_context(0),
        stubborn.clone(),
    );

    table.terminate_all();

    assert_eq!(graceful.kills(), vec![15]);
    assert_eq!(stubborn.kills(), vec![15, 9]);
    assert_eq!(
        table
            .get(pid1)
            .expect("graceful process should remain as zombie")
            .status,
        ProcessStatus::Exited
    );
    assert_eq!(
        table
            .get(pid2)
            .expect("stubborn process should remain as zombie")
            .status,
        ProcessStatus::Exited
    );
    assert_eq!(table.zombie_timer_count(), 0);
}

#[test]
fn list_processes_returns_a_snapshot_of_registered_processes() {
    let table = ProcessTable::new();
    let pid1 = table.allocate_pid();
    let pid2 = table.allocate_pid();

    table.register(
        pid1,
        "wasmvm",
        "ls",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    table.register(
        pid2,
        "node",
        "node",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );

    let processes = table.list_processes();
    assert_eq!(processes.len(), 2);
    assert_eq!(processes.get(&pid1).expect("process info").command, "ls");
    assert_eq!(processes.get(&pid2).expect("process info").driver, "node");
}

#[test]
fn waitpid_rejects_unknown_processes() {
    let table = ProcessTable::new();
    assert_error_code(table.waitpid(9999), "ESRCH");
}

#[test]
fn waitpid_for_supports_pid_zero_and_negative_process_group_selectors() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let same_group_child = MockDriverProcess::new();
    let other_group_child = MockDriverProcess::new();

    let parent_pid = table.allocate_pid();
    let same_group_child_pid = table.allocate_pid();
    let other_group_child_pid = table.allocate_pid();

    table.register(
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    table.register(
        same_group_child_pid,
        "wasmvm",
        "same-group",
        Vec::new(),
        create_context(parent_pid),
        same_group_child.clone(),
    );
    table.register(
        other_group_child_pid,
        "wasmvm",
        "other-group",
        Vec::new(),
        create_context(parent_pid),
        other_group_child.clone(),
    );
    table
        .setpgid(other_group_child_pid, 0)
        .expect("child should become group leader");

    other_group_child.exit(13);
    assert_eq!(
        table
            .waitpid_for(parent_pid, 0, WaitPidFlags::WNOHANG)
            .expect("pid=0 wait should succeed"),
        None
    );

    same_group_child.exit(11);
    assert_eq!(
        table
            .waitpid_for(parent_pid, 0, WaitPidFlags::empty())
            .expect("pid=0 wait should reap same-group child"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: same_group_child_pid,
            status: 11,
            event: ProcessWaitEvent::Exited,
        })
    );
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                -(other_group_child_pid as i32),
                WaitPidFlags::empty(),
            )
            .expect("negative pgid wait should reap matching child"),
        Some(agent_os_kernel::process_table::ProcessWaitResult {
            pid: other_group_child_pid,
            status: 13,
            event: ProcessWaitEvent::Exited,
        })
    );
}

#[test]
fn zombie_reaper_uses_a_single_worker_for_many_exits() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_millis(100));
    let mut pids = Vec::new();

    for index in 0..100 {
        let process = MockDriverProcess::new();
        let pid = table.allocate_pid();
        table.register(
            pid,
            "wasmvm",
            format!("proc-{index}"),
            Vec::new(),
            create_context(0),
            process.clone(),
        );
        process.exit(0);
        pids.push(pid);
    }

    assert_eq!(table.zombie_reaper_thread_spawn_count(), 1);
    assert_eq!(table.zombie_timer_count(), 100);

    wait_for(|| table.zombie_timer_count() == 0, Duration::from_secs(2));

    for pid in pids {
        assert!(table.get(pid).is_none(), "process {pid} should be reaped");
    }
}

#[test]
fn zombie_reaper_preserves_child_exit_code_while_parent_is_alive() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_millis(50));
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
        parent,
    );
    table.register(
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );

    child.exit(41);
    thread::sleep(Duration::from_millis(200));

    assert_eq!(
        table
            .waitpid(child_pid)
            .expect("child exit code should be preserved"),
        (child_pid, 41)
    );
}

#[test]
fn zombie_reaper_reaps_exited_children_after_their_parent_exits() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_millis(50));
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

    child.exit(17);
    thread::sleep(Duration::from_millis(120));
    parent.exit(0);

    wait_for(
        || table.get(parent_pid).is_none() && table.get(child_pid).is_none(),
        Duration::from_secs(1),
    );
}
