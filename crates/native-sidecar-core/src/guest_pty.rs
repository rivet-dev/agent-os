//! Guest PTY kernel-call dispatch.
//!
//! Pseudo-terminal syscalls flow through the same generic synchronous wire
//! payload as guest networking (`GuestKernelCallRequest` ->
//! `GuestKernelResultResponse`) carrying a `pty.*` `operation` string plus a
//! JSON `payload`. This module is the backend-agnostic dispatcher that decodes a
//! `pty.*` operation and routes it into the kernel's `PtyManager`, exactly as
//! [`crate::guest_net`] does for the `net.*`/`dns.*` family. It is delegated to
//! from `handle_guest_kernel_call` and unit-tested without an executor.
//!
//! The PTY is inherently fd/stream based, so unlike the path-based filesystem
//! family the read/write operations carry a kernel fd and route through the
//! kernel's generic by-fd read/write (which already dispatches pty fds through
//! the line discipline). `pty.open` allocates a master/slave pair; the host
//! drives the master while a guest's std streams bind to the slave.

use crate::SidecarCoreError;
use agentos_kernel::kernel::{KernelError, KernelVm};
use agentos_kernel::pty::{PartialTermios, PartialTermiosControlChars};
use agentos_kernel::vfs::VirtualFileSystem;
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_READ_MAX_BYTES: usize = 64 * 1024;
/// Guest pty reads run on the sidecar's synchronous request path, so a blocking
/// read is clamped to a small ceiling: the guest re-reads rather than parking
/// the sidecar (mirrors the `net.poll` 50 ms ceiling in [`crate::guest_net`]).
const MAX_PTY_READ_WAIT_MS: u64 = 50;

/// True when `operation` names a `pty.*` guest kernel call.
pub fn is_pty_operation(operation: &str) -> bool {
    operation.starts_with("pty.")
}

/// Dispatch a single `pty.*` guest kernel call into the kernel. `request` is the
/// already-decoded JSON request body; the returned `Value` is the JSON response
/// body the guest sync-bridge consumes.
pub fn dispatch_pty_operation<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    match operation {
        "pty.open" => pty_open(kernel, pid, driver),
        "pty.read" => pty_read(kernel, pid, driver, request),
        "pty.write" => pty_write(kernel, pid, driver, request),
        "pty.close" => pty_close(kernel, pid, driver, request),
        "pty.resize" => pty_resize(kernel, pid, driver, request),
        "pty.setForegroundPgid" => pty_set_foreground_pgid(kernel, pid, driver, request),
        "pty.tcgetattr" => pty_tcgetattr(kernel, pid, driver, request),
        "pty.tcsetattr" => pty_tcsetattr(kernel, pid, driver, request),
        other => Err(SidecarCoreError::new(format!(
            "unsupported guest pty operation: {other}"
        ))),
    }
}

fn pty_open<F>(kernel: &mut KernelVm<F>, pid: u32, driver: &str) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let (master_fd, slave_fd, path) = kernel.open_pty(driver, pid).map_err(kernel_error)?;
    Ok(json!({ "masterFd": master_fd, "slaveFd": slave_fd, "path": path }))
}

fn pty_read<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let max_bytes = optional_u64(request, "maxBytes")
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_READ_MAX_BYTES);
    let wait_ms = optional_u64(request, "timeoutMs")
        .unwrap_or(MAX_PTY_READ_WAIT_MS)
        .min(MAX_PTY_READ_WAIT_MS);
    let timeout = Some(Duration::from_millis(wait_ms));
    match kernel.fd_read_with_timeout_result(driver, pid, fd, max_bytes, timeout) {
        Ok(Some(bytes)) => Ok(json!({ "data": encode_data(&bytes) })),
        // No data within the (short) timeout. A zero/short blocking read with an empty
        // line discipline buffer surfaces as EAGAIN; the guest polls again, so this is
        // a no-data result (`data: null`), not an error — mirrors guest_net.net_read.
        Ok(None) => Ok(json!({ "data": Value::Null })),
        Err(error) if would_block(&error) => Ok(json!({ "data": Value::Null })),
        Err(error) => Err(kernel_error(error)),
    }
}

fn pty_write<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let data = decode_data(request)?;
    let written = kernel
        .fd_write(driver, pid, fd, &data)
        .map_err(kernel_error)?;
    Ok(json!({ "written": written }))
}

fn pty_close<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    kernel.fd_close(driver, pid, fd).map_err(kernel_error)?;
    Ok(json!({}))
}

fn pty_resize<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let cols = require_u16(request, "cols")?;
    let rows = require_u16(request, "rows")?;
    kernel
        .pty_resize(driver, pid, fd, cols, rows)
        .map_err(kernel_error)?;
    Ok(json!({}))
}

fn pty_set_foreground_pgid<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let pgid = optional_u32(request, "pgid")?.unwrap_or(pid);
    if pgid == pid {
        kernel.setpgid(driver, pid, pgid).map_err(kernel_error)?;
    }
    kernel
        .pty_set_foreground_pgid(driver, pid, fd, pgid)
        .map_err(kernel_error)?;
    Ok(json!({}))
}

