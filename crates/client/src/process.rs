//! Process execution & management methods + supporting types.
//!
//! Ported from `packages/core/src/agent-os.ts` (process methods) and `runtime-compat.ts`
//! (`ExecOptions`, `ExecResult`, `ProcessInfo`, etc.).
//!
//! Two distinct process views: SDK-spawned processes (`processes` map, keyed by user-facing pid)
//! back `spawn` + the stdin/stdout/stderr/exit subscriptions + `wait/list/get/stop/kill`; the kernel
//! process table backs `exec`, `all_processes`, `process_tree`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use scc::HashMap as SccHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use agentos_sidecar_client::wire::{self, EventPayload, ProcessSnapshotStatus, StreamChannel};

use crate::agent_os::{AgentOs, ProcessEntry};
use crate::command_line::resolve_exec_command;
use crate::error::ClientError;
use crate::stream::Subscription;

/// Broadcast channel capacity for a spawned process's stdout/stderr fan-out.
const PROCESS_STREAM_CAPACITY: usize = 1024;

/// Maximum SDK-spawned process entries retained per VM.
const PROCESS_REGISTRY_LIMIT: usize = 1024;

/// Maximum first-observed process timestamp entries retained per VM.
const OBSERVED_PROCESS_TIME_LIMIT: usize = 4096;

/// Maximum bytes captured by `exec` across stdout and stderr.
const EXEC_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Default guest working directory for `exec`/`spawn`, matching the TS sidecar client.
pub(crate) const DEFAULT_EXEC_CWD: &str = "/workspace";

/// Base value for the synthetic display-pid sequence used by `spawn` (TS `SYNTHETIC_PID_BASE`). The
/// first spawned process is assigned exactly this value.
pub(crate) const SYNTHETIC_PID_BASE: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Timing-mitigation mode for an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimingMitigation {
    #[default]
    Off,
    Freeze,
}

/// `stdin` value: a string or raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinInput {
    Text(String),
    Bytes(Vec<u8>),
}

/// A raw-byte streaming callback for stdout/stderr (TS `(data: Uint8Array) => void`). Invoked once
/// per output chunk as it arrives. Never assume UTF-8: chunks are delivered as raw bytes.
pub type OutputCallback = Box<dyn FnMut(&[u8]) + Send>;

/// Base options shared by `exec` and `spawn`.
///
/// `on_stdout`/`on_stderr` mirror the TS `ExecOptions.onStdout`/`onStderr` raw-byte streaming
/// callbacks. For `exec` they fire for the duration of the call; for `spawn` they are seeded into the
/// stdout/stderr fan-out at spawn time (matching the TS initial-handler-set behavior).
pub struct ExecOptions {
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub stdin: Option<StdinInput>,
    pub timeout: Option<f64>,
    pub on_stdout: Option<OutputCallback>,
    pub on_stderr: Option<OutputCallback>,
    pub capture_stdio: Option<bool>,
    pub file_path: Option<String>,
    pub cpu_time_limit_ms: Option<f64>,
    pub timing_mitigation: Option<TimingMitigation>,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            cwd: Some(DEFAULT_EXEC_CWD.to_string()),
            stdin: None,
            timeout: None,
            on_stdout: None,
            on_stderr: None,
            capture_stdio: None,
            file_path: None,
            cpu_time_limit_ms: None,
            timing_mitigation: None,
        }
    }
}

/// Result of `exec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `stdio` mode for a spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnStdio {
    #[default]
    Pipe,
    Inherit,
}

/// Callback-free options for portable `spawn`.
#[derive(Default)]
pub struct SpawnOptions {
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub stdio: Option<SpawnStdio>,
    pub stdin_fd: Option<i32>,
    pub stdout_fd: Option<i32>,
    pub stderr_fd: Option<i32>,
    pub stream_stdin: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub pid: u32,
    pub stream: ProcessStream,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExit {
    pub pid: u32,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
}

/// Public JSON info for SDK-spawned processes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedProcessInfo {
    pub pid: u32,
    pub command: String,
    pub args: Vec<String>,
    pub running: bool,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    /// Epoch milliseconds when `spawn` registered the process.
    #[serde(rename = "startedAt")]
    pub started_at: i64,
}

/// The pid returned by `spawn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnHandle {
    pub pid: u32,
}

/// Process status from the kernel process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessStatus {
    Running,
    Exited,
}

/// Full kernel process info (TS `KernelProcessInfo`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub driver: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub status: ProcessStatus,
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "exitTime")]
    pub exit_time: Option<f64>,
}

/// A node in the process forest (`ProcessInfo` + children).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessTreeNode {
    #[serde(flatten)]
    pub info: ProcessInfo,
    pub children: Vec<ProcessTreeNode>,
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

impl AgentOs {
    /// Run a command to completion. The wire `Execute` request starts the process and returns a
    /// process id immediately; stdout/stderr are accumulated and the call resolves once the matching
    /// `ProcessExited` event arrives. This mirrors the TS pass-through to `kernel.exec` semantically:
    /// the result is the full captured stdout/stderr plus exit code.
    pub async fn exec_process(&self, command: &str, options: ExecOptions) -> Result<ExecResult> {
        // Parse the command line into a `(command, args)` pair the same way the sidecar's
        // child_process path does: shell-free argv lists spawn directly (preserving the command's
        // real exit code), while shell syntax or a builtin head runs under `sh -c <line>`.
        let (resolved_command, resolved_args) = resolve_exec_command(command)?;
        self.exec_argv_process(&resolved_command, &resolved_args, options)
            .await
    }

