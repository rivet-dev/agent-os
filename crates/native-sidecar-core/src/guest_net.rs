//! Shared synchronous guest kernel-call dispatcher.
//!
//! Guest networking, and (later) spawn/WASI, syscalls flow through a single
//! generic synchronous wire payload (`GuestKernelCallRequest` ->
//! `GuestKernelResultResponse`) carrying an `operation` string plus a JSON
//! `payload`, mirroring the native `service_javascript_sync_rpc` design. This
//! module is the backend-agnostic dispatcher that decodes the operation and
//! routes it into the kernel, exactly as `guest_fs::handle_guest_filesystem_call`
//! does for the filesystem family. It is unit-tested without an executor.

use crate::SidecarCoreError;
use agentos_kernel::dns::DnsLookupPolicy;
use agentos_kernel::kernel::{KernelError, KernelVm};
use agentos_kernel::poll::{
    PollEvents, PollTargetEntry, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT,
};
use agentos_kernel::socket_table::{InetSocketAddress, SocketId, SocketShutdown, SocketSpec};
use agentos_kernel::vfs::VirtualFileSystem;
use base64::Engine;
use serde_json::{json, Value};

const DEFAULT_LOOPBACK_HOST: &str = "127.0.0.1";
const DEFAULT_LISTEN_BACKLOG: usize = 128;
const DEFAULT_READ_MAX_BYTES: usize = 64 * 1024;
/// Guest `net.poll` waits run on the sidecar's synchronous request path, so the
/// caller-requested timeout is clamped to a small ceiling: the executor blocks
/// on its own sync-bridge and re-polls rather than parking the sidecar. Mirrors
/// the native `clamp_javascript_net_poll_wait` 50 ms ceiling.
const MAX_POLL_WAIT_MS: i64 = 50;

/// Dispatch a single synchronous guest kernel call into the kernel.
///
/// `payload` is the JSON request body for `operation`; the returned bytes are
/// the JSON response body. Errors map kernel failures (including POSIX errno
/// codes) into [`SidecarCoreError`] so both backends surface identical messages.
pub fn handle_guest_kernel_call<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    requester_driver: &str,
    operation: &str,
    payload: &[u8],
) -> Result<Vec<u8>, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let request = decode_request(payload)?;

    let response = match operation {
        "net.connect" => net_connect(kernel, pid, requester_driver, &request)?,
        "net.listen" => net_listen(kernel, pid, requester_driver, &request)?,
        "net.accept" => net_accept(kernel, pid, requester_driver, &request)?,
        "net.write" => net_write(kernel, pid, requester_driver, &request)?,
        "net.read" => net_read(kernel, pid, requester_driver, &request)?,
        "net.poll" => net_poll(kernel, pid, requester_driver, &request)?,
        "net.shutdown" => net_shutdown(kernel, pid, requester_driver, &request)?,
        "net.close" => net_close(kernel, pid, requester_driver, &request)?,
        "net.udp_bind" => net_udp_bind(kernel, pid, requester_driver, &request)?,
        "net.send_to" => net_send_to(kernel, pid, requester_driver, &request)?,
        "net.recv_from" => net_recv_from(kernel, pid, requester_driver, &request)?,
        "dgram.create" => dgram_create(kernel, pid, requester_driver, &request)?,
        "dgram.bind" => dgram_bind(kernel, pid, requester_driver, &request)?,
        "dgram.send" => dgram_send(kernel, pid, requester_driver, &request)?,
        "dgram.recv" => dgram_recv(kernel, pid, requester_driver, &request)?,
        "dgram.close" => net_close(kernel, pid, requester_driver, &request)?,
        "dgram.address" => dgram_address(kernel, &request)?,
        "dns.lookup" => dns_lookup(kernel, &request)?,
        other if crate::guest_pty::is_pty_operation(other) => {
            crate::guest_pty::dispatch_pty_operation(
                kernel,
                pid,
                requester_driver,
                other,
                &request,
            )?
        }
        other => {
            return Err(SidecarCoreError::new(format!(
                "unsupported guest kernel call operation: {other}"
            )))
        }
    };

    serde_json::to_vec(&response).map_err(|error| {
        SidecarCoreError::new(format!(
            "failed to encode guest kernel call response: {error}"
        ))
    })
}

