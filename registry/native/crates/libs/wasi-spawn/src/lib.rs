//! WASI process spawning via host_process FFI.
//!
//! Provides `WasiChild` — a synchronous child process handle with pipe-based
//! stdout/stderr capture, wait, and kill. Uses wasi-ext FFI directly instead
//! of std::process::Command for explicit control over pipe lifecycle.
//!
//! Designed for codex-rs WASI integration: replaces tokio::process::Command
//! on wasm32-wasip1 where tokio process/signal features are unavailable.

use std::io::{self, Read};

/// Captured output from a child process.
pub struct WasiOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

/// Handle to a spawned child process with pipe-based I/O capture.
///
/// Created by [`spawn_child`]. Owns the read ends of stdout/stderr pipes.
/// The write ends are closed in the parent after spawn (POSIX close-after-fork).
pub struct WasiChild {
    pid: u32,
    stdout_fd: Option<RawFd>,
    stderr_fd: Option<RawFd>,
    exited: bool,
}

/// Raw file descriptor type matching WASI u32 FDs.
type RawFd = u32;

fn errno_to_io_error(errno: wasi_ext::Errno) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("wasi errno {}", errno))
}

/// Read from a raw WASI file descriptor into a buffer.
///
/// Uses std::fs::File::from_raw_fd for WASI fd_read dispatch.
fn fd_read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // Safety: fd is a valid local FD from pipe() registered in the WASI FD table.
    // We use ManuallyDrop to avoid closing the FD when done reading.
    use std::os::fd::FromRawFd;
    let file = unsafe { std::fs::File::from_raw_fd(fd as i32) };
    let result = (&file).read(buf);
    // Don't close the FD — WasiChild manages its lifetime
    std::mem::forget(file);
    result
}

/// Close a raw WASI file descriptor.
fn fd_close(fd: RawFd) {
    use std::os::fd::FromRawFd;
    // Safety: fd is a valid local FD. Drop closes it via WASI fd_close.
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

fn spawn_child_with_stdin_fd(
    argv: &[&str],
    env: &[(&str, &str)],
    cwd: &str,
    stdin_fd: u32,
) -> io::Result<WasiChild> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }

    // Create stdout pipe
    let (stdout_read, stdout_write) = wasi_ext::pipe().map_err(errno_to_io_error)?;

    // Create stderr pipe
    let (stderr_read, stderr_write) = wasi_ext::pipe().map_err(|e| {
        fd_close(stdout_read);
        fd_close(stdout_write);
        errno_to_io_error(e)
    })?;

    // Serialize argv and envp
    let argv_buf = serialize_null_separated(argv);
    let envp_buf = serialize_env(env);

    // Spawn child with pipe-captured stdout/stderr and caller-selected stdin.
    let result = wasi_ext::spawn(
        &argv_buf,
        &envp_buf,
        stdin_fd,
        stdout_write,
        stderr_write,
        cwd.as_bytes(),
    );

    // Close write ends in parent (POSIX close-after-fork)
    fd_close(stdout_write);
    fd_close(stderr_write);

    match result {
        Ok(pid) => Ok(WasiChild {
            pid,
            stdout_fd: Some(stdout_read),
            stderr_fd: Some(stderr_read),
            exited: false,
        }),
        Err(errno) => {
            fd_close(stdout_read);
            fd_close(stderr_read);
            Err(errno_to_io_error(errno))
        }
    }
}

/// Spawn a child process with pipe-captured stdout and stderr.
///
/// Creates pipes for stdout/stderr, spawns the child via host_process FFI,
/// and returns a `WasiChild` handle. The parent's stdin is inherited.
///
/// # Arguments
/// * `argv` - Command and arguments (argv[0] is the program name)
/// * `env` - Environment variable pairs (empty inherits parent env via host)
/// * `cwd` - Working directory for the child
pub fn spawn_child(argv: &[&str], env: &[(&str, &str)], cwd: &str) -> io::Result<WasiChild> {
    spawn_child_with_stdin_fd(argv, env, cwd, 0)
}

/// Spawn a child process with stdin ignored and stdout/stderr captured.
///
/// This is appropriate for agent tool calls or other subprocesses that must not
/// inherit an interactive control stream from the parent process.
pub fn spawn_child_ignore_stdin(
    argv: &[&str],
    env: &[(&str, &str)],
    cwd: &str,
) -> io::Result<WasiChild> {
    spawn_child_with_stdin_fd(argv, env, cwd, u32::MAX)
}

/// Spawn a child process inheriting all stdio (no pipe capture).
///
/// Useful for interactive commands where output should go directly to
/// the parent's terminal.
pub fn spawn_child_inherit(
    argv: &[&str],
    env: &[(&str, &str)],
    cwd: &str,
) -> io::Result<WasiChild> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }

    let argv_buf = serialize_null_separated(argv);
    let envp_buf = serialize_env(env);

    let pid = wasi_ext::spawn(
        &argv_buf,
        &envp_buf,
        0,
        1,
        2, // inherit all stdio
        cwd.as_bytes(),
    )
    .map_err(errno_to_io_error)?;

    Ok(WasiChild {
        pid,
        stdout_fd: None,
        stderr_fd: None,
        exited: false,
    })
}

impl WasiChild {
    /// Get the child's virtual PID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Read from the child's stdout pipe.
    ///
    /// Returns 0 bytes when the pipe is closed (child exited or closed stdout).
    pub fn read_stdout(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stdout_fd {
            Some(fd) => fd_read(fd, buf),
            None => Ok(0),
        }
    }

    /// Read from the child's stderr pipe.
    ///
    /// Returns 0 bytes when the pipe is closed (child exited or closed stderr).
    pub fn read_stderr(&self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stderr_fd {
            Some(fd) => fd_read(fd, buf),
            None => Ok(0),
        }
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

        // Decode exit status using bash 128+signal convention
        // Normal exit: status is the exit code directly
        // Signal kill: status is 128 + signal number
        Ok(status as i32)
    }

    /// Send a signal to the child process.
    ///
    /// Common signals: SIGTERM (15), SIGKILL (9).
    pub fn kill(&mut self, signal: u32) -> io::Result<()> {
        wasi_ext::kill(self.pid, signal).map_err(errno_to_io_error)
    }

    /// Send SIGTERM to the child process.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.kill(15)
    }

    /// Read all stdout and stderr, then wait for exit.
    ///
    /// Reads stdout fully, then stderr fully, then waits. For codex-rs,
    /// this replaces the concurrent tokio::spawn approach since WASI is
    /// single-threaded.
    pub fn consume_output(&mut self) -> io::Result<WasiOutput> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        // Read stdout to EOF
        if self.stdout_fd.is_some() {
            let mut buf = [0u8; 4096];
            loop {
                match self.read_stdout(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => stdout.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(e) => return Err(e),
                }
            }
        }

        // Read stderr to EOF
        if self.stderr_fd.is_some() {
            let mut buf = [0u8; 4096];
            loop {
                match self.read_stderr(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => stderr.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                    Err(e) => return Err(e),
                }
            }
        }

        let exit_code = self.wait()?;

        Ok(WasiOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}

impl Drop for WasiChild {
    fn drop(&mut self) {
        // Close pipe read ends
        if let Some(fd) = self.stdout_fd.take() {
            fd_close(fd);
        }
        if let Some(fd) = self.stderr_fd.take() {
            fd_close(fd);
        }
    }
}
