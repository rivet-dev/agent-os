// AgentOS exposes a Linux system identity through a runtime-neutral host ABI.

#![warn(unused_results)]

use std::ffi::{OsStr, OsString};
use std::io;

use crate::{PlatformInfoAPI, PlatformInfoError, UNameAPI};

const IDENTITY_BUFFER_BYTES: usize = 256;

#[link(wasm_import_module = "host_system")]
unsafe extern "C" {
    fn get_identity(field: u32, buffer: *mut u8, capacity: u32) -> u32;
}

fn identity(field: u32) -> Result<OsString, PlatformInfoError> {
    let mut buffer = [0u8; IDENTITY_BUFFER_BYTES];
    let errno = unsafe { get_identity(field, buffer.as_mut_ptr(), buffer.len() as u32) };
    if errno != 0 {
        return Err(io::Error::from_raw_os_error(errno as i32).into());
    }
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unterminated system identity"))?;
    let value = std::str::from_utf8(&buffer[..length])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(OsString::from(value))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformInfo {
    sysname: OsString,
    nodename: OsString,
    release: OsString,
    version: OsString,
    machine: OsString,
    osname: OsString,
}

impl PlatformInfoAPI for PlatformInfo {
    fn new() -> Result<Self, PlatformInfoError> {
        Ok(Self {
            nodename: identity(0)?,
            sysname: identity(1)?,
            release: identity(2)?,
            version: identity(3)?,
            machine: identity(4)?,
            osname: OsString::from("GNU/Linux"),
        })
    }
}

impl UNameAPI for PlatformInfo {
    fn sysname(&self) -> &OsStr {
        &self.sysname
    }

    fn nodename(&self) -> &OsStr {
        &self.nodename
    }

    fn release(&self) -> &OsStr {
        &self.release
    }

    fn version(&self) -> &OsStr {
        &self.version
    }

    fn machine(&self) -> &OsStr {
        &self.machine
    }

    fn osname(&self) -> &OsStr {
        &self.osname
    }
}