fn pty_tcgetattr<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let termios = kernel.tcgetattr(driver, pid, fd).map_err(kernel_error)?;
    Ok(json!({
        "icrnl": termios.icrnl,
        "opost": termios.opost,
        "onlcr": termios.onlcr,
        "icanon": termios.icanon,
        "echo": termios.echo,
        "isig": termios.isig,
        "cc": {
            "vintr": termios.cc.vintr,
            "vquit": termios.cc.vquit,
            "vsusp": termios.cc.vsusp,
            "veof": termios.cc.veof,
            "verase": termios.cc.verase,
            "vkill": termios.cc.vkill,
            "vwerase": termios.cc.vwerase,
        },
    }))
}

fn pty_tcsetattr<F>(
    kernel: &mut KernelVm<F>,
    pid: u32,
    driver: &str,
    request: &Value,
) -> Result<Value, SidecarCoreError>
where
    F: VirtualFileSystem + 'static,
{
    let fd = require_fd(request)?;
    let partial = parse_partial_termios(request);
    kernel
        .tcsetattr(driver, pid, fd, partial)
        .map_err(kernel_error)?;
    Ok(json!({}))
}

/// A `pty.tcsetattr` request supplies only the termios fields it wants changed;
/// every field is optional so the guest can flip raw mode (icanon/echo/opost)
/// without restating the rest.
fn parse_partial_termios(request: &Value) -> PartialTermios {
    let cc = request.get("cc").map(|cc| PartialTermiosControlChars {
        vintr: optional_u8(cc, "vintr"),
        vquit: optional_u8(cc, "vquit"),
        vsusp: optional_u8(cc, "vsusp"),
        veof: optional_u8(cc, "veof"),
        verase: optional_u8(cc, "verase"),
        vkill: optional_u8(cc, "vkill"),
        vwerase: optional_u8(cc, "vwerase"),
    });
    PartialTermios {
        icrnl: optional_bool(request, "icrnl"),
        opost: optional_bool(request, "opost"),
        onlcr: optional_bool(request, "onlcr"),
        icanon: optional_bool(request, "icanon"),
        echo: optional_bool(request, "echo"),
        isig: optional_bool(request, "isig"),
        cc,
    }
}

fn require_fd(request: &Value) -> Result<u32, SidecarCoreError> {
    let fd = optional_u64(request, "fd")
        .ok_or_else(|| SidecarCoreError::new("guest pty call requires numeric field `fd`"))?;
    u32::try_from(fd)
        .map_err(|_| SidecarCoreError::new("guest pty call field `fd` must be a valid descriptor"))
}

fn require_u16(request: &Value, key: &str) -> Result<u16, SidecarCoreError> {
    let value = optional_u64(request, key).ok_or_else(|| {
        SidecarCoreError::new(format!("guest pty call requires numeric field `{key}`"))
    })?;
    u16::try_from(value)
        .map_err(|_| SidecarCoreError::new(format!("guest pty call field `{key}` is out of range")))
}

fn optional_u32(request: &Value, key: &str) -> Result<Option<u32>, SidecarCoreError> {
    optional_u64(request, key)
        .map(u32::try_from)
        .transpose()
        .map_err(|_| SidecarCoreError::new(format!("guest pty call field `{key}` is out of range")))
}

fn optional_u64(request: &Value, key: &str) -> Option<u64> {
    request.get(key).and_then(Value::as_u64)
}

fn optional_bool(request: &Value, key: &str) -> Option<bool> {
    request.get(key).and_then(Value::as_bool)
}

fn optional_u8(request: &Value, key: &str) -> Option<u8> {
    optional_u64(request, key).and_then(|value| u8::try_from(value).ok())
}

fn decode_data(request: &Value) -> Result<Vec<u8>, SidecarCoreError> {
    let data = request
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarCoreError::new("guest pty call requires string field `data`"))?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| SidecarCoreError::new(format!("invalid base64 guest pty data: {error}")))
}

