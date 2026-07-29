//! std patch artifact (pipeline-only codex port) — wasi public child-pipe fd traits.
//!
//! PROBLEM (real agentos bug, found 2026-06-23): the patched wasi std provides
//! the SEOS process impl (`sys/process/wasi.rs`, `ChildPipe(FileDesc)`) but NOT the
//! public `process::{ChildStdin,ChildStdout,ChildStderr}` fd traits — `os/unix/process.rs`
//! has them (AsRawFd/IntoRawFd/AsFd/From<_> for OwnedFd) but there is no `os/wasi`
//! equivalent. Without them, `tokio::process`'s wasi imp (and any fd-extracting code)
//! cannot get the child pipe fds. Also: `std-patches/0001-wasi-process-spawn.patch` is
//! STALE vs nightly-2026-03-01 (leaves sys/process/{mod.rs,wasi.rs}.rej) and should be
//! refreshed.
//!
//! FIX, two parts:
//!
//! (1) In `library/std/src/sys/process/wasi.rs`, add the inner-fd plumbing on ChildPipe
//!     (FileDesc already impls AsRawFd / IntoInner<OwnedFd>):
//!
//!     impl crate::os::fd::AsRawFd for ChildPipe {
//!         fn as_raw_fd(&self) -> crate::os::fd::RawFd { self.0.as_raw_fd() }
//!     }
//!     impl crate::os::fd::AsFd for ChildPipe {
//!         fn as_fd(&self) -> crate::os::fd::BorrowedFd<'_> { self.0.as_fd() }
//!     }
//!     impl crate::sys::IntoInner<FileDesc> for ChildPipe {
//!         fn into_inner(self) -> FileDesc { self.0 }
//!     }
//!
//! (2) New `library/std/src/os/wasi/process.rs` (mirrors os/unix/process.rs:455-560),
//!     wired via `pub mod process;` in `os/wasi/mod.rs`:

#![allow(unused)]

use crate::os::wasi::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use crate::process;
use crate::sys::{AsInner, IntoInner};

macro_rules! impl_child_pipe_fd {
    ($t:ty) => {
        #[stable(feature = "process_extensions", since = "1.2.0")]
        impl AsRawFd for $t {
            #[inline]
            fn as_raw_fd(&self) -> RawFd {
                self.as_inner().as_raw_fd()
            }
        }

        #[stable(feature = "into_raw_os", since = "1.4.0")]
        impl IntoRawFd for $t {
            #[inline]
            fn into_raw_fd(self) -> RawFd {
                // ChildStd* -> imp::ChildPipe -> FileDesc -> raw fd
                self.into_inner().into_inner().into_inner().into_raw_fd()
            }
        }

        #[stable(feature = "io_safety", since = "1.63.0")]
        impl AsFd for $t {
            #[inline]
            fn as_fd(&self) -> BorrowedFd<'_> {
                self.as_inner().as_fd()
            }
        }

        #[stable(feature = "io_safety", since = "1.63.0")]
        impl From<$t> for OwnedFd {
            #[inline]
            fn from(child: $t) -> OwnedFd {
                child.into_inner().into_inner().into_inner()
            }
        }
    };
}

impl_child_pipe_fd!(process::ChildStdin);
impl_child_pipe_fd!(process::ChildStdout);
impl_child_pipe_fd!(process::ChildStderr);

// NOTE: exact `into_inner()` chain depth (ChildStd* → ChildPipe → FileDesc → OwnedFd)
// must be confirmed against the wasi sys impl during application; adjust the macro
// body to match (unix uses `self.into_inner().into_inner().into_raw_fd()`).
// Once these land, the tokio wasi process imp (`wasi-process-imp.rs`) `stdio`
// (Into<OwnedFd>) + `convert_to_stdio` (via File) compile, and tokio::process works.
