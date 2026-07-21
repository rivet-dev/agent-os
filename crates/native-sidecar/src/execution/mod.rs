//! Process execution, networking, and runtime event handling extracted from service.rs.

mod child_process;
use self::child_process::*;
mod coordinator;
use self::coordinator::*;
mod launch;
pub(crate) use self::launch::sanitize_javascript_child_process_internal_bootstrap_env;
use self::launch::*;
mod host_dispatch;
use self::host_dispatch::*;
pub(crate) use host_dispatch::checked_deferred_guest_wait_deadline;
mod process;
use self::process::*;
pub(crate) use self::process::{settle_execution_host_call, terminate_child_process_tree};
mod process_events;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::process_events::send_binding_process_event;
use self::process_events::*;
pub(crate) use self::process_events::{
    mark_execute_exit_event_queued, record_execute_exit_event_queue_wait, record_execute_phase,
    record_execute_response_to_exit_milestone,
};
mod signals;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::signals::runtime_child_is_alive;
use self::signals::*;
pub(crate) use self::signals::{
    apply_kernel_signal_registration, canonical_signal_name, parse_signal,
    protocol_signal_registration, signal_runtime_process,
};
mod stdio;
use self::stdio::*;
pub(crate) use self::stdio::{
    close_kernel_process_stdin, flush_pending_kernel_stdin, install_kernel_ignored_stdin,
    kernel_poll_response, kernel_stdin_read_response, parse_kernel_poll_args,
    parse_kernel_stdin_read_args, service_javascript_kernel_fd_write_sync_rpc,
    write_kernel_process_stdin,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::stdio::{drain_tty_master_output, install_kernel_stdin_pipe};
mod network;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::network::reserve_udp_receive_buffer;
use self::network::*;
pub(crate) use self::network::{
    build_socket_path_context, finalize_net_connect, format_dns_resource,
    reserve_tls_write_payload, HickoryDnsResolver,
};
mod javascript;
use self::javascript::*;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::javascript::{
    clamp_javascript_net_poll_wait, service_javascript_net_sync_rpc, NetServiceRequest,
};
pub(crate) use self::javascript::{
    deferred_kernel_wait_request_for_process, dispatch_loopback_http_request_deferred,
    ensure_vm_fetch_response_frame_within_limit, error_code, host_bytes_value,
    host_service_error_code, javascript_sync_rpc_arg_bool, javascript_sync_rpc_arg_i32,
    javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional,
    javascript_sync_rpc_arg_u64, javascript_sync_rpc_arg_u64_optional,
    javascript_sync_rpc_bytes_arg, javascript_sync_rpc_encoding,
    javascript_sync_rpc_may_make_fd_readable, javascript_sync_rpc_may_make_fd_writable,
    javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32,
    service_javascript_crypto_sync_rpc, service_javascript_sync_rpc, HostServiceResponse,
    JavascriptSyncRpcServiceRequest, KernelPollFdRequest, LoopbackHttpDispatchRequest,
};
use agentos_vm_config as vm_config;

use crate::bindings::{
    format_binding_failure_output, is_binding_command, normalized_binding_command_name,
    resolve_binding_command, BindingCommandResolution,
};
use crate::filesystem::{
    service_javascript_fs_read_sync_rpc, service_javascript_fs_readdir_raw_sync_rpc,
    service_javascript_fs_sync_rpc, service_javascript_module_sync_rpc,
};
use crate::protocol::{
    CloseStdinRequest, DgramBindOptions, DgramConnectOptions, DgramCreateSocketOptions,
    DgramSendOptions, EventFrame, EventPayload, ExecuteRequest, FindBoundUdpRequest,
    FindListenerRequest, GetProcessSnapshotRequest, GetResourceSnapshotRequest,
    GetSignalStateRequest, GetZombieTimerCountRequest, GuestKernelCallRequest,
    GuestKernelResultResponse, GuestRuntimeKind, JavascriptDnsLookupRequest,
    JavascriptDnsResolveRequest, JavascriptNetBindConnectedUnixRequest,
    JavascriptNetConnectRequest, JavascriptNetListenRequest, JavascriptNetReserveTcpPortRequest,
    KillProcessRequest, OwnershipScope, ProcessExitedEvent, ProcessOutputEvent,
    ProcessSnapshotEntry, ProcessSnapshotStatus, PtyResizedResponse, QueueSnapshotEntry,
    RequestFrame, ResizePtyRequest, ResourceSnapshotResponse, ResponseFrame, ResponsePayload,
    RetainedExecutionLanguage, SidecarRequestPayload, SignalDispositionAction,
    SignalHandlerRegistration, SocketStateEntry, StandaloneWasmBackend, StreamChannel,
    VmFetchRequest, VmFetchResponse, WasmPermissionTier, WriteStdinRequest,
};
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event_or_stderr,
    javascript_error, kernel_error, log_stale_process_event, normalize_host_path, normalize_path,
    parse_javascript_child_process_spawn_request, path_is_within_root,
    process_event_queue_overflow_error, python_error, wasm_error,
};
use crate::state::{
    async_completion_channel, tcp_socket_event_retained_bytes, unix_listener_event_retained_bytes,
    ActiveCipherSession, ActiveDhSession, ActiveDiffieHellmanSession, ActiveEcdhSession,
    ActiveExecutableImage, ActiveExecution, ActiveExecutionEvent, ActiveHashSession,
    ActiveHttp2Server, ActiveHttp2Session, ActiveHttp2Stream, ActiveHttpServer, ActiveProcess,
    ActiveRealIntervalTimer, ActiveSqliteDatabase, ActiveSqliteStatement, ActiveTcpListener,
    ActiveTcpSocket, ActiveTlsState, ActiveUdpSocket, ActiveUnixListener, ActiveUnixSocket,
    AsyncCompletionReceiver, AsyncCompletionSender, BindingExecution, BridgeError, DatagramEvent,
    DeferredGuestWait, DeferredGuestWaitKind, DeferredKernelPoll, DeferredKernelRead,
    DeferredKernelReadResponse, ExecutionAdapterPolicy, ExecutionHostCall, ExitedProcessSnapshot,
    GuestUnixAddress, GuestUnixAddressRegistry, GuestUnixAddressRegistryEntry,
    GuestUnixConnectionState, HostNetTransferDescription, HostNetTransferDescriptionRegistry,
    Http2BridgeEvent, Http2ResponseSender, Http2RuntimeSnapshot, Http2SessionCommand,
    Http2SessionSnapshot, Http2SocketSnapshot, HttpLoopbackTarget, KernelSocketReadinessEvent,
    KernelSocketReadinessRegistry, KernelSocketReadinessTarget, ListenerConnectionRetirement,
    NativeCapabilityKey, NativePlainSocketCommand, NativeTlsCommand, NativeUdpCommand,
    NativeUdpSendPayload, NativeUdpSocketOption, NetworkResourceCounts, PendingChildProcessSync,
    PendingChildProcessSyncCompletion, PendingHttpRequest, PendingKernelStdin, PendingNetConnect,
    PendingNetConnectState, PendingTcpSocket, PendingUnixConnectionGuard, PendingUnixSocket,
    PlainSocketWritePayload, ProcNetEntry, ProcessEventEnvelope, QueuedHttp2Command,
    QueuedHttp2Event, ReactorIoLimits, ResolvedChildProcessExecution, ResolvedTcpConnectAddr,
    SharedBridge, SharedSidecarRequestClient, SidecarKernel, SocketDescriptionLease, SocketFamily,
    SocketPathContext, SocketQueryKind, SocketReadState, SocketReadTerminal,
    SocketReadinessRegistration, SocketReadinessSubscribers, TcpListenerEvent, TcpSocketEvent,
    TlsBridgeOptions, TlsClientHello, TlsDataValue, TlsMaterial, TlsWritePayload, UdpFamily,
    UnixListenerEvent, VmDnsConfig, VmFetchBodyMode, VmFetchStreamState, VmListenPolicy,
    VmPendingBudgetReservation, VmPendingByteBudget, VmState, BINDING_DRIVER_NAME,
    DEFAULT_NET_BACKLOG, EXECUTION_DRIVER_NAME, EXECUTION_SANDBOX_ROOT_ENV, JAVASCRIPT_COMMAND,
    LOOPBACK_EXEMPT_PORTS_ENV, PYTHON_COMMAND, VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY,
    WASM_COMMAND, WASM_EXEC_COMMIT_RPC_ENV, WASM_STDIO_SYNC_RPC_ENV,
};
use crate::wire::{ProtocolFrame as WireProtocolFrame, WireFrameCodec};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use base64::Engine;
use bytes::Bytes;
use h2::{client, server, Reason};
use hickory_resolver::proto::rr::{RData, Record, RecordType};
use hmac::{Hmac, Mac};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, Uri};
use md5::Md5;
use nix::libc;
use nix::poll::{poll, PollFd as NixPollFd, PollFlags, PollTimeout};
use nix::sys::signal::{kill as send_signal, Signal};
#[cfg(target_os = "linux")]
use nix::sys::socket::connect as connect_socket;
use nix::sys::socket::{bind as bind_socket, UnixAddr};
use nix::sys::wait::WaitStatus;
#[cfg(not(target_os = "macos"))]
use nix::sys::wait::{waitid as wait_on_child, Id as WaitId, WaitPidFlag};
#[cfg(target_os = "macos")]
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::Pid;
use openssl::bn::{BigNum, BigNumContext};
use openssl::derive::Deriver;
use openssl::dh::Dh;
use openssl::ec::{EcGroup, EcKey, EcPoint, PointConversionForm};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{Id as PKeyId, PKey, Params, Private, Public};
use openssl::rand::rand_bytes;
use openssl::rsa::{Padding, Rsa};
use openssl::sign::{Signer, Verifier};
use pbkdf2::pbkdf2_hmac;

