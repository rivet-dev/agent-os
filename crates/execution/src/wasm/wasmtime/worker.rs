//! Killable subprocess boundary for explicitly threaded WebAssembly.
//!
//! The worker owns Wasmtime Engine/Store/Instance/native-thread state only.
//! Kernel state and every host capability remain in the parent sidecar. The
//! protocol is length-delimited, typed, bounded, and carries owned values;
//! guest memory is never shared with the parent across an async wait.

use super::super::StartWasmExecutionRequest;
use super::lifecycle::Control;
use crate::backend::{HostCallReply, HostServiceError};
use crate::host::{HostOperation, HostProcessContext, ProcessOperation, SignalOperation};
use agentos_runtime::{RuntimeConfig, SidecarRuntime};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const WORKER_MODE_ARGUMENT: &str = "--agentos-wasmtime-thread-worker";
const MAX_STARTUP_HEADER_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_WORKER_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_WORKER_FRAME_BYTES: usize = 128 * 1024 * 1024;
const WORKER_FINISH_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
struct WorkerStartup {
    request: StartWasmExecutionRequest,
    process: HostProcessContext,
    module_bytes: usize,
    max_frame_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
enum WorkerCall {
    Adapter {
        method: String,
        args: Vec<Value>,
        raw: Vec<RawArgument>,
    },
    OpenExecutableImage {
        descriptor: u32,
    },
    ReadExecutableImage {
        handle: u64,
        offset: u64,
        max_bytes: usize,
    },
    CloseExecutableImage {
        handle: u64,
    },
    Signal(SignalOperation),
}

#[derive(Debug, Serialize, Deserialize)]
struct RawArgument {
    index: usize,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
enum WorkerFrame {
    Call {
        id: u64,
        call: WorkerCall,
    },
    Stderr {
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    Finished {
        result: Result<i32, HostServiceError>,
    },
    GroupFailed {
        error: HostServiceError,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) enum ParentFrame {
    Reply {
        id: u64,
        result: Result<HostCallReply, HostServiceError>,
    },
    SignalWake,
    FinishedAck,
}

struct OutboundFrame {
    frame: WorkerFrame,
    flushed: Option<std::sync::mpsc::SyncSender<Result<(), HostServiceError>>>,
}

type PendingCall = tokio::sync::oneshot::Sender<Result<HostCallReply, HostServiceError>>;
type PendingCallMap = Mutex<HashMap<u64, PendingCall>>;

#[derive(Clone)]
pub(super) struct WorkerIpcClient {
    process: HostProcessContext,
    next_call_id: Arc<AtomicU64>,
    sender: std::sync::mpsc::SyncSender<OutboundFrame>,
    pending: Arc<PendingCallMap>,
    signal_pending: Arc<AtomicUsize>,
    finish_ack: Arc<Mutex<Option<std::sync::mpsc::SyncSender<()>>>>,
    failed: Arc<AtomicBool>,
}

impl std::fmt::Debug for WorkerIpcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerIpcClient")
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

impl WorkerIpcClient {
    fn start(
        process: HostProcessContext,
        maximum_pending: usize,
        max_frame_bytes: usize,
    ) -> Result<Self, HostServiceError> {
        let maximum_pending = maximum_pending.max(1);
        let (sender, receiver) = std::sync::mpsc::sync_channel::<OutboundFrame>(maximum_pending);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let signal_pending = Arc::new(AtomicUsize::new(0));
        let finish_ack: Arc<Mutex<Option<std::sync::mpsc::SyncSender<()>>>> =
            Arc::new(Mutex::new(None));
        let failed = Arc::new(AtomicBool::new(false));

        let writer_pending = Arc::clone(&pending);
        let writer_failed = Arc::clone(&failed);
        // AGENTOS_THREAD_SITE: threaded-wasmtime-ipc-writer
        std::thread::Builder::new()
            .name(String::from("agentos-wasmtime-worker-ipc-write"))
            .spawn(move || {
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();
                while let Ok(outbound) = receiver.recv() {
                    let result =
                        write_frame_blocking(&mut stdout, &outbound.frame, max_frame_bytes);
                    if let Some(flushed) = outbound.flushed {
                        if flushed.send(result.clone()).is_err() {
                            eprintln!(
                                "ERR_AGENTOS_WASMTIME_WORKER_IPC_ACK: flush waiter was dropped"
                            );
                        }
                    }
                    if let Err(error) = result {
                        writer_failed.store(true, Ordering::Release);
                        fail_pending(&writer_pending, error);
                        break;
                    }
                }
            })
            .map_err(|error| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WORKER_IPC_THREAD",
                    format!("failed to start worker IPC writer: {error}"),
                )
            })?;

        let reader_pending = Arc::clone(&pending);
        let reader_signals = Arc::clone(&signal_pending);
        let reader_finish_ack = Arc::clone(&finish_ack);
        let reader_failed = Arc::clone(&failed);
        // AGENTOS_THREAD_SITE: threaded-wasmtime-ipc-reader
        std::thread::Builder::new()
            .name(String::from("agentos-wasmtime-worker-ipc-read"))
            .spawn(move || {
                let stdin = std::io::stdin();
                let mut stdin = stdin.lock();
                loop {
                    match read_frame_blocking::<_, ParentFrame>(&mut stdin, max_frame_bytes) {
                        Ok(ParentFrame::Reply { id, result }) => {
                            let waiter = match reader_pending.lock() {
                                Ok(mut pending) => pending.remove(&id),
                                Err(poisoned) => {
                                    eprintln!(
                                        "ERR_AGENTOS_WASMTIME_WORKER_PENDING_POISONED: recovering reply state"
                                    );
                                    poisoned.into_inner().remove(&id)
                                }
                            };
                            if let Some(waiter) = waiter {
                                if waiter.send(result).is_err() {
                                    eprintln!(
                                        "ERR_AGENTOS_WASMTIME_WORKER_REPLY_DROPPED: call {id} no longer has a waiter"
                                    );
                                }
                            }
                        }
                        Ok(ParentFrame::SignalWake) => {
                            if reader_signals
                                .fetch_update(
                                Ordering::AcqRel,
                                Ordering::Acquire,
                                |current| current.checked_add(1),
                                )
                                .is_err()
                            {
                                let error = HostServiceError::new(
                                    "ERR_AGENTOS_WASMTIME_WORKER_SIGNAL_LIMIT",
                                    "thread-worker signal wake counter overflowed",
                                );
                                reader_failed.store(true, Ordering::Release);
                                fail_pending(&reader_pending, error);
                                break;
                            }
                        }
                        Ok(ParentFrame::FinishedAck) => {
                            let waiter = match reader_finish_ack.lock() {
                                Ok(mut waiter) => waiter.take(),
                                Err(poisoned) => {
                                    eprintln!(
                                        "ERR_AGENTOS_WASMTIME_WORKER_FINISH_POISONED: recovering finish acknowledgement state"
                                    );
                                    poisoned.into_inner().take()
                                }
                            };
                            if let Some(waiter) = waiter {
                                if waiter.send(()).is_err() {
                                    eprintln!(
                                        "ERR_AGENTOS_WASMTIME_WORKER_FINISH_ACK_DROPPED: finish waiter was dropped"
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            reader_failed.store(true, Ordering::Release);
                            fail_pending(&reader_pending, error);
                            match reader_finish_ack.lock() {
                                Ok(mut waiter) => {
                                    waiter.take();
                                }
                                Err(poisoned) => {
                                    eprintln!(
                                        "ERR_AGENTOS_WASMTIME_WORKER_FINISH_POISONED: recovering failed finish state"
                                    );
                                    poisoned.into_inner().take();
                                }
                            }
                            break;
                        }
                    }
                }
            })
            .map_err(|error| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WORKER_IPC_THREAD",
                    format!("failed to start worker IPC reader: {error}"),
                )
            })?;

        Ok(Self {
            process,
            next_call_id: Arc::new(AtomicU64::new(1)),
            sender,
            pending,
            signal_pending,
            finish_ack,
            failed,
        })
    }

    pub(super) fn process(&self) -> HostProcessContext {
        self.process
    }

    pub(super) fn signal_pending(&self) -> bool {
        self.signal_pending.load(Ordering::Acquire) > 0
    }

    pub(super) async fn submit_adapter_call(
        &self,
        method: String,
        args: Vec<Value>,
        raw_bytes_args: HashMap<usize, Vec<u8>>,
    ) -> Result<HostCallReply, HostServiceError> {
        let raw = raw_bytes_args
            .into_iter()
            .map(|(index, bytes)| RawArgument { index, bytes })
            .collect();
        self.call(WorkerCall::Adapter { method, args, raw }).await
    }

    pub(super) async fn submit(
        &self,
        operation: HostOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let signal_delivery = matches!(
            operation,
            HostOperation::Signal(
                SignalOperation::TakePublishedDelivery
                    | SignalOperation::TakePublishedDeliveryForThread { .. }
            )
        );
        let call = match operation {
            HostOperation::Process(ProcessOperation::OpenExecutableImage {
                source: crate::host::ExecutableImageSource::Descriptor(descriptor),
                resolution: None,
            }) => WorkerCall::OpenExecutableImage { descriptor },
            HostOperation::Process(ProcessOperation::ReadExecutableImage {
                handle,
                offset,
                max_bytes,
            }) => WorkerCall::ReadExecutableImage {
                handle,
                offset,
                max_bytes: max_bytes.get(),
            },
            HostOperation::Process(ProcessOperation::CloseExecutableImage { handle }) => {
                WorkerCall::CloseExecutableImage { handle }
            }
            HostOperation::Signal(operation) => WorkerCall::Signal(operation),
            _ => {
                return Err(HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WORKER_OPERATION",
                    "thread worker attempted an unsupported typed host operation",
                ));
            }
        };
        let reply = self.call(call).await?;
        if signal_delivery
            && matches!(&reply, HostCallReply::Json(value) if !value.is_null())
            && self
                .signal_pending
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.saturating_sub(1))
                })
                .is_err()
        {
            return Err(HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_WORKER_SIGNAL_STATE",
                "thread-worker signal wake state could not be settled",
            ));
        }
        Ok(reply)
    }

    pub(super) async fn publish_stderr(&self, bytes: Vec<u8>) -> Result<(), HostServiceError> {
        self.send(WorkerFrame::Stderr { bytes }, false)
    }

    pub(super) fn report_group_failure(
        &self,
        error: HostServiceError,
    ) -> Result<(), HostServiceError> {
        self.send(WorkerFrame::GroupFailed { error }, false)
    }

    fn call(
        &self,
        call: WorkerCall,
    ) -> impl std::future::Future<Output = Result<HostCallReply, HostServiceError>> {
        let result = (|| {
            if self.failed.load(Ordering::Acquire) {
                return Err(worker_pipe_closed());
            }
            let id = self
                .next_call_id
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(1)
                })
                .map_err(|_| {
                    HostServiceError::new(
                        "EOVERFLOW",
                        "thread-worker host-call identity space is exhausted",
                    )
                })?;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.pending
                .lock()
                .map_err(|_| worker_pipe_closed())?
                .insert(id, sender);
            if let Err(error) = self.send(WorkerFrame::Call { id, call }, false) {
                match self.pending.lock() {
                    Ok(mut pending) => {
                        pending.remove(&id);
                    }
                    Err(poisoned) => {
                        eprintln!(
                            "ERR_AGENTOS_WASMTIME_WORKER_PENDING_POISONED: recovering failed call state"
                        );
                        poisoned.into_inner().remove(&id);
                    }
                }
                return Err(error);
            }
            Ok((id, receiver))
        })();
        async move {
            let (id, receiver) = result?;
            receiver.await.map_err(|_| {
                HostServiceError::new(
                    "EPIPE",
                    format!("thread-worker host reply {id} was dropped"),
                )
            })?
        }
    }

    fn send(&self, frame: WorkerFrame, flush: bool) -> Result<(), HostServiceError> {
        let (flushed, receiver) = if flush {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        self.sender
            .try_send(OutboundFrame { frame, flushed })
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
                    "thread-worker outbound IPC queue is full",
                ),
                std::sync::mpsc::TrySendError::Disconnected(_) => worker_pipe_closed(),
            })?;
        if let Some(receiver) = receiver {
            receiver.recv().map_err(|_| worker_pipe_closed())??;
        }
        Ok(())
    }

    fn finish(&self, result: Result<i32, HostServiceError>) -> Result<(), HostServiceError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.finish_ack
            .lock()
            .map_err(|_| worker_pipe_closed())?
            .replace(sender);
        if let Err(error) = self.send(WorkerFrame::Finished { result }, true) {
            match self.finish_ack.lock() {
                Ok(mut waiter) => {
                    waiter.take();
                }
                Err(poisoned) => {
                    eprintln!(
                        "ERR_AGENTOS_WASMTIME_WORKER_FINISH_POISONED: recovering failed completion state"
                    );
                    poisoned.into_inner().take();
                }
            }
            return Err(error);
        }
        receiver
            .recv_timeout(WORKER_FINISH_ACK_TIMEOUT)
            .map_err(|error| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WORKER_FINISH_ACK",
                    format!("parent did not acknowledge worker completion: {error}"),
                )
            })
    }
}

