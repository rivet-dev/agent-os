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

pub(super) fn write_all_nonblocking<S>(
    stream: &mut S,
    contents: &[u8],
    limits: ReactorIoLimits,
) -> Result<(), SidecarError>
where
    S: Write + AsFd,
{
    let deadline = Instant::now() + limits.operation_deadline;
    let mut remaining = contents;
    let mut operations = 0;
    while !remaining.is_empty() {
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
                if !wait_fd_writable_until(stream.as_fd(), deadline) {
                    return Err(sidecar_net_error(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "ERR_AGENTOS_OPERATION_DEADLINE: socket write exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ),
                    )));
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
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let (max_bytes, timeout_ms) = parse_kernel_stdin_read_args(request)?;
    let timeout_ms = timeout_ms.ok_or_else(|| {
        SidecarError::InvalidState(String::from(
            "an indefinite __kernel_stdin_read must use the deferred readiness path",
        ))
    })?;
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
    request: &JavascriptSyncRpcRequest,
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
        Err(SidecarError::Kernel(error)) if error.starts_with("EAGAIN:") => Ok(Value::Null),
        Err(error) => Err(error),
    }
}

pub(super) fn service_javascript_pty_set_raw_mode_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
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
    request: &JavascriptSyncRpcRequest,
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
    request: &JavascriptSyncRpcRequest,
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
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_stdio_write fd")?;
    let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "__kernel_stdio_write chunk")?;
    if fd != 1 && fd != 2 {
        return Err(SidecarError::InvalidState(format!(
            "__kernel_stdio_write only supports fd 1/2, got {fd}"
        )));
    }

    // COOKED TTY (line shell): route the write through the PTY slave so it flows
    // through process_output (ONLCR) into the master output buffer interleaved
    // with cooked-mode echo, then surface that single ordered master stream so
    // ONLCR + echo reach the host. stderr shares the master, merging onto Stdout.
    let raw_mode = tty_is_raw_mode(kernel, process);
    if process.tty_master_fd.is_some() && !raw_mode {
        let written = if fd == 1 {
            kernel
                .write_process_stdout(EXECUTION_DRIVER_NAME, process.kernel_pid, &chunk)
                .map_err(kernel_error)?
        } else {
            kernel
                .write_process_stderr(EXECUTION_DRIVER_NAME, process.kernel_pid, &chunk)
                .map_err(kernel_error)?
        };
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
    if written > 0
        && process.tty_master_fd.is_none()
        && kernel
            .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
            .map_err(kernel_error)?
            .filetype
            == agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
    {
        crate::filesystem::mirror_kernel_fd_contents_to_process_shadow(
            kernel,
            process,
            process.kernel_pid,
            fd,
        )?;
    }

    let event = if fd == 1 {
        ActiveExecutionEvent::Stdout(chunk[..written].to_vec())
    } else {
        ActiveExecutionEvent::Stderr(chunk[..written].to_vec())
    };
    process.queue_pending_execution_event(event)?;

    Ok(json!(written))
}

pub(crate) fn service_javascript_kernel_fd_write_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_write fd")?;
    let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "fd_write data")?;
    let written = kernel
        .fd_write_nonblocking(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &chunk)
        .map_err(kernel_error)?;
    // `process.fd_write` is the WASM runner's kernel-fd path. The kernel VFS is
    // authoritative here, including host-backed mounts; mirroring the whole
    // regular file after each chunk makes streamed writes quadratic and fails
    // once the growing file exceeds the configured single-read bound.
    Ok(Value::from(written))
}

pub(super) fn service_javascript_kernel_poll_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let (fd_requests, timeout_ms) = parse_kernel_poll_args(request)?;
    kernel_poll_response(kernel, process.kernel_pid, &fd_requests, timeout_ms)
}

