//! Custom WASM import bindings for wasmVM host syscalls.
//!
//! Declares extern functions for `host_process`, `host_user`, and `host_net`
//! modules that the JS host runtime provides. These extend standard WASI with
//! process management, user/group identity, and TCP socket capabilities.
//!
//! Signatures match spec section 4.3.

#![no_std]

/// WASI-style errno type. 0 = success.
pub type Errno = u32;

// WASI errno constants
pub const ERRNO_SUCCESS: Errno = 0;
pub const ERRNO_BADF: Errno = 8;
pub const ERRNO_INVAL: Errno = 28;
pub const ERRNO_NOSYS: Errno = 52;
pub const ERRNO_NOENT: Errno = 44;
pub const ERRNO_SRCH: Errno = 71; // No such process
pub const ERRNO_CHILD: Errno = 10; // No child processes
const POLLFD_BYTES: usize = 8;

fn checked_u32_len(len: usize) -> Result<u32, Errno> {
    u32::try_from(len).map_err(|_| ERRNO_INVAL)
}

fn validate_returned_len(len: u32, capacity: usize) -> Result<u32, Errno> {
    match usize::try_from(len) {
        Ok(len) if len <= capacity => Ok(len as u32),
        _ => Err(ERRNO_INVAL),
    }
}

fn validate_poll_buffer_len(buffer_len: usize, nfds: u32) -> Result<(), Errno> {
    let nfds = usize::try_from(nfds).map_err(|_| ERRNO_INVAL)?;
    let expected = nfds.checked_mul(POLLFD_BYTES).ok_or(ERRNO_INVAL)?;
    if buffer_len == expected {
        Ok(())
    } else {
        Err(ERRNO_INVAL)
    }
}

// ============================================================
// host_process module — process management and FD operations
// ============================================================

#[link(wasm_import_module = "host_process")]
extern "C" {
    /// Spawn a child process.
    ///
    /// Arguments are serialized as a byte buffer pointed to by `argv_ptr`/`argv_len`.
    /// Environment is serialized similarly via `envp_ptr`/`envp_len`.
    /// File descriptors `stdin_fd`, `stdout_fd`, `stderr_fd` are inherited.
    /// Current working directory is passed as `cwd_ptr`/`cwd_len`.
    /// On success, the child's virtual PID is written to `ret_pid`.
    /// Returns errno.
    fn proc_spawn(
        argv_ptr: *const u8,
        argv_len: u32,
        envp_ptr: *const u8,
        envp_len: u32,
        stdin_fd: u32,
        stdout_fd: u32,
        stderr_fd: u32,
        cwd_ptr: *const u8,
        cwd_len: u32,
        ret_pid: *mut u32,
    ) -> Errno;

    /// Wait for a child process to exit.
    ///
    /// Blocks (via Atomics.wait on the host side) until the child exits.
    /// `options` is reserved (pass 0). Exit status is written to `ret_status`.
    /// The actual waited-for PID is written to `ret_pid` (important for pid=-1).
    /// Returns errno.
    fn proc_waitpid(pid: u32, options: u32, ret_status: *mut u32, ret_pid: *mut u32) -> Errno;

    /// Send a signal to a process.
    ///
    /// Only SIGTERM (15) and SIGKILL (9) are meaningful.
    /// Returns errno.
    fn proc_kill(pid: u32, signal: u32) -> Errno;

    /// Get the current process's virtual PID.
    ///
    /// Writes PID to `ret_pid`. Returns errno.
    fn proc_getpid(ret_pid: *mut u32) -> Errno;

    /// Get the parent process's virtual PID.
    ///
    /// Writes parent PID to `ret_pid`. Returns errno.
    fn proc_getppid(ret_pid: *mut u32) -> Errno;

    /// Create an anonymous pipe.
    ///
    /// Writes the read-end FD to `ret_read_fd` and write-end FD to `ret_write_fd`.
    /// Returns errno.
    fn fd_pipe(ret_read_fd: *mut u32, ret_write_fd: *mut u32) -> Errno;

    /// Duplicate a file descriptor.
    ///
    /// The new FD number is written to `ret_new_fd`. Returns errno.
    fn fd_dup(fd: u32, ret_new_fd: *mut u32) -> Errno;

    /// Duplicate a file descriptor to a specific number.
    ///
    /// `old_fd` is duplicated to `new_fd`. If `new_fd` is already open, it is closed first.
    /// Returns errno.
    fn fd_dup2(old_fd: u32, new_fd: u32) -> Errno;

    /// Sleep for the specified number of milliseconds.
    ///
    /// Blocks via Atomics.wait on the host side. Returns errno.
    fn sleep_ms(milliseconds: u32) -> Errno;

    /// Allocate a pseudo-terminal (PTY) master/slave pair.
    ///
    /// On success, the master FD is written to `ret_master_fd` and the slave FD
    /// to `ret_slave_fd`. Both ends are installed in the process's kernel FD table.
    /// Returns errno.
    fn pty_open(ret_master_fd: *mut u32, ret_slave_fd: *mut u32) -> Errno;

    /// Register a signal handler disposition (POSIX sigaction).
    ///
    /// `signal` is the signal number (1-64).
    /// `action` encodes the disposition: 0=SIG_DFL, 1=SIG_IGN, 2=user handler.
    /// `mask_lo` / `mask_hi` encode the low/high 32 bits of sa_mask, and `flags`
    /// carries the raw POSIX sa_flags bitmask.
    /// When action=2, the C sysroot still holds the actual function pointer; the
    /// kernel only needs the metadata that affects delivery semantics.
    /// Returns errno.
    fn proc_sigaction(signal: u32, action: u32, mask_lo: u32, mask_hi: u32, flags: u32) -> Errno;
}