fn fail_pending(pending: &PendingCallMap, error: HostServiceError) {
    let mut pending = match pending.lock() {
        Ok(pending) => pending,
        Err(poisoned) => {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_WORKER_PENDING_POISONED: recovering pending-call state during failure"
            );
            poisoned.into_inner()
        }
    };
    let waiters = pending
        .drain()
        .map(|(_, waiter)| waiter)
        .collect::<Vec<_>>();
    drop(pending);
    for waiter in waiters {
        if waiter.send(Err(error.clone())).is_err() {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_WORKER_PENDING_DROPPED: pending call waiter was dropped"
            );
        }
    }
}

fn worker_pipe_closed() -> HostServiceError {
    HostServiceError::new(
        "EPIPE",
        "thread-worker host-operation IPC channel is closed",
    )
}

pub fn run_worker_entry() -> Result<(), HostServiceError> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let startup: WorkerStartup = read_frame_blocking(&mut stdin, MAX_STARTUP_HEADER_BYTES)?;
    let maximum_module_bytes = startup
        .request
        .limits
        .max_module_file_bytes
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(256 * 1024 * 1024);
    if startup.module_bytes > maximum_module_bytes {
        return Err(HostServiceError::limit(
            "ERR_AGENTOS_WASMTIME_MODULE_FILE_LIMIT",
            "limits.resources.maxWasmModuleFileBytes",
            maximum_module_bytes as u64,
            startup.module_bytes as u64,
        ));
    }
    let mut module = vec![0; startup.module_bytes];
    stdin.read_exact(&mut module).map_err(worker_read_error)?;
    drop(stdin);

    let client = WorkerIpcClient::start(
        startup.process,
        startup.request.limits.pending_event_count.unwrap_or(64),
        startup.max_frame_bytes,
    )?;
    let runtime = SidecarRuntime::process(&RuntimeConfig::default()).map_err(|error| {
        HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_RUNTIME", error.to_string())
    })?;
    let context = runtime.context();
    let client_for_run = client.clone();
    let result = runtime.block_on(super::lifecycle::run_worker_loaded_module(
        startup.request,
        module,
        context,
        client_for_run,
    ));
    client.finish(result)
}