    /// Run a command to completion from an already-structured `(command, args)` argv, bypassing the
    /// `exec` command-line parser. Each `args` element is sent verbatim as a distinct argv element —
    /// no whitespace re-splitting, no shell metacharacter detection, and no routing through
    /// `sh -c`. Callers that already hold a structured argv (for example the cron `Exec` action)
    /// must use this so the structured-argv contract is preserved end to end.
    pub async fn exec_argv_process(
        &self,
        command: &str,
        args: &[String],
        mut options: ExecOptions,
    ) -> Result<ExecResult> {
        let process_id = self.next_process_id();

        // Subscribe to events BEFORE issuing the request so no output/exit is missed between the
        // request landing and the subscription being installed.
        let mut events = self.transport().subscribe_wire_events();

        let resolved_command = command.to_owned();
        let resolved_args = args.to_vec();
        let started = self
            .send_execute(
                &process_id,
                Some(resolved_command),
                resolved_args,
                options.env.clone(),
                options.cwd.clone(),
            )
            .await
            .context("exec: Execute request failed")?;
        debug_assert_eq!(started.process_id, process_id);

        // Deliver any provided stdin, then close stdin so a non-interactive run observes EOF. This
        // mirrors the TS `runAndCapture` path (`proc.writeStdin(options.stdin); proc.closeStdin()`).
        if let Some(stdin) = options.stdin.take() {
            let chunk = stdin_to_bytes(stdin);
            let ownership = self.vm_scope();
            let _ = self
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::WriteStdinRequest(wire::WriteStdinRequest {
                        process_id: process_id.clone(),
                        chunk,
                    }),
                )
                .await;
        }
        {
            let ownership = self.vm_scope();
            let _ = self
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::CloseStdinRequest(wire::CloseStdinRequest {
                        process_id: process_id.clone(),
                    }),
                )
                .await;
        }

        let mut on_stdout = options.on_stdout.take();
        let mut on_stderr = options.on_stderr.take();

        // A `timeout` (ms) bounds the run: when it elapses, SIGKILL the process and keep draining
        // until the exit event lands. This mirrors the TS `runAndCapture` timeout race that kills the
        // process and then awaits its exit code.
        let timeout_deadline = options
            .timeout
            .filter(|ms| ms.is_finite() && *ms >= 0.0)
            .map(|ms| {
                tokio::time::Instant::now() + std::time::Duration::from_secs_f64(ms / 1000.0)
            });
        let mut killed_for_timeout = false;

        let capture_stdio = options.capture_stdio.unwrap_or(true);
        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        let mut captured_output_bytes = 0usize;
        let mut capture_error: Option<ClientError> = None;
        let exit_code = loop {
            let recv = events.recv();
            let frame = match timeout_deadline {
                Some(deadline) => {
                    tokio::select! {
                        result = recv => result,
                        _ = tokio::time::sleep_until(deadline), if !killed_for_timeout => {
                            killed_for_timeout = true;
                            self.kill_wire_process(&process_id, "SIGKILL");
                            continue;
                        }
                    }
                }
                None => recv.await,
            };
            let (_, payload) = match frame {
                Ok(frame) => frame,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ClientError::Sidecar(
                        "exec: event stream closed before process exit".to_owned(),
                    )
                    .into());
                }
            };
            match payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => {
                            if let Some(cb) = on_stdout.as_mut() {
                                cb(&output.chunk);
                            }
                            if capture_stdio && capture_error.is_none() {
                                match append_exec_output(
                                    &mut stdout,
                                    &output.chunk,
                                    &mut captured_output_bytes,
                                    "stdout",
                                ) {
                                    Ok(()) => {}
                                    Err(error) => {
                                        self.kill_wire_process(&process_id, "SIGKILL");
                                        capture_error = Some(error);
                                    }
                                }
                            }
                        }
                        StreamChannel::Stderr => {
                            if let Some(cb) = on_stderr.as_mut() {
                                cb(&output.chunk);
                            }
                            if capture_stdio && capture_error.is_none() {
                                match append_exec_output(
                                    &mut stderr,
                                    &output.chunk,
                                    &mut captured_output_bytes,
                                    "stderr",
                                ) {
                                    Ok(()) => {}
                                    Err(error) => {
                                        self.kill_wire_process(&process_id, "SIGKILL");
                                        capture_error = Some(error);
                                    }
                                }
                            }
                        }
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    break exited.exit_code;
                }
                EventPayload::ProcessOutputEvent(_)
                | EventPayload::ProcessExitedEvent(_)
                | EventPayload::ExecutionOutputEvent(_)
                | EventPayload::ExecutionCompletedEvent(_)
                | EventPayload::VmLifecycleEvent(_)
                | EventPayload::StructuredEvent(_)
                | EventPayload::ExtEnvelope(_) => {}
            }
        };

        if let Some(error) = capture_error {
            return Err(error.into());
        }

        Ok(ExecResult {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    /// Spawn a process. SYNC; returns `{ pid }` only. Installs stdout/stderr fan-out over broadcast
    /// channels and wires exit via a background event-pump task. The user-facing `pid` is the
    /// SDK-allocated map key (the wire `process_id` is held inside the [`ProcessEntry`]).
    pub fn spawn_process(
        &self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
    ) -> Result<SpawnHandle> {
        let registry_guard = self.inner().process_registry_lock.lock();
        self.prune_exited_processes_locked(1);
        if self.process_registry_len_locked() >= PROCESS_REGISTRY_LIMIT {
            return Err(ClientError::Sidecar(format!(
                "process registry limit exceeded: at most {PROCESS_REGISTRY_LIMIT} processes can be tracked per VM"
            ))
            .into());
        }

        // Draw the public pid from the dedicated synthetic-pid space (TS `nextSyntheticPid`), seeded
        // at `SYNTHETIC_PID_BASE`. `exec` uses a separate counter so it never perturbs this sequence.
        let pid = self
            .inner()
            .synthetic_pid_counter
            .fetch_add(1, Ordering::SeqCst) as u32;
        let process_id = format!("proc-{pid}-{}", uuid::Uuid::new_v4());

        let (stdout_tx, _) = broadcast::channel::<Vec<u8>>(PROCESS_STREAM_CAPACITY);
        let (stderr_tx, _) = broadcast::channel::<Vec<u8>>(PROCESS_STREAM_CAPACITY);
        let (output_tx, _) = broadcast::channel::<ProcessOutput>(PROCESS_STREAM_CAPACITY);
        // Seeded `None`; the already-exited branch of `on_process_exit` fires immediately once this
        // watch holds `Some(code)`.
        let (exit_tx, _) = watch::channel::<Option<i32>>(None);
        // Seeded `None`; filled with the kernel pid once the `Execute` response lands so
        // `all_processes`/`process_tree` can remap the kernel snapshot back to this display pid.
        let (kernel_pid_tx, _) = watch::channel::<Option<u32>>(None);

        let entry = ProcessEntry {
            command: command.to_owned(),
            args: args.clone(),
            stdout_tx: stdout_tx.clone(),
            stderr_tx: stderr_tx.clone(),
            output_tx: output_tx.clone(),
            exit_tx: exit_tx.clone(),
            process_id: process_id.clone(),
            kernel_pid: kernel_pid_tx.clone(),
            output_tasks: Vec::new(),
            started_at: epoch_ms_now() as i64,
        };
        // `spawn` is documented as overwriting any prior entry for a freshly allocated pid; the pid
        // is monotonic so a collision is not expected.
        let _ = self.inner().processes.insert(pid, entry);
        drop(registry_guard);

        // Subscribe to events before issuing the request so the pump sees everything.
        let events = self.transport().subscribe_wire_events();

        let this = self.clone();
        let command = command.to_owned();
        tokio::spawn(async move {
            this.run_spawn(
                pid,
                process_id,
                command,
                args,
                options,
                events,
                stdout_tx,
                stderr_tx,
                output_tx,
                exit_tx,
                kernel_pid_tx,
            )
            .await;
        });

        Ok(SpawnHandle { pid })
    }

    /// Write to a spawned process's stdin. SYNC. Errors with `ProcessNotFound`.
    pub fn write_process_stdin(
        &self,
        pid: u32,
        data: StdinInput,
    ) -> std::result::Result<(), ClientError> {
        let process_id = self.lookup_process_id(pid)?;
        let chunk: Vec<u8> = stdin_to_bytes(data);
        let this = self.clone();
        // Fire-and-forget: the TS API is synchronous and does not surface a write error.
        tokio::spawn(async move {
            let ownership = this.vm_scope();
            let _ = this
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::WriteStdinRequest(wire::WriteStdinRequest {
                        process_id,
                        chunk,
                    }),
                )
                .await;
        });
        Ok(())
    }

    /// Close a spawned process's stdin. SYNC. Errors with `ProcessNotFound`.
    pub fn close_process_stdin(&self, pid: u32) -> std::result::Result<(), ClientError> {
        let process_id = self.lookup_process_id(pid)?;
        let this = self.clone();
        tokio::spawn(async move {
            let ownership = this.vm_scope();
            let _ = this
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::CloseStdinRequest(wire::CloseStdinRequest { process_id }),
                )
                .await;
        });
        Ok(())
    }

    /// Subscribe to the unified stdout/stderr event stream for a process.
    pub fn on_process_output(
        &self,
        pid: u32,
        mut handler: impl FnMut(ProcessOutput) + Send + 'static,
    ) -> std::result::Result<Subscription, ClientError> {
        let mut rx = self
            .inner()
            .processes
            .read(&pid, |_, entry| entry.output_tx.subscribe())
            .ok_or(ClientError::ProcessNotFound(pid))?;
        let task = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => handler(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Ok(Subscription::new(move || task.abort()))
    }

    /// Register a once-only exit handler. If the process has already exited, the handler fires
    /// immediately and synchronously and a no-op unsubscribe is returned (the `watch` already holds
    /// `Some(code)`). Otherwise the handler fires once when the exit code lands. The exit code is
    /// `i32`, never null.
    pub fn on_process_exit(
        &self,
        pid: u32,
        handler: impl FnOnce(ProcessExit) + Send + 'static,
    ) -> std::result::Result<Subscription, ClientError> {
        let mut rx = self
            .inner()
            .processes
            .read(&pid, |_, entry| entry.exit_tx.subscribe())
            .ok_or(ClientError::ProcessNotFound(pid))?;

        // Already-exited branch: fire immediately + synchronously, return a no-op unsubscribe.
        if let Some(code) = *rx.borrow() {
            handler(ProcessExit {
                pid,
                exit_code: code,
            });
            return Ok(Subscription::noop());
        }

        // Otherwise wait for the watch to transition to `Some(code)` and fire exactly once. The
        // returned `Subscription` cancels the waiting task on drop (= unsubscribe).
        let task = tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                if let Some(code) = *rx.borrow() {
                    handler(ProcessExit {
                        pid,
                        exit_code: code,
                    });
                    return;
                }
            }
        });
        Ok(Subscription::new(move || task.abort()))
    }

    /// Await a spawned process's exit code. Unknown-pid lookup errors (synchronously in TS; here the
    /// lookup error is returned before any awaiting begins).
    pub async fn wait_process(&self, pid: u32) -> std::result::Result<i32, ClientError> {
        let mut rx = self
            .inner()
            .processes
            .read(&pid, |_, entry| entry.exit_tx.subscribe())
            .ok_or(ClientError::ProcessNotFound(pid))?;

        if let Some(code) = *rx.borrow() {
            return Ok(code);
        }
        while rx.changed().await.is_ok() {
            if let Some(code) = *rx.borrow() {
                return Ok(code);
            }
        }
        Err(ClientError::Sidecar(format!(
            "wait_process: exit channel closed before process {pid} reported an exit code"
        )))
    }

    /// List SDK-spawned processes only. `running = exit_code.is_none()`.
    pub fn list_processes(&self) -> Vec<SpawnedProcessInfo> {
        let mut out = Vec::new();
        self.inner().processes.scan(|pid, entry| {
            let exit_code = *entry.exit_tx.borrow();
            out.push(SpawnedProcessInfo {
                pid: *pid,
                command: entry.command.clone(),
                args: entry.args.clone(),
                running: exit_code.is_none(),
                exit_code,
                started_at: entry.started_at,
            });
        });
        out
    }

    /// List ALL kernel processes (native sidecar process snapshot).
    ///
    /// The kernel snapshot keys processes by their raw kernel pid. SDK-spawned root processes carry a
    /// synthetic display pid (the `spawn` return value); this remaps each snapshot entry's
    /// pid/ppid/pgid/sid back to that display pid via the per-process `kernel_pid` watch, so a caller
    /// can correlate `spawn()` with `all_processes()`/`process_tree()`. Results are sorted ascending
    /// by display pid (TS `snapshotProcesses` `.sort((l,r) => l.pid - r.pid)`).
    pub async fn all_processes(&self) -> Result<Vec<ProcessInfo>> {
        let ownership = self.vm_scope();
        let response = self
            .transport()
            .request_wire(ownership, wire::RequestPayload::GetProcessSnapshotRequest)
            .await
            .context("all_processes: GetProcessSnapshot request failed")?;
        let snapshot = match response {
            wire::ResponsePayload::ProcessSnapshotResponse(snapshot) => snapshot,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(ClientError::from_rejection(rejected).into());
            }
            other => {
                return Err(ClientError::Sidecar(format!(
                    "all_processes: unexpected response {other:?}"
                ))
                .into());
            }
        };

        // Snapshot the SDK process registry, keyed by wire `process_id`, capturing exit code,
        // command, and args. This mirrors the TS `trackedProcessesById` lookup used to build
        // `displayPidByKernelPid` and override fields.
        struct Tracked {
            exit_code: Option<i32>,
            command: String,
            args: Vec<String>,
        }
        let mut tracked_by_process_id: BTreeMap<String, Tracked> = BTreeMap::new();
        let mut display_pid_by_kernel_pid: BTreeMap<u32, u32> = BTreeMap::new();
        self.inner().processes.scan(|display_pid, entry| {
            let exit_code = *entry.exit_tx.borrow();
            if let Some(kernel_pid) = *entry.kernel_pid.borrow() {
                display_pid_by_kernel_pid.insert(kernel_pid, *display_pid);
            }
            tracked_by_process_id.insert(
                entry.process_id.clone(),
                Tracked {
                    exit_code,
                    command: entry.command.clone(),
                    args: entry.args.clone(),
                },
            );
        });

        let now_ms = epoch_ms_now();
        let mut seen_display_pids: std::collections::BTreeSet<u32> =
            std::collections::BTreeSet::new();
        let mut out: Vec<ProcessInfo> = Vec::new();

        for entry in snapshot.processes {
            let tracked = tracked_by_process_id.get(&entry.process_id);
            let display_pid = display_pid_by_kernel_pid
                .get(&entry.pid)
                .copied()
                .unwrap_or(entry.pid);
            let display_ppid = display_pid_by_kernel_pid
                .get(&entry.ppid)
                .copied()
                .unwrap_or(entry.ppid);
            let display_pgid = display_pid_by_kernel_pid
                .get(&entry.pgid)
                .copied()
                .unwrap_or(entry.pgid);
            let display_sid = display_pid_by_kernel_pid
                .get(&entry.sid)
                .copied()
                .unwrap_or(entry.sid);

            // First-observed start time, keyed by `"<process_id>:<kernel_pid>"` (TS `processKey`).
            let process_key = format!("{}:{}", entry.process_id, entry.pid);
            let start_time = self.observed_start_time(&process_key, now_ms);

            // Status/exit code: a tracked process whose SDK exit code is known is `exited`; otherwise
            // a tracked process is `running`; an untracked process uses the snapshot status.
            let (status, exit_code) = match tracked {
                Some(t) => match t.exit_code {
                    Some(code) => (ProcessStatus::Exited, Some(code)),
                    None => (ProcessStatus::Running, entry.exit_code),
                },
                None => {
                    let status = match entry.status {
                        ProcessSnapshotStatus::Running | ProcessSnapshotStatus::Stopped => {
                            ProcessStatus::Running
                        }
                        ProcessSnapshotStatus::Exited => ProcessStatus::Exited,
                    };
                    (status, entry.exit_code)
                }
            };

            // Exit time: only tracked-and-exited processes carry one (TS `tracked?.exitTime`).
            let exit_time = match (tracked, status) {
                (Some(_), ProcessStatus::Exited) => {
                    Some(self.observed_exit_time(&entry.process_id, now_ms))
                }
                _ => None,
            };

            let (command, args) = match tracked {
                Some(t) => (t.command.clone(), t.args.clone()),
                None => (entry.command, entry.args),
            };

            seen_display_pids.insert(display_pid);
            out.push(ProcessInfo {
                pid: display_pid,
                ppid: display_ppid,
                pgid: display_pgid,
                sid: display_sid,
                driver: entry.driver,
                command,
                args,
                cwd: entry.cwd,
                status,
                exit_code,
                start_time,
                exit_time,
            });
        }

        // Tracked processes not yet present in the snapshot (the spawn `Execute` has not surfaced in
        // the kernel table yet). TS fills these with `ppid:0, pgid/sid = pid`.
        self.inner().processes.scan(|display_pid, entry| {
            if seen_display_pids.contains(display_pid) {
                return;
            }
            let exit_code = *entry.exit_tx.borrow();
            let process_key = format!("{}:{}", entry.process_id, display_pid);
            let start_time = self.observed_start_time(&process_key, now_ms);
            let (status, exit_time) = match exit_code {
                Some(_) => (
                    ProcessStatus::Exited,
                    Some(self.observed_exit_time(&entry.process_id, now_ms)),
                ),
                None => (ProcessStatus::Running, None),
            };
            out.push(ProcessInfo {
                pid: *display_pid,
                ppid: 0,
                pgid: *display_pid,
                sid: *display_pid,
                driver: String::new(),
                command: entry.command.clone(),
                args: entry.args.clone(),
                cwd: String::new(),
                status,
                exit_code,
                start_time,
                exit_time,
            });
        });

        out.sort_by_key(|info| info.pid);
        Ok(out)
    }

    /// Return the first-observed start time for a process key, recording `now` the first time it is
    /// seen so later snapshots report a stable timestamp (TS `observedProcessStartTimes`).
    fn observed_start_time(&self, process_key: &str, now_ms: f64) -> f64 {
        let _guard = self.inner().observed_process_time_lock.lock();
        if let Some(existing) = self
            .inner()
            .observed_process_start_times
            .read(process_key, |_, value| *value)
        {
            return existing;
        }
        let _ = self
            .inner()
            .observed_process_start_times
            .insert(process_key.to_owned(), now_ms);
        prune_string_f64_map(
            &self.inner().observed_process_start_times,
            OBSERVED_PROCESS_TIME_LIMIT,
        );
        // Re-read to honor a racing insert that may have won; either value is a valid first-observed
        // timestamp.
        self.inner()
            .observed_process_start_times
            .read(process_key, |_, value| *value)
            .unwrap_or(now_ms)
    }

    /// Return the first-observed exit time for an SDK process id, recording `now` on first sight.
    fn observed_exit_time(&self, process_id: &str, now_ms: f64) -> f64 {
        let _guard = self.inner().observed_process_time_lock.lock();
        if let Some(existing) = self
            .inner()
            .observed_process_exit_times
            .read(process_id, |_, value| *value)
        {
            return existing;
        }
        let _ = self
            .inner()
            .observed_process_exit_times
            .insert(process_id.to_owned(), now_ms);
        prune_string_f64_map(
            &self.inner().observed_process_exit_times,
            OBSERVED_PROCESS_TIME_LIMIT,
        );
        self.inner()
            .observed_process_exit_times
            .read(process_id, |_, value| *value)
            .unwrap_or(now_ms)
    }

    /// Build the process forest from `all_processes`, linked by `ppid`.
    pub async fn process_tree(&self) -> Result<Vec<ProcessTreeNode>> {
        let processes = self.all_processes().await?;
        Ok(build_process_forest(processes))
    }

    /// Get a single SDK-spawned process's info. Errors (not None) when not found.
    pub fn get_process(&self, pid: u32) -> std::result::Result<SpawnedProcessInfo, ClientError> {
        self.inner()
            .processes
            .read(&pid, |pid, entry| {
                let exit_code = *entry.exit_tx.borrow();
                SpawnedProcessInfo {
                    pid: *pid,
                    command: entry.command.clone(),
                    args: entry.args.clone(),
                    running: exit_code.is_none(),
                    exit_code,
                    started_at: entry.started_at,
                }
            })
            .ok_or(ClientError::ProcessNotFound(pid))
    }

    /// SIGTERM a spawned process. No-op if already exited; errors if unknown.
    pub fn stop_process(&self, pid: u32) -> std::result::Result<(), ClientError> {
        self.signal_process(pid, "SIGTERM")
    }

    /// SIGKILL a spawned process. No-op if already exited; errors if unknown.
    pub fn kill_process(&self, pid: u32) -> std::result::Result<(), ClientError> {
        self.signal_process(pid, "SIGKILL")
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Build the VM-scoped ownership for a wire request.
    fn vm_scope(&self) -> wire::OwnershipScope {
        wire::OwnershipScope::VmOwnership(wire::VmOwnership {
            connection_id: self.connection_id().to_string(),
            session_id: self.wire_session_id().to_string(),
            vm_id: self.vm_id().to_string(),
        })
    }

    /// Allocate a fresh wire `process_id` (used by `exec`, which does not register in the SDK map).
    fn next_process_id(&self) -> String {
        let n = self.inner().process_counter.fetch_add(1, Ordering::SeqCst);
        format!("proc-{n}-{}", uuid::Uuid::new_v4())
    }

    /// Resolve the wire `process_id` for an SDK pid, erroring with `ProcessNotFound` if unknown.
    fn lookup_process_id(&self, pid: u32) -> std::result::Result<String, ClientError> {
        self.inner()
            .processes
            .read(&pid, |_, entry| entry.process_id.clone())
            .ok_or(ClientError::ProcessNotFound(pid))
    }

    /// Send the `Execute` wire request, mapping a rejection into [`ClientError::Kernel`].
    async fn send_execute(
        &self,
        process_id: &str,
        command: Option<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    ) -> std::result::Result<wire::ProcessStartedResponse, ClientError> {
        let ownership = self.vm_scope();
        let response = self
            .transport()
            .request_wire(
                ownership,
                wire::RequestPayload::ExecuteRequest(wire::ExecuteRequest {
                    process_id: process_id.to_owned(),
                    command,
                    runtime: None,
                    entrypoint: None,
                    args,
                    env: env.into_iter().collect(),
                    cwd,
                    wasm_permission_tier: None,
                }),
            )
            .await?;
        match response {
            wire::ResponsePayload::ProcessStartedResponse(started) => Ok(started),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected))
            }
            other => Err(ClientError::Sidecar(format!(
                "Execute: unexpected response {other:?}"
            ))),
        }
    }

    /// Fire-and-forget kill of a wire process by its `process_id` (used by `exec` timeout). The TS
    /// timeout path calls `proc.kill(9)`, which maps to a `SIGKILL` kill request.
    fn kill_wire_process(&self, process_id: &str, signal: &str) {
        let process_id = process_id.to_owned();
        let signal = signal.to_owned();
        let this = self.clone();
        tokio::spawn(async move {
            let ownership = this.vm_scope();
            let _ = this
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::KillProcessRequest(wire::KillProcessRequest {
                        process_id,
                        signal,
                    }),
                )
                .await;
        });
    }

    /// Send a kill signal for an SDK pid. No-op if already exited; errors with `ProcessNotFound` if
    /// the pid is unknown.
    fn signal_process(&self, pid: u32, signal: &str) -> std::result::Result<(), ClientError> {
        let (process_id, already_exited) = self
            .inner()
            .processes
            .read(&pid, |_, entry| {
                (entry.process_id.clone(), entry.exit_tx.borrow().is_some())
            })
            .ok_or(ClientError::ProcessNotFound(pid))?;
        if already_exited {
            return Ok(());
        }
        let signal = signal.to_owned();
        let this = self.clone();
        tokio::spawn(async move {
            let ownership = this.vm_scope();
            let _ = this
                .transport()
                .request_wire(
                    ownership,
                    wire::RequestPayload::KillProcessRequest(wire::KillProcessRequest {
                        process_id,
                        signal,
                    }),
                )
                .await;
        });
        Ok(())
    }

    fn process_registry_len_locked(&self) -> usize {
        let mut count = 0usize;
        self.inner().processes.scan(|_, _| {
            count += 1;
        });
        count
    }

    fn prune_exited_processes_locked(&self, reserve_slots: usize) {
        let mut entries = Vec::new();
        self.inner().processes.scan(|pid, entry| {
            entries.push((*pid, entry.exit_tx.borrow().is_some()));
        });
        let target_len = PROCESS_REGISTRY_LIMIT.saturating_sub(reserve_slots);
        if entries.len() <= target_len {
            return;
        }

        for pid in exited_pids_to_prune(entries, target_len) {
            self.remove_process_tracking_locked(pid);
        }
    }

    fn remove_process_tracking_locked(&self, pid: u32) {
        if let Some((_, entry)) = self.inner().processes.remove(&pid) {
            let _time_guard = self.inner().observed_process_time_lock.lock();
            let _ = self
                .inner()
                .observed_process_exit_times
                .remove(&entry.process_id);
            let fallback_start_key = format!("{}:{pid}", entry.process_id);
            let _ = self
                .inner()
                .observed_process_start_times
                .remove(&fallback_start_key);
            if let Some(kernel_pid) = *entry.kernel_pid.borrow() {
                let start_key = format!("{}:{kernel_pid}", entry.process_id);
                let _ = self.inner().observed_process_start_times.remove(&start_key);
            }
        }
    }

    /// Background pump for a spawned process: issue the `Execute` request, then fan kernel
    /// `ProcessOutput`/`ProcessExited` events for this process id into the per-process broadcast and
    /// watch channels. Exited entries are retained for post-exit inspection, then pruned oldest-first
    /// under registry pressure.
    #[allow(clippy::too_many_arguments)]
    async fn run_spawn(
        self,
        pid: u32,
        process_id: String,
        command: String,
        args: Vec<String>,
        options: SpawnOptions,
        mut events: broadcast::Receiver<(wire::OwnershipScope, EventPayload)>,
        stdout_tx: broadcast::Sender<Vec<u8>>,
        stderr_tx: broadcast::Sender<Vec<u8>>,
        output_tx: broadcast::Sender<ProcessOutput>,
        exit_tx: watch::Sender<Option<i32>>,
        kernel_pid_tx: watch::Sender<Option<u32>>,
    ) {
        match self
            .send_execute(
                &process_id,
                Some(command),
                args,
                options.env.clone(),
                options.cwd.clone(),
            )
            .await
        {
            Ok(started) => {
                // Seed the kernel pid so `all_processes`/`process_tree` can remap this process's
                // kernel-snapshot entry back to its display pid.
                if let Some(kernel_pid) = started.pid {
                    let _ = kernel_pid_tx.send(Some(kernel_pid));
                }
            }
            Err(error) => {
                // The native TS launch-failure path emits the error message (plus a trailing
                // newline) on stderr and resolves the wait with exit code 1 (`startTrackedProcess`
                // catch -> stderr handlers + `finishProcess(entry, 1)`).
                let message = format!("{error}\n");
                let bytes = message.into_bytes();
                let _ = stderr_tx.send(bytes.clone());
                let _ = output_tx.send(ProcessOutput {
                    pid,
                    stream: ProcessStream::Stderr,
                    data: bytes,
                });
                tracing::error!(?error, pid, %process_id, "spawn: Execute request failed");
                let _ = exit_tx.send(Some(1));
                let _guard = self.inner().process_registry_lock.lock();
                self.prune_exited_processes_locked(0);
                return;
            }
        }

        loop {
            let (_, payload) = match events.recv().await {
                Ok(frame) => frame,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    // The event stream closed before an exit event landed. The TS fallback treats a
                    // process that has fully disappeared from the VM snapshot as reaped with exit
                    // code 0; mirror that terminal value so waiters resolve instead of hanging.
                    let _ = exit_tx.send(Some(0));
                    break;
                }
            };
            match payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    let bytes = output.chunk;
                    let _ = output_tx.send(ProcessOutput {
                        pid,
                        stream: match output.channel {
                            StreamChannel::Stdout => ProcessStream::Stdout,
                            StreamChannel::Stderr => ProcessStream::Stderr,
                        },
                        data: bytes.clone(),
                    });
                    match output.channel {
                        StreamChannel::Stdout => {
                            let _ = stdout_tx.send(bytes);
                        }
                        StreamChannel::Stderr => {
                            let _ = stderr_tx.send(bytes);
                        }
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    let _ = exit_tx.send(Some(exited.exit_code));
                    break;
                }
                EventPayload::ProcessOutputEvent(_)
                | EventPayload::ProcessExitedEvent(_)
                | EventPayload::ExecutionOutputEvent(_)
                | EventPayload::ExecutionCompletedEvent(_)
                | EventPayload::VmLifecycleEvent(_)
                | EventPayload::StructuredEvent(_)
                | EventPayload::ExtEnvelope(_) => {}
            }
        }
        let _guard = self.inner().process_registry_lock.lock();
        self.prune_exited_processes_locked(0);
    }
}

