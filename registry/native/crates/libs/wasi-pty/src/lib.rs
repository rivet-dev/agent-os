//! WASI PTY-based process management via host_process FFI.
//!
//! Provides [`WasiPtyChild`] — an interactive process handle using a pseudo-terminal
//! instead of pipes. This is the WASI equivalent of `ExecCommandSession` from
//! `codex-utils-pty` (which wraps `portable-pty` on native platforms).
//!
//! Key difference from [`wasi_spawn::WasiChild`]:
//! - Uses a PTY master/slave pair instead of separate stdout/stderr pipes
//! - All child output (stdout + stderr) is multiplexed through the PTY master
//! - Supports interactive programs that require a terminal (e.g., editors, shells)
//! - The PTY provides terminal emulation (line discipline, echo, signals)

use std::io::{self, Read, Write};

/// Handle to a spawned process connected via a pseudo-terminal.
///
/// Created by [`spawn_pty`]. The child process has the PTY slave as its
/// stdin/stdout/stderr. The parent reads and writes via the PTY master FD.
///
/// This is the WASI equivalent of `SpawnedPty` from `codex-utils-pty`.
pub struct WasiSpawnedPty {
    master_fd: RawFd,
}

/// Interactive process session using a PTY.
///
/// Created by [`spawn_session`]. Wraps a [`WasiSpawnedPty`] and the child
/// process handle, providing a unified API for interactive process management.
///
/// This is the WASI equivalent of `ExecCommandSession` from `codex-utils-pty`.
pub struct WasiPtyChild {
    pid: u32,
    master_fd: RawFd,
    exited: bool,
}

type RawFd = u32;

fn errno_to_io_error(errno: wasi_ext::Errno) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("wasi errno {}", errno))
}

/// Read from a raw WASI file descriptor into a buffer.
fn fd_read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    use std::os::fd::FromRawFd;
    let file = unsafe { std::fs::File::from_raw_fd(fd as i32) };
    let result = (&file).read(buf);
    std::mem::forget(file);
    result
}

/// Write to a raw WASI file descriptor from a buffer.
fn fd_write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    use std::os::fd::FromRawFd;
    let file = unsafe { std::fs::File::from_raw_fd(fd as i32) };
    let result = (&file).write(buf);
    std::mem::forget(file);
    result
}

/// Close a raw WASI file descriptor.
fn fd_close(fd: RawFd) {
    use std::os::fd::FromRawFd;
    drop(unsafe { std::fs::File::from_raw_fd(fd as i32) });
}

/// Serialize strings as null-separated byte buffer for proc_spawn.
fn serialize_null_separated(items: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            buf.push(0);
        }
        buf.extend_from_slice(item.as_bytes());
    }
    buf
}

/// Serialize environment as KEY=VALUE null-separated pairs for proc_spawn.
fn serialize_env(env: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (i, (key, value)) in env.iter().enumerate() {
        if i > 0 {
            buf.push(0);
        }
        buf.extend_from_slice(key.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(value.as_bytes());
    }
    buf
}

/// Spawn a child process connected via a PTY.
///
/// Allocates a PTY master/slave pair, spawns the child with the slave FD as
/// stdin/stdout/stderr, and returns a [`WasiPtyChild`] handle. The slave FD
/// is closed in the parent after spawn (POSIX close-after-fork).
///
/// # Arguments
/// * `argv` - Command and arguments (argv[0] is the program name)
/// * `env` - Environment variable pairs (empty inherits parent env via host)
/// * `cwd` - Working directory for the child
pub fn spawn_session(argv: &[&str], env: &[(&str, &str)], cwd: &str) -> io::Result<WasiPtyChild> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }

    // Allocate PTY master/slave pair via kernel
    let (master_fd, slave_fd) = wasi_ext::openpty().map_err(errno_to_io_error)?;

    let argv_buf = serialize_null_separated(argv);
    let envp_buf = serialize_env(env);

    // Spawn child with PTY slave as all stdio
    let result = wasi_ext::spawn(
        &argv_buf,
        &envp_buf,
        slave_fd,
        slave_fd,
        slave_fd,
        cwd.as_bytes(),
    );

    // Close slave FD in parent (POSIX close-after-fork) — child has its own ref
    fd_close(slave_fd);

    match result {
        Ok(pid) => Ok(WasiPtyChild {
            pid,
            master_fd,
            exited: false,
        }),
        Err(errno) => {
            fd_close(master_fd);
            Err(errno_to_io_error(errno))
        }
    }
}