// ============================================================
// host_user module — user/group identity and terminal detection
// ============================================================

#[link(wasm_import_module = "host_user")]
extern "C" {
    /// Get the real user ID. Writes to `ret_uid`. Returns errno.
    fn getuid(ret_uid: *mut u32) -> Errno;

    /// Get the real group ID. Writes to `ret_gid`. Returns errno.
    fn getgid(ret_gid: *mut u32) -> Errno;

    /// Get the effective user ID. Writes to `ret_uid`. Returns errno.
    fn geteuid(ret_uid: *mut u32) -> Errno;

    /// Get the effective group ID. Writes to `ret_gid`. Returns errno.
    fn getegid(ret_gid: *mut u32) -> Errno;

    /// Check if a file descriptor refers to a terminal.
    ///
    /// Writes 1 (true) or 0 (false) to `ret_bool`. Returns errno.
    fn isatty(fd: u32, ret_bool: *mut u32) -> Errno;

    /// Get passwd entry for a user ID.
    ///
    /// Serialized passwd string (username:x:uid:gid:gecos:home:shell) is written
    /// to `buf_ptr` with max length `buf_len`. Actual length written to `ret_len`.
    /// Returns errno.
    fn getpwuid(uid: u32, buf_ptr: *mut u8, buf_len: u32, ret_len: *mut u32) -> Errno;
}

// ============================================================
// Safe Rust wrappers — host_process
// ============================================================