/// Assemble a process forest from a flat process list, linking children by `ppid`.
///
/// Mirrors the TS `processTree` `nodeMap` algorithm exactly: a process is a root iff its `ppid` is
/// NOT present among the listed pids. A self-parented process (`ppid == pid`) finds itself as its
/// parent, so it is attached as its own child and is excluded from the roots (effectively dropped
/// from the output tree). A `seen` guard prevents the self-cycle from recursing forever.
fn build_process_forest(processes: Vec<ProcessInfo>) -> Vec<ProcessTreeNode> {
    use std::collections::BTreeMap as Map;

    let pids: std::collections::BTreeSet<u32> = processes.iter().map(|p| p.pid).collect();
    // Children adjacency keyed by parent pid, preserving input (sorted) order.
    let mut children_of: Map<u32, Vec<usize>> = Map::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, proc) in processes.iter().enumerate() {
        if pids.contains(&proc.ppid) {
            children_of.entry(proc.ppid).or_default().push(index);
        } else {
            roots.push(index);
        }
    }

    fn build_node(
        index: usize,
        processes: &[ProcessInfo],
        children_of: &Map<u32, Vec<usize>>,
        seen: &mut std::collections::BTreeSet<usize>,
    ) -> ProcessTreeNode {
        let info = processes[index].clone();
        seen.insert(index);
        let child_indices: Vec<usize> = children_of
            .get(&info.pid)
            .map(|indices| {
                indices
                    .iter()
                    .copied()
                    .filter(|child_index| !seen.contains(child_index))
                    .collect()
            })
            .unwrap_or_default();
        let children = child_indices
            .into_iter()
            .map(|child_index| build_node(child_index, processes, children_of, seen))
            .collect();
        ProcessTreeNode { info, children }
    }

    let mut seen = std::collections::BTreeSet::new();
    roots
        .into_iter()
        .map(|index| build_node(index, &processes, &children_of, &mut seen))
        .collect()
}