pub(super) async fn run_worker_process(
    module: Vec<u8>,
    request: StartWasmExecutionRequest,
    host: super::store::WasmtimeHostClient,
    control: Arc<Control>,
) -> Result<i32, HostServiceError> {
    let executable = worker_executable()?;
    let max_frame_bytes = request
        .limits
        .pending_event_bytes
        .unwrap_or(DEFAULT_MAX_WORKER_FRAME_BYTES)
        .saturating_add(super::store::max_host_reply_bytes(&request)?)
        .clamp(DEFAULT_MAX_WORKER_FRAME_BYTES, MAX_WORKER_FRAME_BYTES);
    let startup = WorkerStartup {
        process: host.process(),
        module_bytes: module.len(),
        max_frame_bytes,
        request: request.clone(),
    };
    let mut child = tokio::process::Command::new(&executable)
        .arg(WORKER_MODE_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_WORKER_SPAWN",
                format!("failed to spawn {}: {error}", executable.display()),
            )
        })?;
    let pid = child.id().ok_or_else(|| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_SPAWN",
            "thread worker did not expose a native process id",
        )
    })?;
    let Some(mut input) = child.stdin.take() else {
        terminate_and_reap(&mut child, control.teardown_timeout).await?;
        return Err(worker_pipe_closed());
    };
    let Some(mut output) = child.stdout.take() else {
        terminate_and_reap(&mut child, control.teardown_timeout).await?;
        return Err(worker_pipe_closed());
    };
    let startup_result = async {
        write_frame_async(&mut input, &startup, MAX_STARTUP_HEADER_BYTES).await?;
        input.write_all(&module).await.map_err(worker_write_error)?;
        input.flush().await.map_err(worker_write_error)
    }
    .await;
    if let Err(error) = startup_result {
        terminate_and_reap(&mut child, control.teardown_timeout).await?;
        return Err(error);
    }
    control.worker_pid.store(pid, Ordering::Release);
    let (control_sender, mut control_receiver) = tokio::sync::mpsc::channel(16);
    if let Err(error) = control.set_worker_input(control_sender) {
        terminate_and_reap(&mut child, control.teardown_timeout).await?;
        control.worker_pid.store(0, Ordering::Release);
        return Err(error);
    }

    let wall_clock_limit_ms = request.limits.wall_clock_limit_ms;
    let wall_clock = async move {
        if let Some(milliseconds) = wall_clock_limit_ms {
            tokio::time::sleep(Duration::from_millis(milliseconds)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(wall_clock);
    let result = loop {
        tokio::select! {
            biased;
            () = control.cancel_notify.notified() => {
                break Err(HostServiceError::new(
                    "ECANCELED",
                    "threaded WebAssembly worker was canceled",
                ));
            }
            () = &mut wall_clock => {
                break Err(HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_WALL_CLOCK_LIMIT",
                    "threaded WebAssembly worker exceeded its wall-clock budget",
                ).with_details(serde_json::json!({
                    "limitName": "limits.resources.maxWasmWallClockTimeMs",
                    "limit": request.limits.wall_clock_limit_ms,
                })));
            }
            control_frame = control_receiver.recv() => {
                if let Some(frame) = control_frame {
                    if let Err(error) = write_frame_async(&mut input, &frame, max_frame_bytes).await {
                        break Err(error);
                    }
                }
            }
            frame = read_frame_async::<_, WorkerFrame>(&mut output, max_frame_bytes) => {
                match frame {
                    Ok(WorkerFrame::Call { id, call }) => {
                        let reply = dispatch_worker_call(&host, call).await;
                        if let Err(error) = write_frame_async(
                            &mut input,
                            &ParentFrame::Reply { id, result: reply },
                            max_frame_bytes,
                        ).await {
                            break Err(error);
                        }
                    }
                    Ok(WorkerFrame::Stderr { bytes }) => {
                        if let Err(error) = host.publish_stderr(bytes).await {
                            break Err(error);
                        }
                    }
                    Ok(WorkerFrame::Finished { result }) => {
                        if let Err(error) = write_frame_async(
                            &mut input,
                            &ParentFrame::FinishedAck,
                            max_frame_bytes,
                        ).await {
                            break Err(error);
                        }
                        break result;
                    }
                    Ok(WorkerFrame::GroupFailed { error }) => break Err(error),
                    Err(error) => break Err(error),
                }
            }
        }
    };
    control.clear_worker_input();
    let cleanup = reap_after_result(&mut child, control.teardown_timeout, result.is_err()).await;
    control.worker_pid.store(0, Ordering::Release);
    cleanup?;
    result
}

async fn reap_after_result(
    child: &mut tokio::process::Child,
    timeout: Duration,
    force: bool,
) -> Result<(), HostServiceError> {
    let status = match child.try_wait() {
        Ok(status) => status,
        Err(error) => {
            let primary = worker_wait_error(error);
            return match terminate_and_reap(child, timeout).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combined_cleanup_error(primary, cleanup)),
            };
        }
    };
    if status.is_some() {
        return Ok(());
    }
    if force {
        if let Err(error) = child.start_kill() {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_WORKER_KILL: initial forced termination failed: {error}"
            );
            terminate_and_reap(child, timeout).await?;
            return Ok(());
        }
    }
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            let primary = worker_wait_error(error);
            match terminate_and_reap(child, timeout).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combined_cleanup_error(primary, cleanup)),
            }
        }
        Err(_) => {
            let primary = HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_WORKER_REAP_TIMEOUT",
                "threaded WebAssembly worker exceeded the cooperative reaping deadline",
            );
            match terminate_and_reap(child, timeout).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combined_cleanup_error(primary, cleanup)),
            }
        }
    }
}

