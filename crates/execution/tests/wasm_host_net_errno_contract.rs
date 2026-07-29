//! Pins the error boundary for WASI `fd_read`/`fd_write` on sidecar-backed
//! network sockets. Guest-memory faults are `EFAULT`; typed sidecar/RPC socket
//! errors must retain their Linux errno through `mapHostProcessError`.

use std::fs;
use std::path::PathBuf;

fn runner_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/runners/wasm-runner.mjs");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_offset = source
        .find(start)
        .unwrap_or_else(|| panic!("missing runner source marker: {start}"));
    let tail = &source[start_offset..];
    let end_offset = tail
        .find(end)
        .unwrap_or_else(|| panic!("missing runner source marker after {start}: {end}"));
    &tail[..end_offset]
}

#[test]
fn host_net_typed_errors_keep_linux_errno_mappings() {
    let source = runner_source();
    let error_map = between(
        &source,
        "function mapHostProcessError(",
        "function seekGuestFileHandle(",
    );
    assert!(
        error_map.contains("case 'EPIPE':\n      return WASI_ERRNO_PIPE;")
            && error_map.contains("case 'ECONNRESET':\n      return WASI_ERRNO_CONNRESET;"),
        "host-net RPC failures must preserve representative Linux socket errnos"
    );
}

#[test]
fn host_net_fd_read_keeps_guest_faults_separate_from_socket_errors() {
    let source = runner_source();
    let guest_marshal = between(
        &source,
        "function writeHostNetBytesToGuestIovs(",
        "function readHostNetSocketToGuestIovs(",
    );
    assert!(
        guest_marshal.contains("writeBytesToGuestIovs(iovs, iovsLen, bytes)")
            && guest_marshal.contains("catch {")
            && guest_marshal.contains("return WASI_ERRNO_FAULT;"),
        "host-net read guest-memory marshalling must return EFAULT"
    );

    let socket_read = between(
        &source,
        "function readHostNetSocketToGuestIovs(",
        "function writeHostNetSocketFromGuestIovs(",
    );
    assert!(
        socket_read.contains("readReadyHostNetSocket(socket")
            && socket_read.contains("catch (error) {")
            && socket_read.contains("return mapHostProcessError(error);"),
        "host-net fd_read must map typed net.socket_read RPC errors"
    );
    assert!(
        !socket_read.contains("writeBytesToGuestIovs("),
        "host-net fd_read must isolate all fallible guest-memory writes in the EFAULT helper"
    );
}

#[test]
fn host_net_fd_write_keeps_guest_faults_separate_from_socket_errors() {
    let source = runner_source();
    let socket_write = between(
        &source,
        "function writeHostNetSocketFromGuestIovs(",
        "function dequeuePipeBytes(",
    );
    let guest_fault = socket_write
        .find("bytes = collectGuestIovBytes(iovs, iovsLen);")
        .expect("host-net fd_write must collect guest iovecs");
    let rpc = socket_write
        .find("callSyncRpc('net.write'")
        .expect("host-net fd_write must call the sidecar net.write RPC");
    let mapped_error = socket_write
        .rfind("return mapHostProcessError(error);")
        .expect("host-net fd_write must map typed net.write RPC errors");

    assert!(
        socket_write[guest_fault..rpc].contains("return WASI_ERRNO_FAULT;"),
        "guest iovec reads must return EFAULT before the RPC boundary"
    );
    assert!(
        mapped_error > rpc,
        "typed net.write RPC errors must be mapped after the RPC boundary"
    );
    assert!(
        !socket_write[rpc..].contains("return WASI_ERRNO_FAULT;"),
        "the net.write RPC catch must not collapse typed errors to EFAULT"
    );
}

#[test]
fn host_net_socket_families_match_the_owned_wasi_libc_abi() {
    let source = runner_source();
    assert!(
        source.contains("const HOST_NET_AF_INET = 1;")
            && source.contains("const HOST_NET_AF_INET6 = 2;")
            && source.contains("const HOST_NET_AF_UNIX = 3;"),
        "host_net domain values must match the AgentOS wasi-libc p1 ABI"
    );

    let socket_import = between(&source, "  net_socket(", "  net_set_nonblock(");
    assert!(
        socket_import.contains("numericDomain === HOST_NET_AF_UNIX")
            && socket_import.contains("localUnixAddress: numericDomain === HOST_NET_AF_UNIX")
            && socket_import.contains("normalizeHostNetSocketType(sockType)"),
        "net_socket must classify AF_UNIX by the owned libc ABI constant"
    );
    assert!(
        source.contains("case POSIX_SOCK_STREAM:")
            && source.contains("return flags | HOST_NET_SOCK_STREAM;")
            && source.contains("case POSIX_SOCK_DGRAM:")
            && source.contains("return flags | HOST_NET_SOCK_DGRAM;"),
        "net_socket must canonicalize POSIX socket types before descriptor transfer"
    );
    assert!(
        !socket_import.contains("numericDomain === 1"),
        "AF_INET=1 must never be mistaken for AF_UNIX"
    );

    let connect_import = between(&source, "  net_connect(", "  net_bind(");
    assert!(
        connect_import.contains("Number(socket.domain) !== HOST_NET_AF_UNIX")
            && connect_import.contains("Number(socket.domain) === HOST_NET_AF_UNIX"),
        "net_connect must use the shared AF_UNIX ABI constant"
    );
}

#[test]
fn host_net_empty_read_invalidates_cached_poll_readiness() {
    let source = runner_source();
    let read_ready = between(
        &source,
        "function readReadyHostNetSocket(",
        "function pollHostNetSocket(",
    );
    let data_branch = read_ready
        .find("if (result.kind === 'data')")
        .expect("host-net read helper must classify data results");
    let invalidate = read_ready[data_branch..]
        .find("socket.readableHint = false;")
        .expect("empty host-net reads must invalidate cached poll readiness");
    let end_branch = read_ready[data_branch..]
        .find("if (result.kind === 'end')")
        .expect("host-net read helper must classify EOF");

    assert!(
        invalidate < end_branch,
        "EAGAIN/timeout and EOF must clear a stale POLLIN hint before the next poll"
    );
}

#[test]
fn blocking_kernel_pipe_writes_pump_wasm_children_on_backpressure() {
    let source = runner_source();
    let start = source
        .rfind("wasiImport.fd_write = (fd, iovs, iovsLen, nwrittenPtr) => {")
        .expect("missing final WASM fd_write override");
    let end = source[start..]
        .find("wasiImport.poll_oneoff =")
        .expect("missing poll_oneoff after final fd_write");
    let fd_write = &source[start..start + end];
    assert!(
        fd_write.contains("error?.code !== 'EAGAIN'")
            && fd_write.contains("process.fd_stat")
            && fd_write.contains("pumpSpawnedChildrenOrWait(SPAWNED_CHILD_WAIT_SLICE_MS)"),
        "blocking kernel-pipe writes must schedule the child that can free pipe capacity"
    );
}
