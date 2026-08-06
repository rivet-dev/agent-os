//! agentOS POSIX backend for `portable-pty`.
//!
//! Rust names the guest target `wasm32-wasip1`, but agentOS provides real PTY,
//! process, descriptor, terminal-size, wait, and signal operations. This module
//! only exposes those existing operations through the `portable-pty` traits.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Result as IoResult, Write};
use std::os::fd::FromRawFd;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Error};

use crate::{Child, ChildKiller, ExitStatus, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};

const MAX_ITEMS: usize = 4_096;
const MAX_SERIALIZED_BYTES: usize = 1024 * 1024;
const WNOHANG: u32 = 1;
const SIGHUP: u32 = 1;

#[link(wasm_import_module = "host_tty")]
extern "C" {
    #[link_name = "get_size"]
    fn host_tty_get_size(fd: u32, cols: *mut u16, rows: *mut u16) -> u32;
    #[link_name = "set_size"]
    fn host_tty_set_size(fd: u32, cols: u16, rows: u16) -> u32;
}

fn errno_error(operation: &str, errno: wasi_ext::Errno) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("{operation} failed with WASI errno {errno}"),
    )
}

fn close_fd(fd: u32) {
    // SAFETY: each caller transfers one owned descriptor into this File.
    drop(unsafe { File::from_raw_fd(fd as i32) });
}

fn dup_file(fd: u32) -> IoResult<File> {
    let duplicate = wasi_ext::dup(fd).map_err(|errno| errno_error("dup", errno))?;
    // SAFETY: dup returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(duplicate as i32) })
}

fn set_size(fd: u32, size: PtySize) -> IoResult<()> {
    // SAFETY: host_tty is the agentOS kernel PTY bridge and accepts scalar values.
    let errno = unsafe { host_tty_set_size(fd, size.cols, size.rows) };
    if errno == 0 {
        Ok(())
    } else {
        Err(errno_error("set PTY size", errno))
    }
}

fn get_size(fd: u32) -> IoResult<PtySize> {
    let mut cols = 0;
    let mut rows = 0;
    // SAFETY: cols and rows are valid writable u16 pointers for the duration of the call.
    let errno = unsafe { host_tty_get_size(fd, &mut cols, &mut rows) };
    if errno != 0 {
        return Err(errno_error("get PTY size", errno));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn push_item(buffer: &mut Vec<u8>, item: &[u8], label: &str) -> anyhow::Result<()> {
    if item.contains(&0) {
        bail!("{label} contains NUL");
    }
    let separator = usize::from(!buffer.is_empty());
    let next_len = buffer
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(item.len()))
        .ok_or_else(|| anyhow!("serialized {label} length overflowed"))?;
    if next_len > MAX_SERIALIZED_BYTES {
        bail!("serialized process data exceeds {MAX_SERIALIZED_BYTES} bytes");
    }
    if separator != 0 {
        buffer.push(0);
    }
    buffer.extend_from_slice(item);
    Ok(())
}

fn os_bytes(value: &OsStr) -> &[u8] {
    value.as_encoded_bytes()
}

#[derive(Default)]
pub struct WasiPtySystem;

impl PtySystem for WasiPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let (master_fd, slave_fd) =
            wasi_ext::openpty().map_err(|errno| errno_error("openpty", errno))?;
        if let Err(error) = set_size(master_fd, size) {
            close_fd(slave_fd);
            close_fd(master_fd);
            return Err(error.into());
        }
        Ok(PtyPair {
            slave: Box::new(WasiSlave { fd: slave_fd }),
            master: Box::new(WasiMaster { fd: master_fd }),
        })
    }
}

struct WasiMaster {
    fd: u32,
}

impl Drop for WasiMaster {
    fn drop(&mut self) {
        close_fd(self.fd);
    }
}