fn net_connect<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = require_port(request, "port")?;
    let socket_id = kernel
        .socket_create(driver, pid, SocketSpec::tcp())
        .map_err(kernel_error)?;
    if let Err(error) = kernel.socket_connect_inet_loopback(
        driver,
        pid,
        socket_id,
        InetSocketAddress::new(host, port),
    ) {
        if let Err(cleanup_error) = kernel.socket_close(driver, pid, socket_id) {
            eprintln!(
                "ERR_AGENTOS_SOCKET_ROLLBACK: failed to close socket {socket_id} after connect failure: {cleanup_error}"
            );
        }
        return Err(kernel_error(error));
    }
    let record = kernel.socket_get(socket_id);
    let local = record.as_ref().and_then(|record| record.local_address());
    Ok(json!({
        "socketId": socket_id,
        "localAddress": local.map(InetSocketAddress::host),
        "localPort": local.map(InetSocketAddress::port),
        "remoteAddress": host,
        "remotePort": port,
    }))
}

fn net_listen<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = require_port(request, "port")?;
    let backlog = optional_u64(request, "backlog")
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LISTEN_BACKLOG);
    let socket_id = kernel
        .socket_create(driver, pid, SocketSpec::tcp())
        .map_err(kernel_error)?;
    let result = kernel
        .socket_bind_inet(driver, pid, socket_id, InetSocketAddress::new(host, port))
        .and_then(|()| kernel.socket_listen(driver, pid, socket_id, backlog));
    if let Err(error) = result {
        if let Err(cleanup_error) = kernel.socket_close(driver, pid, socket_id) {
            eprintln!(
                "ERR_AGENTOS_SOCKET_ROLLBACK: failed to close socket {socket_id} after listen setup failure: {cleanup_error}"
            );
        }
        return Err(kernel_error(error));
    }
    let record = kernel.socket_get(socket_id);
    let local = record.as_ref().and_then(|record| record.local_address());
    Ok(json!({
        "socketId": socket_id,
        "localAddress": local.map(InetSocketAddress::host),
        "localPort": local.map(InetSocketAddress::port),
    }))
}

fn net_accept<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let listener = require_socket_id(request)?;
    match kernel.socket_accept(driver, pid, listener) {
        Ok(socket_id) => {
            let record = kernel.socket_get(socket_id);
            let peer = record.as_ref().and_then(|record| record.peer_address());
            Ok(json!({
                "socketId": socket_id,
                "remoteAddress": peer.map(InetSocketAddress::host),
                "remotePort": peer.map(InetSocketAddress::port),
            }))
        }
        Err(error) if would_block(&error) => Ok(json!({ "socketId": Value::Null })),
        Err(error) => Err(kernel_error(error)),
    }
}

fn net_write<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let data = decode_data(request)?;
    let written = kernel
        .socket_write(driver, pid, socket_id, &data)
        .map_err(kernel_error)?;
    Ok(json!({ "written": written }))
}

fn net_read<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let max_bytes = optional_u64(request, "maxBytes")
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_READ_MAX_BYTES);
    match kernel.socket_read(driver, pid, socket_id, max_bytes) {
        Ok(Some(bytes)) => Ok(json!({ "data": encode_data(&bytes), "closed": false })),
        Ok(None) => Ok(json!({ "data": Value::Null, "closed": true })),
        Err(error) if would_block(&error) => Ok(json!({ "data": Value::Null, "closed": false })),
        Err(error) => Err(kernel_error(error)),
    }
}

fn net_poll<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let events = optional_u64(request, "events")
        .map(|bits| PollEvents::from_bits(bits as u16))
        .unwrap_or(POLLIN | POLLOUT | POLLHUP | POLLERR);
    let timeout_ms = optional_u64(request, "timeoutMs")
        .map(|value| value as i64)
        .unwrap_or(0)
        .clamp(0, MAX_POLL_WAIT_MS) as i32;
    let result = kernel
        .poll_targets(
            driver,
            pid,
            vec![PollTargetEntry::socket(socket_id, events)],
            timeout_ms,
        )
        .map_err(kernel_error)?;
    let revents = result
        .targets
        .first()
        .map(|entry| entry.revents)
        .unwrap_or_else(PollEvents::empty);
    Ok(json!({
        "revents": revents.bits(),
        "readable": revents.intersects(POLLIN),
        "writable": revents.intersects(POLLOUT),
        "hangup": revents.intersects(POLLHUP),
        "error": revents.intersects(POLLERR | POLLNVAL),
    }))
}

