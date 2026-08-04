//! Process execution, networking, and runtime event handling extracted from service.rs.

mod child_process;
use self::child_process::*;
pub(crate) use self::child_process::{
    service_owned_child_bridge_event, OwnedChildBridgeEventService,
};
mod coordinator;
use self::coordinator::*;
pub(crate) use self::coordinator::{
    close_stdin_owned, guest_kernel_core_error, resize_pty_owned, write_stdin_owned,
    OwnedVmRouteFuture, OwnedVmRouteInput,
};
mod launch;
use self::launch::*;
pub(crate) use self::launch::{
    execute_owned, host_path_from_runtime_guest_mappings, initial_shadow_sync_inventory,
    is_protected_agentos_shadow_sync_path,
    sanitize_javascript_child_process_internal_bootstrap_env,
    sync_active_process_host_writes_to_kernel, sync_process_host_writes_to_kernel,
};
mod process;
pub(crate) use self::process::terminate_child_process_tree;
use self::process::*;
mod process_events;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::process_events::send_binding_process_event;
use self::process_events::*;
pub(crate) use self::process_events::{
    mark_execute_exit_event_queued, record_execute_exit_event_queue_wait, record_execute_phase,
    record_execute_response_to_exit_milestone, OwnedJavascriptEventService,
    OwnedPythonEventService, OwnedPythonSocketCompletionService,
};
pub(crate) use self::signals::kill_process_owned;
mod signals;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::signals::runtime_child_is_alive;
use self::signals::*;
pub(crate) use self::signals::{
    apply_active_process_default_signal, canonical_signal_name, parse_signal,
    signal_runtime_process,
};
mod stdio;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::stdio::drain_tty_master_output;
use self::stdio::*;
pub(crate) use self::stdio::{
    close_kernel_process_stdin, flush_pending_kernel_stdin, install_kernel_stdin_pipe,
    kernel_poll_response, kernel_stdin_read_response, parse_kernel_poll_args,
    parse_kernel_stdin_read_args, service_javascript_kernel_fd_write_sync_rpc,
    write_kernel_process_stdin,
};
mod network;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::network::reserve_udp_receive_buffer;
use self::network::*;
pub(crate) use self::network::{
    build_javascript_socket_path_context, finalize_javascript_net_connect, format_dns_resource,
    reserve_tls_write_payload,
};
mod javascript;
use self::javascript::*;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use self::javascript::{
    clamp_javascript_net_poll_wait, service_javascript_net_sync_rpc,
    JavascriptNetSyncRpcServiceRequest,
};
pub(crate) use self::javascript::{
    deferred_kernel_wait_request_for_process, dispatch_loopback_http_request,
    dispatch_loopback_http_request_deferred, ensure_vm_fetch_response_frame_within_limit,
    error_code, ignore_stale_javascript_sync_rpc_response, javascript_sync_rpc_arg_bool,
    javascript_sync_rpc_arg_i32, javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32,
    javascript_sync_rpc_arg_u32_optional, javascript_sync_rpc_arg_u64,
    javascript_sync_rpc_arg_u64_optional, javascript_sync_rpc_bytes_arg,
    javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding, javascript_sync_rpc_error_code,
    javascript_sync_rpc_may_make_fd_readable, javascript_sync_rpc_may_make_fd_writable,
    javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32,
    service_javascript_crypto_sync_rpc, service_javascript_sync_rpc,
    service_owned_javascript_sync_rpc_request, settle_javascript_sync_rpc_completion,
    JavascriptSyncRpcServiceRequest, JavascriptSyncRpcServiceResponse, KernelPollFdRequest,
    LoopbackHttpDispatchRequest,
};
mod python;
pub(crate) use self::python::{
    prepare_owned_python_process_event_service, respond_owned_python_rpc,
    service_owned_python_socket_connect_completion, service_owned_python_vfs_rpc_request,
};

use agentos_vm_config as vm_config;

