use super::*;

pub(super) fn wait_fd_readable_until(fd: BorrowedFd<'_>, deadline: Instant) -> bool {
    wait_fd_until(fd, deadline, PollFlags::POLLIN)
}

fn wait_fd_writable_until(fd: BorrowedFd<'_>, deadline: Instant) -> bool {
    wait_fd_until(fd, deadline, PollFlags::POLLOUT)
}

fn wait_fd_until(fd: BorrowedFd<'_>, deadline: Instant, interest: PollFlags) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }

    let timeout_ms = remaining.as_millis().saturating_add(u128::from(
        !remaining.subsec_nanos().is_multiple_of(1_000_000),
    ));
    let timeout =
        PollTimeout::try_from(timeout_ms.min(i32::MAX as u128)).unwrap_or(PollTimeout::MAX);
    let mut fds = [NixPollFd::new(fd, interest)];
    match poll(&mut fds, timeout) {
        Ok(0) => false,
        Ok(_) => fds[0]
            .revents()
            .unwrap_or_else(PollFlags::empty)
            .intersects(interest | PollFlags::POLLHUP | PollFlags::POLLERR),
        Err(_) => true,
    }
}

fn socket_write_deadline_error(limit: Duration) -> SidecarError {
    sidecar_net_error(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "ERR_AGENTOS_OPERATION_DEADLINE: socket write exceeded {}ms; raise limits.reactor.operationDeadlineMs",
            limit.as_millis()
        ),
    ))
}

pub(super) fn write_all_nonblocking<S>(
    stream: &mut S,
    contents: &[u8],
    limits: ReactorIoLimits,
) -> Result<(), SidecarError>
where
    S: Write + AsFd,
{
    let mut deadline = OperationDeadlineTracker::new(limits.operation_deadline);
    let mut remaining = contents;
    let mut operations = 0;
    while !remaining.is_empty() {
        deadline.observe("synchronous socket write");
        if deadline.expired() {
            return Err(socket_write_deadline_error(limits.operation_deadline));
        }
        if operations >= limits.operation_quantum.max(1) {
            std::thread::yield_now();
            operations = 0;
        }
        let chunk_len = remaining.len().min(limits.byte_quantum.max(1));
        match stream.write(&remaining[..chunk_len]) {
            Ok(0) => {
                return Err(sidecar_net_error(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "socket write returned zero bytes",
                )))
            }
            Ok(written) => {
                remaining = &remaining[written..];
                operations += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                deadline.observe("synchronous socket write");
                if !wait_fd_writable_until(stream.as_fd(), deadline.next_edge()) {
                    deadline.observe("synchronous socket write");
                    if !deadline.expired() {
                        continue;
                    }
                    return Err(socket_write_deadline_error(limits.operation_deadline));
                }
            }
            Err(error) => return Err(sidecar_net_error(error)),
        }
    }
    Ok(())
}

pub(super) fn service_javascript_kernel_stdin_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let (max_bytes, timeout_ms) = parse_kernel_stdin_read_args(request)?;
    let timeout_ms = timeout_ms.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "an indefinite __kernel_stdin_read must use the deferred readiness path",
        ))
    })?;
    typed_kernel_stdin_read(kernel, process, max_bytes, timeout_ms)
}

pub(super) fn typed_kernel_stdin_read(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    max_bytes: usize,
    timeout_ms: u64,
) -> Result<Value, SidecarError> {
    // The sidecar writer is nonblocking, so input larger than the kernel pipe
    // remains in a bounded owner-side backlog. Every compatibility read must
    // refill that pipe before probing it; otherwise the guest stalls after the
    // first pipe-capacity chunk even though writeStdin already accepted more.
    flush_pending_kernel_stdin(kernel, process)?;
    kernel_stdin_read_response(
        kernel,
        process.kernel_pid,
        process.kernel_stdin_reader_fd,
        max_bytes,
        Duration::from_millis(timeout_ms),
    )
}

/// Parse `__kernel_stdin_read` args: (max bytes, requested timeout ms).
pub(crate) fn parse_kernel_stdin_read_args(
    request: &HostRpcRequest,
) -> Result<(usize, Option<u64>), SidecarError> {
    let max_bytes =
        javascript_sync_rpc_arg_u64_optional(&request.args, 0, "__kernel_stdin_read max bytes")?
            .map(|value| value.clamp(1, DEFAULT_KERNEL_STDIN_READ_MAX_BYTES as u64) as usize)
            .unwrap_or(DEFAULT_KERNEL_STDIN_READ_MAX_BYTES);
    // Explicit null means "wait for readiness without a recurring timeout".
    // Omitting the argument preserves the bounded compatibility default.
    let timeout_ms = if request.args.get(1).is_some_and(Value::is_null) {
        None
    } else {
        Some(
            javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "__kernel_stdin_read timeout ms",
            )?
            .unwrap_or(DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS),
        )
    };
    Ok((max_bytes, timeout_ms))
}

