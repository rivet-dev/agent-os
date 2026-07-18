use agentos_kernel::process_runtime::{
    ProcessControlRequest, ProcessExit, ProcessRuntimeEndpoint, ProcessRuntimeEndpointError,
    ProcessRuntimeIdentity, ProcessTermination,
};
use agentos_kernel::process_table::{
    ProcessContext, ProcessEntry, ProcessResult, ProcessStatus, ProcessTable, ProcessWaitEvent,
    SigmaskHow, SignalAction, SignalDisposition, SignalSet, WaitPidFlags, SIGCHLD, SIGCONT, SIGHUP,
    SIGSTOP, SIGTERM, SIGTSTP,
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
    binding: Option<(ProcessTable, u32)>,
    ignore_sigterm: bool,
}

#[derive(Default)]
struct MockDriverProcess {
    state: Mutex<MockProcessState>,
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
        })
    }

    fn bind(&self, table: &ProcessTable, pid: u32) {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .binding = Some((table.clone(), pid));
    }

    fn schedule_exit(self: &Arc<Self>, delay: Duration, exit_code: i32) {
        let process = Arc::clone(self);
        thread::spawn(move || {
            thread::sleep(delay);
            process.exit(exit_code);
        });
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
                .expect("mock process exit must be reported");
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

impl ProcessRuntimeEndpoint for MockDriverProcess {
    fn identity(&self) -> Option<ProcessRuntimeIdentity> {
        None
    }

    fn request_control(
        &self,
        request: ProcessControlRequest,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        let (binding, stop_transition, termination) = {
            let mut state = self.state.lock().expect("mock process lock poisoned");
            let signal = match request {
                ProcessControlRequest::Checkpoint => state
                    .binding
                    .as_ref()
                    .and_then(|(table, pid)| table.sigpending(*pid).ok())
                    .and_then(|pending| pending.signals().into_iter().next()),
                ProcessControlRequest::Stop { signal } => Some(signal),
                ProcessControlRequest::Continue => Some(SIGCONT),
                ProcessControlRequest::Terminate(ProcessTermination::Signal { signal, .. }) => {
                    Some(signal)
                }
                ProcessControlRequest::Terminate(ProcessTermination::RuntimeFault)
                | ProcessControlRequest::Cancel(_) => None,
            };
            if let Some(signal) = signal {
                state.kills.push(signal);
            }
            let termination = match request {
                ProcessControlRequest::Terminate(ProcessTermination::Signal { signal, .. })
                    if signal == 9 || (signal == SIGTERM && !state.ignore_sigterm) =>
                {
                    Some(ProcessExit::Signaled {
                        signal,
                        core_dumped: false,
                    })
                }
                ProcessControlRequest::Terminate(ProcessTermination::RuntimeFault)
                | ProcessControlRequest::Cancel(_) => Some(ProcessExit::Exited(1)),
                _ => None,
            };
            let stop_transition = match request {
                ProcessControlRequest::Stop { signal } => Some((true, Some(signal))),
                ProcessControlRequest::Continue => Some((false, None)),
                _ => None,
            };
            (state.binding.clone(), stop_transition, termination)
        };

        if let Some((table, pid)) = binding {
            if let Some((stopped, signal)) = stop_transition {
                if stopped {
                    table
                        .mark_stopped(pid, signal.expect("stop transition signal"))
                        .expect("mock stop transition must be recorded");
                } else {
                    table
                        .mark_continued(pid)
                        .expect("mock continue transition must be recorded");
                }
            }
            if let Some(termination) = termination {
                table
                    .report_exit(pid, termination)
                    .expect("mock termination must be reported");
            }
        }
        Ok(())
    }
}

fn register(
    table: &ProcessTable,
    pid: u32,
    driver: impl Into<String>,
    command: impl Into<String>,
    args: Vec<String>,
    context: ProcessContext,
    process: Arc<MockDriverProcess>,
) -> ProcessEntry {
    let entry = ProcessTable::register(table, pid, driver, command, args, context, process.clone());
    process.bind(table, pid);
    entry
}

fn catch_signal(table: &ProcessTable, pid: u32, signal: i32) {
    table
        .signal_action(
            pid,
            signal,
            Some(SignalAction {
                disposition: SignalDisposition::User,
                ..SignalAction::DEFAULT
            }),
        )
        .expect("install caught signal");
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

    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);

    let parent_entry = register(
        &table,
        parent_pid,
        "wasmvm",
        "grep",
        vec![String::from("-r"), String::from("foo")],
        create_context(0),
        parent,
    );
    let child_entry = register(
        &table,
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
    let pid = allocate_pid(&table);
    register(
        &table,
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

    let exited_pid = allocate_pid(&table);
    register(
        &table,
        exited_pid,
        "wasmvm",
        "true",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    table
        .mark_exited(exited_pid, 42)
        .expect("exit must be recorded");

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
fn long_lived_parent_retains_zombies_until_waited_under_pressure() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let mut child_pids = Vec::new();

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );

    for index in 0..100 {
        let child = MockDriverProcess::new();
        let child_pid = allocate_pid(&table);
        register(
            &table,
            child_pid,
            "wasmvm",
            format!("child-{index}"),
            Vec::new(),
            create_context(parent_pid),
            child.clone(),
        );
        child.exit(index);
        child_pids.push((child_pid, index));
    }

    for (child_pid, _) in &child_pids {
        assert_eq!(
            table
                .get(*child_pid)
                .expect("child zombie should be retained")
                .status,
            ProcessStatus::Exited
        );
    }
    assert_eq!(table.zombie_timer_count(), child_pids.len());

    for (child_pid, status) in child_pids {
        assert_eq!(
            table
                .waitpid_for(parent_pid, -1, WaitPidFlags::empty())
                .expect("parent wait should succeed"),
            Some(agentos_kernel::process_table::ProcessWaitResult {
                pid: child_pid,
                status,
                event: ProcessWaitEvent::Exited,
            })
        );
    }
    assert_eq!(table.zombie_timer_count(), 0);
}

#[test]
fn allocate_pid_wraps_without_reusing_live_or_zombie_entries() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let max_pid = i32::MAX as u32;
    let cursor_seed = MockDriverProcess::new();
    let live_high = MockDriverProcess::new();
    let zombie_high = MockDriverProcess::new();
    let live_one = MockDriverProcess::new();

    // Registering max_pid - 2 after the high PIDs moves the public allocation cursor back to max_pid - 1.
    register(
        &table,
        max_pid - 1,
        "wasmvm",
        "live-high",
        Vec::new(),
        create_context(0),
        live_high,
    );
    register(
        &table,
        max_pid,
        "wasmvm",
        "zombie-high",
        Vec::new(),
        create_context(0),
        zombie_high.clone(),
    );
    register(
        &table,
        max_pid - 2,
        "wasmvm",
        "cursor-seed",
        Vec::new(),
        create_context(0),
        cursor_seed,
    );
    register(
        &table,
        1,
        "wasmvm",
        "live-one",
        Vec::new(),
        create_context(0),
        live_one,
    );
    zombie_high.exit(0);

    assert_eq!(
        table
            .get(max_pid)
            .expect("zombie high PID should remain allocated")
            .status,
        ProcessStatus::Exited
    );

    assert_eq!(table.allocate_pid().expect("allocate wrapped pid"), 2);
    assert_eq!(table.allocate_pid().expect("allocate next pid"), 3);
}

#[test]
fn waitpid_for_supports_wnohang_and_waiting_for_any_child() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child_a = MockDriverProcess::new();
    let child_b = MockDriverProcess::new();

    let parent_pid = allocate_pid(&table);
    let child_a_pid = allocate_pid(&table);
    let child_b_pid = allocate_pid(&table);

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    register(
        &table,
        child_a_pid,
        "wasmvm",
        "child-a",
        Vec::new(),
        create_context(parent_pid),
        child_a,
    );
    register(
        &table,
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
        Some(agentos_kernel::process_table::ProcessWaitResult {
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
    let pid = allocate_pid(&table);
    register(
        &table,
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

    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child,
    );
    catch_signal(&table, parent_pid, SIGCHLD);

    table
        .mark_stopped(child_pid, SIGSTOP)
        .expect("stop must be recorded");
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
        Some(agentos_kernel::process_table::ProcessWaitResult {
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

    table
        .mark_continued(child_pid)
        .expect("continue must be recorded");
    assert_eq!(
        table
            .waitpid_for(
                parent_pid,
                child_pid as i32,
                WaitPidFlags::WNOHANG | WaitPidFlags::WCONTINUED,
            )
            .expect("wcontinued wait should succeed"),
        Some(agentos_kernel::process_table::ProcessWaitResult {
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
fn nonterminal_wait_query_never_reaps_an_exited_child() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );

    table
        .mark_stopped(child_pid, SIGSTOP)
        .expect("stop must be recorded");
    child.exit(17);
    assert_eq!(
        table
            .take_nonterminal_wait_event_for(parent_pid, child_pid as i32, WaitPidFlags::WUNTRACED,)
            .expect("nonterminal wait should succeed"),
        None,
        "terminal state must supersede an unconsumed stop notification"
    );
    assert_eq!(
        table
            .get(child_pid)
            .expect("transition-only query must preserve zombie")
            .status,
        ProcessStatus::Exited
    );
    assert_eq!(
        table
            .waitpid_for(parent_pid, child_pid as i32, WaitPidFlags::WNOHANG)
            .expect("terminal wait should succeed"),
        Some(agentos_kernel::process_table::ProcessWaitResult {
            pid: child_pid,
            status: 17,
            event: ProcessWaitEvent::Exited,
        })
    );
    assert!(table.get(child_pid).is_none());
}

#[test]
fn detailed_wait_preserves_exact_signaled_termination() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child,
    );

    let termination = ProcessExit::Signaled {
        signal: SIGTERM,
        core_dumped: true,
    };
    table
        .report_exit(child_pid, termination)
        .expect("termination must be reported");
    let transition = table
        .waitpid_for_detailed(parent_pid, child_pid as i32, WaitPidFlags::WNOHANG)
        .expect("detailed wait should succeed")
        .expect("terminal transition should be ready");
    assert_eq!(transition.result.pid, child_pid);
    assert_eq!(transition.result.status, 128 + SIGTERM);
    assert_eq!(transition.result.event, ProcessWaitEvent::Exited);
    assert_eq!(transition.termination, Some(termination));
    assert!(table.get(child_pid).is_none());
}

#[test]
fn kill_routes_signals_and_validates_process_existence() {
    let table = ProcessTable::new();
    let process = MockDriverProcess::new();
    let pid = allocate_pid(&table);
    register(
        &table,
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

    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );
    catch_signal(&table, parent_pid, SIGCHLD);

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
        Some(agentos_kernel::process_table::ProcessWaitResult {
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
        Some(agentos_kernel::process_table::ProcessWaitResult {
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
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );
    catch_signal(&table, parent_pid, SIGCHLD);

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
fn blocked_sigchld_is_queued_until_the_parent_unblocks_it() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    let sigchld_mask = SignalSet::from_signal(SIGCHLD).expect("SIGCHLD should be valid");

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );
    catch_signal(&table, parent_pid, SIGCHLD);

    assert_eq!(
        table
            .sigprocmask(parent_pid, SigmaskHow::Block, sigchld_mask)
            .expect("block SIGCHLD"),
        SignalSet::empty()
    );

    child.exit(0);

    wait_for(
        || {
            table
                .get(child_pid)
                .is_some_and(|entry| entry.status == ProcessStatus::Exited)
        },
        Duration::from_millis(100),
    );
    assert!(parent.kills().is_empty(), "SIGCHLD should remain pending");
    assert_eq!(
        table.sigpending(parent_pid).expect("pending signals"),
        sigchld_mask
    );

    table
        .sigprocmask(parent_pid, SigmaskHow::Unblock, sigchld_mask)
        .expect("unblock SIGCHLD");

    wait_for(
        || parent.kills() == vec![SIGCHLD],
        Duration::from_millis(100),
    );
    assert_eq!(
        table.sigpending(parent_pid).expect("pending signals"),
        sigchld_mask,
        "checkpoint wake does not consume a caught signal"
    );
    let delivery = table
        .begin_signal_delivery(parent_pid)
        .expect("begin signal delivery")
        .expect("caught SIGCHLD delivery");
    assert_eq!(delivery.signal, SIGCHLD);
    table
        .end_signal_delivery(parent_pid, delivery.token)
        .expect("finish signal delivery");
    assert_eq!(
        table.sigpending(parent_pid).expect("pending signals"),
        SignalSet::empty()
    );
}

#[test]
fn killed_child_delivers_sigchld_to_living_parent() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
        child_pid,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(parent_pid),
        child.clone(),
    );
    catch_signal(&table, parent_pid, SIGCHLD);

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
fn blocked_sigterm_is_delivered_when_the_process_unblocks_it() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let process = MockDriverProcess::new();
    let pid = allocate_pid(&table);
    let sigterm_mask = SignalSet::from_signal(SIGTERM).expect("SIGTERM should be valid");

    register(
        &table,
        pid,
        "wasmvm",
        "sleep",
        Vec::new(),
        create_context(0),
        process.clone(),
    );

    table
        .sigprocmask(pid, SigmaskHow::Block, sigterm_mask)
        .expect("block SIGTERM");
    table
        .kill(pid as i32, SIGTERM)
        .expect("queue blocked SIGTERM");

    assert!(
        process.kills().is_empty(),
        "blocked SIGTERM should not deliver"
    );
    assert_eq!(
        table.sigpending(pid).expect("pending signals"),
        sigterm_mask
    );

    table
        .sigprocmask(pid, SigmaskHow::Unblock, sigterm_mask)
        .expect("unblock SIGTERM");

    wait_for(
        || process.kills() == vec![SIGTERM],
        Duration::from_millis(100),
    );
    assert_eq!(table.waitpid(pid).expect("reap SIGTERM exit"), (pid, 143));
}

#[test]
fn process_groups_and_sessions_follow_legacy_rules() {
    let table = ProcessTable::new();

    let p1 = allocate_pid(&table);
    let p2 = allocate_pid(&table);
    let p3 = allocate_pid(&table);
    let p4 = allocate_pid(&table);

    register(
        &table,
        p1,
        "wasmvm",
        "sh",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    register(
        &table,
        p2,
        "wasmvm",
        "child",
        Vec::new(),
        create_context(p1),
        MockDriverProcess::new(),
    );
    register(
        &table,
        p3,
        "wasmvm",
        "peer",
        Vec::new(),
        create_context(p1),
        MockDriverProcess::new(),
    );
    register(
        &table,
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
    let pid1 = allocate_pid(&table);
    let pid2 = allocate_pid(&table);

    register(
        &table,
        pid1,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(0),
        leader.clone(),
    );
    register(
        &table,
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
fn negative_pid_signal_zero_checks_process_group_liveness() {
    let table = ProcessTable::new();
    let leader = MockDriverProcess::new();
    let peer = MockDriverProcess::new();
    let leader_pid = allocate_pid(&table);
    let peer_pid = allocate_pid(&table);

    register(
        &table,
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(0),
        leader.clone(),
    );
    register(
        &table,
        peer_pid,
        "wasmvm",
        "peer",
        Vec::new(),
        create_context(leader_pid),
        peer.clone(),
    );
    table
        .setpgid(peer_pid, leader_pid)
        .expect("peer joins leader group");

    table
        .kill(-(leader_pid as i32), 0)
        .expect("signal 0 should check process group liveness");

    assert!(leader.kills().is_empty());
    assert!(peer.kills().is_empty());
    assert_error_code(table.kill(-999, 0), "ESRCH");
}

#[test]
fn negative_pid_kill_reaches_stopped_but_not_exited_group_members() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let init = MockDriverProcess::new();
    let parent = MockDriverProcess::new();
    let leader = MockDriverProcess::stubborn();
    let stopped = MockDriverProcess::stubborn();
    let zombie = MockDriverProcess::stubborn();
    let init_pid = allocate_pid(&table);
    let parent_pid = allocate_pid(&table);
    let leader_pid = allocate_pid(&table);
    let stopped_pid = allocate_pid(&table);
    let zombie_pid = allocate_pid(&table);

    register(
        &table,
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent,
    );
    register(
        &table,
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(parent_pid),
        leader.clone(),
    );
    register(
        &table,
        stopped_pid,
        "wasmvm",
        "stopped",
        Vec::new(),
        create_context(parent_pid),
        stopped.clone(),
    );
    register(
        &table,
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
    table
        .mark_stopped(stopped_pid, SIGSTOP)
        .expect("stop must be recorded");
    zombie.exit(23);

    table
        .kill(-(leader_pid as i32), 15)
        .expect("group kill should include live stopped members");

    assert_eq!(leader.kills(), vec![15]);
    assert_eq!(stopped.kills(), vec![15]);
    assert!(zombie.kills().is_empty());
}

#[test]
fn exiting_parent_reparents_children_to_pid_one_when_available() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
    let init = MockDriverProcess::new();
    let parent = MockDriverProcess::new();
    let child = MockDriverProcess::new();
    let init_pid = allocate_pid(&table);
    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);

    register(
        &table,
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent.clone(),
    );
    register(
        &table,
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
    let init_pid = allocate_pid(&table);
    let parent_pid = allocate_pid(&table);
    let leader_pid = allocate_pid(&table);
    let stopped_pid = allocate_pid(&table);

    register(
        &table,
        init_pid,
        "wasmvm",
        "init",
        Vec::new(),
        create_context(0),
        init,
    );
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(init_pid),
        parent.clone(),
    );
    register(
        &table,
        leader_pid,
        "wasmvm",
        "leader",
        Vec::new(),
        create_context(parent_pid),
        leader.clone(),
    );
    register(
        &table,
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
    table
        .mark_stopped(stopped_pid, SIGSTOP)
        .expect("stop must be recorded");

    parent.exit(0);

    assert_eq!(leader.kills(), vec![SIGHUP, SIGCONT]);
    assert_eq!(stopped.kills(), vec![SIGHUP, SIGCONT]);
}

#[test]
fn terminate_all_escalates_from_sigterm_to_sigkill_for_survivors() {
    let table = ProcessTable::new();
    let graceful = MockDriverProcess::new();
    let stubborn = MockDriverProcess::stubborn();
    let stopped = MockDriverProcess::new();

    let pid1 = allocate_pid(&table);
    let pid2 = allocate_pid(&table);
    let pid3 = allocate_pid(&table);
    register(
        &table,
        pid1,
        "wasmvm",
        "graceful",
        Vec::new(),
        create_context(0),
        graceful.clone(),
    );
    register(
        &table,
        pid2,
        "wasmvm",
        "stubborn",
        Vec::new(),
        create_context(0),
        stubborn.clone(),
    );
    register(
        &table,
        pid3,
        "wasmvm",
        "stopped",
        Vec::new(),
        create_context(0),
        stopped.clone(),
    );
    table
        .mark_stopped(pid3, SIGSTOP)
        .expect("stop must be recorded");

    table.terminate_all();

    assert_eq!(graceful.kills(), vec![15]);
    assert_eq!(stubborn.kills(), vec![15, 9]);
    assert_eq!(stopped.kills(), vec![15]);
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
    assert_eq!(
        table
            .get(pid3)
            .expect("stopped process should remain as zombie")
            .status,
        ProcessStatus::Exited
    );
    assert_eq!(table.zombie_timer_count(), 0);
}

#[test]
fn list_processes_returns_a_snapshot_of_registered_processes() {
    let table = ProcessTable::new();
    let pid1 = allocate_pid(&table);
    let pid2 = allocate_pid(&table);

    register(
        &table,
        pid1,
        "wasmvm",
        "ls",
        Vec::new(),
        create_context(0),
        MockDriverProcess::new(),
    );
    register(
        &table,
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

    let parent_pid = allocate_pid(&table);
    let same_group_child_pid = allocate_pid(&table);
    let other_group_child_pid = allocate_pid(&table);

    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    register(
        &table,
        same_group_child_pid,
        "wasmvm",
        "same-group",
        Vec::new(),
        create_context(parent_pid),
        same_group_child.clone(),
    );
    register(
        &table,
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
        Some(agentos_kernel::process_table::ProcessWaitResult {
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
        Some(agentos_kernel::process_table::ProcessWaitResult {
            pid: other_group_child_pid,
            status: 13,
            event: ProcessWaitEvent::Exited,
        })
    );
}

#[test]
fn zombie_reaper_is_cooperatively_driven_for_many_exits() {
    let table = ProcessTable::with_zombie_ttl(Duration::from_millis(100));
    let mut pids = Vec::new();

    for index in 0..100 {
        let process = MockDriverProcess::new();
        let pid = allocate_pid(&table);
        register(
            &table,
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

    assert_eq!(table.zombie_timer_count(), 100);
    assert!(
        table
            .next_zombie_reap_deadline()
            .is_some_and(|deadline| deadline <= Instant::now() + Duration::from_millis(100)),
        "runtime adapter should be able to arm the earliest cooperative reap deadline"
    );

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

    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent,
    );
    register(
        &table,
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

    let parent_pid = allocate_pid(&table);
    let child_pid = allocate_pid(&table);
    register(
        &table,
        parent_pid,
        "wasmvm",
        "parent",
        Vec::new(),
        create_context(0),
        parent.clone(),
    );
    register(
        &table,
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