fn net_udp_bind<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = require_port(request, "port")?;
    let socket_id = kernel
        .socket_create(driver, pid, SocketSpec::udp())
        .map_err(kernel_error)?;
    if let Err(error) =
        kernel.socket_bind_inet(driver, pid, socket_id, InetSocketAddress::new(host, port))
    {
        if let Err(cleanup_error) = kernel.socket_close(driver, pid, socket_id) {
            eprintln!(
                "ERR_AGENTOS_SOCKET_ROLLBACK: failed to close socket {socket_id} after UDP bind failure: {cleanup_error}"
            );
        }
        return Err(kernel_error(error));
    }
    let record = kernel.socket_get(socket_id);
    let local = record.as_ref().and_then(|record| record.local_address());
    Ok(json!({
        "socketId": socket_id,
        "localAddress": local.map(InetSocketAddress::host),
        "localPort": local.map(InetSocketAddress::port),
    }))
}

fn net_send_to<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = require_port(request, "port")?;
    let data = decode_data(request)?;
    let written = kernel
        .socket_send_to_inet_loopback(
            driver,
            pid,
            socket_id,
            InetSocketAddress::new(host, port),
            &data,
        )
        .map_err(kernel_error)?;
    Ok(json!({ "written": written }))
}

fn net_recv_from<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let max_bytes = optional_u64(request, "maxBytes")
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_READ_MAX_BYTES);
    match kernel.socket_recv_datagram(driver, pid, socket_id, max_bytes) {
        Ok(Some(datagram)) => {
            let source = datagram.source_address();
            Ok(json!({
                "data": encode_data(datagram.payload()),
                "remoteAddress": source.map(InetSocketAddress::host),
                "remotePort": source.map(InetSocketAddress::port),
            }))
        }
        Ok(None) => Ok(json!({ "data": Value::Null })),
        Err(error) if would_block(&error) => Ok(json!({ "data": Value::Null })),
        Err(error) => Err(kernel_error(error)),
    }
}

fn dgram_create<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    _request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = kernel
        .socket_create(driver, pid, SocketSpec::udp())
        .map_err(kernel_error)?;
    Ok(json!({ "socketId": socket_id }))
}

fn dgram_bind<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = match optional_u64(request, "port") {
        Some(value) => u16::try_from(value).map_err(|_| {
            SidecarCoreError::new("guest kernel call field `port` must be a valid port")
        })?,
        None => 0,
    };
    kernel
        .socket_bind_inet(driver, pid, socket_id, InetSocketAddress::new(host, port))
        .map_err(kernel_error)?;
    let record = kernel.socket_get(socket_id);
    let local = record.as_ref().and_then(|record| record.local_address());
    Ok(json!({
        "host": local.map(InetSocketAddress::host),
        "port": local.map(InetSocketAddress::port),
    }))
}

fn dgram_send<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    // Auto-bind an unbound datagram socket to an ephemeral port, as Node does on
    // the first send.
    let needs_bind = kernel
        .socket_get(socket_id)
        .map(|record| record.local_address().is_none())
        .unwrap_or(false);
    if needs_bind {
        // Auto-bind to loopback (not 0.0.0.0) so the receiver sees a 127.0.0.1
        // source for loopback delivery, matching POSIX/Node semantics.
        kernel
            .socket_bind_inet(
                driver,
                pid,
                socket_id,
                InetSocketAddress::new("127.0.0.1", 0),
            )
            .map_err(kernel_error)?;
    }
    let host = optional_str(request, "host").unwrap_or(DEFAULT_LOOPBACK_HOST);
    let port = require_port(request, "port")?;
    let data = decode_data(request)?;
    let written = kernel
        .socket_send_to_inet_loopback(
            driver,
            pid,
            socket_id,
            InetSocketAddress::new(host, port),
            &data,
        )
        .map_err(kernel_error)?;
    Ok(json!({ "bytes": written }))
}

fn dgram_recv<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let max_bytes = optional_u64(request, "maxBytes")
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_READ_MAX_BYTES);
    match kernel.socket_recv_datagram(driver, pid, socket_id, max_bytes) {
        Ok(Some(datagram)) => {
            let source = datagram.source_address();
            Ok(json!({
                "data": encode_data(datagram.payload()),
                "remoteAddress": source.map(InetSocketAddress::host),
                "remotePort": source.map(InetSocketAddress::port),
            }))
        }
        Ok(None) => Ok(json!({ "data": Value::Null })),
        Err(error) if would_block(&error) => Ok(json!({ "data": Value::Null })),
        Err(error) => Err(kernel_error(error)),
    }
}