async fn terminate_and_reap(
    child: &mut tokio::process::Child,
    timeout: Duration,
) -> Result<(), HostServiceError> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(error) => eprintln!(
            "ERR_AGENTOS_WASMTIME_WORKER_WAIT: pre-kill status check failed; forcing termination: {error}"
        ),
    }
    if let Err(error) = child.start_kill() {
        eprintln!(
            "ERR_AGENTOS_WASMTIME_WORKER_KILL: forced termination request failed; waiting for reap: {error}"
        );
    }
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(worker_wait_error(error)),
        Err(_) => Err(HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_REAP_TIMEOUT",
            "threaded WebAssembly worker could not be reaped after forced termination",
        )),
    }
}

fn combined_cleanup_error(
    primary: HostServiceError,
    cleanup: HostServiceError,
) -> HostServiceError {
    HostServiceError::new(
        "ERR_AGENTOS_WASMTIME_WORKER_CLEANUP",
        format!(
            "worker cleanup failed after {}: {}; cleanup failure {}: {}",
            primary.code, primary.message, cleanup.code, cleanup.message
        ),
    )
}

async fn dispatch_worker_call(
    host: &super::store::WasmtimeHostClient,
    call: WorkerCall,
) -> Result<HostCallReply, HostServiceError> {
    match call {
        WorkerCall::Adapter { method, args, raw } => {
            host.submit_adapter_call(
                method,
                args,
                raw.into_iter().map(|raw| (raw.index, raw.bytes)).collect(),
            )
            .await
        }
        WorkerCall::OpenExecutableImage { descriptor } => {
            host.submit(
                HostOperation::Process(ProcessOperation::OpenExecutableImage {
                    source: crate::host::ExecutableImageSource::Descriptor(descriptor),
                    resolution: None,
                }),
                std::mem::size_of::<u32>(),
            )
            .await
        }
        WorkerCall::ReadExecutableImage {
            handle,
            offset,
            max_bytes,
        } => {
            let limit = crate::backend::PayloadLimit::new(
                "limits.wasm.workerExecutableReadBytes",
                max_bytes,
            )?;
            host.submit(
                HostOperation::Process(ProcessOperation::ReadExecutableImage {
                    handle,
                    offset,
                    max_bytes: crate::host::BoundedUsize::try_new(max_bytes, &limit)?,
                }),
                std::mem::size_of::<u64>() * 2 + std::mem::size_of::<usize>(),
            )
            .await
        }
        WorkerCall::CloseExecutableImage { handle } => {
            host.submit(
                HostOperation::Process(ProcessOperation::CloseExecutableImage { handle }),
                std::mem::size_of::<u64>(),
            )
            .await
        }
        WorkerCall::Signal(operation) => {
            host.submit(
                HostOperation::Signal(operation),
                std::mem::size_of::<SignalOperation>(),
            )
            .await
        }
    }
}