/// Spawn a child process with the given arguments, environment, stdio FDs, and working directory.
///
/// Returns `Ok(pid)` on success, `Err(errno)` on failure.
pub fn spawn(
    argv: &[u8],
    envp: &[u8],
    stdin_fd: u32,
    stdout_fd: u32,
    stderr_fd: u32,
    cwd: &[u8],
) -> Result<u32, Errno> {
    let mut pid: u32 = 0;
    let argv_len = checked_u32_len(argv.len())?;
    let envp_len = checked_u32_len(envp.len())?;
    let cwd_len = checked_u32_len(cwd.len())?;
    let errno = unsafe {
        proc_spawn(
            argv.as_ptr(),
            argv_len,
            envp.as_ptr(),
            envp_len,
            stdin_fd,
            stdout_fd,
            stderr_fd,
            cwd.as_ptr(),
            cwd_len,
            &mut pid,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok(pid)
    } else {
        Err(errno)
    }
}

/// Wait for a child process to exit.
///
/// Returns `Ok((exit_status, actual_pid))` on success, `Err(errno)` on failure.
/// The actual_pid is the PID of the child that exited (relevant for pid=0xFFFFFFFF / -1).
pub fn waitpid(pid: u32, options: u32) -> Result<(u32, u32), Errno> {
    let mut status: u32 = 0;
    let mut actual_pid: u32 = 0;
    let errno = unsafe { proc_waitpid(pid, options, &mut status, &mut actual_pid) };
    if errno == ERRNO_SUCCESS {
        Ok((status, actual_pid))
    } else {
        Err(errno)
    }
}

/// Send a signal to a process.
///
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn kill(pid: u32, signal: u32) -> Result<(), Errno> {
    let errno = unsafe { proc_kill(pid, signal) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Get the current process's virtual PID.
///
/// Returns `Ok(pid)` on success, `Err(errno)` on failure.
pub fn getpid() -> Result<u32, Errno> {
    let mut pid: u32 = 0;
    let errno = unsafe { proc_getpid(&mut pid) };
    if errno == ERRNO_SUCCESS {
        Ok(pid)
    } else {
        Err(errno)
    }
}

/// Get the parent process's virtual PID.
///
/// Returns `Ok(pid)` on success, `Err(errno)` on failure.
pub fn getppid() -> Result<u32, Errno> {
    let mut pid: u32 = 0;
    let errno = unsafe { proc_getppid(&mut pid) };
    if errno == ERRNO_SUCCESS {
        Ok(pid)
    } else {
        Err(errno)
    }
}

/// Create an anonymous pipe.
///
/// Returns `Ok((read_fd, write_fd))` on success, `Err(errno)` on failure.
pub fn pipe() -> Result<(u32, u32), Errno> {
    let mut read_fd: u32 = 0;
    let mut write_fd: u32 = 0;
    let errno = unsafe { fd_pipe(&mut read_fd, &mut write_fd) };
    if errno == ERRNO_SUCCESS {
        Ok((read_fd, write_fd))
    } else {
        Err(errno)
    }
}

/// Duplicate a file descriptor.
///
/// Returns `Ok(new_fd)` on success, `Err(errno)` on failure.
pub fn dup(fd: u32) -> Result<u32, Errno> {
    let mut new_fd: u32 = 0;
    let errno = unsafe { fd_dup(fd, &mut new_fd) };
    if errno == ERRNO_SUCCESS {
        Ok(new_fd)
    } else {
        Err(errno)
    }
}

/// Duplicate a file descriptor to a specific number.
///
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn dup2(old_fd: u32, new_fd: u32) -> Result<(), Errno> {
    let errno = unsafe { fd_dup2(old_fd, new_fd) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Sleep for the specified number of milliseconds.
///
/// Blocks via Atomics.wait on the host side instead of busy-waiting.
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn host_sleep_ms(milliseconds: u32) -> Result<(), Errno> {
    let errno = unsafe { sleep_ms(milliseconds) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Allocate a pseudo-terminal (PTY) master/slave pair.
///
/// Returns `Ok((master_fd, slave_fd))` on success, `Err(errno)` on failure.
/// The master FD is used to read output and write input.
/// The slave FD is passed to a child process as its stdin/stdout/stderr.
pub fn openpty() -> Result<(u32, u32), Errno> {
    let mut master_fd: u32 = 0;
    let mut slave_fd: u32 = 0;
    let errno = unsafe { pty_open(&mut master_fd, &mut slave_fd) };
    if errno == ERRNO_SUCCESS {
        Ok((master_fd, slave_fd))
    } else {
        Err(errno)
    }
}

/// Register a signal handler disposition (POSIX sigaction).
///
/// `signal` is the signal number (1-64).
/// `action` encodes the disposition: 0=SIG_DFL, 1=SIG_IGN, 2=user handler (C-side holds pointer).
/// `mask_lo` / `mask_hi` encode the low/high 32 bits of sa_mask, and `flags`
/// carries the raw POSIX sa_flags bitmask.
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn sigaction_set(signal: u32, action: u32, mask_lo: u32, mask_hi: u32, flags: u32) -> Result<(), Errno> {
    let errno = unsafe { proc_sigaction(signal, action, mask_lo, mask_hi, flags) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

// ============================================================
// host_net module — TCP socket operations
// ============================================================

#[link(wasm_import_module = "host_net")]
extern "C" {
    /// Create a socket.
    ///
    /// `domain` is the address family (e.g. AF_INET=2).
    /// `sock_type` is the socket type (e.g. SOCK_STREAM=1).
    /// `protocol` is the protocol (0 for default).
    /// On success, the socket FD is written to `ret_fd`.
    /// Returns errno.
    fn net_socket(domain: u32, sock_type: u32, protocol: u32, ret_fd: *mut u32) -> Errno;

    /// Connect a socket to a remote address.
    ///
    /// `addr_ptr`/`addr_len` point to a serialized address string (host:port).
    /// Returns errno.
    fn net_connect(fd: u32, addr_ptr: *const u8, addr_len: u32) -> Errno;

    /// Send data on a connected socket.
    ///
    /// `buf_ptr`/`buf_len` point to the data to send.
    /// `flags` are send flags (0 for default).
    /// Number of bytes sent is written to `ret_sent`.
    /// Returns errno.
    fn net_send(fd: u32, buf_ptr: *const u8, buf_len: u32, flags: u32, ret_sent: *mut u32) -> Errno;

    /// Receive data from a connected socket.
    ///
    /// `buf_ptr`/`buf_len` point to the receive buffer.
    /// `flags` are recv flags (0 for default).
    /// Number of bytes received is written to `ret_received`.
    /// Returns errno.
    fn net_recv(fd: u32, buf_ptr: *mut u8, buf_len: u32, flags: u32, ret_received: *mut u32) -> Errno;

    /// Close a socket.
    ///
    /// Returns errno.
    fn net_close(fd: u32) -> Errno;

    /// Resolve a hostname to an address.
    ///
    /// `host_ptr`/`host_len` point to the hostname string.
    /// `port_ptr`/`port_len` point to the port/service string.
    /// Resolved address is written to `ret_addr` buffer with max length from `ret_addr_len`.
    /// Actual length is written back to `ret_addr_len`.
    /// Returns errno.
    fn net_getaddrinfo(
        host_ptr: *const u8,
        host_len: u32,
        port_ptr: *const u8,
        port_len: u32,
        ret_addr: *mut u8,
        ret_addr_len: *mut u32,
    ) -> Errno;

    /// Upgrade a connected TCP socket to TLS.
    ///
    /// `hostname_ptr`/`hostname_len` point to the SNI hostname string.
    /// After success, net_send/net_recv on this fd use the encrypted TLS stream.
    /// Returns errno.
    fn net_tls_connect(fd: u32, hostname_ptr: *const u8, hostname_len: u32) -> Errno;

    /// Set a socket option.
    ///
    /// `level` is the protocol level (e.g. SOL_SOCKET=1).
    /// `optname` is the option name.
    /// `optval_ptr`/`optval_len` point to the option value.
    /// Returns errno.
    fn net_setsockopt(fd: u32, level: u32, optname: u32, optval_ptr: *const u8, optval_len: u32) -> Errno;

    /// Get the local address of a socket.
    ///
    /// The serialized address string is written to `ret_addr` with maximum
    /// length from `ret_addr_len`. The actual length is written back.
    /// Returns errno.
    fn net_getsockname(fd: u32, ret_addr: *mut u8, ret_addr_len: *mut u32) -> Errno;

    /// Get the peer address of a connected socket.
    ///
    /// The serialized address string is written to `ret_addr` with maximum
    /// length from `ret_addr_len`. The actual length is written back.
    /// Returns errno.
    fn net_getpeername(fd: u32, ret_addr: *mut u8, ret_addr_len: *mut u32) -> Errno;

    /// Poll socket FDs for readiness.
    ///
    /// `fds_ptr` points to a packed array of poll entries (8 bytes each):
    ///   [fd: i32, events: i16, revents: i16] per entry.
    /// `nfds` is the number of entries.
    /// `timeout_ms` is the timeout: 0=non-blocking, -1=block forever, >0=milliseconds.
    /// On return, revents fields are updated in-place and `ret_ready` receives
    /// the number of FDs with non-zero revents.
    /// Returns errno.
    fn net_poll(fds_ptr: *mut u8, nfds: u32, timeout_ms: i32, ret_ready: *mut u32) -> Errno;

    /// Bind a socket to a local address.
    ///
    /// `addr_ptr`/`addr_len` point to a serialized address string (host:port or unix path).
    /// Returns errno.
    fn net_bind(fd: u32, addr_ptr: *const u8, addr_len: u32) -> Errno;

    /// Mark a bound socket as listening for incoming connections.
    ///
    /// `backlog` is the maximum pending connection queue length.
    /// Returns errno.
    fn net_listen(fd: u32, backlog: u32) -> Errno;

    /// Accept an incoming connection on a listening socket.
    ///
    /// On success, the new connected socket FD is written to `ret_fd`,
    /// and the remote address string is written to `ret_addr` with its
    /// length in `ret_addr_len`.
    /// Returns errno.
    fn net_accept(fd: u32, ret_fd: *mut u32, ret_addr: *mut u8, ret_addr_len: *mut u32) -> Errno;

    /// Send a datagram to a specific destination address (UDP).
    ///
    /// `buf_ptr`/`buf_len` point to the data to send.
    /// `flags` are send flags (0 for default).
    /// `addr_ptr`/`addr_len` point to the destination address string (host:port).
    /// Number of bytes sent is written to `ret_sent`.
    /// Returns errno.
    fn net_sendto(
        fd: u32,
        buf_ptr: *const u8,
        buf_len: u32,
        flags: u32,
        addr_ptr: *const u8,
        addr_len: u32,
        ret_sent: *mut u32,
    ) -> Errno;

    /// Receive a datagram from a UDP socket with source address.
    ///
    /// `buf_ptr`/`buf_len` point to the receive buffer.
    /// `flags` are recv flags (0 for default).
    /// Number of bytes received is written to `ret_received`.
    /// Source address string is written to `ret_addr` with length in `ret_addr_len`.
    /// Returns errno.
    fn net_recvfrom(
        fd: u32,
        buf_ptr: *mut u8,
        buf_len: u32,
        flags: u32,
        ret_received: *mut u32,
        ret_addr: *mut u8,
        ret_addr_len: *mut u32,
    ) -> Errno;
}

// ============================================================
// Safe Rust wrappers — host_net
// ============================================================

/// Create a socket.
///
/// Returns `Ok(fd)` on success, `Err(errno)` on failure.
pub fn socket(domain: u32, sock_type: u32, protocol: u32) -> Result<u32, Errno> {
    let mut fd: u32 = 0;
    let errno = unsafe { net_socket(domain, sock_type, protocol, &mut fd) };
    if errno == ERRNO_SUCCESS {
        Ok(fd)
    } else {
        Err(errno)
    }
}

/// Connect a socket to a remote address.
///
/// `addr` is a serialized address string (e.g. "host:port").
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn connect(fd: u32, addr: &[u8]) -> Result<(), Errno> {
    let addr_len = checked_u32_len(addr.len())?;
    let errno = unsafe { net_connect(fd, addr.as_ptr(), addr_len) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Send data on a connected socket.
///
/// Returns `Ok(bytes_sent)` on success, `Err(errno)` on failure.
pub fn send(fd: u32, buf: &[u8], flags: u32) -> Result<u32, Errno> {
    let buf_len = checked_u32_len(buf.len())?;
    let mut sent: u32 = 0;
    let errno = unsafe { net_send(fd, buf.as_ptr(), buf_len, flags, &mut sent) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(sent, buf.len())
    } else {
        Err(errno)
    }
}

/// Receive data from a connected socket.
///
/// Returns `Ok(bytes_received)` on success, `Err(errno)` on failure.
pub fn recv(fd: u32, buf: &mut [u8], flags: u32) -> Result<u32, Errno> {
    let buf_len = checked_u32_len(buf.len())?;
    let mut received: u32 = 0;
    let errno = unsafe { net_recv(fd, buf.as_mut_ptr(), buf_len, flags, &mut received) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(received, buf.len())
    } else {
        Err(errno)
    }
}

/// Close a socket.
///
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn net_close_socket(fd: u32) -> Result<(), Errno> {
    let errno = unsafe { net_close(fd) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Resolve a hostname to an address.
///
/// Writes the resolved address into `buf` and returns the number of bytes written.
/// Returns `Ok(len)` on success, `Err(errno)` on failure.
pub fn getaddrinfo(host: &[u8], port: &[u8], buf: &mut [u8]) -> Result<u32, Errno> {
    let host_len = checked_u32_len(host.len())?;
    let port_len = checked_u32_len(port.len())?;
    let mut len = checked_u32_len(buf.len())?;
    let errno = unsafe {
        net_getaddrinfo(
            host.as_ptr(),
            host_len,
            port.as_ptr(),
            port_len,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

/// Set a socket option.
///
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn setsockopt(fd: u32, level: u32, optname: u32, optval: &[u8]) -> Result<(), Errno> {
    let optval_len = checked_u32_len(optval.len())?;
    let errno = unsafe { net_setsockopt(fd, level, optname, optval.as_ptr(), optval_len) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Get the local address of a socket.
///
/// Writes the serialized address into `buf` and returns the number of bytes written.
/// Returns `Ok(len)` on success, `Err(errno)` on failure.
pub fn getsockname(fd: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = checked_u32_len(buf.len())?;
    let errno = unsafe { net_getsockname(fd, buf.as_mut_ptr(), &mut len) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

/// Get the peer address of a connected socket.
///
/// Writes the serialized address into `buf` and returns the number of bytes written.
/// Returns `Ok(len)` on success, `Err(errno)` on failure.
pub fn getpeername(fd: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = checked_u32_len(buf.len())?;
    let errno = unsafe { net_getpeername(fd, buf.as_mut_ptr(), &mut len) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

/// Upgrade a connected TCP socket to TLS.
///
/// `hostname` is used for SNI (Server Name Indication).
/// After success, `send`/`recv` on this fd use the encrypted TLS stream.
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn tls_connect(fd: u32, hostname: &[u8]) -> Result<(), Errno> {
    let hostname_len = checked_u32_len(hostname.len())?;
    let errno = unsafe { net_tls_connect(fd, hostname.as_ptr(), hostname_len) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Poll socket FDs for readiness.
///
/// `fds` is a mutable slice of pollfd-like entries (8 bytes each: fd i32, events i16, revents i16).
/// `timeout_ms` is the timeout: 0=non-blocking, -1=block forever, >0=milliseconds.
/// Returns `Ok(ready_count)` on success, `Err(errno)` on failure.
pub fn poll(fds: &mut [u8], nfds: u32, timeout_ms: i32) -> Result<u32, Errno> {
    validate_poll_buffer_len(fds.len(), nfds)?;
    let mut ready: u32 = 0;
    let errno = unsafe { net_poll(fds.as_mut_ptr(), nfds, timeout_ms, &mut ready) };
    if errno == ERRNO_SUCCESS {
        Ok(ready)
    } else {
        Err(errno)
    }
}

/// Bind a socket to a local address.
///
/// `addr` is a serialized address string (e.g. "host:port" or "/path/to/socket").
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn bind(fd: u32, addr: &[u8]) -> Result<(), Errno> {
    let addr_len = checked_u32_len(addr.len())?;
    let errno = unsafe { net_bind(fd, addr.as_ptr(), addr_len) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Mark a bound socket as listening for incoming connections.
///
/// `backlog` is the maximum pending connection queue length.
/// Returns `Ok(())` on success, `Err(errno)` on failure.
pub fn listen(fd: u32, backlog: u32) -> Result<(), Errno> {
    let errno = unsafe { net_listen(fd, backlog) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Accept an incoming connection on a listening socket.
///
/// Returns `Ok((fd, addr_len))` on success, where the remote address string
/// has been written into `addr_buf` with length `addr_len`.
/// Returns `Err(errno)` on failure.
pub fn accept(fd: u32, addr_buf: &mut [u8]) -> Result<(u32, u32), Errno> {
    let mut new_fd: u32 = 0;
    let mut addr_len = checked_u32_len(addr_buf.len())?;
    let errno = unsafe { net_accept(fd, &mut new_fd, addr_buf.as_mut_ptr(), &mut addr_len) };
    if errno == ERRNO_SUCCESS {
        Ok((new_fd, validate_returned_len(addr_len, addr_buf.len())?))
    } else {
        Err(errno)
    }
}

/// Send a datagram to a specific destination address (UDP).
///
/// `addr` is the destination address string (e.g. "host:port").
/// Returns `Ok(bytes_sent)` on success, `Err(errno)` on failure.
pub fn sendto(fd: u32, buf: &[u8], flags: u32, addr: &[u8]) -> Result<u32, Errno> {
    let buf_len = checked_u32_len(buf.len())?;
    let addr_len = checked_u32_len(addr.len())?;
    let mut sent: u32 = 0;
    let errno = unsafe {
        net_sendto(
            fd,
            buf.as_ptr(),
            buf_len,
            flags,
            addr.as_ptr(),
            addr_len,
            &mut sent,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(sent, buf.len())
    } else {
        Err(errno)
    }
}

/// Receive a datagram from a UDP socket with source address.
///
/// Writes received data into `buf` and the source address string into `addr_buf`.
/// Returns `Ok((bytes_received, addr_len))` on success, `Err(errno)` on failure.
pub fn recvfrom(fd: u32, buf: &mut [u8], flags: u32, addr_buf: &mut [u8]) -> Result<(u32, u32), Errno> {
    let buf_len = checked_u32_len(buf.len())?;
    let mut received: u32 = 0;
    let mut addr_len = checked_u32_len(addr_buf.len())?;
    let errno = unsafe {
        net_recvfrom(
            fd,
            buf.as_mut_ptr(),
            buf_len,
            flags,
            &mut received,
            addr_buf.as_mut_ptr(),
            &mut addr_len,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok((
            validate_returned_len(received, buf.len())?,
            validate_returned_len(addr_len, addr_buf.len())?,
        ))
    } else {
        Err(errno)
    }
}

// ============================================================
// Safe Rust wrappers — host_user
// ============================================================

/// Get the real user ID.
///
/// Returns `Ok(uid)` on success, `Err(errno)` on failure.
pub fn get_uid() -> Result<u32, Errno> {
    let mut uid: u32 = 0;
    let errno = unsafe { getuid(&mut uid) };
    if errno == ERRNO_SUCCESS {
        Ok(uid)
    } else {
        Err(errno)
    }
}

/// Get the real group ID.
///
/// Returns `Ok(gid)` on success, `Err(errno)` on failure.
pub fn get_gid() -> Result<u32, Errno> {
    let mut gid: u32 = 0;
    let errno = unsafe { getgid(&mut gid) };
    if errno == ERRNO_SUCCESS {
        Ok(gid)
    } else {
        Err(errno)
    }
}

/// Get the effective user ID.
///
/// Returns `Ok(uid)` on success, `Err(errno)` on failure.
pub fn get_euid() -> Result<u32, Errno> {
    let mut uid: u32 = 0;
    let errno = unsafe { geteuid(&mut uid) };
    if errno == ERRNO_SUCCESS {
        Ok(uid)
    } else {
        Err(errno)
    }
}

/// Get the effective group ID.
///
/// Returns `Ok(gid)` on success, `Err(errno)` on failure.
pub fn get_egid() -> Result<u32, Errno> {
    let mut gid: u32 = 0;
    let errno = unsafe { getegid(&mut gid) };
    if errno == ERRNO_SUCCESS {
        Ok(gid)
    } else {
        Err(errno)
    }
}

/// Check if a file descriptor is a terminal.
///
/// Returns `Ok(true)` if it's a terminal, `Ok(false)` otherwise, `Err(errno)` on failure.
pub fn is_atty(fd: u32) -> Result<bool, Errno> {
    let mut result: u32 = 0;
    let errno = unsafe { isatty(fd, &mut result) };
    if errno == ERRNO_SUCCESS {
        Ok(result != 0)
    } else {
        Err(errno)
    }
}

/// Get the passwd entry for a user ID.
///
/// Writes the serialized passwd entry into `buf` and returns the number of bytes written.
/// Returns `Ok(len)` on success, `Err(errno)` on failure.
pub fn get_pwuid(uid: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len: u32 = 0;
    let buf_len = checked_u32_len(buf.len())?;
    let errno = unsafe { getpwuid(uid, buf.as_mut_ptr(), buf_len, &mut len) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_buffer_validation_requires_exact_pollfd_capacity() {
        assert_eq!(validate_poll_buffer_len(POLLFD_BYTES, 1), Ok(()));
        assert_eq!(validate_poll_buffer_len(POLLFD_BYTES - 1, 1), Err(ERRNO_INVAL));
        assert_eq!(validate_poll_buffer_len(POLLFD_BYTES + 1, 1), Err(ERRNO_INVAL));
    }

    #[test]
    fn returned_lengths_must_fit_in_the_supplied_buffer() {
        assert_eq!(validate_returned_len(4, 4), Ok(4));
        assert_eq!(validate_returned_len(5, 4), Err(ERRNO_INVAL));
    }
}