fn dgram_address<F>(kernel: &KernelVm<F>, request: &Value) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let record = kernel.socket_get(socket_id);
    let local = record.as_ref().and_then(|record| record.local_address());
    Ok(json!({
        "host": local.map(InetSocketAddress::host),
        "port": local.map(InetSocketAddress::port),
    }))
}

fn dns_lookup<F>(kernel: &KernelVm<F>, request: &Value) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let hostname = optional_str(request, "hostname").ok_or_else(|| {
        SidecarCoreError::new("guest dns.lookup requires string field `hostname`")
    })?;
    let resolution = kernel
        .resolve_dns(hostname, DnsLookupPolicy::CheckPermissions)
        .map_err(kernel_error)?;
    let addresses = resolution
        .addresses()
        .iter()
        .map(|address| {
            json!({
                "address": address.to_string(),
                "family": if address.is_ipv6() { 6 } else { 4 },
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "hostname": resolution.hostname(),
        "addresses": addresses,
    }))
}

fn net_shutdown<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    let how = match optional_str(request, "how").unwrap_or("both") {
        "read" => SocketShutdown::Read,
        "write" => SocketShutdown::Write,
        "both" => SocketShutdown::Both,
        other => {
            return Err(SidecarCoreError::new(format!(
                "guest net.shutdown received unsupported `how` value: {other}"
            )))
        }
    };
    kernel
        .socket_shutdown(driver, pid, socket_id, how)
        .map_err(kernel_error)?;
    Ok(json!({}))
}

fn net_close<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let socket_id = require_socket_id(request)?;
    kernel
        .socket_close(driver, pid, socket_id)
        .map_err(kernel_error)?;
    Ok(json!({}))
}

fn decode_request(payload: &[u8]) -> Result<Value, SidecarCoreError> {
    if payload.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(payload).map_err(|error| {
        SidecarCoreError::new(format!("invalid guest kernel call payload: {error}"))
    })
}

fn optional_str<'a>(request: &'a Value, key: &str) -> Option<&'a str> {
    request.get(key).and_then(Value::as_str)
}

fn optional_u64(request: &Value, key: &str) -> Option<u64> {
    request.get(key).and_then(Value::as_u64)
}

fn require_port(request: &Value, key: &str) -> Result<u16, SidecarCoreError> {
    let value = optional_u64(request, key).ok_or_else(|| {
        SidecarCoreError::new(format!("guest kernel call requires numeric field `{key}`"))
    })?;
    u16::try_from(value).map_err(|_| {
        SidecarCoreError::new(format!(
            "guest kernel call field `{key}` must be a valid port"
        ))
    })
}

fn require_socket_id(request: &Value) -> Result<SocketId, SidecarCoreError> {
    optional_u64(request, "socketId")
        .ok_or_else(|| SidecarCoreError::new("guest kernel call requires numeric field `socketId`"))
}

fn decode_data(request: &Value) -> Result<Vec<u8>, SidecarCoreError> {
    let data = optional_str(request, "data")
        .ok_or_else(|| SidecarCoreError::new("guest kernel call requires string field `data`"))?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| {
            SidecarCoreError::new(format!("invalid base64 guest socket data: {error}"))
        })
}