/// Allocate a PTY pair without spawning a process.
///
/// Returns a [`WasiSpawnedPty`] for the master end. The slave FD is returned
/// separately so the caller can pass it to [`wasi_spawn::spawn_child`] or
/// use it directly.
pub fn open_pty() -> io::Result<(WasiSpawnedPty, RawFd)> {
    let (master_fd, slave_fd) = wasi_ext::openpty().map_err(errno_to_io_error)?;

    Ok((WasiSpawnedPty { master_fd }, slave_fd))
}

impl WasiSpawnedPty {
    /// Get the master FD for direct I/O.
    pub fn master_fd(&self) -> RawFd {
        self.master_fd
    }

    /// Read output from the PTY master (data written by the child).
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        fd_read(self.master_fd, buf)
    }

    /// Write input to the PTY master (delivered to the child's stdin).
    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        fd_write(self.master_fd, buf)
    }
}

impl Drop for WasiSpawnedPty {
    fn drop(&mut self) {
        fd_close(self.master_fd);
    }
}

impl WasiPtyChild {
    /// Get the child's virtual PID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Get the PTY master FD for direct I/O if needed.
    pub fn master_fd(&self) -> RawFd {
        self.master_fd
    }

    /// Read output from the child via the PTY master.
    ///
    /// All child output (stdout and stderr) is multiplexed through the PTY.
    /// Returns 0 bytes when the PTY slave is closed (child exited).
    pub fn read_output(&self, buf: &mut [u8]) -> io::Result<usize> {
        fd_read(self.master_fd, buf)
    }

    /// Write input to the child via the PTY master.
    ///
    /// Data is delivered to the child's stdin through the PTY line discipline.
    pub fn write_stdin(&self, buf: &[u8]) -> io::Result<usize> {
        fd_write(self.master_fd, buf)
    }

    /// Wait for the child to exit. Returns the exit code.
    ///
    /// Blocks via host_process_waitpid (Atomics.wait on host side).
    pub fn wait(&mut self) -> io::Result<i32> {
        if self.exited {
            return Err(io::Error::new(io::ErrorKind::Other, "already waited"));
        }

        let (status, _actual_pid) = wasi_ext::waitpid(self.pid, 0).map_err(errno_to_io_error)?;

        self.exited = true;
        Ok(status as i32)
    }

    /// Send a signal to the child process.
    pub fn kill(&mut self, signal: u32) -> io::Result<()> {
        wasi_ext::kill(self.pid, signal).map_err(errno_to_io_error)
    }

    /// Send SIGTERM to the child process.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.kill(15)
    }

    /// Read all output from the PTY, then wait for exit.
    ///
    /// Reads output until the PTY master gets EOF (child closed slave),
    /// then waits for the child to exit.
    pub fn consume_output(&mut self) -> io::Result<wasi_spawn::WasiOutput> {
        let mut stdout = Vec::new();

        // Read all output from PTY master until EOF
        let mut buf = [0u8; 4096];
        loop {
            match self.read_output(&mut buf) {
                Ok(0) => break,
                Ok(n) => stdout.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                Err(e) => return Err(e),
            }
        }

        let exit_code = self.wait()?;

        // PTY multiplexes stdout+stderr, so stderr is empty
        Ok(wasi_spawn::WasiOutput {
            stdout,
            stderr: Vec::new(),
            exit_code,
        })
    }
}

impl Drop for WasiPtyChild {
    fn drop(&mut self) {
        fd_close(self.master_fd);
    }
}