fn encode_data(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn would_block(error: &KernelError) -> bool {
    matches!(error.code(), "EAGAIN" | "EWOULDBLOCK")
}

fn kernel_error(error: KernelError) -> SidecarCoreError {
    SidecarCoreError::typed(error.code(), error.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_net::handle_guest_kernel_call;
    use agentos_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_kernel() -> KernelVm<MemoryFileSystem> {
        let mut config = KernelVmConfig::new("guest-pty-test");
        config.permissions = Permissions::allow_all();
        KernelVm::new(MemoryFileSystem::new(), config)
    }

    fn guest_pid(kernel: &mut KernelVm<MemoryFileSystem>) -> u32 {
        kernel
            .create_virtual_process(
                "shell",
                "shell",
                "sh",
                Vec::new(),
                VirtualProcessOptions::default(),
            )
            .expect("spawn guest process")
            .pid()
    }

    /// Drive a `pty.*` call through the same public entry the wire dispatcher uses,
    /// so the test covers the `handle_guest_kernel_call` -> `dispatch_pty_operation`
    /// delegation, not just the inner functions.
    fn call(
        kernel: &mut KernelVm<MemoryFileSystem>,
        pid: u32,
        operation: &str,
        request: Value,
    ) -> Value {
        let payload = serde_json::to_vec(&request).expect("encode request");
        let bytes =
            handle_guest_kernel_call(kernel, pid, "shell", operation, &payload).expect("dispatch");
        serde_json::from_slice(&bytes).expect("decode response")
    }

    fn read_until_data(kernel: &mut KernelVm<MemoryFileSystem>, pid: u32, fd: u64) -> Vec<u8> {
        for _ in 0..200 {
            let response = call(kernel, pid, "pty.read", json!({ "fd": fd }));
            if let Some(data) = response["data"].as_str() {
                return base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("decode pty read");
            }
        }
        panic!("pty.read timed out");
    }

    #[test]
    fn loopback_pty_round_trip_through_kernel_line_discipline() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);

        let pair = call(&mut kernel, pid, "pty.open", json!({}));
        let master = pair["masterFd"].as_u64().expect("master fd");
        let slave = pair["slaveFd"].as_u64().expect("slave fd");
        assert!(pair["path"]
            .as_str()
            .expect("pty path")
            .starts_with("/dev/"));

        // Raw mode so the loopback carries exactly what is written.
        call(
            &mut kernel,
            pid,
            "pty.tcsetattr",
            json!({ "fd": slave, "icanon": false, "echo": false, "opost": false, "isig": false }),
        );

        let input = base64::engine::general_purpose::STANDARD.encode(b"ping-pty");
        call(
            &mut kernel,
            pid,
            "pty.write",
            json!({ "fd": master, "data": input }),
        );
        assert_eq!(read_until_data(&mut kernel, pid, slave), b"ping-pty");

        let reply = base64::engine::general_purpose::STANDARD.encode(b"ECHO:ping-pty");
        call(
            &mut kernel,
            pid,
            "pty.write",
            json!({ "fd": slave, "data": reply }),
        );
        assert_eq!(read_until_data(&mut kernel, pid, master), b"ECHO:ping-pty");

        call(&mut kernel, pid, "pty.close", json!({ "fd": slave }));
        call(&mut kernel, pid, "pty.close", json!({ "fd": master }));
    }

    #[test]
    fn tcsetattr_then_tcgetattr_reflects_raw_mode() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let pair = call(&mut kernel, pid, "pty.open", json!({}));
        let slave = pair["slaveFd"].as_u64().expect("slave fd");

        // Default is canonical + echo.
        let before = call(&mut kernel, pid, "pty.tcgetattr", json!({ "fd": slave }));
        assert_eq!(before["icanon"].as_bool(), Some(true));
        assert_eq!(before["echo"].as_bool(), Some(true));

        call(
            &mut kernel,
            pid,
            "pty.tcsetattr",
            json!({ "fd": slave, "icanon": false, "echo": false }),
        );
        let after = call(&mut kernel, pid, "pty.tcgetattr", json!({ "fd": slave }));
        assert_eq!(after["icanon"].as_bool(), Some(false));
        assert_eq!(after["echo"].as_bool(), Some(false));
        // isig was not in the partial update, so it is untouched.
        assert_eq!(after["isig"].as_bool(), Some(true));
    }

    #[test]
    fn empty_pty_read_is_null_not_error() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let pair = call(&mut kernel, pid, "pty.open", json!({}));
        let master = pair["masterFd"].as_u64().expect("master fd");

        // Nothing has been written, so a zero-timeout poll must report no data rather
        // than surfacing EAGAIN as a dispatch error (the interactive shell polls).
        let response = call(
            &mut kernel,
            pid,
            "pty.read",
            json!({ "fd": master, "timeoutMs": 0 }),
        );
        assert!(response["data"].is_null());
    }

    #[test]
    fn set_foreground_pgid_defaults_to_active_guest_process_group() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let pair = call(&mut kernel, pid, "pty.open", json!({}));
        let master = pair["masterFd"].as_u64().expect("master fd");

        call(
            &mut kernel,
            pid,
            "pty.setForegroundPgid",
            json!({ "fd": master }),
        );

        assert_eq!(kernel.getpgid("shell", pid).expect("pgid"), pid);
        assert_eq!(
            kernel
                .tcgetpgrp("shell", pid, master as u32)
                .expect("foreground pgid"),
            pid
        );
    }

    #[test]
    fn unsupported_pty_operation_is_rejected() {
        let mut kernel = test_kernel();
        let pid = guest_pid(&mut kernel);
        let payload = serde_json::to_vec(&json!({})).unwrap();
        let error = handle_guest_kernel_call(&mut kernel, pid, "shell", "pty.bogus", &payload)
            .expect_err("unknown pty op rejected");
        assert!(error.to_string().contains("pty.bogus"));
    }
}