fn encode_data(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn would_block(error: &KernelError) -> bool {
    matches!(error.code(), "EAGAIN" | "EWOULDBLOCK")
}

fn kernel_error(error: KernelError) -> SidecarCoreError {
    SidecarCoreError::typed(error.code(), error.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_kernel() -> KernelVm<MemoryFileSystem> {
        let mut config = KernelVmConfig::new("guest-net-test");
        config.permissions = Permissions::allow_all();
        KernelVm::new(MemoryFileSystem::new(), config)
    }

    fn guest_pid(kernel: &mut KernelVm<MemoryFileSystem>) -> u32 {
        kernel
            .create_virtual_process(
                "shell",
                "shell",
                "sh",
                Vec::new(),
                VirtualProcessOptions::default(),
            )
            .expect("spawn guest process")
            .pid()
    }

    fn call(
        kernel: &mut KernelVm<MemoryFileSystem>,
        pid: u32,
        operation: &str,
        request: Value,
    ) -> Value {
        let payload = serde_json::to_vec(&request).expect("encode request");
        let bytes =
            handle_guest_kernel_call(kernel, pid, "shell", operation, &payload).expect("dispatch");
        serde_json::from_slice(&bytes).expect("decode response")
    }

    fn call_error(
        kernel: &mut KernelVm<MemoryFileSystem>,
        pid: u32,
        operation: &str,
        request: Value,
    ) -> SidecarCoreError {
        let payload = serde_json::to_vec(&request).expect("encode request");
        handle_guest_kernel_call(kernel, pid, "shell", operation, &payload)
            .expect_err("guest networking call must fail")
    }

    #[test]
    fn failed_socket_setup_rolls_back_allocated_kernel_sockets() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let baseline = kernel.resource_snapshot().sockets;

        let _connect_error = call_error(
            &mut kernel,
            pid,
            "net.connect",
            json!({ "host": "127.0.0.1", "port": 44991 }),
        );
        assert_eq!(kernel.resource_snapshot().sockets, baseline);

        call(
            &mut kernel,
            pid,
            "net.listen",
            json!({ "host": "127.0.0.1", "port": 44992 }),
        );
        let after_listener = kernel.resource_snapshot().sockets;
        let _listen_error = call_error(
            &mut kernel,
            pid,
            "net.listen",
            json!({ "host": "127.0.0.1", "port": 44992 }),
        );
        assert_eq!(kernel.resource_snapshot().sockets, after_listener);

        call(
            &mut kernel,
            pid,
            "net.udp_bind",
            json!({ "host": "127.0.0.1", "port": 44993 }),
        );
        let after_udp = kernel.resource_snapshot().sockets;
        let _udp_error = call_error(
            &mut kernel,
            pid,
            "net.udp_bind",
            json!({ "host": "127.0.0.1", "port": 44993 }),
        );
        assert_eq!(kernel.resource_snapshot().sockets, after_udp);
    }

    #[test]
    fn loopback_tcp_round_trip_through_kernel() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);

        let listener = call(
            &mut kernel,
            pid,
            "net.listen",
            json!({ "host": "127.0.0.1", "port": 44551 }),
        );
        let listener_id = listener["socketId"].as_u64().expect("listener socket id");

        let client = call(
            &mut kernel,
            pid,
            "net.connect",
            json!({ "host": "127.0.0.1", "port": 44551 }),
        );
        let client_id = client["socketId"].as_u64().expect("client socket id");

        let accepted = call(
            &mut kernel,
            pid,
            "net.accept",
            json!({ "socketId": listener_id }),
        );
        let accepted_id = accepted["socketId"].as_u64().expect("accepted socket id");

        let payload = b"hello kernel loopback";
        let write = call(
            &mut kernel,
            pid,
            "net.write",
            json!({ "socketId": client_id, "data": encode_data(payload) }),
        );
        assert_eq!(write["written"].as_u64(), Some(payload.len() as u64));

        let read = call(
            &mut kernel,
            pid,
            "net.read",
            json!({ "socketId": accepted_id }),
        );
        assert_eq!(read["closed"].as_bool(), Some(false));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(read["data"].as_str().expect("read data"))
            .expect("decode read data");
        assert_eq!(decoded, payload);

        call(
            &mut kernel,
            pid,
            "net.close",
            json!({ "socketId": client_id }),
        );
        call(
            &mut kernel,
            pid,
            "net.close",
            json!({ "socketId": accepted_id }),
        );
        call(
            &mut kernel,
            pid,
            "net.close",
            json!({ "socketId": listener_id }),
        );
    }

    #[test]
    fn accept_without_pending_connection_reports_null_socket() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let listener = call(
            &mut kernel,
            pid,
            "net.listen",
            json!({ "host": "127.0.0.1", "port": 44552 }),
        );
        let listener_id = listener["socketId"].as_u64().expect("listener socket id");

        let accepted = call(
            &mut kernel,
            pid,
            "net.accept",
            json!({ "socketId": listener_id }),
        );
        assert!(accepted["socketId"].is_null());
    }

    #[test]
    fn poll_reports_readability_after_loopback_write() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let listener = call(
            &mut kernel,
            pid,
            "net.listen",
            json!({ "host": "127.0.0.1", "port": 44561 }),
        );
        let listener_id = listener["socketId"].as_u64().expect("listener socket id");
        let client = call(
            &mut kernel,
            pid,
            "net.connect",
            json!({ "host": "127.0.0.1", "port": 44561 }),
        );
        let client_id = client["socketId"].as_u64().expect("client socket id");
        let accepted = call(
            &mut kernel,
            pid,
            "net.accept",
            json!({ "socketId": listener_id }),
        );
        let accepted_id = accepted["socketId"].as_u64().expect("accepted socket id");

        let before = call(
            &mut kernel,
            pid,
            "net.poll",
            json!({ "socketId": accepted_id }),
        );
        assert_eq!(before["readable"].as_bool(), Some(false));

        call(
            &mut kernel,
            pid,
            "net.write",
            json!({ "socketId": client_id, "data": encode_data(b"ping") }),
        );

        let after = call(
            &mut kernel,
            pid,
            "net.poll",
            json!({ "socketId": accepted_id }),
        );
        assert_eq!(after["readable"].as_bool(), Some(true));
    }

    #[test]
    fn udp_loopback_datagram_round_trip() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let receiver = call(
            &mut kernel,
            pid,
            "net.udp_bind",
            json!({ "host": "127.0.0.1", "port": 45601 }),
        );
        let receiver_id = receiver["socketId"].as_u64().expect("receiver socket id");
        let sender = call(
            &mut kernel,
            pid,
            "net.udp_bind",
            json!({ "host": "127.0.0.1", "port": 45602 }),
        );
        let sender_id = sender["socketId"].as_u64().expect("sender socket id");

        let payload = b"datagram payload";
        let sent = call(
            &mut kernel,
            pid,
            "net.send_to",
            json!({ "socketId": sender_id, "host": "127.0.0.1", "port": 45601, "data": encode_data(payload) }),
        );
        assert_eq!(sent["written"].as_u64(), Some(payload.len() as u64));

        let received = call(
            &mut kernel,
            pid,
            "net.recv_from",
            json!({ "socketId": receiver_id }),
        );
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(received["data"].as_str().expect("datagram data"))
            .expect("decode datagram");
        assert_eq!(decoded, payload);
        assert_eq!(received["remotePort"].as_u64(), Some(45602));
    }

    #[test]
    fn dns_lookup_resolves_literal_addresses() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let resolution = call(
            &mut kernel,
            pid,
            "dns.lookup",
            json!({ "hostname": "127.0.0.1" }),
        );
        assert_eq!(resolution["hostname"].as_str(), Some("127.0.0.1"));
        let addresses = resolution["addresses"].as_array().expect("addresses array");
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0]["address"].as_str(), Some("127.0.0.1"));
        assert_eq!(addresses[0]["family"].as_u64(), Some(4));
    }

    #[test]
    fn dgram_loopback_round_trip_with_auto_bind_sender() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);

        let receiver = call(&mut kernel, pid, "dgram.create", json!({}));
        let receiver_id = receiver["socketId"].as_u64().expect("receiver id");
        call(
            &mut kernel,
            pid,
            "dgram.bind",
            json!({ "socketId": receiver_id, "host": "127.0.0.1", "port": 46711 }),
        );

        // Sender is never bound explicitly -> dgram.send auto-binds it.
        let sender = call(&mut kernel, pid, "dgram.create", json!({}));
        let sender_id = sender["socketId"].as_u64().expect("sender id");
        let payload = b"dgram-auto-bind";
        let sent = call(
            &mut kernel,
            pid,
            "dgram.send",
            json!({ "socketId": sender_id, "host": "127.0.0.1", "port": 46711, "data": encode_data(payload) }),
        );
        assert_eq!(sent["bytes"].as_u64(), Some(payload.len() as u64));

        let recv = call(
            &mut kernel,
            pid,
            "dgram.recv",
            json!({ "socketId": receiver_id }),
        );
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(recv["data"].as_str().expect("recv data"))
            .expect("decode");
        assert_eq!(decoded, payload);
        // The auto-bound sender got an ephemeral port in the dynamic range.
        assert!(recv["remotePort"].as_u64().expect("remote port") >= 49152);
    }

    #[test]
    fn unsupported_operation_is_reported() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let error =
            handle_guest_kernel_call(&mut kernel, pid, "shell", "net.teleport", b"{}").unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported guest kernel call operation: net.teleport"));
    }
}