/// One bounded stdin read against the kernel. `Duration::ZERO` = non-blocking
/// probe (deferred servicing re-checks readiness with this before replying).
pub(crate) fn kernel_stdin_read_response(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    kernel_fd: u32,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Value, SidecarError> {
    match kernel
        .fd_read_with_timeout_result(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            kernel_fd,
            max_bytes,
            Some(timeout),
        )
        .map_err(kernel_error)
    {
        Ok(Some(chunk)) if !chunk.is_empty() => Ok(json!({
            "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk),
        })),
        Ok(Some(_)) => Ok(Value::Null),
        Ok(None) => Ok(json!({
            "done": true,
        })),
        Err(error) if guest_error_code(&error) == Some("EAGAIN") => Ok(Value::Null),
        Err(error) => Err(error),
    }
}

pub(super) fn service_javascript_pty_set_raw_mode_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let enabled = javascript_sync_rpc_arg_bool(&request.args, 0, "__pty_set_raw_mode enabled")?;
    process.tty_raw_mode_generation = kernel
        .pty_set_raw_mode(EXECUTION_DRIVER_NAME, process.kernel_pid, 0, enabled)
        .map_err(kernel_error)?;
    Ok(Value::Null)
}

/// Release the generation-scoped raw-mode lease held by an exiting process.
///
/// A child that inherited a terminal usually no longer owns the host-facing
/// master descriptor. In that case `tty_master_owner` identifies a descriptor
/// for the same PTY that remains valid while the child is being reaped. The
/// generation prevents delayed cleanup from overwriting a newer terminal mode.
pub(super) fn release_inherited_child_raw_mode(
    kernel: &mut SidecarKernel,
    child: &ActiveProcess,
) -> Result<(), SidecarError> {
    let Some(generation) = child.tty_raw_mode_generation else {
        return Ok(());
    };
    let (descriptor_owner_pid, fd) = child.tty_master_owner.unwrap_or((child.kernel_pid, 0));
    kernel
        .pty_release_raw_mode(
            EXECUTION_DRIVER_NAME,
            descriptor_owner_pid,
            fd,
            child.kernel_pid,
            generation,
        )
        .map(|_| ())
        .map_err(kernel_error)
}

pub(super) fn service_javascript_kernel_isatty_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_isatty fd")?;
    let is_tty = kernel
        .isatty(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map_err(kernel_error)?;
    Ok(json!(is_tty))
}

pub(super) fn service_javascript_kernel_tty_size_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tty_size fd")?;
    let size = kernel
        .pty_window_size(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map_err(kernel_error)?;
    Ok(json!({
        "cols": size.cols,
        "rows": size.rows,
    }))
}

pub(super) fn service_javascript_kernel_tty_set_size_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tty_set_size fd")?;
    let cols = javascript_sync_rpc_arg_u32(&request.args, 1, "__kernel_tty_set_size cols")?;
    let rows = javascript_sync_rpc_arg_u32(&request.args, 2, "__kernel_tty_set_size rows")?;
    let cols = u16::try_from(cols).map_err(|_| {
        SidecarError::Host(HostServiceError::new("EINVAL", "TTY columns exceed u16"))
    })?;
    let rows = u16::try_from(rows)
        .map_err(|_| SidecarError::Host(HostServiceError::new("EINVAL", "TTY rows exceed u16")))?;
    kernel
        .pty_resize(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, cols, rows)
        .map_err(kernel_error)?;
    Ok(Value::Null)
}

const TTY_IFLAG_ICRNL: u32 = 1 << 0;
const TTY_OFLAG_OPOST: u32 = 1 << 1;
const TTY_OFLAG_ONLCR: u32 = 1 << 2;
const TTY_LFLAG_ICANON: u32 = 1 << 3;
const TTY_LFLAG_ECHO: u32 = 1 << 4;
const TTY_LFLAG_ISIG: u32 = 1 << 5;