/// Convert a [`StdinInput`] to raw bytes. A string is delivered as its UTF-8 bytes; raw bytes are
/// delivered verbatim (binary-safe, never lossy).
fn stdin_to_bytes(input: StdinInput) -> Vec<u8> {
    match input {
        StdinInput::Text(text) => text.into_bytes(),
        StdinInput::Bytes(bytes) => bytes,
    }
}

fn append_exec_output(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    captured_output_bytes: &mut usize,
    channel: &str,
) -> std::result::Result<(), ClientError> {
    let next_total = captured_output_bytes
        .checked_add(chunk.len())
        .ok_or_else(|| exec_output_limit_error(channel, usize::MAX))?;
    if next_total > EXEC_OUTPUT_CAPTURE_LIMIT_BYTES {
        return Err(exec_output_limit_error(channel, next_total));
    }
    buffer.extend_from_slice(chunk);
    *captured_output_bytes = next_total;
    Ok(())
}

fn exec_output_limit_error(channel: &str, size: usize) -> ClientError {
    ClientError::Sidecar(format!(
        "exec {channel} capture is {size} bytes, limit is {EXEC_OUTPUT_CAPTURE_LIMIT_BYTES}"
    ))
}

fn exited_pids_to_prune(mut entries: Vec<(u32, bool)>, target_len: usize) -> Vec<u32> {
    if entries.len() <= target_len {
        return Vec::new();
    }
    let mut remove_count = entries.len() - target_len;
    entries.sort_by_key(|(pid, _)| *pid);
    let mut out = Vec::new();
    for (pid, exited) in entries {
        if remove_count == 0 {
            break;
        }
        if !exited {
            continue;
        }
        out.push(pid);
        remove_count -= 1;
    }
    out
}