fn worker_executable() -> Result<PathBuf, HostServiceError> {
    if let Some(path) = std::env::var_os("AGENTOS_WASMTIME_WORKER_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_PATH",
            "AGENTOS_WASMTIME_WORKER_PATH does not name a file",
        ));
    }
    let current = std::env::current_exe().map_err(|error| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_PATH",
            format!("cannot resolve current executable: {error}"),
        )
    })?;
    if current
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "agentos-native-sidecar")
    {
        return Ok(current);
    }
    let sibling = current
        .parent()
        .and_then(|directory| directory.parent())
        .map(|directory| directory.join("agentos-native-sidecar"));
    sibling.filter(|path| path.is_file()).ok_or_else(|| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_PATH",
            "cannot locate the agentos-native-sidecar worker executable",
        )
    })
}

fn encode<T: Serialize>(value: &T, maximum: usize) -> Result<Vec<u8>, HostServiceError> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|error| {
        HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_IPC_ENCODE", error.to_string())
    })?;
    if bytes.len() > maximum {
        return Err(HostServiceError::limit(
            "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
            "limits.wasm.workerIpcFrameBytes",
            maximum as u64,
            bytes.len() as u64,
        ));
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HostServiceError> {
    ciborium::from_reader(bytes).map_err(|error| {
        HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_IPC_DECODE", error.to_string())
    })
}

fn write_frame_blocking<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
    maximum: usize,
) -> Result<(), HostServiceError> {
    let bytes = encode(value, maximum)?;
    let length = u32::try_from(bytes.len()).map_err(|_| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
            "IPC frame exceeds u32",
        )
    })?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(worker_write_error)?;
    writer.write_all(&bytes).map_err(worker_write_error)?;
    writer.flush().map_err(worker_write_error)
}

