//! wasm32-wasip1 process `imp` for Tokio.
//!
//! Approach: route to the PATCHED `std::process` (host_process bridge). The VM is
//! single-threaded. An inherited-FD child can use the interruptible blocking
//! `wait()` import because agentOS suspends the Store while sibling processes
//! continue independently. Captured children use nonblocking `try_wait()`
//! probes so callers that manage their own pipes can still drain concurrently.
//! Tokio's patched `wait_with_output()` drains both kernel-backed pipes to EOF
//! before reaping the child, avoiding a waitpid/read busy loop entirely.
//! `ChildStdio` uses nonblocking OS fds so captured-output drains yield instead
//! of pinning the only guest executor thread.
//! No SIGCHLD / orphan reaping / mio / pidfd on wasi.
//!
//! Wiring: in `src/process/mod.rs`, add alongside the unix/windows imp selection:
//!   #[path = "wasi.rs"] #[cfg(target_os = "wasi")] mod imp;
//! and add `#[cfg(target_os = "wasi")] use imp::*;` to the imp re-export block.
//! Also remove `#[cfg(not(target_os = "wasi"))]` from `cfg_process!` (macros/cfg.rs)
//! and build the wasm target with `RUSTFLAGS="--cfg tokio_unstable"`.

use crate::io::AsyncRead;
use crate::io::AsyncWrite;
use crate::io::ReadBuf;
use crate::process::kill::Kill;
use crate::process::SpawnedChild;

use std::fmt;
use std::future::Future;
use std::io;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::pin::Pin;
use std::process::Child as StdChild;
use std::process::ExitStatus;
use std::process::Stdio;
use std::task::Context;
use std::task::Poll;

const CHILD_WAIT_POLL_INTERVAL_MS: u32 = 1;
const CHILD_WAIT_POLLS_PER_BACKOFF: u8 = 1;
const CHILD_STDIO_RETRY_INTERVAL_MS: u32 = 1;

#[link(wasm_import_module = "host_process")]
unsafe extern "C" {
    #[link_name = "sleep_ms"]
    fn agentos_sleep_ms(milliseconds: u32) -> u32;
}

/// No-op orphan queue: wasm32-wasip1 has no SIGCHLD, and the host reaps the
/// child when `wait()` returns. Kept to satisfy the imp surface.
#[derive(Debug)]
pub(crate) struct GlobalOrphanQueue;

impl GlobalOrphanQueue {
    pub(crate) fn reap_orphans() {}
}

pub(crate) struct Child {
    inner: StdChild,
    has_captured_output: bool,
    pending_polls: u8,
}

impl fmt::Debug for Child {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Child").field("pid", &self.id()).finish()
    }
}

pub(crate) fn build_child(mut child: StdChild) -> io::Result<SpawnedChild> {
    let has_captured_output = child.stdout.is_some() || child.stderr.is_some();
    let stdin = child.stdin.take().map(stdio).transpose()?;
    let stdout = child.stdout.take().map(stdio).transpose()?;
    let stderr = child.stderr.take().map(stdio).transpose()?;
    Ok(SpawnedChild {
        child: Child {
            inner: child,
            has_captured_output,
            pending_polls: 0,
        },
        stdin,
        stdout,
        stderr,
    })
}

impl Child {
    pub(crate) fn id(&self) -> u32 {
        self.inner.id()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.inner.try_wait()
    }
}

impl Kill for Child {
    fn kill(&mut self) -> io::Result<()> {
        self.inner.kill()
    }
}

impl Future for Child {
    type Output = io::Result<ExitStatus>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let child = self.get_mut();
        if !child.has_captured_output {
            // Inherited and explicitly mapped pipeline FDs are sidecar-owned,
            // so sibling processes keep moving data while this Store is
            // suspended. A caught signal interrupts the deferred host wait and
            // lets the guest scheduler service its signal futures before retrying.
            return match child.inner.wait() {
                Ok(status) => Poll::Ready(Ok(status)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Err(error) => Poll::Ready(Err(error)),
            };
        }

        match child.inner.try_wait() {
            Ok(Some(status)) => Poll::Ready(Ok(status)),
            Ok(None) => {
                // `wake_by_ref` without a delay makes the single-threaded guest
                // runtime issue waitpid + clock_time host calls at CPU speed.
                // The agentOS sleep import suspends the Wasmtime Store on the
                // sidecar's deferred process.sleep operation (and V8 on its
                // dedicated guest thread), so this does not block a shared
                // Tokio worker. Rust's WASI thread::sleep must not be used here:
                // its current implementation busy-polls clock_time.
                child.pending_polls += 1;
                if child.pending_polls == CHILD_WAIT_POLLS_PER_BACKOFF {
                    child.pending_polls = 0;
                    let errno = unsafe { agentos_sleep_ms(CHILD_WAIT_POLL_INTERVAL_MS) };
                    if errno != 0 {
                        return Poll::Ready(Err(io::Error::from_raw_os_error(errno as i32)));
                    }
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// Async-shaped child pipe backed by a blocking OS fd. `poll_*` resolve on the
/// first poll (single-threaded VM).
#[derive(Debug)]
pub(crate) struct ChildStdio {
    fd: OwnedFd,
}

impl AsRawFd for ChildStdio {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl AsyncRead for ChildStdio {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        let mut file = unsafe { std::fs::File::from_raw_fd(me.fd.as_raw_fd()) };
        let res = file.read(buf.initialize_unfilled());
        let _ = file.into_raw_fd(); // don't close the borrowed fd
        match res {
            Ok(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            // The child pipe fd is set non-blocking in `stdio()`, so a read with
            // no data available returns `WouldBlock` (WASI EAGAIN) instead of
            // pinning the single-threaded executor. Re-poll cooperatively: yield
            // to the timeout future + submission loop, then try again. The host
            // `_fdRead` still drives the child via `_pumpPipeProducers`, so the
            // child keeps making progress between reads.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let errno = unsafe { agentos_sleep_ms(CHILD_STDIO_RETRY_INTERVAL_MS) };
                if errno != 0 {
                    return Poll::Ready(Err(io::Error::from_raw_os_error(errno as i32)));
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl AsyncWrite for ChildStdio {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let mut file = unsafe { std::fs::File::from_raw_fd(me.fd.as_raw_fd()) };
        let res = file.write(buf);
        let _ = file.into_raw_fd();
        Poll::Ready(res)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// WASI host import: set fd flags (FDFLAGS_NONBLOCK = 0x0004). std already imports
// this from `wasi_snapshot_preview1`; re-declaring resolves to the same import.
#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    #[link_name = "fd_fdstat_set_flags"]
    fn __wasi_fd_fdstat_set_flags(fd: u32, flags: u16) -> u16;
}

pub(crate) fn stdio<T: Into<OwnedFd>>(io: T) -> io::Result<ChildStdio> {
    let fd = io.into();
    // Mark the child pipe fd non-blocking so `poll_read`/`poll_write` get
    // `WouldBlock` instead of blocking the only executor thread (no I/O reactor
    // on single-threaded wasm32-wasip1). FDFLAGS_NONBLOCK = 0x0004.
    unsafe {
        let _ = __wasi_fd_fdstat_set_flags(fd.as_raw_fd() as u32, 0x0004);
    }
    Ok(ChildStdio { fd })
}

pub(crate) fn convert_to_stdio(io: ChildStdio) -> io::Result<Stdio> {
    // wasi `Stdio` is `From<File>` (not `From<OwnedFd>`); `File` is `From<OwnedFd>`.
    Ok(Stdio::from(std::fs::File::from(io.fd)))
}