fn prune_string_f64_map(map: &SccHashMap<String, f64>, limit: usize) {
    let mut keys = Vec::new();
    map.scan(|key, _| {
        keys.push(key.clone());
    });
    if keys.len() <= limit {
        return;
    }
    let remove_count = keys.len() - limit;
    keys.sort();
    for key in keys.into_iter().take(remove_count) {
        let _ = map.remove(&key);
    }
}

/// Drive a caller-supplied output callback from a fresh subscription on the given broadcast channel.
/// Each chunk delivered to the channel is forwarded to `callback` as raw bytes. The task ends when
/// the channel closes (process exit), matching the TS handler-set lifetime.
///
/// Returns the spawned task's handle so the owner can abort it on teardown: a [`ProcessEntry`]
/// retains its own `stdout_tx`/`stderr_tx` clone for late subscribers, so the broadcast channel
/// never closes (and this task never observes `Closed`) until the entry is dropped. `shutdown`
/// drains the registry and aborts these handles rather than waiting on the channel close.
pub(crate) fn install_output_callback(
    tx: broadcast::Sender<Vec<u8>>,
    mut callback: OutputCallback,
) -> JoinHandle<()> {
    let mut rx = tx.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => callback(&chunk),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Drain the SDK-spawned process registry, dropping each entry's retained sender clones and aborting
/// its per-process output-callback tasks. Called from `shutdown` so the output tasks (which would
/// otherwise await a `Closed` that never fires, see [`install_output_callback`]) cannot outlive the
/// disposed VM. Mirrors the `pending_shell_exits` / ACP-terminal drain in `shutdown`.
pub(crate) fn drain_process_output_tasks(processes: &SccHashMap<u32, ProcessEntry>) {
    let mut tasks = Vec::new();
    processes.retain(|_, entry| {
        tasks.append(&mut entry.output_tasks);
        false
    });
    for task in tasks {
        task.abort();
    }
}

/// Current wall-clock time as epoch milliseconds (TS `Date.now()`).
fn epoch_ms_now() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::{
        append_exec_output, drain_process_output_tasks, exited_pids_to_prune,
        install_output_callback, prune_string_f64_map, ExecOptions, OutputCallback,
        DEFAULT_EXEC_CWD, EXEC_OUTPUT_CAPTURE_LIMIT_BYTES,
    };
    use crate::agent_os::ProcessEntry;
    use scc::HashMap as SccHashMap;
    use tokio::sync::{broadcast, watch};

    /// Regression for the per-process output-callback leak (H3): a `ProcessEntry` retains clones of
    /// its `stdout_tx`/`stderr_tx`, so the output tasks never observe the broadcast `Closed` and hang
    /// forever unless teardown aborts them. `drain_process_output_tasks` must empty the registry and
    /// abort every retained output task.
    #[tokio::test]
    async fn drain_process_output_tasks_clears_registry_and_aborts_tasks() {
        let processes: SccHashMap<u32, ProcessEntry> = SccHashMap::new();

        let (stdout_tx, _) = broadcast::channel::<Vec<u8>>(8);
        let (stderr_tx, _) = broadcast::channel::<Vec<u8>>(8);
        let (output_tx, _) = broadcast::channel(8);
        let (exit_tx, _) = watch::channel::<Option<i32>>(None);
        let (kernel_pid_tx, _) = watch::channel::<Option<u32>>(None);

        // A task that never completes on its own, standing in for an output-callback task that is
        // waiting on a `Closed` that the retained sender clone prevents.
        let task = tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
        let abort_handle = task.abort_handle();

        let entry = ProcessEntry {
            command: "sleep".to_string(),
            args: vec!["3600".to_string()],
            stdout_tx,
            stderr_tx,
            output_tx,
            exit_tx,
            process_id: "proc-test".to_string(),
            kernel_pid: kernel_pid_tx,
            output_tasks: vec![task],
            started_at: 0,
        };
        let _ = processes.insert(1, entry);

        assert!(!abort_handle.is_finished(), "task should start alive");

        drain_process_output_tasks(&processes);

        assert!(processes.is_empty(), "registry must be cleared on drain");

        // The abort is asynchronous; give the runtime a bounded window to reap the cancelled task.
        for _ in 0..100 {
            if abort_handle.is_finished() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            abort_handle.is_finished(),
            "output task must be aborted after drain"
        );
    }

    /// Regression for the H3 wiring (not just the drain helper): `spawn`/`spawn_inner` must capture
    /// the `JoinHandle` returned by `install_output_callback` into `ProcessEntry::output_tasks`. If a
    /// refactor forgot to push the handle, the callback task would be unreachable and
    /// `drain_process_output_tasks` would have nothing to abort, re-leaking the task. This reproduces
    /// that exact seam and asserts the stored handle is the live callback task.
    #[tokio::test]
    async fn install_output_callback_handle_is_captured_into_process_entry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let (stdout_tx, _) = broadcast::channel::<Vec<u8>>(8);
        let (stderr_tx, _) = broadcast::channel::<Vec<u8>>(8);
        let (output_tx, _) = broadcast::channel(8);
        let (exit_tx, _) = watch::channel::<Option<i32>>(None);
        let (kernel_pid_tx, _) = watch::channel::<Option<u32>>(None);

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_cb = Arc::clone(&calls);
        let cb: OutputCallback = Box::new(move |_chunk: &[u8]| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
        });

        // The exact seam from `spawn_inner`: capture the returned handle in `output_tasks`.
        let output_tasks = vec![install_output_callback(stdout_tx.clone(), cb)];

        let entry = ProcessEntry {
            command: "sleep".to_string(),
            args: vec!["3600".to_string()],
            stdout_tx: stdout_tx.clone(),
            stderr_tx,
            output_tx,
            exit_tx,
            process_id: "proc-test".to_string(),
            kernel_pid: kernel_pid_tx,
            output_tasks,
            started_at: 0,
        };

        assert_eq!(
            entry.output_tasks.len(),
            1,
            "the install_output_callback handle must be captured on the entry"
        );

        // Prove the captured handle is the live callback task: a chunk on the channel runs it.
        stdout_tx
            .send(b"hello".to_vec())
            .expect("broadcast send to subscribed callback task");
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the stored handle must drive the registered callback"
        );

        // And it is the handle `drain_process_output_tasks` aborts on teardown.
        let processes: SccHashMap<u32, ProcessEntry> = SccHashMap::new();
        let _ = processes.insert(1, entry);
        drain_process_output_tasks(&processes);
        assert!(processes.is_empty(), "registry must be cleared on drain");
    }

    #[test]
    fn exec_options_default_uses_workspace_cwd() {
        assert_eq!(
            ExecOptions::default().cwd.as_deref(),
            Some(DEFAULT_EXEC_CWD)
        );
    }

    #[test]
    fn append_exec_output_rejects_capture_over_limit() {
        let mut buffer = vec![0u8; EXEC_OUTPUT_CAPTURE_LIMIT_BYTES - 1];
        let mut captured = buffer.len();

        append_exec_output(&mut buffer, &[1], &mut captured, "stdout")
            .expect("chunk at limit should fit");
        assert_eq!(captured, EXEC_OUTPUT_CAPTURE_LIMIT_BYTES);

        let error = append_exec_output(&mut buffer, &[2], &mut captured, "stdout")
            .expect_err("chunk over limit should fail");
        assert!(
            error.to_string().contains("exec stdout capture is"),
            "unexpected error: {error}"
        );
        assert_eq!(captured, EXEC_OUTPUT_CAPTURE_LIMIT_BYTES);
        assert_eq!(buffer.len(), EXEC_OUTPUT_CAPTURE_LIMIT_BYTES);
    }

    #[test]
    fn exited_pid_pruning_keeps_live_entries_and_removes_oldest_exited() {
        let pids = exited_pids_to_prune(vec![(3, true), (1, false), (2, true), (4, true)], 2);
        assert_eq!(pids, vec![2, 3]);
    }

    #[test]
    fn observed_time_pruning_enforces_limit() {
        let map = SccHashMap::new();
        let _ = map.insert("b".to_string(), 2.0);
        let _ = map.insert("a".to_string(), 1.0);
        let _ = map.insert("c".to_string(), 3.0);

        prune_string_f64_map(&map, 2);

        assert!(map.read("a", |_, _| ()).is_none());
        assert!(map.read("b", |_, _| ()).is_some());
        assert!(map.read("c", |_, _| ()).is_some());
    }
}
