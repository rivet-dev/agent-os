//! Custom WASM import bindings for wasmVM host syscalls.
//!
//! Declares extern functions for `host_process`, `host_user`, and `host_net`
//! modules that the JS host runtime provides. These extend standard WASI with
//! process management, user/group identity, and TCP socket capabilities.
//!
//! Signatures match spec section 4.3.

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};

/// WASI-style errno type. 0 = success.
pub type Errno = u32;

// WASI errno constants
pub const ERRNO_SUCCESS: Errno = 0;
pub const ERRNO_BADF: Errno = 8;
pub const ERRNO_INVAL: Errno = 28;
pub const ERRNO_IO: Errno = 29;
pub const ERRNO_NOSYS: Errno = 52;
pub const ERRNO_NOTSUP: Errno = 58;
pub const ERRNO_PROTONOSUPPORT: Errno = 66;
pub const ERRNO_NOENT: Errno = 44;
pub const ERRNO_SRCH: Errno = 71; // No such process
pub const ERRNO_CHILD: Errno = 12; // No child processes
/// WASI `errno::again` (EAGAIN/EWOULDBLOCK). Returned by a non-blocking `recv`
/// when no data is currently available.
pub const ERRNO_AGAIN: Errno = 6;
const POLLFD_BYTES: usize = 8;

/// `SOL_SOCKET` socket option level (matches the host_net shim's accepted level).
pub const SOL_SOCKET: u32 = 1;
/// `SO_RCVTIMEO` recv-timeout socket option name (64-bit timeval layout, which the
/// host_net shim parses as two little-endian `i64`s: seconds + microseconds).
pub const SO_RCVTIMEO: u32 = 20;
/// Size of the `timeval` struct the host_net shim expects for `SO_RCVTIMEO`
/// (two 64-bit fields: `tv_sec` + `tv_usec`).
const TIMEVAL_BYTES: usize = 16;

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