pub(super) fn service_javascript_kernel_tcgetattr_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetattr fd")?;
    let termios = kernel
        .tcgetattr(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map_err(kernel_error)?;
    let mut flags = 0_u32;
    if termios.icrnl {
        flags |= TTY_IFLAG_ICRNL;
    }
    if termios.opost {
        flags |= TTY_OFLAG_OPOST;
    }
    if termios.onlcr {
        flags |= TTY_OFLAG_ONLCR;
    }
    if termios.icanon {
        flags |= TTY_LFLAG_ICANON;
    }
    if termios.echo {
        flags |= TTY_LFLAG_ECHO;
    }
    if termios.isig {
        flags |= TTY_LFLAG_ISIG;
    }
    Ok(json!({
        "flags": flags,
        "cc": [
            termios.cc.vintr,
            termios.cc.vquit,
            termios.cc.vsusp,
            termios.cc.veof,
            termios.cc.verase,
            termios.cc.vkill,
            termios.cc.vwerase,
        ],
    }))
}

pub(super) fn service_javascript_kernel_tcsetattr_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcsetattr fd")?;
    let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "__kernel_tcsetattr flags")?;
    let cc = request
        .args
        .get(2)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            SidecarError::Host(HostServiceError::new(
                "EINVAL",
                "TTY control characters must be an array",
            ))
        })?;
    if cc.len() != 7 {
        return Err(SidecarError::Host(HostServiceError::new(
            "EINVAL",
            format!(
                "TTY control character array must contain 7 bytes, observed {}",
                cc.len()
            ),
        )));
    }
    let mut parsed = [0_u8; 7];
    for (index, value) in cc.iter().enumerate() {
        let value = value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| {
                SidecarError::Host(HostServiceError::new(
                    "EINVAL",
                    "TTY control character must be a byte",
                ))
            })?;
        parsed[index] = value;
    }
    kernel
        .tcsetattr(
            EXECUTION_DRIVER_NAME,
            process.kernel_pid,
            fd,
            agentos_kernel::pty::PartialTermios {
                icrnl: Some(flags & TTY_IFLAG_ICRNL != 0),
                opost: Some(flags & TTY_OFLAG_OPOST != 0),
                onlcr: Some(flags & TTY_OFLAG_ONLCR != 0),
                icanon: Some(flags & TTY_LFLAG_ICANON != 0),
                echo: Some(flags & TTY_LFLAG_ECHO != 0),
                isig: Some(flags & TTY_LFLAG_ISIG != 0),
                cc: Some(agentos_kernel::pty::PartialTermiosControlChars {
                    vintr: Some(parsed[0]),
                    vquit: Some(parsed[1]),
                    vsusp: Some(parsed[2]),
                    veof: Some(parsed[3]),
                    verase: Some(parsed[4]),
                    vkill: Some(parsed[5]),
                    vwerase: Some(parsed[6]),
                }),
            },
        )
        .map_err(kernel_error)?;
    Ok(Value::Null)
}

pub(super) fn service_javascript_kernel_tcgetpgrp_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetpgrp fd")?;
    kernel
        .tcgetpgrp(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map(Value::from)
        .map_err(kernel_error)
}

pub(super) fn service_javascript_kernel_tcsetpgrp_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcsetpgrp fd")?;
    let pgid = javascript_sync_rpc_arg_u32(&request.args, 1, "__kernel_tcsetpgrp pgid")?;
    kernel
        .pty_set_foreground_pgid(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, pgid)
        .map(|()| Value::Null)
        .map_err(kernel_error)
}

pub(super) fn service_javascript_kernel_tcgetsid_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetsid fd")?;
    kernel
        .tcgetsid(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map(Value::from)
        .map_err(kernel_error)
}

/// A TTY in raw mode (no echo, no canonical) — like cfmakeraw. Full-screen apps
/// (vim) run raw and drive their own cursor/CRLF, so their output must be passed
/// through untouched, NOT round-tripped through the slave->process_output->master
/// path (which buffers/reorders escape sequences and corrupts the screen).
fn tty_is_raw_mode(kernel: &SidecarKernel, process: &ActiveProcess) -> bool {
    let Some(master_fd) = process.tty_master_fd else {
        return false;
    };
    match kernel.tcgetattr(EXECUTION_DRIVER_NAME, process.kernel_pid, master_fd) {
        Ok(termios) => !termios.echo && !termios.icanon,
        Err(_) => false,
    }
}