use crate::bindings::{
    format_binding_failure_output, is_binding_command, normalized_binding_command_name,
    resolve_binding_command, BindingCommandResolution,
};
use crate::filesystem::{
    handle_python_vfs_rpc_request as filesystem_handle_python_vfs_rpc_request,
    service_javascript_fs_read_sync_rpc, service_javascript_fs_readdir_raw_sync_rpc,
    service_javascript_fs_sync_rpc, service_javascript_module_sync_rpc,
    service_owned_python_filesystem_rpc_request,
};
use crate::protocol::{
    CloseStdinRequest, EventFrame, EventPayload, ExecuteRequest, FindBoundUdpRequest,
    FindListenerRequest, GetProcessSnapshotRequest, GetResourceSnapshotRequest,
    GetSignalStateRequest, GetZombieTimerCountRequest, GuestKernelCallRequest,
    GuestKernelResultResponse, GuestRuntimeKind, JavascriptChildProcessSpawnOptions,
    JavascriptChildProcessSpawnRequest, JavascriptDgramBindRequest, JavascriptDgramConnectRequest,
    JavascriptDgramCreateSocketRequest, JavascriptDgramSendRequest, JavascriptDnsLookupRequest,
    JavascriptDnsResolveRequest, JavascriptNetBindConnectedUnixRequest,
    JavascriptNetConnectRequest, JavascriptNetListenRequest, JavascriptNetReserveTcpPortRequest,
    JavascriptPosixSpawnFileAction, JavascriptSpawnHostNetFd, KillProcessRequest, OwnershipScope,
    ProcessExitedEvent, ProcessOutputEvent, ProcessSnapshotEntry, ProcessSnapshotStatus,
    PtyResizedResponse, QueueSnapshotEntry, RequestFrame, ResizePtyRequest,
    ResourceSnapshotResponse, ResponseFrame, ResponsePayload, RetainedExecutionLanguage,
    SidecarRequestPayload, SignalDispositionAction, SignalHandlerRegistration, SocketStateEntry,
    StreamChannel, VmFetchRequest, VmFetchResponse, WasmPermissionTier, WriteStdinRequest,
};
use crate::service::{
    audit_fields, dirname, emit_security_audit_event, emit_structured_event_or_stderr,
    javascript_error, kernel_error, log_stale_process_event, normalize_host_path, normalize_path,
    parse_javascript_child_process_spawn_request, path_is_within_root,
    process_event_queue_overflow_error, python_error, wasm_error,
};
use crate::state::{
    async_completion_channel, ActiveCipherSession, ActiveDhSession, ActiveDiffieHellmanSession,
    ActiveEcdhSession, ActiveExecution, ActiveExecutionEvent, ActiveHashSession, ActiveHttp2Server,
    ActiveHttp2Session, ActiveHttp2Stream, ActiveHttpServer, ActiveMappedHostFd, ActiveProcess,
    ActiveRealIntervalTimer, ActiveSqliteDatabase, ActiveSqliteStatement, ActiveTcpListener,
    ActiveTcpSocket, ActiveTlsState, ActiveUdpSocket, ActiveUnixListener, ActiveUnixSocket,
    AsyncCompletionReceiver, AsyncCompletionSender, BindingExecution, BridgeError,
    ExitedProcessSnapshot, GuestUnixAddress, GuestUnixAddressRegistry,
    GuestUnixAddressRegistryEntry, GuestUnixConnectionState, HostNetTransferDescription,
    HostNetTransferDescriptionRegistry, Http2BridgeEvent, Http2ResponseSender,
    Http2RuntimeSnapshot, Http2SessionCommand, Http2SessionSnapshot, Http2SocketSnapshot,
    JavascriptHttpLoopbackTarget, JavascriptSocketFamily, JavascriptSocketPathContext,
    JavascriptTcpListenerEvent, JavascriptTcpSocketEvent, JavascriptTlsBridgeOptions,
    JavascriptTlsClientHello, JavascriptTlsDataValue, JavascriptTlsMaterial, JavascriptUdpFamily,
    JavascriptUdpSocketEvent, JavascriptUnixListenerEvent, KernelSocketReadinessEvent,
    KernelSocketReadinessRegistry, KernelSocketReadinessTarget, ListenerConnectionRetirement,
    NativeCapabilityKey, NativePlainSocketCommand, NativeTlsCommand, NativeUdpCommand,
    NativeUdpSendPayload, NativeUdpSocketOption, NetworkResourceCounts, PendingChildProcessSync,
    PendingChildProcessSyncCompletion, PendingHttpRequest, PendingJavascriptNetConnect,
    PendingJavascriptNetConnectState, PendingKernelStdin, PendingPythonTcpConnect,
    PendingTcpSocket, PendingUnixConnectionGuard, PendingUnixSocket, PlainSocketWritePayload,
    ProcNetEntry, ProcessEventEnvelope, PythonHostSocket, PythonSocketConnectCompletion,
    PythonTcpReadBuffer, QueuedHttp2Command, QueuedHttp2Event, ReactorIoLimits,
    ResolvedChildProcessExecution, ResolvedTcpConnectAddr, ShadowNodeType,
    ShadowSyncInventoryEntry, SharedBridge, SharedSidecarRequestClient, SidecarKernel,
    SocketDescriptionLease, SocketQueryKind, SocketReadinessRegistration,
    SocketReadinessSubscribers, TlsWritePayload, VmDnsConfig, VmFetchBodyMode, VmFetchStreamState,
    VmHandle, VmListenPolicy, VmPendingByteBudget, VmState, BINDING_DRIVER_NAME,
    DEFAULT_JAVASCRIPT_NET_BACKLOG, EXECUTION_DRIVER_NAME, EXECUTION_SANDBOX_ROOT_ENV,
    JAVASCRIPT_COMMAND, LOOPBACK_EXEMPT_PORTS_ENV, MAPPED_HOST_FD_START, PYTHON_COMMAND,
    VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY, WASM_COMMAND, WASM_EXEC_COMMIT_RPC_ENV,
    WASM_STDIO_SYNC_RPC_ENV,
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
use agentos_execution::wasm::WasmExecutionError;
use agentos_execution::{
    javascript::handle_internal_bridge_call_from_host_context, v8_host::V8SessionHandle,
    v8_runtime, CreateJavascriptContextRequest, CreatePythonContextRequest,
    CreateWasmContextRequest, GuestModuleReader, GuestRuntimeConfig, JavascriptExecutionEvent,
    JavascriptExecutionLimits, JavascriptSyncRpcRequest, ModuleFsReader,
    NodeSignalDispositionAction, NodeSignalHandlerRegistration, PythonExecutionEvent,
    PythonExecutionLimits, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponder,
    PythonVfsRpcResponsePayload, StartJavascriptExecutionRequest, StartPythonExecutionRequest,
    StartWasmExecutionRequest, WasmExecutionEvent, WasmExecutionLimits,
    WasmPermissionTier as ExecutionWasmPermissionTier,
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
use agentos_kernel::process_table::{ProcessStatus, WaitPidFlags, SIGKILL, SIGTERM};
use agentos_kernel::pty::MAX_PTY_BUFFER_BYTES;
use agentos_kernel::root_fs::RootFilesystemMode;
use agentos_kernel::socket_table::{
    reset_socket_read_trace, set_socket_read_trace_enabled, socket_read_trace_snapshot,
    InetSocketAddress, SocketDomain, SocketId, SocketShutdown as KernelSocketShutdown, SocketSpec,
    SocketState, SocketType,
};
use agentos_native_sidecar_core::ca::CA_CERTIFICATES_GUEST_PATH;
use agentos_native_sidecar_core::{
    apply_process_signal_state_update, bound_udp_snapshot_response, bridge_buffer_value,
    decode_base64, decode_bridge_buffer_value, decode_encoded_bytes_value, encoded_bytes_value,
    ensure_vm_fetch_raw_response_buffer_within_limit, ensure_vm_fetch_response_within_limit,
    listener_snapshot_response, local_endpoint_value, parse_kernel_http_fetch_response,
    parse_process_signal_state_request, process_killed_response,
    process_snapshot_entry_from_kernel, process_snapshot_response, process_started_response,
    remote_endpoint_value, shared_guest_runtime_identity, signal_state_response,
    socket_addr_family, socket_address_value, stdin_closed_response, stdin_written_response,
    tcp_socket_info_value, unix_socket_info_value, zombie_timer_count_response,
    SharedProcessSnapshotEntry, SharedProcessSnapshotStatus, SidecarCoreError,
    VM_FETCH_BUFFER_LIMIT_BYTES,
};
use agentos_runtime::accounting::{
    Reservation, ResourceClass, ResourceLedger, ResourceLimit, SharedReservation,
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
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
const JAVASCRIPT_NET_TIMEOUT_SENTINEL: &str = "__agentos_net_timeout__";
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

fn socket_completion_capacity(limits: ReactorIoLimits) -> usize {
    debug_assert!(
        limits.max_async_completions > 0,
        "limits.reactor.maxAsyncCompletions is validated before VM admission"
    );
    limits.max_async_completions
}

fn listener_accept_capacity(backlog: Option<u32>, limits: ReactorIoLimits) -> usize {
    usize::try_from(backlog.unwrap_or(DEFAULT_JAVASCRIPT_NET_BACKLOG))
        .expect("default backlog fits within usize")
        .max(1)
        .min(socket_completion_capacity(limits))
}

const BINDING_HOST_CALL_BLOCKING_JOB_BYTES: usize = 64 * 1024;

pub(crate) const MAX_PER_PROCESS_STATE_HANDLES: usize = 1024;
const HTTP_LOOPBACK_REQUEST_TIMEOUT_MS_ENV: &str = "AGENTOS_TEST_HTTP_LOOPBACK_REQUEST_TIMEOUT_MS";

#[cfg(test)]
mod configured_socket_capacity_tests {
    use super::{listener_accept_capacity, reactor_io_limits, socket_completion_capacity};
    use crate::limits::VmLimits;

    #[test]
    fn socket_and_accept_queues_are_individually_bounded_by_vm_completion_limit() {
        let mut limits = VmLimits::default();
        limits.reactor.max_async_completions = 3;
        let reactor = reactor_io_limits(&limits);

        assert_eq!(socket_completion_capacity(reactor), 3);
        assert_eq!(listener_accept_capacity(Some(100), reactor), 3);
        assert_eq!(listener_accept_capacity(Some(2), reactor), 2);
    }
}