impl MasterPty for WasiMaster {
    fn resize(&self, size: PtySize) -> Result<(), Error> {
        set_size(self.fd, size).context("failed to resize agentOS PTY")
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        get_size(self.fd).context("failed to read agentOS PTY size")
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, Error> {
        Ok(Box::new(dup_file(self.fd)?))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, Error> {
        Ok(Box::new(dup_file(self.fd)?))
    }
}

struct WasiSlave {
    fd: u32,
}

impl Drop for WasiSlave {
    fn drop(&mut self) {
        close_fd(self.fd);
    }
}

impl SlavePty for WasiSlave {
    fn spawn_command(
        &self,
        command: crate::CommandBuilder,
    ) -> Result<Box<dyn Child + Send + Sync>, Error> {
        let default_shell = OsStr::new("/bin/sh");
        let argv: Vec<&OsStr> = if command.is_default_prog() {
            vec![default_shell]
        } else {
            command
                .get_argv()
                .iter()
                .map(|value| value.as_os_str())
                .collect()
        };
        if argv.is_empty() || argv.len() > MAX_ITEMS {
            bail!("process argument count must be between 1 and {MAX_ITEMS}");
        }

        let mut argv_bytes = Vec::new();
        for argument in &argv {
            push_item(&mut argv_bytes, os_bytes(argument), "argument")?;
        }

        let environment: Vec<_> = command.iter_full_env().collect();
        if environment.len() > MAX_ITEMS {
            bail!("process environment count exceeds {MAX_ITEMS}");
        }
        let mut env_bytes = Vec::new();
        for (key, value) in environment {
            if os_bytes(key).is_empty() || os_bytes(key).contains(&b'=') {
                bail!("invalid process environment key");
            }
            let mut entry = Vec::with_capacity(os_bytes(key).len() + os_bytes(value).len() + 1);
            entry.extend_from_slice(os_bytes(key));
            entry.push(b'=');
            entry.extend_from_slice(os_bytes(value));
            push_item(&mut env_bytes, &entry, "environment entry")?;
        }

        let cwd = match command.get_cwd() {
            Some(cwd) => os_bytes(cwd.as_os_str()).to_vec(),
            None => std::env::current_dir()
                .context("failed to resolve process working directory")?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        };
        if cwd.contains(&0) {
            bail!("process working directory contains NUL");
        }

        let pid = wasi_ext::spawn(
            os_bytes(argv[0]),
            &argv_bytes,
            &env_bytes,
            self.fd,
            self.fd,
            self.fd,
            &cwd,
        )
        .map_err(|errno| errno_error("PTY process spawn", errno))?;

        Ok(Box::new(WasiChild {
            state: Arc::new(WasiChildState { pid }),
            reaped: false,
        }))
    }
}

#[derive(Debug)]
struct WasiChildState {
    pid: u32,
}

#[derive(Debug)]
struct WasiChild {
    state: Arc<WasiChildState>,
    reaped: bool,
}

impl WasiChild {
    fn status(&mut self, options: u32) -> IoResult<Option<ExitStatus>> {
        if self.reaped {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "PTY child has already been reaped",
            ));
        }
        let status = wasi_ext::waitpid(self.state.pid, options)
            .map_err(|errno| errno_error("waitpid", errno))?;
        if status.pid == 0 {
            return Ok(None);
        }
        self.reaped = true;
        if status.signal == 0 {
            Ok(Some(ExitStatus::with_exit_code(status.exit_code)))
        } else {
            Ok(Some(ExitStatus::with_signal(&format!(
                "signal {}",
                status.signal
            ))))
        }
    }
}

impl Child for WasiChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.status(WNOHANG)
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        self.status(0)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "blocking wait returned before the PTY child exited",
            )
        })
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.state.pid)
    }
}

impl ChildKiller for WasiChild {
    fn kill(&mut self) -> IoResult<()> {
        wasi_ext::kill(self.state.pid, SIGHUP).map_err(|errno| errno_error("kill", errno))
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self {
            state: Arc::clone(&self.state),
            reaped: false,
        })
    }
}
