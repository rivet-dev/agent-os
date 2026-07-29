//! WASI-specific extensions to primitives in the [`std::process`] module.
//!
//! Mirrors `os/unix/process.rs`' child-pipe fd traits for wasm32-wasip1 so that
//! `tokio::process` (and other fd-extracting code) can reach the parent-side
//! pipe ends of a spawned child. (agentos pipeline-only codex port.)
//!
//! [`std::process`]: crate::process

#![stable(feature = "rust1", since = "1.0.0")]

use crate::os::wasi::io::{AsFd, AsRawFd, BorrowedFd, IntoRawFd, OwnedFd, RawFd};
use crate::process;
use crate::sys::{AsInner, AsInnerMut, FromInner, IntoInner};

const WASI_ERRNO_BADF: i32 = 8;

/// WASI-specific child process descriptor mappings.
#[stable(feature = "rust1", since = "1.0.0")]
pub trait CommandExt: crate::sealed::Sealed {
    /// Duplicates `source` onto `target` in the child before execution.
    ///
    /// The command owns `source` until spawning completes, and the extra source
    /// descriptor is closed in the child after all mappings are applied.
    #[stable(feature = "rust1", since = "1.0.0")]
    fn fd_mapping(
        &mut self,
        source: OwnedFd,
        target: RawFd,
    ) -> crate::io::Result<&mut process::Command>;
}

#[stable(feature = "rust1", since = "1.0.0")]
impl CommandExt for process::Command {
    fn fd_mapping(
        &mut self,
        source: OwnedFd,
        target: RawFd,
    ) -> crate::io::Result<&mut process::Command> {
        let target = u32::try_from(target)
            .map_err(|_| crate::io::Error::from_raw_os_error(WASI_ERRNO_BADF))?;
        self.as_inner_mut().fd_mapping(source, target)?;
        Ok(self)
    }
}

macro_rules! impl_child_pipe_fd {
    ($t:ty) => {
        #[stable(feature = "process_extensions", since = "1.2.0")]
        impl AsRawFd for $t {
            #[inline]
            fn as_raw_fd(&self) -> RawFd {
                self.as_inner().as_fd().as_raw_fd()
            }
        }

        #[stable(feature = "into_raw_os", since = "1.4.0")]
        impl IntoRawFd for $t {
            #[inline]
            fn into_raw_fd(self) -> RawFd {
                self.into_inner().into_inner().into_raw_fd()
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
                child.into_inner().into_inner()
            }
        }
    };
}

impl_child_pipe_fd!(process::ChildStdin);
impl_child_pipe_fd!(process::ChildStdout);
impl_child_pipe_fd!(process::ChildStderr);

/// WASI-specific extension to construct an [`ExitStatus`] from a raw code,
/// mirroring `std::os::unix::process::ExitStatusExt::from_raw`. (agentos
/// pipeline-only codex port — codex's synthetic exit statuses need this.)
#[stable(feature = "rust1", since = "1.0.0")]
pub trait ExitStatusExt {
    /// Construct an `ExitStatus` from the given raw code.
    #[stable(feature = "exit_status_from", since = "1.12.0")]
    fn from_raw(raw: i32) -> Self;

    /// If the process was terminated by a signal, returns that signal.
    ///
    /// AgentOS preserves the host signal in the POSIX wait status returned by
    /// the WASI process broker.
    #[stable(feature = "process_extensions", since = "1.2.0")]
    fn signal(&self) -> Option<i32>;
}

#[stable(feature = "exit_status_from", since = "1.12.0")]
impl ExitStatusExt for process::ExitStatus {
    fn from_raw(raw: i32) -> Self {
        process::ExitStatus::from_inner(crate::sys::process::ExitStatus::from(raw))
    }

    fn signal(&self) -> Option<i32> {
        self.as_inner().signal()
    }
}