/// Non-blocking drain of the PTY master output buffer for a TTY process.
///
/// For a TTY (PTY-backed) process the master output buffer is the single
/// ordered output stream: it carries cooked-mode echo plus ONLCR-processed
/// guest output, already merged FIFO. A zero-timeout master read returns the
/// whole current buffer (so echo and guest output stay grouped) or EAGAIN when
/// empty, which is mapped to `Ok(None)`. Returns `Ok(None)` for non-TTY
/// processes (no master fd).
pub(crate) fn drain_tty_master_output(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<Option<Vec<u8>>, SidecarError> {
    let Some(master_fd) = process.tty_master_fd else {
        return Ok(None);
    };
    match kernel.fd_read_with_timeout_result(
        EXECUTION_DRIVER_NAME,
        process.kernel_pid,
        master_fd,
        MAX_PTY_BUFFER_BYTES,
        Some(Duration::ZERO),
    ) {
        Ok(Some(bytes)) if !bytes.is_empty() => Ok(Some(bytes)),
        Ok(_) => Ok(None),
        Err(error) if error.code() == "EAGAIN" => Ok(None),
        Err(error) => Err(kernel_error(error)),
    }
}

pub(super) fn service_javascript_kernel_stdio_write_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_stdio_write fd")?;
    let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "__kernel_stdio_write chunk")?;
    typed_kernel_stdio_write(kernel, process, fd, chunk)
}

pub(super) fn typed_kernel_stdio_write(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    fd: u32,
    chunk: Vec<u8>,
) -> Result<Value, SidecarError> {
    let is_stdout = kernel_stdio_output_is_stdout(kernel, process.kernel_pid, fd)?;

    // COOKED TTY (line shell): route the write through the PTY slave so it flows
    // through process_output (ONLCR) into the master output buffer interleaved
    // with cooked-mode echo, then surface that single ordered master stream so
    // ONLCR + echo reach the host. stderr shares the master, merging onto Stdout.
    let raw_mode = tty_is_raw_mode(kernel, process);
    if process.tty_master_fd.is_some() && !raw_mode {
        let written = kernel
            .fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &chunk)
            .map_err(kernel_error)?;
        if let Some(master_bytes) = drain_tty_master_output(kernel, process)? {
            process.queue_pending_execution_event(ActiveExecutionEvent::Stdout(master_bytes))?;
        }
        return Ok(json!(written));
    }

    // RAW TTY (full-screen app) or non-TTY: emit the guest's bytes unmodified.
    // For a raw TTY we must NOT write through the slave (that would fill the
    // never-drained master and corrupt rendering); for non-TTY we write to the
    // underlying fd so pipes/files actually receive it.
    let written = if process.tty_master_fd.is_some() {
        chunk.len()
    } else {
        kernel
            .fd_write_nonblocking(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &chunk)
            .map_err(kernel_error)?
    };
    let event = if is_stdout {
        ActiveExecutionEvent::Stdout(chunk[..written].to_vec())
    } else {
        ActiveExecutionEvent::Stderr(chunk[..written].to_vec())
    };
    process.queue_pending_execution_event(event)?;

    Ok(json!(written))
}

/// Classify an fd against the kernel's authoritative stdio descriptions.
///
/// The path identifies ordinary stdout/stderr aliases and preserves cross-dup
/// routing (`1>&2`/`2>&1`). A PTY slave instead has a `/dev/pts/...` path, so
/// terminal aliases must be matched by open-file-description identity. Only an
/// alias of canonical fd 1 or 2 qualifies; an unrelated PTY is not host stdio.
pub(super) fn kernel_stdio_output_is_stdout(
    kernel: &SidecarKernel,
    kernel_pid: u32,
    fd: u32,
) -> Result<bool, SidecarError> {
    let descriptor_path = kernel
        .fd_path(EXECUTION_DRIVER_NAME, kernel_pid, fd)
        .map_err(kernel_error)?;
    match descriptor_path.as_str() {
        "/dev/stdout" => return Ok(true),
        "/dev/stderr" => return Ok(false),
        _ => {}
    }

    if kernel
        .isatty(EXECUTION_DRIVER_NAME, kernel_pid, fd)
        .map_err(kernel_error)?
    {
        let description = kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, kernel_pid, fd)
            .map_err(kernel_error)?
            .0;
        for (stdio_fd, is_stdout) in [(1, true), (2, false)] {
            match kernel.fd_description_identity(EXECUTION_DRIVER_NAME, kernel_pid, stdio_fd) {
                Ok((stdio_description, _)) if description == stdio_description => {
                    return Ok(is_stdout);
                }
                Ok(_) => {}
                Err(error) if error.code() == "EBADF" => {}
                Err(error) => return Err(kernel_error(error)),
            }
        }
    }

    Err(SidecarError::host(
        "EINVAL",
        format!("__kernel_stdio_write fd {fd} does not reference stdout or stderr"),
    ))
}