fn read_frame_blocking<R: Read, T: DeserializeOwned>(
    reader: &mut R,
    maximum: usize,
) -> Result<T, HostServiceError> {
    let mut length = [0; 4];
    reader.read_exact(&mut length).map_err(worker_read_error)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > maximum {
        return Err(HostServiceError::limit(
            "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
            "limits.wasm.workerIpcFrameBytes",
            maximum as u64,
            length as u64,
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).map_err(worker_read_error)?;
    decode(&bytes)
}

async fn write_frame_async<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
    maximum: usize,
) -> Result<(), HostServiceError> {
    let bytes = encode(value, maximum)?;
    let length = u32::try_from(bytes.len()).map_err(|_| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
            "IPC frame exceeds u32",
        )
    })?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .map_err(worker_write_error)?;
    writer.write_all(&bytes).await.map_err(worker_write_error)?;
    writer.flush().await.map_err(worker_write_error)
}

async fn read_frame_async<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    maximum: usize,
) -> Result<T, HostServiceError> {
    let mut length = [0; 4];
    reader
        .read_exact(&mut length)
        .await
        .map_err(worker_read_error)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > maximum {
        return Err(HostServiceError::limit(
            "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT",
            "limits.wasm.workerIpcFrameBytes",
            maximum as u64,
            length as u64,
        ));
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(worker_read_error)?;
    decode(&bytes)
}

fn worker_read_error(error: std::io::Error) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_IPC_READ", error.to_string())
}