fn validate_poll_ready_count(ready: u32, nfds: u32) -> Result<u32, Errno> {
    if ready <= nfds {
        Ok(ready)
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
    /// The executable path is separate from the serialized argument vector so `argv[0]` is
    /// preserved verbatim.
    /// Environment is serialized similarly via `envp_ptr`/`envp_len`.
    /// Ordered child file actions are serialized separately from the caller's
    /// descriptor table. Spawn attributes carry the effective signal mask and
    /// requested default dispositions.
    /// Current working directory is passed as `cwd_ptr`/`cwd_len`.
    /// On success, the child's virtual PID is written to `ret_pid`.
    /// Returns errno.
    fn proc_spawn_v3(
        exec_path_ptr: *const u8,
        exec_path_len: u32,
        argv_ptr: *const u8,
        argv_len: u32,
        envp_ptr: *const u8,
        envp_len: u32,
        actions_ptr: *const u8,
        actions_len: u32,
        cwd_ptr: *const u8,
        cwd_len: u32,
        attr_flags: u32,
        sigdefault_lo: u32,
        sigdefault_hi: u32,
        sigmask_lo: u32,
        sigmask_hi: u32,
        pgroup: u32,
        ret_pid: *mut u32,
    ) -> Errno;

    /// Query or update the authoritative process signal mask.
    ///
    /// `how` uses the POSIX SIG_BLOCK/SIG_UNBLOCK/SIG_SETMASK values; 3 is the
    /// internal query-only operation used for `sigprocmask(..., NULL, ...)`.
    fn proc_signal_mask_v2(
        how: u32,
        set_lo: u32,
        set_hi: u32,
        ret_old_lo: *mut u32,
        ret_old_hi: *mut u32,
    ) -> Errno;

    /// Replace the current process image. Success never returns.
    fn proc_exec(
        exec_path_ptr: *const u8,
        exec_path_len: u32,
        argv_ptr: *const u8,
        argv_len: u32,
        envp_ptr: *const u8,
        envp_len: u32,
        cloexec_fds_ptr: *const u32,
        cloexec_fds_len: u32,
    ) -> Errno;

    /// Replace the current process image from an already-open executable fd.
    /// Success never returns and does not change the descriptor's offset.
    fn proc_fexec(
        exec_fd: u32,
        argv_ptr: *const u8,
        argv_len: u32,
        envp_ptr: *const u8,
        envp_len: u32,
        cloexec_fds_ptr: *const u32,
        cloexec_fds_len: u32,
    ) -> Errno;

    /// Wait for a child process to exit.
    ///
    /// Blocks (via Atomics.wait on the host side) until the child exits.
    /// `options` accepts the WNOHANG bit. Normal exit and terminating signal are returned separately.
    /// The actual waited-for PID is written to `ret_pid` (important for pid=-1).
    /// Returns errno.
    fn proc_waitpid_v2(
        pid: u32,
        options: u32,
        ret_exit_code: *mut u32,
        ret_signal: *mut u32,
        ret_pid: *mut u32,
        ret_core_dumped: *mut u32,
    ) -> Errno;

    /// Send a signal to a process.
    ///
    /// Standard Linux signals 1 through 64 are supported; signal zero performs
    /// existence and permission checks without delivery.
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

    /// Get a process group ID. PID zero selects the current process.
    fn proc_getpgid(pid: u32, ret_pgid: *mut u32) -> Errno;

    /// Move a process into a process group using Linux setpgid semantics.
    fn proc_setpgid(pid: u32, pgid: u32) -> Errno;

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

    /// Close every guest-visible descriptor greater than or equal to `low_fd`.
    ///
    /// This includes high synthetic descriptors that cannot be reached by a
    /// bounded libc-side loop. Returns errno.
    fn proc_closefrom(low_fd: u32) -> Errno;

    /// Create a connected local socket pair in the kernel FD table.
    ///
    /// On success, the two socket FDs are written to `ret_fd0` and `ret_fd1`.
    /// Returns errno.
    #[link_name = "fd_socketpair"]
    fn fd_socketpair_import(
        socket_kind: u32,
        nonblocking: u32,
        close_on_exec: u32,
        ret_fd0: *mut u32,
        ret_fd1: *mut u32,
    ) -> Errno;

    /// Send data and kernel FDs over a local socket.
    ///
    /// `rights_ptr` points to `rights_len` guest FD numbers. The host duplicates
    /// their kernel handles into the receiver when the message is received.
    /// Returns errno.
    fn fd_sendmsg_rights(
        socket_fd: u32,
        data_ptr: *const u8,
        data_len: u32,
        rights_ptr: *const u32,
        rights_len: u32,
        flags: u32,
        ret_sent: *mut u32,
    ) -> Errno;

    /// Receive data and kernel FDs from a local socket.
    ///
    /// `rights_ptr` has room for `rights_capacity` guest FD numbers. The host
    /// reports the received byte count, FD count, and message flags separately.
    /// Returns errno.
    fn fd_recvmsg_rights(
        socket_fd: u32,
        data_ptr: *mut u8,
        data_len: u32,
        rights_ptr: *mut u32,
        rights_capacity: u32,
        flags: u32,
        ret_received: *mut u32,
        ret_rights_len: *mut u32,
        ret_msg_flags: *mut u32,
    ) -> Errno;

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

#[link(wasm_import_module = "host_fs")]
extern "C" {
    fn remount(
        path_ptr: *const u8,
        path_len: u32,
        options_ptr: *const u8,
        options_len: u32,
    ) -> Errno;
    fn path_owner(
        fd: u32,
        path_ptr: *const u8,
        path_len: u32,
        follow_symlinks: u32,
        ret_uid: *mut u32,
        ret_gid: *mut u32,
    ) -> Errno;
    fn path_chown(
        fd: u32,
        path_ptr: *const u8,
        path_len: u32,
        uid: u32,
        gid: u32,
        follow_symlinks: u32,
    ) -> Errno;
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

    #[link_name = "getresuid"]
    fn host_getresuid(ret_uid: *mut u32, ret_euid: *mut u32, ret_suid: *mut u32) -> Errno;
    #[link_name = "getresgid"]
    fn host_getresgid(ret_gid: *mut u32, ret_egid: *mut u32, ret_sgid: *mut u32) -> Errno;
    #[link_name = "setuid"]
    fn host_setuid(uid: u32) -> Errno;
    #[link_name = "seteuid"]
    fn host_seteuid(uid: u32) -> Errno;
    #[link_name = "setreuid"]
    fn host_setreuid(uid: u32, euid: u32) -> Errno;
    #[link_name = "setresuid"]
    fn host_setresuid(uid: u32, euid: u32, suid: u32) -> Errno;
    #[link_name = "setgid"]
    fn host_setgid(gid: u32) -> Errno;
    #[link_name = "setegid"]
    fn host_setegid(gid: u32) -> Errno;
    #[link_name = "setregid"]
    fn host_setregid(gid: u32, egid: u32) -> Errno;
    #[link_name = "setresgid"]
    fn host_setresgid(gid: u32, egid: u32, sgid: u32) -> Errno;
    #[link_name = "getgroups"]
    fn host_getgroups(size: u32, groups_ptr: *mut u32, ret_count: *mut u32) -> Errno;
    #[link_name = "setgroups"]
    fn host_setgroups(count: u32, groups_ptr: *const u32) -> Errno;

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
    #[link_name = "getpwnam"]
    fn host_getpwnam(
        name_ptr: *const u8,
        name_len: u32,
        buf_ptr: *mut u8,
        buf_len: u32,
        ret_len: *mut u32,
    ) -> Errno;
    #[link_name = "getpwent"]
    fn host_getpwent(index: u32, buf_ptr: *mut u8, buf_len: u32, ret_len: *mut u32) -> Errno;
    #[link_name = "getgrgid"]
    fn host_getgrgid(gid: u32, buf_ptr: *mut u8, buf_len: u32, ret_len: *mut u32) -> Errno;
    #[link_name = "getgrnam"]
    fn host_getgrnam(
        name_ptr: *const u8,
        name_len: u32,
        buf_ptr: *mut u8,
        buf_len: u32,
        ret_len: *mut u32,
    ) -> Errno;
    #[link_name = "getgrent"]
    fn host_getgrent(index: u32, buf_ptr: *mut u8, buf_len: u32, ret_len: *mut u32) -> Errno;
}

// ============================================================
// Safe Rust wrappers — host_process
// ============================================================

/// Spawn a child process with the given arguments, environment, stdio FDs, and working directory.
///
/// Returns `Ok(pid)` on success, `Err(errno)` on failure.
pub fn spawn(
    exec_path: &[u8],
    argv: &[u8],
    envp: &[u8],
    stdin_fd: u32,
    stdout_fd: u32,
    stderr_fd: u32,
    cwd: &[u8],
) -> Result<u32, Errno> {
    const FDOP_OPEN: u32 = 3;
    const FDOP_DUP2: u32 = 2;
    const ACTION_BYTES: usize = 24;
    const O_WRONLY: u32 = 1;

    // Rust's Command implementation supplies only the three stdio overrides.
    // Encode them in the same ordered action stream used by libc posix_spawn.
    let mut actions = [0_u8; (ACTION_BYTES * 3) + (b"/dev/null".len() * 3)];
    let mut actions_len = 0;
    for (source, target) in [(stdin_fd, 0_u32), (stdout_fd, 1_u32), (stderr_fd, 2_u32)].into_iter()
    {
        if source == target {
            continue;
        }
        let path = if source == u32::MAX {
            b"/dev/null".as_slice()
        } else {
            &[]
        };
        let base = actions_len;
        let command = if path.is_empty() {
            FDOP_DUP2
        } else {
            FDOP_OPEN
        };
        actions[base..base + 4].copy_from_slice(&command.to_le_bytes());
        actions[base + 4..base + 8].copy_from_slice(&target.to_le_bytes());
        if command == FDOP_DUP2 {
            actions[base + 8..base + 12].copy_from_slice(&source.to_le_bytes());
        } else if target != 0 {
            actions[base + 12..base + 16].copy_from_slice(&O_WRONLY.to_le_bytes());
        }
        actions[base + 20..base + 24].copy_from_slice(&(path.len() as u32).to_le_bytes());
        actions[base + ACTION_BYTES..base + ACTION_BYTES + path.len()].copy_from_slice(path);
        actions_len += ACTION_BYTES + path.len();
    }
    let (sigmask_lo, sigmask_hi) = signal_mask(3, 0, 0)?;
    let mut pid: u32 = 0;
    let exec_path_len = checked_u32_len(exec_path.len())?;
    let argv_len = checked_u32_len(argv.len())?;
    let envp_len = checked_u32_len(envp.len())?;
    let cwd_len = checked_u32_len(cwd.len())?;
    let errno = unsafe {
        proc_spawn_v3(
            exec_path.as_ptr(),
            exec_path_len,
            argv.as_ptr(),
            argv_len,
            envp.as_ptr(),
            envp_len,
            actions.as_ptr(),
            checked_u32_len(actions_len)?,
            cwd.as_ptr(),
            cwd_len,
            0,
            0,
            0,
            sigmask_lo,
            sigmask_hi,
            0,
            &mut pid,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok(pid)
    } else {
        Err(errno)
    }
}

/// Query or update the authoritative process signal mask.
pub fn signal_mask(how: u32, set_lo: u32, set_hi: u32) -> Result<(u32, u32), Errno> {
    let mut old_lo = 0;
    let mut old_hi = 0;
    let errno = unsafe { proc_signal_mask_v2(how, set_lo, set_hi, &mut old_lo, &mut old_hi) };
    if errno == ERRNO_SUCCESS {
        Ok((old_lo, old_hi))
    } else {
        Err(errno)
    }
}

/// Return the process group for `pid`; zero selects the current process.
pub fn getpgid(pid: u32) -> Result<u32, Errno> {
    let mut pgid = 0;
    let errno = unsafe { proc_getpgid(pid, &mut pgid) };
    if errno == ERRNO_SUCCESS {
        Ok(pgid)
    } else {
        Err(errno)
    }
}

/// Apply Linux setpgid semantics to a process owned by the current driver.
pub fn setpgid(pid: u32, pgid: u32) -> Result<(), Errno> {
    let errno = unsafe { proc_setpgid(pid, pgid) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Replace the current process image. Returns only when the host rejects the
/// request; a successful replacement terminates this WASM execution.
pub fn exec(
    exec_path: &[u8],
    argv: &[u8],
    envp: &[u8],
) -> Result<core::convert::Infallible, Errno> {
    exec_with_cloexec(exec_path, argv, envp, &[])
}

/// Replace the current process image and close the supplied descriptors on a
/// successful replacement. Returns only when the host rejects the request.
pub fn exec_with_cloexec(
    exec_path: &[u8],
    argv: &[u8],
    envp: &[u8],
    cloexec_fds: &[u32],
) -> Result<core::convert::Infallible, Errno> {
    let errno = unsafe {
        proc_exec(
            exec_path.as_ptr(),
            checked_u32_len(exec_path.len())?,
            argv.as_ptr(),
            checked_u32_len(argv.len())?,
            envp.as_ptr(),
            checked_u32_len(envp.len())?,
            cloexec_fds.as_ptr(),
            checked_u32_len(cloexec_fds.len())?,
        )
    };
    Err(if errno == ERRNO_SUCCESS {
        ERRNO_IO
    } else {
        errno
    })
}

/// Replace the current process image from an open descriptor. The host reads
/// from offset zero without changing the descriptor offset, matching fexecve.
pub fn fexec_with_cloexec(
    exec_fd: u32,
    argv: &[u8],
    envp: &[u8],
    cloexec_fds: &[u32],
) -> Result<core::convert::Infallible, Errno> {
    let errno = unsafe {
        proc_fexec(
            exec_fd,
            argv.as_ptr(),
            checked_u32_len(argv.len())?,
            envp.as_ptr(),
            checked_u32_len(envp.len())?,
            cloexec_fds.as_ptr(),
            checked_u32_len(cloexec_fds.len())?,
        )
    };
    Err(if errno == ERRNO_SUCCESS {
        ERRNO_IO
    } else {
        errno
    })
}

/// The result of waiting for a child process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitStatus {
    /// The child's exit code when `signal` is zero.
    pub exit_code: u32,
    /// The signal that terminated the child, or zero for a normal exit.
    pub signal: u32,
    /// The PID that was reaped (relevant when waiting for any child).
    pub pid: u32,
    /// Whether the terminating signal produced a core-dump wait status.
    pub core_dumped: bool,
}

impl WaitStatus {
    /// Convert to the shell convention used by the legacy `wasi-spawn` API.
    pub fn shell_exit_code(self) -> u32 {
        if self.signal == 0 {
            self.exit_code
        } else {
            128 + self.signal
        }
    }
}

/// Wait for a child process to exit without conflating normal exits and signals.
///
/// Returns `Ok(status)` on success, `Err(errno)` on failure.
pub fn waitpid(pid: u32, options: u32) -> Result<WaitStatus, Errno> {
    let mut exit_code: u32 = 0;
    let mut signal: u32 = 0;
    let mut actual_pid: u32 = 0;
    let mut core_dumped: u32 = 0;
    let errno = unsafe {
        proc_waitpid_v2(
            pid,
            options,
            &mut exit_code,
            &mut signal,
            &mut actual_pid,
            &mut core_dumped,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok(WaitStatus {
            exit_code,
            signal,
            pid: actual_pid,
            core_dumped: core_dumped != 0,
        })
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

/// Close every guest-visible descriptor greater than or equal to `low_fd`.
pub fn closefrom(low_fd: u32) -> Result<(), Errno> {
    let errno = unsafe { proc_closefrom(low_fd) };
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

/// Create a connected local socket pair.
///
/// Returns `Ok((fd0, fd1))` on success, `Err(errno)` on failure.
pub fn socketpair(domain: u32, sock_type: u32, protocol: u32) -> Result<(u32, u32), Errno> {
    const AF_UNIX: u32 = 1;
    const SOCK_TYPE_MASK: u32 = 0x0f;
    const SOCK_STREAM: u32 = 1;
    const SOCK_DGRAM: u32 = 2;
    const SOCK_SEQPACKET: u32 = 5;
    const SOCK_NONBLOCK: u32 = 0o4000;
    const SOCK_CLOEXEC: u32 = 0o2000000;

    if domain != AF_UNIX {
        return Err(ERRNO_NOTSUP);
    }
    if protocol != 0 && protocol != AF_UNIX {
        return Err(ERRNO_PROTONOSUPPORT);
    }
    if sock_type & !(SOCK_TYPE_MASK | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(ERRNO_INVAL);
    }
    let socket_kind = match sock_type & SOCK_TYPE_MASK {
        SOCK_STREAM => 1,
        SOCK_DGRAM => 2,
        SOCK_SEQPACKET => 3,
        _ => return Err(ERRNO_NOTSUP),
    };
    let mut fd0 = 0;
    let mut fd1 = 0;
    let errno = unsafe {
        fd_socketpair_import(
            socket_kind,
            u32::from(sock_type & SOCK_NONBLOCK != 0),
            u32::from(sock_type & SOCK_CLOEXEC != 0),
            &mut fd0,
            &mut fd1,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok((fd0, fd1))
    } else {
        Err(errno)
    }
}

/// Send data and guest FDs over a local socket.
///
/// Returns `Ok(bytes_sent)` on success, `Err(errno)` on failure.
pub fn sendmsg_rights(
    socket_fd: u32,
    data: &[u8],
    rights: &[u32],
    flags: u32,
) -> Result<u32, Errno> {
    let data_len = checked_u32_len(data.len())?;
    let rights_len = checked_u32_len(rights.len())?;
    let mut sent = 0;
    let errno = unsafe {
        fd_sendmsg_rights(
            socket_fd,
            data.as_ptr(),
            data_len,
            rights.as_ptr(),
            rights_len,
            flags,
            &mut sent,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(sent, data.len())
    } else {
        Err(errno)
    }
}

/// Receive data and guest FDs from a local socket.
///
/// Returns `(bytes_received, rights_received, message_flags)` on success.
pub fn recvmsg_rights(
    socket_fd: u32,
    data: &mut [u8],
    rights: &mut [u32],
    flags: u32,
) -> Result<(u32, u32, u32), Errno> {
    let data_len = checked_u32_len(data.len())?;
    let rights_capacity = checked_u32_len(rights.len())?;
    let mut received = 0;
    let mut rights_len = 0;
    let mut msg_flags = 0;
    let errno = unsafe {
        fd_recvmsg_rights(
            socket_fd,
            data.as_mut_ptr(),
            data_len,
            rights.as_mut_ptr(),
            rights_capacity,
            flags,
            &mut received,
            &mut rights_len,
            &mut msg_flags,
        )
    };
    if errno != ERRNO_SUCCESS {
        return Err(errno);
    }
    let received = validate_returned_len(received, data.len())?;
    let rights_len = validate_returned_len(rights_len, rights.len())?;
    Ok((received, rights_len, msg_flags))
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
pub fn sigaction_set(
    signal: u32,
    action: u32,
    mask_lo: u32,
    mask_hi: u32,
    flags: u32,
) -> Result<(), Errno> {
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
    fn net_send(fd: u32, buf_ptr: *const u8, buf_len: u32, flags: u32, ret_sent: *mut u32)
        -> Errno;

    /// Receive data from a connected socket.
    ///
    /// `buf_ptr`/`buf_len` point to the receive buffer.
    /// `flags` are recv flags (0 for default).
    /// Number of bytes received is written to `ret_received`.
    /// Returns errno.
    fn net_recv(
        fd: u32,
        buf_ptr: *mut u8,
        buf_len: u32,
        flags: u32,
        ret_received: *mut u32,
    ) -> Errno;

    /// Close a socket.
    ///
    /// Returns errno.
    fn net_close(fd: u32) -> Errno;

    /// Resolve a hostname to an address.
    ///
    /// `host_ptr`/`host_len` point to the hostname string.
    /// `port_ptr`/`port_len` point to the port/service string.
    /// `family` is 0 for any address family, 4 for IPv4, and 6 for IPv6.
    /// Resolved address is written to `ret_addr` buffer with max length from `ret_addr_len`.
    /// Actual length is written back to `ret_addr_len`.
    /// Returns errno.
    fn net_getaddrinfo(
        host_ptr: *const u8,
        host_len: u32,
        port_ptr: *const u8,
        port_len: u32,
        family: u32,
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
    fn net_setsockopt(
        fd: u32,
        level: u32,
        optname: u32,
        optval_ptr: *const u8,
        optval_len: u32,
    ) -> Errno;

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

/// Outcome of a cooperative (non-blocking) `recv`.
///
/// Distinguishes "no data right now, try again later" (`WouldBlock`) from real
/// data, EOF, and hard errors so callers can yield to the runtime instead of
/// blocking the single guest thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvOutcome {
    /// Read `usize` bytes into the buffer.
    Read(usize),
    /// Peer closed the connection (orderly EOF).
    Eof,
    /// No data available yet; the socket has a non-zero `SO_RCVTIMEO` set and the
    /// host returned `EAGAIN`. Caller should yield and re-poll.
    WouldBlock,
}

/// Receive data, mapping the host's `EAGAIN` to [`RecvOutcome::WouldBlock`].
///
/// Use this on sockets that have opted into non-blocking behavior via
/// [`set_recv_timeout_ms`]. On such sockets the host polls briefly then returns
/// `EAGAIN` instead of blocking the thread, letting the caller cooperatively
/// yield. Sockets with no recv timeout still block (this returns `Read`/`Eof`).
pub fn recv_cooperative(fd: u32, buf: &mut [u8], flags: u32) -> Result<RecvOutcome, Errno> {
    match recv(fd, buf, flags) {
        Ok(0) => Ok(RecvOutcome::Eof),
        Ok(n) => Ok(RecvOutcome::Read(n as usize)),
        Err(ERRNO_AGAIN) => Ok(RecvOutcome::WouldBlock),
        Err(e) => Err(e),
    }
}

/// Mark a socket non-blocking for recv by setting a small, non-zero
/// `SO_RCVTIMEO`.
///
/// The host_net shim polls up to this timeout then returns `EAGAIN` when no data
/// arrived. A zero timeout is rejected by the host (it would mean "blocking"),
/// so callers should pass a small non-zero value (e.g. 2ms). Leaving a socket
/// without ever calling this keeps the default blocking recv behavior, so other
/// guests are unaffected.
pub fn set_recv_timeout_ms(fd: u32, millis: u32) -> Result<(), Errno> {
    let micros: u64 = (millis as u64).saturating_mul(1000);
    let secs = micros / 1_000_000;
    let usec = micros % 1_000_000;
    let mut timeval = [0u8; TIMEVAL_BYTES];
    timeval[0..8].copy_from_slice(&secs.to_le_bytes());
    timeval[8..16].copy_from_slice(&usec.to_le_bytes());
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeval)
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
            0,
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
        validate_poll_ready_count(ready, nfds)
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
pub fn recvfrom(
    fd: u32,
    buf: &mut [u8],
    flags: u32,
    addr_buf: &mut [u8],
) -> Result<(u32, u32), Errno> {
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

fn errno_result(errno: Errno) -> Result<(), Errno> {
    if errno == ERRNO_SUCCESS {
        Ok(())
    } else {
        Err(errno)
    }
}

fn optional_id(id: Option<u32>) -> u32 {
    id.unwrap_or(u32::MAX)
}

pub fn get_resuid() -> Result<(u32, u32, u32), Errno> {
    let (mut uid, mut euid, mut suid) = (0, 0, 0);
    let errno = unsafe { host_getresuid(&mut uid, &mut euid, &mut suid) };
    (errno == ERRNO_SUCCESS)
        .then_some((uid, euid, suid))
        .ok_or(errno)
}

pub fn get_resgid() -> Result<(u32, u32, u32), Errno> {
    let (mut gid, mut egid, mut sgid) = (0, 0, 0);
    let errno = unsafe { host_getresgid(&mut gid, &mut egid, &mut sgid) };
    (errno == ERRNO_SUCCESS)
        .then_some((gid, egid, sgid))
        .ok_or(errno)
}

pub fn set_uid(uid: u32) -> Result<(), Errno> {
    errno_result(unsafe { host_setuid(uid) })
}

pub fn set_euid(uid: u32) -> Result<(), Errno> {
    errno_result(unsafe { host_seteuid(uid) })
}

pub fn set_reuid(uid: Option<u32>, euid: Option<u32>) -> Result<(), Errno> {
    errno_result(unsafe { host_setreuid(optional_id(uid), optional_id(euid)) })
}

pub fn set_resuid(uid: Option<u32>, euid: Option<u32>, suid: Option<u32>) -> Result<(), Errno> {
    errno_result(unsafe { host_setresuid(optional_id(uid), optional_id(euid), optional_id(suid)) })
}

pub fn set_gid(gid: u32) -> Result<(), Errno> {
    errno_result(unsafe { host_setgid(gid) })
}

pub fn set_egid(gid: u32) -> Result<(), Errno> {
    errno_result(unsafe { host_setegid(gid) })
}

pub fn set_regid(gid: Option<u32>, egid: Option<u32>) -> Result<(), Errno> {
    errno_result(unsafe { host_setregid(optional_id(gid), optional_id(egid)) })
}

pub fn set_resgid(gid: Option<u32>, egid: Option<u32>, sgid: Option<u32>) -> Result<(), Errno> {
    errno_result(unsafe { host_setresgid(optional_id(gid), optional_id(egid), optional_id(sgid)) })
}

pub fn get_groups() -> Result<Vec<u32>, Errno> {
    let mut count = 0;
    let errno = unsafe { host_getgroups(0, core::ptr::null_mut(), &mut count) };
    if errno != ERRNO_SUCCESS {
        return Err(errno);
    }
    let mut groups = vec![0; count as usize];
    let errno = unsafe { host_getgroups(count, groups.as_mut_ptr(), &mut count) };
    if errno != ERRNO_SUCCESS {
        return Err(errno);
    }
    groups.truncate(count as usize);
    Ok(groups)
}

pub fn set_groups(groups: &[u32]) -> Result<(), Errno> {
    errno_result(unsafe { host_setgroups(checked_u32_len(groups.len())?, groups.as_ptr()) })
}

pub fn path_ids(path: &str, follow_symlinks: bool) -> Result<(u32, u32), Errno> {
    let (mut uid, mut gid) = (0, 0);
    let errno = unsafe {
        path_owner(
            u32::MAX,
            path.as_ptr(),
            checked_u32_len(path.len())?,
            u32::from(follow_symlinks),
            &mut uid,
            &mut gid,
        )
    };
    if errno == ERRNO_SUCCESS {
        Ok((uid, gid))
    } else {
        Err(errno)
    }
}

pub fn chown_path(path: &str, uid: u32, gid: u32, follow_symlinks: bool) -> Result<(), Errno> {
    errno_result(unsafe {
        path_chown(
            u32::MAX,
            path.as_ptr(),
            checked_u32_len(path.len())?,
            uid,
            gid,
            u32::from(follow_symlinks),
        )
    })
}

pub fn remount_path(path: &str, options: &str) -> Result<(), Errno> {
    errno_result(unsafe {
        remount(
            path.as_ptr(),
            checked_u32_len(path.len())?,
            options.as_ptr(),
            checked_u32_len(options.len())?,
        )
    })
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

pub fn get_pwnam(name: &str, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = 0;
    let errno = unsafe {
        host_getpwnam(
            name.as_ptr(),
            checked_u32_len(name.len())?,
            buf.as_mut_ptr(),
            checked_u32_len(buf.len())?,
            &mut len,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

pub fn get_pwent(index: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = 0;
    let errno = unsafe {
        host_getpwent(
            index,
            buf.as_mut_ptr(),
            checked_u32_len(buf.len())?,
            &mut len,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

pub fn get_grgid(gid: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = 0;
    let errno =
        unsafe { host_getgrgid(gid, buf.as_mut_ptr(), checked_u32_len(buf.len())?, &mut len) };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

pub fn get_grnam(name: &str, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = 0;
    let errno = unsafe {
        host_getgrnam(
            name.as_ptr(),
            checked_u32_len(name.len())?,
            buf.as_mut_ptr(),
            checked_u32_len(buf.len())?,
            &mut len,
        )
    };
    if errno == ERRNO_SUCCESS {
        validate_returned_len(len, buf.len())
    } else {
        Err(errno)
    }
}

pub fn get_grent(index: u32, buf: &mut [u8]) -> Result<u32, Errno> {
    let mut len = 0;
    let errno = unsafe {
        host_getgrent(
            index,
            buf.as_mut_ptr(),
            checked_u32_len(buf.len())?,
            &mut len,
        )
    };
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
        assert_eq!(
            validate_poll_buffer_len(POLLFD_BYTES - 1, 1),
            Err(ERRNO_INVAL)
        );
        assert_eq!(
            validate_poll_buffer_len(POLLFD_BYTES + 1, 1),
            Err(ERRNO_INVAL)
        );
    }

    #[test]
    fn returned_lengths_must_fit_in_the_supplied_buffer() {
        assert_eq!(validate_returned_len(4, 4), Ok(4));
        assert_eq!(validate_returned_len(5, 4), Err(ERRNO_INVAL));
    }

    #[test]
    fn poll_ready_count_must_not_exceed_nfds() {
        assert_eq!(validate_poll_ready_count(0, 0), Ok(0));
        assert_eq!(validate_poll_ready_count(2, 2), Ok(2));
        assert_eq!(validate_poll_ready_count(3, 2), Err(ERRNO_INVAL));
    }
}