pub(crate) fn service_javascript_kernel_fd_write_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_write fd")?;
    let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "fd_write data")?;
    let written = kernel
        .fd_write_nonblocking(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &chunk)
        .map_err(kernel_error)?;
    // Executor host calls use the kernel VFS as their source of truth,
    // including host-backed mounts. Mirroring the whole regular file after
    // each chunk makes streamed writes quadratic and fails once the growing
    // file exceeds the configured single-read bound.
    Ok(Value::from(written))
}

pub(super) fn service_javascript_kernel_poll_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let (fd_requests, timeout_ms) = parse_kernel_poll_args(request)?;
    kernel_poll_response(kernel, process.kernel_pid, &fd_requests, timeout_ms)
}

/// Parse `__kernel_poll` args: (fd list, requested timeout ms).
pub(crate) fn parse_kernel_poll_args(
    request: &HostRpcRequest,
) -> Result<(Vec<KernelPollFdRequest>, i32), SidecarError> {
    let fd_requests: Vec<KernelPollFdRequest> = serde_json::from_value(
        request
            .args
            .first()
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|error| {
        SidecarError::InvalidState(format!(
            "__kernel_poll fd list must be a JSON array of {{ fd, events }} objects: {error}"
        ))
    })?;
    // Explicit null follows poll(2): wait indefinitely. Omission remains the
    // compatibility non-blocking probe used by synchronous callers.
    let timeout_ms = if request.args.get(1).is_some_and(Value::is_null) {
        -1
    } else {
        let timeout_ms =
            javascript_sync_rpc_arg_u64_optional(&request.args, 1, "__kernel_poll timeout ms")?
                .unwrap_or_default();
        i32::try_from(timeout_ms).map_err(|_| {
            SidecarError::InvalidState(String::from("__kernel_poll timeout ms must fit within i32"))
        })?
    };
    Ok((fd_requests, timeout_ms))
}

/// One bounded kernel poll. Timeout `0` = non-blocking probe (deferred
/// servicing re-checks readiness with this before replying).
pub(crate) fn kernel_poll_response(
    kernel: &SidecarKernel,
    kernel_pid: u32,
    fd_requests: &[KernelPollFdRequest],
    timeout_ms: i32,
) -> Result<Value, SidecarError> {
    let poll_fds = fd_requests
        .iter()
        .map(|entry| PollFd {
            fd: entry.fd,
            events: PollEvents::from_bits(entry.events),
            revents: PollEvents::empty(),
        })
        .collect::<Vec<_>>();
    let result = kernel
        .poll_fds(EXECUTION_DRIVER_NAME, kernel_pid, poll_fds, timeout_ms)
        .map_err(kernel_error)?;
    Ok(json!({
        "readyCount": result.ready_count,
        "fds": result
            .fds
            .into_iter()
            .map(|entry| KernelPollFdResponse {
                fd: entry.fd,
                events: entry.events.bits(),
                revents: entry.revents.bits(),
            })
            .collect::<Vec<_>>(),
    }))
}

pub(crate) fn install_kernel_stdin_pipe(
    kernel: &mut SidecarKernel,
    pid: u32,
) -> Result<u32, SidecarError> {
    let (read_fd, write_fd) = kernel
        .open_pipe(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?;
    kernel
        .fd_dup2(EXECUTION_DRIVER_NAME, pid, read_fd, 0)
        .map_err(kernel_error)?;
    kernel
        .fd_close(EXECUTION_DRIVER_NAME, pid, read_fd)
        .map_err(kernel_error)?;
    // This writer is sidecar-owned plumbing in the guest's fd table. Do not
    // let a spawned descendant inherit a writer for its own stdin pipe, which
    // would keep the pipe open forever and prevent EOF.
    kernel
        .fd_fcntl(
            EXECUTION_DRIVER_NAME,
            pid,
            write_fd,
            agentos_kernel::fd_table::F_SETFD,
            agentos_kernel::fd_table::FD_CLOEXEC,
        )
        .map_err(kernel_error)?;
    // The sidecar services the corresponding reads on this same dispatch
    // path, so a blocking write to a full pipe would deadlock the VM.
    kernel
        .fd_fcntl(
            EXECUTION_DRIVER_NAME,
            pid,
            write_fd,
            agentos_kernel::fd_table::F_SETFL,
            agentos_kernel::fd_table::O_NONBLOCK,
        )
        .map_err(kernel_error)?;
    Ok(write_fd)
}

/// Match Node's `stdio: "ignore"` contract by keeping fd 0 open on
/// `/dev/null`. Closing fd 0 is observably different: guest code that probes or
/// reads stdin receives `EBADF`, while native Node presents an immediate EOF.
pub(crate) fn install_kernel_ignored_stdin(
    kernel: &mut SidecarKernel,
    pid: u32,
) -> Result<(), SidecarError> {
    let null_fd = kernel
        .fd_open(
            EXECUTION_DRIVER_NAME,
            pid,
            "/dev/null",
            agentos_kernel::fd_table::O_RDONLY,
            None,
        )
        .map_err(kernel_error)?;
    if null_fd == 0 {
        return Ok(());
    }
    kernel
        .fd_dup2(EXECUTION_DRIVER_NAME, pid, null_fd, 0)
        .map_err(kernel_error)?;
    kernel
        .fd_close(EXECUTION_DRIVER_NAME, pid, null_fd)
        .map_err(kernel_error)
}

pub(super) fn requested_pty_window_size(env: &BTreeMap<String, String>) -> Option<(u16, u16)> {
    let cols = env
        .get("COLUMNS")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)?;
    let rows = env
        .get("LINES")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value > 0)?;
    Some((cols, rows))
}