use crate::crypto_cipher::{CipherError as AesCipherError, StreamCipherSession};
use agentos_bridge::{queue_tracker, LifecycleState};
use agentos_execution::host::{
    ClockOperation, HostOperation, HostProcessContext, ProcessHostCapabilitySet,
    ProcessLaunchOptions, ProcessLaunchRequest, ProcessOperation, ProcessSpawnFileAction,
    ProcessSpawnHostNetworkDescriptor,
};
use agentos_execution::{
    backend::{
        bounded_execution_event_channel, DescendantOutputOwnership, DescendantWaitOwnership,
        DirectHostReplyHandle, ExecutionBackend, ExecutionBackendKind, ExecutionEvent,
        ExecutionExit, ExecutionWakeHandle, ExecutionWakeIdentity, HostCallIdentity, HostCallReply,
        HostServiceError, PayloadLimit, PublishedSignalCheckpoint, ShutdownOutcome, ShutdownReason,
        SignalCheckpointOutcome, SynchronousFdWritePolicy,
    },
    javascript::handle_internal_bridge_call_from_host_context,
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    ExecutionSignalDispositionAction, ExecutionSignalHandlerRegistration, GuestRuntimeConfig,
    HostRpcRequest, JavascriptExecutionEvent, JavascriptExecutionLimits,
    JavascriptSyncRpcResponder, PythonExecutionEvent, PythonExecutionLimits, PythonVfsRpcResponder,
    StandaloneWasmBackend as ExecutionStandaloneWasmBackend, StartJavascriptExecutionRequest,
    StartPythonExecutionRequest, StartWasmExecutionRequest, WasmExecutionEvent,
    WasmExecutionLimits, WasmPermissionTier as ExecutionWasmPermissionTier,
    TRUSTED_INITIAL_MODULE_PREFIX,
};
use agentos_kernel::dns::{
    DnsLookupPolicy, DnsRecordResolution, DnsResolutionSource as KernelDnsResolutionSource,
};
use agentos_kernel::fd_table::TransferredFd;
use agentos_kernel::kernel::{
    FdTransferRequest, KernelProcessHandle, ReceivedFdRight, SpawnOptions, VirtualProcessOptions,
};
pub(crate) use agentos_kernel::network_policy::format_tcp_resource;
use agentos_kernel::network_policy::{
    is_loopback_ip, loopback_cidr, restricted_non_loopback_ip_range,
};
use agentos_kernel::permissions::NetworkOperation;
use agentos_kernel::poll::{PollEvents, PollFd, PollTargetEntry, POLLERR, POLLHUP, POLLIN};
use agentos_kernel::process_runtime::ProcessRuntimeIdentity;
use agentos_kernel::process_table::{
    ProcessPermissionTier, ProcessStatus, SigmaskHow, SignalSet, WaitPidFlags, SIGTERM,
};
use agentos_kernel::pty::MAX_PTY_BUFFER_BYTES;
use agentos_kernel::socket_table::{
    reset_socket_read_trace, set_socket_read_trace_enabled, socket_read_trace_snapshot,
    InetSocketAddress, SocketDomain, SocketId, SocketShutdown as KernelSocketShutdown, SocketSpec,
    SocketState, SocketType,
};
use agentos_kernel::system::KernelClockId;
use agentos_native_sidecar_core::ca::CA_CERTIFICATES_GUEST_PATH;
use agentos_native_sidecar_core::{
    bound_udp_snapshot_response, bridge_buffer_value, decode_base64, decode_bridge_buffer_value,
    decode_encoded_bytes_value, encoded_bytes_value,
    ensure_vm_fetch_raw_response_buffer_within_limit, ensure_vm_fetch_response_within_limit,
    listener_snapshot_response, local_endpoint_value, parse_kernel_http_fetch_response,
    parse_process_signal_state_request, process_killed_response,
    process_snapshot_entry_from_kernel, process_snapshot_response, process_started_response,
    remote_endpoint_value, shared_guest_runtime_identity_with_system, signal_state_response,
    socket_addr_family, socket_address_value, stdin_closed_response, stdin_written_response,
    tcp_socket_info_value, unix_socket_info_value, zombie_timer_count_response,
    SharedProcessSnapshotEntry, SharedProcessSnapshotStatus, SidecarCoreError,
    VM_FETCH_BUFFER_LIMIT_BYTES,
};
use agentos_runtime::accounting::{
    LimitError, Reservation, ResourceClass, ResourceLedger, ResourceLimit, SharedReservation,
};
use agentos_runtime::capability::{
    CapabilityBackend, CapabilityKind, CapabilityRegistry, PendingCapability,
};
use agentos_runtime::fairness::{FairBudget, FairWorkTurn};
use rusqlite::types::ValueRef as SqliteValueRef;
use rusqlite::{
    backup::Backup as SqliteBackup, Connection as SqliteConnection, OpenFlags as SqliteOpenFlags,
    Statement as SqliteStatement,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use scrypt::{scrypt, Params as ScryptParams};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha1::Sha1;
use sha2::{digest::Digest, Sha224, Sha256, Sha384, Sha512};
use socket2::{Domain, SockAddr, SockRef, Socket, TcpKeepalive, Type};
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::future::Future;
use std::io::{Cursor, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
    UdpSocket,
};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc::{
    channel as tokio_channel, error::TryRecvError as TokioTryRecvError, Receiver as TokioReceiver,
    Sender as TokioSender,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use url::Url;

const DEFAULT_KERNEL_STDIN_READ_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS: u64 = 100;
const NET_TIMEOUT_SENTINEL: &str = "__agentos_net_timeout__";
const PYTHON_PYODIDE_GUEST_ROOT: &str = "/__agentos_pyodide";
const PYTHON_PYODIDE_CACHE_GUEST_ROOT: &str = "/__agentos_pyodide_cache";
fn reactor_io_limits(limits: &crate::limits::VmLimits) -> ReactorIoLimits {
    ReactorIoLimits {
        operation_quantum: limits.reactor.per_handle_operation_quantum,
        byte_quantum: limits.reactor.byte_quantum,
        accept_quantum: limits.reactor.accept_quantum,
        datagram_quantum: limits.reactor.datagram_quantum,
        max_handle_commands: limits.reactor.max_handle_commands,
        max_async_completions: limits.reactor.max_async_completions,
        operation_deadline: Duration::from_millis(limits.reactor.operation_deadline_ms),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeadlineLimitWarning {
    pub(crate) limit_name: &'static str,
    pub(crate) operation: String,
    pub(crate) observed_ms: u128,
    pub(crate) limit_ms: u128,
}

type DeadlineLimitWarningHandler = Arc<dyn Fn(&DeadlineLimitWarning) + Send + Sync>;

fn deadline_limit_warning_handler() -> &'static Mutex<Option<DeadlineLimitWarningHandler>> {
    static HANDLER: OnceLock<Mutex<Option<DeadlineLimitWarningHandler>>> = OnceLock::new();
    HANDLER.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn set_deadline_limit_warning_handler(
    handler: impl Fn(&DeadlineLimitWarning) + Send + Sync + 'static,
) {
    *deadline_limit_warning_handler()
        .lock()
        .expect("deadline warning handler") = Some(Arc::new(handler));
}

pub(crate) fn emit_deadline_limit_warning(operation: &str, observed: Duration, limit: Duration) {
    let warning = DeadlineLimitWarning {
        limit_name: "limits.reactor.operationDeadlineMs",
        operation: operation.to_owned(),
        observed_ms: observed.as_millis(),
        limit_ms: limit.as_millis(),
    };
    eprintln!(
        "WARN_AGENTOS_DEADLINE_NEAR_LIMIT: operation={} observed_ms={} limit_ms={} config={}",
        warning.operation, warning.observed_ms, warning.limit_ms, warning.limit_name
    );
    let handler = match deadline_limit_warning_handler().lock() {
        Ok(handler) => handler.clone(),
        Err(_) => {
            eprintln!(
                "ERR_AGENTOS_DEADLINE_WARNING_HANDLER_POISONED: deadline warning handler was poisoned by a prior panic"
            );
            None
        }
    };
    if let Some(handler) = handler {
        handler(&warning);
    }
}

/// Synchronous state machine for one operation-deadline budget. It is usable
/// by both Tokio futures and poll/re-entry paths, and preserves the original
/// start plus the single warning edge when reconstructed from a parked RPC.
#[derive(Debug, Clone)]
pub(crate) struct OperationDeadlineTracker {
    started: Instant,
    warning_at: Instant,
    deadline: Instant,
    limit: Duration,
    warning_emitted: bool,
}

impl OperationDeadlineTracker {
    pub(crate) fn new(limit: Duration) -> Self {
        let started = Instant::now();
        Self {
            started,
            warning_at: started + limit.saturating_mul(4) / 5,
            deadline: started + limit,
            limit,
            warning_emitted: false,
        }
    }

    pub(crate) fn from_deadline(deadline: Instant, limit: Duration, warning_emitted: bool) -> Self {
        let started = deadline.checked_sub(limit).unwrap_or(deadline);
        Self {
            started,
            warning_at: started + limit.saturating_mul(4) / 5,
            deadline,
            limit,
            warning_emitted,
        }
    }

    pub(crate) fn observe(&mut self, operation: &str) {
        let now = Instant::now();
        if !self.warning_emitted && now >= self.warning_at {
            self.warning_emitted = true;
            emit_deadline_limit_warning(
                operation,
                now.saturating_duration_since(self.started).min(self.limit),
                self.limit,
            );
        }
    }

    pub(crate) fn next_edge(&self) -> Instant {
        if self.warning_emitted {
            self.deadline
        } else {
            self.warning_at
        }
    }

    pub(crate) fn remaining_until_next_edge(&self) -> Duration {
        self.next_edge().saturating_duration_since(Instant::now())
    }

    pub(crate) fn remaining_until_deadline(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub(crate) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn warning_emitted(&self) -> bool {
        self.warning_emitted
    }
}

/// Await one reactor operation with the configured hard deadline and a single
/// host-visible warning after 80% of that budget. The operation future remains
/// pinned across the warning edge; no work is restarted or duplicated.
pub(crate) async fn operation_deadline_timeout<F>(
    operation: &str,
    limit: Duration,
    future: F,
) -> Result<F::Output, tokio::time::error::Elapsed>
where
    F: Future,
{
    operation_deadline_timeout_with_tracker(operation, OperationDeadlineTracker::new(limit), future)
        .await
}

async fn operation_deadline_timeout_with_tracker<F>(
    operation: &str,
    mut deadline: OperationDeadlineTracker,
    future: F,
) -> Result<F::Output, tokio::time::error::Elapsed>
where
    F: Future,
{
    tokio::pin!(future);
    match tokio::time::timeout_at(deadline.next_edge().into(), &mut future).await {
        Ok(output) => Ok(output),
        Err(_) => {
            deadline.observe(operation);
            tokio::time::timeout_at(deadline.deadline().into(), &mut future).await
        }
    }
}

fn socket_completion_capacity(limits: ReactorIoLimits) -> usize {
    debug_assert!(
        limits.max_async_completions > 0,
        "limits.reactor.maxAsyncCompletions is validated before VM admission"
    );
    limits.max_async_completions
}

fn listener_accept_capacity(backlog: Option<u32>, limits: ReactorIoLimits) -> usize {
    usize::try_from(backlog.unwrap_or(DEFAULT_NET_BACKLOG))
        .expect("default backlog fits within usize")
        .max(1)
        .min(socket_completion_capacity(limits))
}

const BINDING_HOST_CALL_BLOCKING_JOB_BYTES: usize = 64 * 1024;

pub(crate) const MAX_PER_PROCESS_STATE_HANDLES: usize = 1024;
const HTTP_LOOPBACK_REQUEST_TIMEOUT_MS_ENV: &str = "AGENTOS_TEST_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS";

#[cfg(test)]
mod configured_socket_capacity_tests {
    use super::{
        listener_accept_capacity, operation_deadline_timeout,
        operation_deadline_timeout_with_tracker, reactor_io_limits,
        set_deadline_limit_warning_handler, socket_completion_capacity, write_all_nonblocking,
        OperationDeadlineTracker,
    };
    use crate::limits::VmLimits;
    use std::io::{self, Write};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct AlwaysWouldBlock {
        fd: UnixStream,
    }

    impl Write for AlwaysWouldBlock {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl AsFd for AlwaysWouldBlock {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }
    }

    #[test]
    fn socket_and_accept_queues_are_individually_bounded_by_vm_completion_limit() {
        let mut limits = VmLimits::default();
        limits.reactor.max_async_completions = 3;
        let reactor = reactor_io_limits(&limits);

        assert_eq!(socket_completion_capacity(reactor), 3);
        assert_eq!(listener_accept_capacity(Some(100), reactor), 3);
        assert_eq!(listener_accept_capacity(Some(2), reactor), 2);
    }

    #[tokio::test]
    async fn operation_deadline_warns_at_eighty_percent_before_success_or_typed_expiry() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let (completion_sender, completion_receiver) = tokio::sync::oneshot::channel();
        let completion_sender = Arc::new(Mutex::new(Some(completion_sender)));
        let warning_completion_sender = Arc::clone(&completion_sender);
        set_deadline_limit_warning_handler(move |warning| {
            if warning.operation.starts_with("deadline-test-")
                || warning.operation == "synchronous socket write"
            {
                sink.lock()
                    .expect("deadline warning sink")
                    .push(warning.clone());
            }
            if warning.operation == "deadline-test-completes-near-limit" {
                if let Some(sender) = warning_completion_sender
                    .lock()
                    .expect("deadline completion sender")
                    .take()
                {
                    sender.send(()).expect("release post-warning completion");
                }
            }
        });

        // Construct the same 80%-warning state with the warning edge already
        // reached and one full second remaining. A 5 ms real-time window made
        // this regression test fail under concurrent linker load even though
        // the production state machine behaved correctly.
        let completed = operation_deadline_timeout_with_tracker(
            "deadline-test-completes-near-limit",
            OperationDeadlineTracker::from_deadline(
                Instant::now() + Duration::from_secs(1),
                Duration::from_secs(5),
                false,
            ),
            async move {
                completion_receiver
                    .await
                    .expect("warning releases completion");
                7
            },
        )
        .await
        .expect("operation may complete after the warning and before expiry");
        assert_eq!(completed, 7);

        operation_deadline_timeout(
            "deadline-test-expires",
            Duration::from_millis(50),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .expect_err("operation must still expire at the hard deadline");

        let warnings = captured.lock().expect("deadline warnings").clone();
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].limit_name, "limits.reactor.operationDeadlineMs");
        assert_eq!(warnings[0].limit_ms, 5_000);
        assert!(
            warnings[0].observed_ms >= 4_000 && warnings[0].observed_ms < 5_000,
            "warning must precede the hard deadline: {:?}",
            warnings[0]
        );
        assert_eq!(warnings[1].limit_name, "limits.reactor.operationDeadlineMs");
        assert_eq!(warnings[1].limit_ms, 50);
        assert!(
            warnings[1].observed_ms >= 40 && warnings[1].observed_ms <= 50,
            "warning must reach the hard deadline: {:?}",
            warnings[1]
        );

        // The synchronous TCP/Unix write path uses the same warning state
        // machine even though its readiness wait is `poll(2)`, not a Future.
        let (fd, _peer) = UnixStream::pair().expect("create deadline test fd");
        let mut blocked = AlwaysWouldBlock { fd };
        let mut limits = VmLimits::default();
        limits.reactor.operation_deadline_ms = 25;
        let error = write_all_nonblocking(&mut blocked, b"x", reactor_io_limits(&limits))
            .expect_err("permanently blocked synchronous write must expire");
        assert!(error.to_string().contains("ERR_AGENTOS_OPERATION_DEADLINE"));

        // A readiness wake may re-park the same RPC. Reconstructing from its
        // absolute deadline and warning bit must neither reset the clock nor
        // emit a duplicate warning.
        let mut parked = super::OperationDeadlineTracker::new(Duration::from_millis(25));
        tokio::time::sleep(Duration::from_millis(21)).await;
        parked.observe("deadline-test-repark");
        let mut reparked = super::OperationDeadlineTracker::from_deadline(
            parked.deadline(),
            Duration::from_millis(25),
            parked.warning_emitted(),
        );
        reparked.observe("deadline-test-repark");
        tokio::time::sleep(reparked.remaining_until_deadline()).await;
        assert!(reparked.expired());

        let warnings = captured
            .lock()
            .expect("deadline warnings after sync paths")
            .clone();
        assert_eq!(warnings.len(), 4);
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| warning.operation == "synchronous socket write")
                .count(),
            1
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| warning.operation == "deadline-test-repark")
                .count(),
            1
        );
    }
}