/// Parse `__kernel_poll` args: (fd list, requested timeout ms).
pub(crate) fn parse_kernel_poll_args(
    request: &JavascriptSyncRpcRequest,
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

pub(super) fn javascript_child_process_stdin_mode(
    request: &JavascriptChildProcessSpawnRequest,
) -> &str {
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
    // Non-TTY JavaScript uses the in-process local stdin bridge, not a kernel
    // fd; a TTY JavaScript process (tty_master_fd set) DOES route through the
    // kernel PTY master, exactly like wasm/python, so line discipline + echo
    // apply.
    if process.runtime == GuestRuntimeKind::JavaScript && process.tty_master_fd.is_none() {
        return Ok(());
    }
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
        forward_tty_slave_input_to_javascript(kernel, process)?;
        return Ok(());
    }
    if process
        .pending_kernel_stdin
        .total
        .saturating_add(chunk.len())
        > process.limits.process.pending_stdin_bytes
    {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_PENDING_STDIN_BYTES_LIMIT: child stdin queue exceeds limits.process.pendingStdinBytes ({})",
            process.limits.process.pending_stdin_bytes
        )));
    }
    if !process
        .vm_pending_stdin_bytes_budget
        .try_reserve(chunk.len())
    {
        return Err(SidecarError::Execution(format!(
            "ERR_AGENTOS_VM_PENDING_STDIN_BYTES_LIMIT: VM child stdin queues exceed limits.process.pendingStdinBytes ({})",
            process.vm_pending_stdin_bytes_budget.limit()
        )));
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
                if process
                    .execution
                    .claim_javascript_sync_rpc_response(request.id)?
                {
                    match kernel.fd_read_with_timeout_result(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        length,
                        Some(Duration::ZERO),
                    ) {
                        Ok(Some(bytes)) => process
                            .execution
                            .respond_claimed_javascript_sync_rpc_success(
                                request.id,
                                javascript_sync_rpc_bytes_value(&bytes),
                            )?,
                        Ok(None) => process
                            .execution
                            .respond_claimed_javascript_sync_rpc_success(
                                request.id,
                                javascript_sync_rpc_bytes_value(&[]),
                            )?,
                        Err(error) => {
                            let error = kernel_error(error);
                            process
                                .execution
                                .respond_claimed_javascript_sync_rpc_error(
                                    request.id,
                                    javascript_sync_rpc_error_code(&error),
                                    error.to_string(),
                                )?;
                        }
                    }
                }
            }
            Err(error) => {
                process.clear_deferred_kernel_wait_rpc();
                if process
                    .execution
                    .claim_javascript_sync_rpc_response(request.id)?
                {
                    process
                        .execution
                        .respond_claimed_javascript_sync_rpc_error(
                            request.id,
                            javascript_sync_rpc_error_code(&error),
                            error.to_string(),
                        )?;
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
                process
                    .execution
                    .respond_javascript_sync_rpc_response(request.id, response.into())
                    .or_else(ignore_stale_javascript_sync_rpc_response)?;
            }
            Err(error) if javascript_sync_rpc_error_code(&error) == "EAGAIN" => {}
            Err(error) => {
                process.clear_deferred_kernel_wait_rpc();
                process
                    .execution
                    .respond_javascript_sync_rpc_error(
                        request.id,
                        javascript_sync_rpc_error_code(&error),
                        javascript_sync_rpc_error_message(&error),
                    )
                    .or_else(ignore_stale_javascript_sync_rpc_response)?;
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

/// For a TTY JavaScript guest, cooked input becomes readable on the PTY slave
/// only after line discipline runs (on newline/VEOF in canonical mode; every
/// byte in raw mode). The V8 isolate has no kernel-fd read loop of its own —
/// its stdin is the stream-stdin dispatch fed by `execution.write_stdin` — so
/// right after each master write, drain whatever the discipline released on
/// the slave (fd 0) and forward it to the isolate. A `None` read is the
/// discipline's VEOF: propagate it as end-of-stdin so `process.stdin` emits
/// `end`. Wasm/python guests read the slave themselves and are skipped.
pub(super) fn forward_tty_slave_input_to_javascript(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
) -> Result<(), SidecarError> {
    if process.tty_master_fd.is_none()
        || !matches!(process.execution, ActiveExecution::Javascript(_))
    {
        return Ok(());
    }
    loop {
        match kernel.fd_read_with_timeout_result(
            EXECUTION_DRIVER_NAME,
            process.kernel_pid,
            0,
            MAX_PTY_BUFFER_BYTES,
            Some(Duration::ZERO),
        ) {
            Ok(Some(bytes)) if !bytes.is_empty() => {
                process.execution.write_stdin(&bytes)?;
            }
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                process.execution.close_stdin()?;
                return Ok(());
            }
            Err(error) if error.code() == "EAGAIN" => return Ok(()),
            Err(error) => return Err(kernel_error(error)),
        }
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
}