pub(super) fn javascript_child_process_stdin_mode(request: &ProcessLaunchRequest) -> &str {
    request
        .options
        .stdio
        .first()
        .map(String::as_str)
        .unwrap_or("pipe")
}

pub(crate) fn write_kernel_process_stdin(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    chunk: &[u8],
) -> Result<(), SidecarError> {
    let Some(writer_fd) = process.kernel_stdin_writer_fd else {
        return Ok(());
    };
    if process.tty_master_fd.is_some() {
        kernel
            .fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd, chunk)
            .map_err(kernel_error)?;
        if let Some(echo) = drain_tty_master_output(kernel, process)? {
            process.queue_pending_execution_event(ActiveExecutionEvent::Stdout(echo))?;
        }
        return Ok(());
    }
    let observed_process_bytes = process
        .pending_kernel_stdin
        .total
        .saturating_add(chunk.len());
    if observed_process_bytes > process.limits.process.pending_stdin_bytes {
        let limit = process.limits.process.pending_stdin_bytes;
        return Err(SidecarError::Host(
            HostServiceError::new(
                "ERR_AGENTOS_PENDING_STDIN_BYTES_LIMIT",
                format!("child stdin queue exceeds limits.process.pendingStdinBytes ({limit})"),
            )
            .with_details(json!({
                "limitName": "limits.process.pendingStdinBytes",
                "limit": limit,
                "observed": observed_process_bytes,
            })),
        ));
    }
    if !process
        .vm_pending_stdin_bytes_budget
        .try_reserve(chunk.len())
    {
        let limit = process.vm_pending_stdin_bytes_budget.limit();
        let observed = process
            .vm_pending_stdin_bytes_budget
            .used()
            .saturating_add(chunk.len());
        return Err(SidecarError::Host(
            HostServiceError::new(
                "ERR_AGENTOS_VM_PENDING_STDIN_BYTES_LIMIT",
                format!("VM child stdin queues exceed limits.process.pendingStdinBytes ({limit})"),
            )
            .with_details(json!({
                "limitName": "limits.process.pendingStdinBytes",
                "limit": limit,
                "observed": observed,
            })),
        ));
    }
    process.pending_kernel_stdin.push(chunk);
    process
        .pending_kernel_stdin_gauge
        .observe_depth(process.pending_kernel_stdin.total);
    flush_pending_kernel_stdin(kernel, process)?;
    Ok(())
}