fn worker_write_error(error: std::io::Error) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_IPC_WRITE", error.to_string())
}

fn worker_wait_error(error: std::io::Error) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_WASMTIME_WORKER_WAIT", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn worker_ipc_rejects_oversized_declared_frames_before_payload_allocation() {
        let mut bytes = Cursor::new(1024_u32.to_be_bytes().to_vec());
        let error = read_frame_blocking::<_, Vec<u8>>(&mut bytes, 16)
            .expect_err("oversized frame header must fail closed");

        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT");
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("limitName"))
                .and_then(serde_json::Value::as_str),
            Some("limits.wasm.workerIpcFrameBytes")
        );
    }

    #[test]
    fn worker_ipc_rejects_malformed_cbor_with_a_stable_typed_error() {
        let mut bytes = Vec::from(1_u32.to_be_bytes());
        bytes.push(0xff);
        let error = read_frame_blocking::<_, Vec<u8>>(&mut Cursor::new(bytes), 16)
            .expect_err("invalid CBOR must not enter worker state");

        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_WORKER_IPC_DECODE");
    }

    #[test]
    fn worker_ipc_rejects_truncated_payloads_with_a_stable_typed_error() {
        let mut bytes = Vec::from(4_u32.to_be_bytes());
        bytes.extend_from_slice(&[0x80]);
        let error = read_frame_blocking::<_, Vec<u8>>(&mut Cursor::new(bytes), 16)
            .expect_err("truncated worker frame must fail closed");

        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_WORKER_IPC_READ");
    }

    #[test]
    fn worker_ipc_bounds_encoded_payloads_before_writing_a_header() {
        let mut output = Vec::new();
        let error = write_frame_blocking(&mut output, &vec![0_u8; 32], 8)
            .expect_err("oversized encoded frame must not be written");

        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT");
        assert!(
            output.is_empty(),
            "failed frame must not write a partial header"
        );
    }
}
