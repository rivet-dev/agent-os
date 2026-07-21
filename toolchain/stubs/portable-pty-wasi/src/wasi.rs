//! wasm32-wasip1 backend stub for portable-pty.
//!
//! The agentos VM brokers process execution via wasi-spawn/wasi-pty; a full
//! PTY bridge is future work. This backend lets portable-pty (and thus
//! codex-utils-pty / codex-core) COMPILE for wasip1. PTY operations return an
//! Unsupported error at runtime (codex's exec falls back to non-PTY execution).
use std::io::{Read, Result as IoResult, Write};

use anyhow::{bail, Error};

use crate::{Child, ChildKiller, ExitStatus, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};

#[derive(Default)]
pub struct WasiPtySystem;

impl PtySystem for WasiPtySystem {
    fn openpty(&self, _size: PtySize) -> anyhow::Result<PtyPair> {
        Ok(PtyPair {
            slave: Box::new(WasiSlave),
            master: Box::new(WasiMaster),
        })
    }
}

struct WasiMaster;

impl MasterPty for WasiMaster {
    fn resize(&self, _size: PtySize) -> Result<(), Error> {
        Ok(())
    }
    fn get_size(&self) -> Result<PtySize, Error> {
        Ok(PtySize::default())
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, Error> {
        bail!("PTY reader unsupported on wasm32-wasip1")
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send>, Error> {
        bail!("PTY writer unsupported on wasm32-wasip1")
    }
}

struct WasiSlave;

impl SlavePty for WasiSlave {
    fn spawn_command(
        &self,
        _cmd: crate::CommandBuilder,
    ) -> Result<Box<dyn Child + Send + Sync>, Error> {
        bail!("PTY spawn unsupported on wasm32-wasip1 (use non-PTY exec)")
    }
}

#[derive(Debug)]
pub struct WasiChild;

impl Child for WasiChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        Ok(None)
    }
    fn wait(&mut self) -> IoResult<ExitStatus> {
        Ok(ExitStatus::with_exit_code(1))
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
}

impl ChildKiller for WasiChild {
    fn kill(&mut self) -> IoResult<()> {
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(WasiChild)
    }
}