pub(crate) fn flush_pending_kernel_stdin(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    if process.tty_master_fd.is_some() {
        return Ok(());
    }
    let Some(writer_fd) = process.kernel_stdin_writer_fd else {
        clear_pending_kernel_stdin(process);
        process.pending_kernel_stdin_gauge.observe_depth(0);
        process.pending_kernel_stdin.close_requested = false;
        return Ok(());
    };
    while let Some(front) = process.pending_kernel_stdin.chunks.pop_front() {
        let offset = process.pending_kernel_stdin.front_offset;
        let slice = &front[offset..];
        match kernel.fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd, slice) {
            Ok(written) if written >= slice.len() => {
                process.pending_kernel_stdin.total = process
                    .pending_kernel_stdin
                    .total
                    .saturating_sub(slice.len());
                process.vm_pending_stdin_bytes_budget.release(slice.len());
                process.pending_kernel_stdin.front_offset = 0;
            }
            Ok(written) => {
                process.pending_kernel_stdin.total =
                    process.pending_kernel_stdin.total.saturating_sub(written);
                process.vm_pending_stdin_bytes_budget.release(written);
                process.pending_kernel_stdin.front_offset = offset + written;
                process.pending_kernel_stdin.chunks.push_front(front);
                break;
            }
            Err(error) if error.code() == "EAGAIN" => {
                process.pending_kernel_stdin.chunks.push_front(front);
                break;
            }
            Err(error) if error.code() == "EPIPE" => {
                clear_pending_kernel_stdin(process);
                process.pending_kernel_stdin.close_requested = false;
                process.kernel_stdin_writer_fd = None;
                if let Err(close_error) =
                    kernel.fd_close(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd)
                {
                    tracing::warn!(
                        process_id = process.kernel_pid,
                        fd = writer_fd,
                        error = %close_error,
                        "failed to close child stdin after EPIPE"
                    );
                }
                return Err(kernel_error(error));
            }
            Err(error) => {
                process.pending_kernel_stdin.chunks.push_front(front);
                return Err(kernel_error(error));
            }
        }
        process
            .pending_kernel_stdin_gauge
            .observe_depth(process.pending_kernel_stdin.total);
    }
    if process.pending_kernel_stdin.is_empty() && process.pending_kernel_stdin.close_requested {
        process.pending_kernel_stdin.close_requested = false;
        if let Some(writer_fd) = process.kernel_stdin_writer_fd.take() {
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd)
                .map_err(kernel_error)?;
        }
    }
    process
        .pending_kernel_stdin_gauge
        .observe_depth(process.pending_kernel_stdin.total);
    Ok(())
}

fn clear_pending_kernel_stdin(process: &mut ActiveProcess) {
    let pending_bytes = process.pending_kernel_stdin.total;
    process.pending_kernel_stdin.clear();
    process.vm_pending_stdin_bytes_budget.release(pending_bytes);
}

fn recheck_ready_deferred_fd_reads(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    let parked_request = process
        .deferred_kernel_wait_rpc
        .as_ref()
        .map(|(request, _)| request.clone())
        .filter(|request| request.method == "process.fd_read");
    if let Some(request) = parked_request {
        let descriptor = (|| {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "fd_read length",
            )?)
            .map_err(|_| SidecarError::InvalidState("fd_read length is too large".into()))?;
            Ok::<_, SidecarError>((fd, length))
        })();
        match descriptor {
            Ok((fd, length)) => {
                process.clear_deferred_kernel_wait_rpc();
                if request
                    .reply
                    .claim()
                    .map_err(|error| SidecarError::Execution(error.to_string()))?
                {
                    match kernel.fd_read_with_timeout_result(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        length,
                        Some(Duration::ZERO),
                    ) {
                        Ok(Some(bytes)) => request
                            .reply
                            .succeed(HostCallReply::Json(host_bytes_value(&bytes)))
                            .map_err(|error| SidecarError::Execution(error.to_string()))?,
                        Ok(None) => request
                            .reply
                            .succeed(HostCallReply::Json(host_bytes_value(&[])))
                            .map_err(|error| SidecarError::Execution(error.to_string()))?,
                        Err(error) => {
                            let error = kernel_error(error);
                            request
                                .reply
                                .fail(host_service_error(&error))
                                .map_err(|error| SidecarError::Execution(error.to_string()))?;
                        }
                    }
                }
            }
            Err(error) => {
                process.clear_deferred_kernel_wait_rpc();
                if request
                    .reply
                    .claim()
                    .map_err(|error| SidecarError::Execution(error.to_string()))?
                {
                    request
                        .reply
                        .fail(host_service_error(&error))
                        .map_err(|error| SidecarError::Execution(error.to_string()))?;
                }
            }
        }
    }
    for child in process.child_processes.values_mut() {
        recheck_ready_deferred_fd_reads(kernel, child)?;
    }
    Ok(())
}

fn recheck_ready_deferred_fd_writes(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    let parked_request = process
        .deferred_kernel_wait_rpc
        .as_ref()
        .map(|(request, _)| request.clone())
        .filter(|request| {
            request.method == "__kernel_stdio_write" || request.method == "process.fd_write"
        });
    if let Some(request) = parked_request {
        let response = if request.method == "__kernel_stdio_write" {
            service_javascript_kernel_stdio_write_sync_rpc(kernel, process, &request)
        } else {
            service_javascript_kernel_fd_write_sync_rpc(kernel, process, &request)
        };
        match response {
            Ok(response) => {
                process.clear_deferred_kernel_wait_rpc();
                settle_execution_host_call(&request.reply, Ok(response.into()))?;
            }
            Err(error) if host_service_error_code(&error) == "EAGAIN" => {}
            Err(error) => {
                process.clear_deferred_kernel_wait_rpc();
                request
                    .reply
                    .fail(host_service_error(&error))
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
            }
        }
    }
    for child in process.child_processes.values_mut() {
        recheck_ready_deferred_fd_writes(kernel, child)?;
    }
    Ok(())
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn wake_ready_deferred_fd_reads(vm: &mut VmState) -> Result<(), SidecarError> {
        let kernel = &mut vm.kernel;
        for process in vm.active_processes.values_mut() {
            recheck_ready_deferred_fd_reads(kernel, process)?;
        }
        Ok(())
    }

    pub(crate) fn wake_ready_deferred_fd_writes(vm: &mut VmState) -> Result<(), SidecarError> {
        let kernel = &mut vm.kernel;
        for process in vm.active_processes.values_mut() {
            recheck_ready_deferred_fd_writes(kernel, process)?;
        }
        Ok(())
    }
}

pub(crate) fn close_kernel_process_stdin(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    if !process.pending_kernel_stdin.is_empty() && process.kernel_stdin_writer_fd.is_some() {
        process.pending_kernel_stdin.close_requested = true;
        return Ok(());
    }
    let Some(writer_fd) = process.kernel_stdin_writer_fd.take() else {
        return Ok(());
    };
    kernel
        .fd_close(EXECUTION_DRIVER_NAME, process.kernel_pid, writer_fd)
        .map_err(kernel_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    #[test]
    fn sidecar_owned_stdin_writer_is_nonblocking() {
        let mut config = KernelVmConfig::new("vm-nonblocking-stdin-writer");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn kernel process");

        let writer_fd = install_kernel_stdin_pipe(&mut kernel, process.pid())
            .expect("install kernel stdin pipe");
        let flags = kernel
            .fd_fcntl(
                EXECUTION_DRIVER_NAME,
                process.pid(),
                writer_fd,
                agentos_kernel::fd_table::F_GETFL,
                0,
            )
            .expect("read stdin writer flags");

        assert_ne!(flags & agentos_kernel::fd_table::O_NONBLOCK, 0);
    }

    #[test]
    fn ignored_stdin_is_open_dev_null_and_reads_eof() {
        let mut config = KernelVmConfig::new("vm-ignored-stdin");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn kernel process");

        install_kernel_ignored_stdin(&mut kernel, process.pid()).expect("install ignored stdin");
        assert_eq!(
            kernel
                .fd_read(EXECUTION_DRIVER_NAME, process.pid(), 0, 16)
                .expect("read ignored stdin"),
            Vec::<u8>::new()
        );
        assert_eq!(
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.pid(), 0)
                .expect("inspect ignored stdin"),
            "/dev/null"
        );
    }

    #[test]
    fn kernel_stdio_classification_preserves_cross_dup_and_pty_identity() {
        let mut config = KernelVmConfig::new("vm-kernel-stdio-classification");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register execution driver");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn kernel process");
        let pid = process.pid();

        let stderr_alias = kernel
            .fd_dup(EXECUTION_DRIVER_NAME, pid, 2)
            .expect("duplicate stderr");
        assert!(!kernel_stdio_output_is_stdout(&kernel, pid, stderr_alias).unwrap());

        let (_unrelated_master, unrelated_slave, _) = kernel
            .open_pty(EXECUTION_DRIVER_NAME, pid)
            .expect("open unrelated PTY");
        let error = kernel_stdio_output_is_stdout(&kernel, pid, unrelated_slave)
            .expect_err("unrelated PTY must not become host stdout");
        assert_eq!(error.code(), Some("EINVAL"));

        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, unrelated_slave, 1)
            .expect("install PTY stdout");
        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, pid, unrelated_slave, 2)
            .expect("install PTY stderr");
        let pty_alias = kernel
            .fd_dup(EXECUTION_DRIVER_NAME, pid, 2)
            .expect("duplicate PTY stderr");
        assert!(
            kernel_stdio_output_is_stdout(&kernel, pid, pty_alias).unwrap(),
            "the shared PTY stream is surfaced as ordered stdout"
        );
    }
}
