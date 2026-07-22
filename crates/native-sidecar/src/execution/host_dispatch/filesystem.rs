use super::*;

const NODE_CWD_FD: u32 = u32::MAX;
const MAX_PATH_BYTES: usize = 4096;
const MAX_XATTR_NAME_BYTES: usize = 255;
const MAX_READDIR_ENTRIES: usize = 4096;
const MAX_CLOSEFROM_TARGETS: usize = 1 << 20;

fn complete_timed_fd_read(data: Option<Vec<u8>>) -> Result<Vec<u8>, HostServiceError> {
    Ok(data.unwrap_or_default())
}

pub(super) fn dispatch_context_deferred_kernel_read<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: FilesystemOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let (fd, max_bytes, requested_timeout_ms, response) = match operation {
        FilesystemOperation::Read {
            fd,
            max_bytes,
            offset: None,
            deadline_ms,
        } => (
            Some(fd),
            max_bytes,
            deadline_ms,
            DeferredKernelReadResponse::DescriptorBytes,
        ),
        FilesystemOperation::StdinRead {
            max_bytes,
            timeout_ms,
        } => (
            None,
            max_bytes,
            Some(timeout_ms),
            DeferredKernelReadResponse::KernelStdin,
        ),
        _ => {
            return Err(SidecarError::host(
                "EINVAL",
                "deferred descriptor-read dispatcher received a non-read operation",
            ));
        }
    };
    let timeout_ms = requested_timeout_ms
        .or_else(|| {
            sidecar
                .vms
                .get(vm_id)
                .and_then(|vm| vm.limits.resources.max_blocking_read_ms)
        })
        .unwrap_or(agentos_kernel::resource_accounting::DEFAULT_BLOCKING_READ_TIMEOUT_MS);
    let deadline = match checked_deferred_guest_wait_deadline(timeout_ms) {
        Ok(deadline) => deadline,
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
    };
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated descriptor-read VM remains registered");
    let runtime = vm.runtime_context.clone();
    let wait_handle = vm.kernel.poll_wait_handle();
    let generation = vm.generation;
    let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
    let process = active_processes
        .get_mut(process_id)
        .expect("validated descriptor-read process remains registered");
    let fd = fd.unwrap_or(process.kernel_stdin_reader_fd);
    service_deferred_kernel_read_with_response(
        generation,
        &runtime,
        wait_handle,
        notify,
        kernel,
        process,
        Some((fd, max_bytes, response, deadline, reply)),
    )
}

/// Park one typed descriptor read without blocking the sidecar actor. Every
/// destructive read is a zero-time owner-thread probe after the direct reply
/// has been claimed; the task retains only notifier/deadline state.
pub(in crate::execution) fn service_deferred_kernel_read(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(u32, BoundedUsize, Instant, DirectHostReplyHandle)>,
) -> Result<(), SidecarError> {
    service_deferred_kernel_read_with_response(
        generation,
        runtime,
        wait_handle,
        notify,
        kernel,
        process,
        incoming.map(|(fd, max_bytes, deadline, reply)| {
            (
                fd,
                max_bytes,
                DeferredKernelReadResponse::DescriptorBytes,
                deadline,
                reply,
            )
        }),
    )
}

/// Park a legacy `__kernel_stdin_read` for any active process in the VM tree.
/// Descendant event pumps use this wrapper so kernel stdin retains the same
/// response shape and bounded owner-side refill semantics as a root process.
pub(in crate::execution) fn service_deferred_kernel_stdin_read(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(BoundedUsize, Instant, DirectHostReplyHandle)>,
) -> Result<(), SidecarError> {
    let fd = process.kernel_stdin_reader_fd;
    service_deferred_kernel_read_with_response(
        generation,
        runtime,
        wait_handle,
        notify,
        kernel,
        process,
        incoming.map(|(max_bytes, deadline, reply)| {
            (
                fd,
                max_bytes,
                DeferredKernelReadResponse::KernelStdin,
                deadline,
                reply,
            )
        }),
    )
}

fn service_deferred_kernel_read_with_response(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::poll::PollWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(
        u32,
        BoundedUsize,
        DeferredKernelReadResponse,
        Instant,
        DirectHostReplyHandle,
    )>,
) -> Result<(), SidecarError> {
    let newly_admitted = incoming.is_some();
    if let Some((fd, max_bytes, response, deadline, reply)) = incoming {
        if process.deferred_kernel_read.is_some() {
            reply
                .fail(HostServiceError::new(
                    "EBUSY",
                    "process already owns a deferred descriptor read",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != process.kernel_pid {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "deferred descriptor-read identity does not match the active kernel process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        if max_bytes.get() > process.limits.wasm.sync_read_limit_bytes {
            reply
                .fail(HostServiceError::limit(
                    "E2BIG",
                    "limits.wasm.syncReadLimitBytes",
                    process.limits.wasm.sync_read_limit_bytes as u64,
                    max_bytes.get() as u64,
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }
        process.deferred_kernel_read = Some(DeferredKernelRead {
            fd,
            max_bytes,
            response,
            reply,
            deadline,
            wake_task: None,
        });
    }

    let now = Instant::now();
    let should_probe = process.deferred_kernel_read.as_ref().is_some_and(|read| {
        newly_admitted
            || now >= read.deadline
            || read
                .wake_task
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished)
    });
    if !should_probe {
        return Ok(());
    }

    let mut read = process
        .deferred_kernel_read
        .take()
        .expect("deferred descriptor read checked above");
    if let Some(task) = read.wake_task.take() {
        task.abort();
    }
    if read.fd == 0 || read.response == DeferredKernelReadResponse::KernelStdin {
        if let Err(error) = flush_pending_kernel_stdin(kernel, process) {
            return read
                .reply
                .fail(host_service_error(&error))
                .map_err(SidecarError::from);
        }
    }

    // Snapshot before the destructive zero-time probe. A readiness edge that
    // races the probe changes this generation, so the waiter immediately
    // schedules another owner-thread probe instead of absorbing the wake.
    let observed = wait_handle.snapshot();
    match kernel.fd_read_with_timeout_result(
        EXECUTION_DRIVER_NAME,
        process.kernel_pid,
        read.fd,
        read.max_bytes.get(),
        Some(Duration::ZERO),
    ) {
        Ok(Some(bytes)) => {
            let value = match read.response {
                DeferredKernelReadResponse::DescriptorBytes => host_bytes_value(&bytes),
                DeferredKernelReadResponse::KernelStdin => json!({
                    "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
            };
            return read
                .reply
                .succeed(HostCallReply::Json(value))
                .map_err(SidecarError::from);
        }
        Ok(None) => {
            let value = match read.response {
                DeferredKernelReadResponse::DescriptorBytes => host_bytes_value(&[]),
                DeferredKernelReadResponse::KernelStdin => json!({ "done": true }),
            };
            return read
                .reply
                .succeed(HostCallReply::Json(value))
                .map_err(SidecarError::from);
        }
        Err(error)
            if matches!(error.code(), "EAGAIN" | "EWOULDBLOCK")
                && kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, read.fd)
                    .map(|stat| stat.flags & agentos_kernel::fd_table::O_NONBLOCK != 0)
                    .map_err(kernel_error)? =>
        {
            return read
                .reply
                .fail(kernel_host_error(error))
                .map_err(SidecarError::from);
        }
        Err(error) if matches!(error.code(), "EAGAIN" | "EWOULDBLOCK") && now >= read.deadline => {
            return match read.response {
                DeferredKernelReadResponse::DescriptorBytes => read
                    .reply
                    .fail(HostServiceError::new(
                        "EAGAIN",
                        "timed fd read is not ready",
                    ))
                    .map_err(SidecarError::from),
                DeferredKernelReadResponse::KernelStdin => read
                    .reply
                    .succeed(HostCallReply::Json(Value::Null))
                    .map_err(SidecarError::from),
            };
        }
        Err(error) if matches!(error.code(), "EAGAIN" | "EWOULDBLOCK") => {}
        Err(error) => {
            return read
                .reply
                .fail(kernel_host_error(error))
                .map_err(SidecarError::from);
        }
    }

    let deadline = read.deadline;
    let wake_task = runtime.spawn(agentos_runtime::TaskClass::Vm, async move {
        let delay = deadline.saturating_duration_since(Instant::now());
        tokio::select! {
            _ = wait_handle.wait_for_change_async(observed) => {}
            _ = tokio::time::sleep(delay) => {}
        }
        notify.notify_one();
    });
    match wake_task {
        Ok(task) => {
            read.wake_task = Some(task);
            process.deferred_kernel_read = Some(read);
            Ok(())
        }
        Err(error) => read
            .reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from),
    }
}

pub(super) fn decode(
    request: &HostRpcRequest,
    nonblocking_unpositioned_writes: bool,
    max_reply_bytes: usize,
) -> Result<Option<FilesystemOperation>, SidecarError> {
    let path_limit = payload_limit("runtime.filesystem.maxPathBytes", MAX_PATH_BYTES)?;
    let name_limit = payload_limit("runtime.filesystem.maxXattrNameBytes", MAX_XATTR_NAME_BYTES)?;
    let response_limit = payload_limit("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)?;
    // This second instance only types the configured maximum forwarded to the
    // kernel. Actual reply sizes are admitted through `response_limit` above.
    let response_bound = agentos_execution::backend::PayloadLimit::with_warning_hook(
        "limits.reactor.maxBridgeResponseBytes",
        max_reply_bytes,
        None,
    )
    .map_err(SidecarError::Host)?;
    let request_limit = payload_limit("limits.reactor.maxBridgeRequestBytes", max_reply_bytes)?;
    let readdir_limit = payload_limit("runtime.filesystem.maxReaddirEntries", MAX_READDIR_ENTRIES)?;

    let path = |index: usize, label: &str| {
        BoundedString::try_new(
            javascript_sync_rpc_arg_str(&request.args, index, label)?.to_owned(),
            &path_limit,
        )
        .map_err(SidecarError::Host)
    };
    let name = |index: usize, label: &str| {
        BoundedString::try_new(
            javascript_sync_rpc_arg_str(&request.args, index, label)?.to_owned(),
            &name_limit,
        )
        .map_err(SidecarError::Host)
    };
    let bytes = |index: usize, label: &str| {
        BoundedBytes::try_new(
            javascript_sync_rpc_request_bytes_arg(request, index, label)?,
            &request_limit,
        )
        .map_err(SidecarError::Host)
    };
    let output_count = |value: u64, label: &str| {
        let value = usize::try_from(value)
            .map_err(|_| SidecarError::host("E2BIG", format!("{label} exceeds usize")))?;
        response_limit
            .admit(encoded_bytes_reply_size(value).ok_or_else(|| {
                SidecarError::host(
                    "E2BIG",
                    format!("{label} encoded reply size overflows usize"),
                )
            })?)
            .map_err(SidecarError::Host)?;
        BoundedUsize::try_new(value, &response_limit).map_err(SidecarError::Host)
    };

    let operation = match request.method.as_str() {
        "__kernel_stdin_read" => {
            let requested = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                0,
                "__kernel_stdin_read max bytes",
            )?
            .unwrap_or(DEFAULT_KERNEL_STDIN_READ_MAX_BYTES as u64)
            .clamp(1, DEFAULT_KERNEL_STDIN_READ_MAX_BYTES as u64);
            let timeout_ms = if request.args.get(1).is_some_and(Value::is_null) {
                return Err(SidecarError::host(
                    "EINVAL",
                    "an indefinite __kernel_stdin_read must use deferred readiness",
                ));
            } else {
                javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "__kernel_stdin_read timeout ms",
                )?
                .unwrap_or(DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS)
            };
            FilesystemOperation::StdinRead {
                max_bytes: output_count(requested, "stdin read length")?,
                timeout_ms,
            }
        }
        "__kernel_stdio_write" => FilesystemOperation::StdioWrite {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_stdio_write fd")?,
            bytes: bytes(1, "__kernel_stdio_write chunk")?,
        },
        "fs.accessSync" => FilesystemOperation::AccessAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "filesystem access path")?,
            mode: javascript_sync_rpc_arg_u32_optional(&request.args, 1, "filesystem access mode")?
                .unwrap_or_default(),
            effective_ids: javascript_sync_rpc_option_bool(&request.args, 2, "effective IDs")
                .unwrap_or(false),
        },
        "fs.chmodForProcessSync" => FilesystemOperation::SetMode {
            target: MetadataTarget::Path {
                dir_fd: NODE_CWD_FD,
                follow_symlinks: true,
            },
            path: Some(path(0, "filesystem chmod path")?),
            mode: javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?,
        },
        "fs.chownSync" | "fs.lchownSync" => FilesystemOperation::SetOwner {
            target: MetadataTarget::Path {
                dir_fd: NODE_CWD_FD,
                follow_symlinks: request.method == "fs.chownSync",
            },
            path: Some(path(0, "filesystem chown path")?),
            uid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                1,
                "filesystem chown uid",
            )?),
            gid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                2,
                "filesystem chown gid",
            )?),
        },
        "fs.truncateForProcessSync" => FilesystemOperation::SetPathLength {
            dir_fd: NODE_CWD_FD,
            path: path(0, "filesystem truncate path")?,
            length: javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "filesystem truncate length",
            )?
            .unwrap_or_default(),
        },
        "fs.namedFifoPeerReadySync" => FilesystemOperation::NamedPipePeerReady {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "named FIFO fd")?,
        },
        "fs.openTmpfileSync" => FilesystemOperation::OpenTmpfileAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "unnamed-file directory")?,
            options: GuestOpenSpec {
                flags: javascript_sync_rpc_arg_u32(&request.args, 1, "unnamed-file flags")?,
                mode: Some(javascript_sync_rpc_arg_u32(
                    &request.args,
                    2,
                    "unnamed-file mode",
                )?),
                rights: GuestOpenRights::Synthesized,
            },
            linkable: javascript_sync_rpc_option_bool(&request.args, 3, "linkable").unwrap_or(true),
        },
        "process.open_tmpfile_at" => FilesystemOperation::OpenTmpfileAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "unnamed-file dir fd")?,
            path: path(1, "unnamed-file directory")?,
            options: GuestOpenSpec {
                flags: javascript_sync_rpc_arg_u32(&request.args, 2, "unnamed-file flags")?,
                mode: Some(javascript_sync_rpc_arg_u32(
                    &request.args,
                    3,
                    "unnamed-file mode",
                )?),
                rights: GuestOpenRights::Synthesized,
            },
            linkable: javascript_sync_rpc_option_bool(&request.args, 4, "linkable").unwrap_or(true),
        },
        "fs.linkFdSync" => FilesystemOperation::LinkDescriptorAt {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "unnamed-file fd")?,
            dir_fd: NODE_CWD_FD,
            path: path(1, "unnamed-file link destination")?,
        },
        "process.fd_link_at" => FilesystemOperation::LinkDescriptorAt {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "unnamed-file fd")?,
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 1, "unnamed-file link dir fd")?,
            path: path(2, "unnamed-file link destination")?,
        },
        "fs.punchHoleSync" => FilesystemOperation::Range {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "punch-hole fd")?,
            operation: FileRangeOperation::PunchHole,
            offset: javascript_sync_rpc_arg_u64(&request.args, 1, "punch-hole offset")?,
            length: javascript_sync_rpc_arg_u64(&request.args, 2, "punch-hole length")?,
            keep_size: true,
        },
        "fs.fallocateSync" | "fs.zeroRangeSync" | "fs.insertRangeSync" | "fs.collapseRangeSync" => {
            FilesystemOperation::Range {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "file-range fd")?,
                operation: match request.method.as_str() {
                    "fs.fallocateSync" => FileRangeOperation::Allocate,
                    "fs.zeroRangeSync" => FileRangeOperation::Zero,
                    "fs.insertRangeSync" => FileRangeOperation::Insert,
                    _ => FileRangeOperation::Collapse,
                },
                offset: javascript_sync_rpc_arg_u64(&request.args, 1, "file-range offset")?,
                length: javascript_sync_rpc_arg_u64(&request.args, 2, "file-range length")?,
                keep_size: request.method == "fs.zeroRangeSync"
                    && javascript_sync_rpc_arg_u32(&request.args, 3, "zero-range keep-size")? != 0,
            }
        }
        "fs.fiemapSync" => FilesystemOperation::Extents {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fiemap fd")?,
            max_entries: BoundedUsize::try_new(MAX_READDIR_ENTRIES, &readdir_limit)
                .map_err(SidecarError::Host)?,
        },
        "fs.fiemapAtSync" => FilesystemOperation::ExtentAt {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fiemap fd")?,
            index: javascript_sync_rpc_arg_u32(&request.args, 1, "fiemap extent index")?,
        },
        "fs.statfsSync" => FilesystemOperation::FilesystemStatsAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "filesystem statfs path")?,
        },
        "process.path_statfs_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "statfs dir fd")?;
            let path = path(1, "filesystem statfs path")?;
            if path.as_str().is_empty() {
                FilesystemOperation::DescriptorFilesystemStats { fd: dir_fd }
            } else {
                FilesystemOperation::FilesystemStatsAt { dir_fd, path }
            }
        }
        "fs.statSync" => FilesystemOperation::NodeStatAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "filesystem stat path")?,
        },
        "fs.mknodSync" => FilesystemOperation::MakeNodeAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "filesystem mknod path")?,
            mode: javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem mknod mode")?,
            device: javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem mknod device")?,
        },
        "process.path_mknod_at" => FilesystemOperation::MakeNodeAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "mknod dir fd")?,
            path: path(1, "filesystem mknod path")?,
            mode: javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem mknod mode")?,
            device: javascript_sync_rpc_arg_u64(&request.args, 3, "filesystem mknod device")?,
        },
        "fs.remountSync" => FilesystemOperation::Remount {
            path: path(0, "filesystem remount path")?,
            options: BoundedString::try_new(
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem remount options")?
                    .to_owned(),
                &request_limit,
            )
            .map_err(SidecarError::Host)?,
        },
        "fs.renameAt2Sync" => FilesystemOperation::RenameAt {
            old_dir_fd: NODE_CWD_FD,
            old_path: path(0, "filesystem renameat2 source")?,
            new_dir_fd: NODE_CWD_FD,
            new_path: path(1, "filesystem renameat2 destination")?,
            flags: javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem renameat2 flags")?,
        },
        "process.path_rename_at2" => FilesystemOperation::RenameAt {
            old_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "renameat2 old dir fd")?,
            old_path: path(1, "filesystem renameat2 source")?,
            new_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 2, "renameat2 new dir fd")?,
            new_path: path(3, "filesystem renameat2 destination")?,
            flags: javascript_sync_rpc_arg_u32(&request.args, 4, "filesystem renameat2 flags")?,
        },
        "process.path_access_at" => FilesystemOperation::AccessAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "access dir fd")?,
            path: path(1, "filesystem access path")?,
            mode: javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem access mode")?,
            effective_ids: javascript_sync_rpc_arg_bool(&request.args, 3, "effective IDs")?,
        },
        "fs.getxattrSync" | "fs.listxattrSync" | "fs.setxattrSync" | "fs.removexattrSync" => {
            let (operation, name_value, value, follow_index) = match request.method.as_str() {
                "fs.getxattrSync" => (XattrOperation::Get, Some(name(1, "xattr name")?), None, 2),
                "fs.listxattrSync" => (XattrOperation::List, None, None, 1),
                "fs.setxattrSync" => (
                    XattrOperation::Set {
                        flags: javascript_sync_rpc_arg_u32(&request.args, 3, "xattr flags")?,
                    },
                    Some(name(1, "xattr name")?),
                    Some(bytes(2, "xattr value")?),
                    4,
                ),
                _ => (
                    XattrOperation::Remove,
                    Some(name(1, "xattr name")?),
                    None,
                    2,
                ),
            };
            FilesystemOperation::Xattr {
                target: MetadataTarget::Path {
                    dir_fd: NODE_CWD_FD,
                    follow_symlinks: javascript_sync_rpc_option_bool(
                        &request.args,
                        follow_index,
                        "follow symlinks",
                    )
                    .unwrap_or(true),
                },
                path: Some(path(0, "xattr path")?),
                name: name_value,
                value,
                operation,
                max_result_bytes: BoundedUsize::try_new(max_reply_bytes, &response_bound)
                    .map_err(SidecarError::Host)?,
            }
        }
        "process.path_getxattr_at"
        | "process.path_listxattr_at"
        | "process.path_setxattr_at"
        | "process.path_removexattr_at" => {
            let (operation, name_value, value, follow_index) = match request.method.as_str() {
                "process.path_getxattr_at" => {
                    (XattrOperation::Get, Some(name(2, "xattr name")?), None, 3)
                }
                "process.path_listxattr_at" => (XattrOperation::List, None, None, 2),
                "process.path_setxattr_at" => (
                    XattrOperation::Set {
                        flags: javascript_sync_rpc_arg_u32(&request.args, 4, "xattr flags")?,
                    },
                    Some(name(2, "xattr name")?),
                    Some(bytes(3, "xattr value")?),
                    5,
                ),
                _ => (
                    XattrOperation::Remove,
                    Some(name(2, "xattr name")?),
                    None,
                    3,
                ),
            };
            FilesystemOperation::Xattr {
                target: MetadataTarget::Path {
                    dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "xattr dir fd")?,
                    follow_symlinks: javascript_sync_rpc_arg_bool(
                        &request.args,
                        follow_index,
                        "follow symlinks",
                    )?,
                },
                path: Some(path(1, "xattr path")?),
                name: name_value,
                value,
                operation,
                max_result_bytes: BoundedUsize::try_new(max_reply_bytes, &response_bound)
                    .map_err(SidecarError::Host)?,
            }
        }
        "fs.fgetxattrSync" | "fs.flistxattrSync" | "fs.fsetxattrSync" | "fs.fremovexattrSync" => {
            let (operation, name_value, value) = match request.method.as_str() {
                "fs.fgetxattrSync" => (XattrOperation::Get, Some(name(1, "xattr name")?), None),
                "fs.flistxattrSync" => (XattrOperation::List, None, None),
                "fs.fsetxattrSync" => (
                    XattrOperation::Set {
                        flags: javascript_sync_rpc_arg_u32(&request.args, 3, "xattr flags")?,
                    },
                    Some(name(1, "xattr name")?),
                    Some(bytes(2, "xattr value")?),
                ),
                _ => (XattrOperation::Remove, Some(name(1, "xattr name")?), None),
            };
            FilesystemOperation::Xattr {
                target: MetadataTarget::Descriptor(javascript_sync_rpc_arg_u32(
                    &request.args,
                    0,
                    "xattr fd",
                )?),
                path: None,
                name: name_value,
                value,
                operation,
                max_result_bytes: BoundedUsize::try_new(max_reply_bytes, &response_bound)
                    .map_err(SidecarError::Host)?,
            }
        }
        "process.fd_pipe" => FilesystemOperation::Pipe,
        "process.fd_snapshot" => FilesystemOperation::Snapshot,
        "process.fd_open" => FilesystemOperation::OpenAt {
            dir_fd: NODE_CWD_FD,
            path: path(0, "fd_open path")?,
            options: open_spec(request, 1, 2, 3, 4)?,
        },
        "process.fd_preopens" => FilesystemOperation::Preopens,
        "process.fd_preopen" => FilesystemOperation::Preopen {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_preopen fd")?,
        },
        "process.fd_read" | "process.fd_pread" => {
            let offset = if request.method == "process.fd_pread" {
                Some(parse_u64_string(request, 2, "fd_pread offset")?)
            } else {
                None
            };
            FilesystemOperation::Read {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?,
                max_bytes: output_count(
                    javascript_sync_rpc_arg_u64(&request.args, 1, "fd_read length")?,
                    "fd_read length",
                )?,
                offset,
                deadline_ms: if offset.is_none() {
                    javascript_sync_rpc_arg_u64_optional(&request.args, 2, "fd_read timeout")?
                } else {
                    None
                },
            }
        }
        "process.fd_write" | "process.fd_pwrite" => FilesystemOperation::Write {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_write fd")?,
            bytes: bytes(1, "fd_write data")?,
            offset: if request.method == "process.fd_pwrite" {
                Some(parse_u64_string(request, 2, "fd_pwrite offset")?)
            } else {
                None
            },
            deadline_ms: None,
            nonblocking: nonblocking_unpositioned_writes && request.method == "process.fd_write",
        },
        "process.fd_sync" | "process.fd_datasync" => FilesystemOperation::Sync {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_sync fd")?,
            kind: if request.method == "process.fd_datasync" {
                DescriptorSyncKind::Data
            } else {
                DescriptorSyncKind::All
            },
        },
        "process.fd_readdir" => FilesystemOperation::ReadDirectory {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_readdir fd")?,
            cookie: parse_u64_string(request, 1, "fd_readdir cookie")?,
            max_entries: BoundedUsize::try_new(
                usize::try_from(javascript_sync_rpc_arg_u64(
                    &request.args,
                    2,
                    "fd_readdir max entries",
                )?)
                .unwrap_or(usize::MAX)
                .min(MAX_READDIR_ENTRIES),
                &readdir_limit,
            )
            .map_err(SidecarError::Host)?,
            max_bytes: BoundedUsize::try_new(max_reply_bytes, &response_bound)
                .map_err(SidecarError::Host)?,
        },
        "process.fd_close" => FilesystemOperation::Close {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_close fd")?,
        },
        "process.fd_closefrom" => FilesystemOperation::CloseFrom {
            min_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_closefrom minimum fd")?,
            exact_fds: request
                .args
                .get(1)
                .filter(|value| !value.is_null())
                .map(|value| {
                    let values = value.as_array().ok_or_else(|| {
                        SidecarError::host(
                            "EINVAL",
                            "fd_closefrom canonical targets must be an array",
                        )
                    })?;
                    let fds = values
                        .iter()
                        .map(|value| {
                            value
                                .as_u64()
                                .and_then(|fd| u32::try_from(fd).ok())
                                .ok_or_else(|| {
                                    SidecarError::host(
                                        "EINVAL",
                                        "fd_closefrom canonical target must be u32",
                                    )
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    BoundedVec::try_new(
                        fds,
                        &PayloadLimit::new("limits.resources.maxOpenFds", MAX_CLOSEFROM_TARGETS)
                            .expect("static closefrom target limit"),
                    )
                    .map_err(SidecarError::from)
                })
                .transpose()?,
        },
        "process.fd_stat" => FilesystemOperation::DescriptorStatus {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_stat fd")?,
        },
        "process.fd_filestat" => FilesystemOperation::DescriptorFileStat {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_filestat fd")?,
        },
        "process.fd_chown" => FilesystemOperation::SetOwner {
            target: MetadataTarget::Descriptor(javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "fd_chown fd",
            )?),
            path: None,
            uid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                1,
                "fd_chown uid",
            )?),
            gid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                2,
                "fd_chown gid",
            )?),
        },
        "process.fd_chmod" => FilesystemOperation::SetMode {
            target: MetadataTarget::Descriptor(javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "fd_chmod fd",
            )?),
            path: None,
            mode: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_chmod mode")?,
        },
        "process.fd_truncate" => FilesystemOperation::SetLength {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_truncate fd")?,
            length: parse_u64_string(request, 1, "fd_truncate length")?,
        },
        "process.fd_utimes" => {
            let flags = javascript_sync_rpc_arg_u32(&request.args, 3, "fd_utimes flags")?;
            let parse_time = |index: usize,
                              explicit: bool,
                              now: bool,
                              label: &str|
             -> Result<Option<u64>, SidecarError> {
                if now || !explicit {
                    return Ok(None);
                }
                Ok(Some(parse_u64_string(request, index, label)?))
            };
            FilesystemOperation::SetTimes {
                target: MetadataTarget::Descriptor(javascript_sync_rpc_arg_u32(
                    &request.args,
                    0,
                    "fd_utimes fd",
                )?),
                path: None,
                update: FileTimeUpdate {
                    atime_ns: parse_time(1, flags & 1 != 0, flags & 2 != 0, "atime")?,
                    mtime_ns: parse_time(2, flags & 4 != 0, flags & 8 != 0, "mtime")?,
                    atime_now: flags & 2 != 0,
                    mtime_now: flags & 8 != 0,
                },
            }
        }
        "process.fd_set_flags" => FilesystemOperation::SetDescriptorFlags {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_set_flags fd")?,
            flags: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_set_flags flags")?,
        },
        "process.fd_getfd" => FilesystemOperation::DescriptorFdFlags {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_getfd fd")?,
        },
        "process.fd_setfd" => FilesystemOperation::SetDescriptorFdFlags {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_setfd fd")?,
            flags: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_setfd flags")?,
        },
        "process.fd_flock" => FilesystemOperation::AdvisoryLock {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_flock fd")?,
            operation: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_flock operation")?,
        },
        "process.fd_record_lock" => FilesystemOperation::RecordLock {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_record_lock fd")?,
            command: match javascript_sync_rpc_arg_u32(&request.args, 1, "fd_record_lock command")?
            {
                12 => RecordLockCommand::Query,
                13 => RecordLockCommand::Set,
                14 => RecordLockCommand::Wait,
                command => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported fd_record_lock command {command}"),
                    ))
                }
            },
            kind: match javascript_sync_rpc_arg_u32(&request.args, 2, "fd_record_lock type")? {
                0 => FilesystemRecordLockKind::Read,
                1 => FilesystemRecordLockKind::Write,
                2 => FilesystemRecordLockKind::Unlock,
                _ => return Err(SidecarError::host("EINVAL", "invalid fd_record_lock type")),
            },
            start: parse_u64_string(request, 3, "fd_record_lock start")?,
            length: parse_u64_string(request, 4, "fd_record_lock length")?,
        },
        "process.fd_record_lock_cancel" => FilesystemOperation::CancelRecordLocks,
        "process.fd_dup" => FilesystemOperation::Duplicate {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup fd")?,
        },
        "process.fd_dup2" => FilesystemOperation::DuplicateTo {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup2 source fd")?,
            target_fd: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_dup2 target fd")?,
        },
        "process.fd_dup_min" => FilesystemOperation::DuplicateMin {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup_min fd")?,
            min_fd: javascript_sync_rpc_arg_u32(&request.args, 1, "fd_dup_min minimum")?,
        },
        "process.fd_move" => FilesystemOperation::Move {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_move fd")?,
            replaced_fd: javascript_sync_rpc_arg_u32_optional(
                &request.args,
                1,
                "fd_move replaced fd",
            )?,
        },
        "process.fd_seek" => {
            let whence = match javascript_sync_rpc_arg_u32(&request.args, 2, "fd_seek whence")? {
                0 => DescriptorWhence::Set,
                1 => DescriptorWhence::Current,
                2 => DescriptorWhence::End,
                3 => DescriptorWhence::Data,
                4 => DescriptorWhence::Hole,
                value => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("invalid fd_seek whence {value}"),
                    ))
                }
            };
            FilesystemOperation::Seek {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_seek fd")?,
                offset: javascript_sync_rpc_arg_str(&request.args, 1, "fd_seek offset")?
                    .parse::<i64>()
                    .map_err(|_| SidecarError::host("EINVAL", "fd_seek offset must be i64"))?,
                whence,
            }
        }
        "process.fd_chdir_path" => FilesystemOperation::DescriptorPath {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fchdir fd")?,
            require_directory: true,
        },
        "process.fd_path" => FilesystemOperation::DescriptorPath {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_path fd")?,
            require_directory: false,
        },
        method if method.starts_with("process.path_") => decode_path_operation(request, &path)?,
        _ => return Ok(None),
    };
    Ok(Some(operation))
}

fn payload_limit(
    name: &'static str,
    maximum: usize,
) -> Result<agentos_execution::backend::PayloadLimit, SidecarError> {
    agentos_execution::backend::PayloadLimit::new(name, maximum).map_err(SidecarError::Host)
}

fn encoded_bytes_reply_size(byte_length: usize) -> Option<usize> {
    // `host_bytes_value` serializes the base64 payload in this
    // fixed JSON object. Admit the encoded shape before any kernel read can
    // consume a pipe or advance a file description.
    const EMPTY_ENCODED_BYTES_JSON_LEN: usize = 37;
    byte_length
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(EMPTY_ENCODED_BYTES_JSON_LEN)
}

fn open_spec(
    request: &HostRpcRequest,
    flags_index: usize,
    mode_index: usize,
    rights_base_index: usize,
    rights_inheriting_index: usize,
) -> Result<GuestOpenSpec, SidecarError> {
    Ok(GuestOpenSpec {
        flags: javascript_sync_rpc_arg_u32(&request.args, flags_index, "open flags")?,
        mode: javascript_sync_rpc_arg_u32_optional(&request.args, mode_index, "open mode")?,
        rights: GuestOpenRights::Explicit {
            base: parse_u64_string(request, rights_base_index, "open base rights")?,
            inheriting: parse_u64_string(
                request,
                rights_inheriting_index,
                "open inheriting rights",
            )?,
        },
    })
}

fn parse_u64_string(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
) -> Result<u64, SidecarError> {
    javascript_sync_rpc_arg_str(&request.args, index, label)?
        .parse::<u64>()
        .map_err(|_| SidecarError::host("EINVAL", format!("{label} must be u64")))
}

fn decode_path_operation(
    request: &HostRpcRequest,
    path: &impl Fn(usize, &str) -> Result<BoundedString, SidecarError>,
) -> Result<FilesystemOperation, SidecarError> {
    let path_bound = agentos_execution::backend::PayloadLimit::with_warning_hook(
        "runtime.filesystem.maxPathBytes",
        MAX_PATH_BYTES,
        None,
    )
    .map_err(SidecarError::Host)?;
    Ok(match request.method.as_str() {
        "process.path_open_at" => FilesystemOperation::OpenAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_open_at dir fd")?,
            path: path(1, "path_open_at path")?,
            options: open_spec(request, 2, 3, 4, 5)?,
        },
        "process.path_mkdir_at" => FilesystemOperation::CreateDirectoryAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_mkdir_at dir fd")?,
            path: path(1, "path_mkdir_at path")?,
            mode: 0o777,
        },
        "process.path_stat_at" => FilesystemOperation::Stat {
            target: MetadataTarget::Path {
                dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_stat_at dir fd")?,
                follow_symlinks: javascript_sync_rpc_arg_bool(
                    &request.args,
                    2,
                    "path_stat_at follow",
                )?,
            },
            path: Some(path(1, "path_stat_at path")?),
        },
        "process.path_chmod_at" => FilesystemOperation::SetMode {
            target: MetadataTarget::Path {
                dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_chmod_at dir fd")?,
                follow_symlinks: true,
            },
            path: Some(path(1, "path_chmod_at path")?),
            mode: javascript_sync_rpc_arg_u32(&request.args, 2, "path_chmod_at mode")?,
        },
        "process.path_chown_at" => FilesystemOperation::SetOwner {
            target: MetadataTarget::Path {
                dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_chown_at dir fd")?,
                follow_symlinks: javascript_sync_rpc_arg_bool(
                    &request.args,
                    4,
                    "path_chown_at follow",
                )?,
            },
            path: Some(path(1, "path_chown_at path")?),
            uid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                2,
                "path_chown_at uid",
            )?),
            gid: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                3,
                "path_chown_at gid",
            )?),
        },
        "process.path_utimes_at" => {
            let flags = javascript_sync_rpc_arg_u32(&request.args, 5, "path_utimes_at flags")?;
            let parse_time = |index: usize,
                              explicit: bool,
                              now: bool,
                              label: &str|
             -> Result<Option<u64>, SidecarError> {
                if now || !explicit {
                    return Ok(None);
                }
                Ok(Some(parse_u64_string(request, index, label)?))
            };
            FilesystemOperation::SetTimes {
                target: MetadataTarget::Path {
                    dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_utimes_at dir fd")?,
                    follow_symlinks: javascript_sync_rpc_arg_bool(
                        &request.args,
                        2,
                        "path_utimes_at follow",
                    )?,
                },
                path: Some(path(1, "path_utimes_at path")?),
                update: FileTimeUpdate {
                    atime_ns: parse_time(3, flags & 1 != 0, flags & 2 != 0, "atime")?,
                    mtime_ns: parse_time(4, flags & 4 != 0, flags & 8 != 0, "mtime")?,
                    atime_now: flags & 2 != 0,
                    mtime_now: flags & 8 != 0,
                },
            }
        }
        "process.path_link_at" => FilesystemOperation::LinkAt {
            old_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_link_at old fd")?,
            old_path: path(1, "path_link_at old path")?,
            new_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 2, "path_link_at new fd")?,
            new_path: path(3, "path_link_at new path")?,
            follow_old: javascript_sync_rpc_arg_bool(&request.args, 4, "path_link_at follow")?,
        },
        "process.path_readlink_at" => FilesystemOperation::ReadLinkAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_readlink_at dir fd")?,
            path: path(1, "path_readlink_at path")?,
            max_bytes: BoundedUsize::try_new(MAX_PATH_BYTES, &path_bound)
                .map_err(SidecarError::Host)?,
        },
        "process.path_remove_dir_at" => FilesystemOperation::UnlinkAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_remove_dir_at dir fd")?,
            path: path(1, "path_remove_dir_at path")?,
            remove_directory: true,
        },
        "process.path_rename_at" => FilesystemOperation::RenameAt {
            old_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_rename_at old fd")?,
            old_path: path(1, "path_rename_at old path")?,
            new_dir_fd: javascript_sync_rpc_arg_u32(&request.args, 2, "path_rename_at new fd")?,
            new_path: path(3, "path_rename_at new path")?,
            flags: 0,
        },
        "process.path_symlink_at" => FilesystemOperation::SymlinkAt {
            target: path(0, "path_symlink_at target")?,
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 1, "path_symlink_at dir fd")?,
            path: path(2, "path_symlink_at path")?,
        },
        "process.path_unlink_at" => FilesystemOperation::UnlinkAt {
            dir_fd: javascript_sync_rpc_arg_u32(&request.args, 0, "path_unlink_at dir fd")?,
            path: path(1, "path_unlink_at path")?,
            remove_directory: false,
        },
        _ => return Err(SidecarError::host("ENOSYS", "unknown path operation")),
    })
}

pub(super) struct FilesystemCapability;

impl SidecarHostCapability<FilesystemOperation> for FilesystemCapability {
    fn requires_claim(operation: &FilesystemOperation) -> bool {
        matches!(
            operation,
            FilesystemOperation::ReadFileAt { .. }
                | FilesystemOperation::WriteFileAt { .. }
                | FilesystemOperation::OpenAt { .. }
                | FilesystemOperation::OpenTmpfileAt { .. }
                | FilesystemOperation::Pipe
                | FilesystemOperation::Snapshot
                | FilesystemOperation::CanonicalPreopens
                | FilesystemOperation::Preopen { .. }
                | FilesystemOperation::Preopens
                | FilesystemOperation::Close { .. }
                | FilesystemOperation::CloseFrom { .. }
                | FilesystemOperation::Renumber { .. }
                | FilesystemOperation::Duplicate { .. }
                | FilesystemOperation::DuplicateTo { .. }
                | FilesystemOperation::DuplicateMin { .. }
                | FilesystemOperation::Move { .. }
                | FilesystemOperation::Read { .. }
                | FilesystemOperation::Write { .. }
                | FilesystemOperation::Seek { .. }
                | FilesystemOperation::Sync { .. }
                | FilesystemOperation::SetDescriptorFdFlags { .. }
                | FilesystemOperation::SetDescriptorFlags { .. }
                | FilesystemOperation::SetLength { .. }
                | FilesystemOperation::SetPathLength { .. }
                | FilesystemOperation::AdvisoryLock { .. }
                | FilesystemOperation::RecordLock { .. }
                | FilesystemOperation::CancelRecordLocks
                | FilesystemOperation::SetTimes { .. }
                | FilesystemOperation::SetMode { .. }
                | FilesystemOperation::SetOwner { .. }
                | FilesystemOperation::SetAttributesAt { .. }
                | FilesystemOperation::CreateDirectoryAt { .. }
                | FilesystemOperation::CreateDirectoriesAt { .. }
                | FilesystemOperation::MakeNodeAt { .. }
                | FilesystemOperation::LinkAt { .. }
                | FilesystemOperation::LinkDescriptorAt { .. }
                | FilesystemOperation::RenameAt { .. }
                | FilesystemOperation::SymlinkAt { .. }
                | FilesystemOperation::UnlinkAt { .. }
                | FilesystemOperation::Range { .. }
                | FilesystemOperation::Xattr {
                    operation: XattrOperation::Set { .. } | XattrOperation::Remove,
                    ..
                }
                | FilesystemOperation::Remount { .. }
                | FilesystemOperation::StdinRead { .. }
                | FilesystemOperation::StdioWrite { .. }
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: FilesystemOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let pid = process.kernel_pid;
        let value = match operation {
            FilesystemOperation::ReadFileAt {
                dir_fd,
                path,
                max_bytes,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                let expected = kernel
                    .stat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    .map_err(kernel_host_error)?;
                let expected = usize::try_from(expected.size).map_err(|_| {
                    HostServiceError::new("EOVERFLOW", "file size exceeds host address space")
                })?;
                if expected > max_bytes.get() {
                    return Err(HostServiceError::limit(
                        "E2BIG",
                        "limits.reactor.maxBridgeResponseBytes",
                        max_bytes.get() as u64,
                        expected as u64,
                    ));
                }
                let bytes = kernel
                    .read_file_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    .map_err(kernel_host_error)?;
                if bytes.len() > max_bytes.get() {
                    return Err(HostServiceError::limit(
                        "E2BIG",
                        "limits.reactor.maxBridgeResponseBytes",
                        max_bytes.get() as u64,
                        bytes.len() as u64,
                    ));
                }
                return Ok(HostCallReply::Raw(bytes));
            }
            FilesystemOperation::WriteFileAt {
                dir_fd,
                path,
                bytes,
                mode,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .write_file_for_process(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &path,
                        bytes.into_vec(),
                        mode,
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::OpenAt {
                dir_fd,
                path,
                options,
            } => {
                let dir_fd = super::canonical_path_dir_fd(dir_fd);
                let parent_fd = (dir_fd != NODE_CWD_FD).then_some(dir_fd);
                let requested_rights = match options.rights {
                    GuestOpenRights::Explicit { base, inheriting } => Some((base, inheriting)),
                    GuestOpenRights::Synthesized => None,
                };
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                Value::from(
                    kernel
                        .fd_open_with_rights(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            parent_fd,
                            &path,
                            options.flags,
                            options.mode,
                            requested_rights,
                        )
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::OpenTmpfileAt {
                dir_fd,
                path,
                options,
                linkable,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                Value::from(
                    kernel
                        .fd_open_tmpfile(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            options.flags,
                            options.mode.unwrap_or_default(),
                            linkable,
                        )
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::Pipe => {
                let (read_fd, write_fd) = kernel
                    .open_pipe(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                json!({ "readFd": read_fd, "writeFd": write_fd })
            }
            FilesystemOperation::Snapshot => Value::Array(
                kernel
                    .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "fd": entry.fd,
                            "descriptionId": entry.description_id.to_string(),
                            "fdFlags": entry.fd_flags,
                            "statusFlags": entry.status_flags,
                            "filetype": entry.filetype,
                            "rightsBase": entry.rights_base.to_string(),
                            "rightsInheriting": entry.rights_inheriting.to_string(),
                            "kind": if entry.is_socket {
                                "socket"
                            } else if entry.is_pipe {
                                "pipe"
                            } else if entry.is_pty {
                                "pty"
                            } else {
                                "file"
                            },
                        })
                    })
                    .collect(),
            ),
            FilesystemOperation::Preopen { fd } => kernel
                .wasi_preopen(EXECUTION_DRIVER_NAME, pid, super::canonical_path_dir_fd(fd))
                .map_err(kernel_host_error)?
                .map(preopen_value)
                .unwrap_or(Value::Null),
            FilesystemOperation::Preopens => {
                let preopens = kernel
                    .initialize_wasi_preopens(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                Value::Array(preopens.into_iter().map(preopen_value).collect())
            }
            FilesystemOperation::CanonicalPreopens => {
                let preopens = kernel
                    .initialize_canonical_wasi_preopens(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                Value::Array(preopens.into_iter().map(preopen_value).collect())
            }
            FilesystemOperation::Close { fd } => {
                kernel
                    .fd_close(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::CloseFrom { min_fd, exact_fds } => {
                let closed = if let Some(fds) = exact_fds {
                    kernel.fd_close_exact(EXECUTION_DRIVER_NAME, pid, fds.into_vec())
                } else {
                    kernel.fd_close_from(EXECUTION_DRIVER_NAME, pid, min_fd)
                }
                .map_err(kernel_host_error)?;
                json!({ "closedFds": closed })
            }
            FilesystemOperation::Renumber { from, to } => {
                kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, pid, from, to)
                    .map_err(kernel_host_error)?;
                kernel
                    .fd_close(EXECUTION_DRIVER_NAME, pid, from)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::Duplicate { fd } => Value::from(
                kernel
                    .fd_dup(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::DuplicateTo { fd, target_fd } => {
                kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, pid, fd, target_fd)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::DuplicateMin { fd, min_fd } => Value::from(
                kernel
                    .fd_fcntl(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        agentos_kernel::fd_table::F_DUPFD,
                        min_fd,
                    )
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::Move { fd, replaced_fd } => Value::from(
                kernel
                    .fd_renumber_projection(EXECUTION_DRIVER_NAME, pid, fd, replaced_fd)
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::Read {
                fd,
                max_bytes,
                offset,
                deadline_ms,
            } => {
                if max_bytes.get() > process.limits.wasm.sync_read_limit_bytes {
                    return Err(HostServiceError::limit(
                        "E2BIG",
                        "limits.wasm.syncReadLimitBytes",
                        process.limits.wasm.sync_read_limit_bytes as u64,
                        max_bytes.get() as u64,
                    ));
                }
                if fd == 0 && offset.is_none() {
                    flush_pending_kernel_stdin(kernel, process).map_err(sidecar_host_error)?;
                }
                let data = match offset {
                    Some(offset) => kernel
                        .fd_pread(EXECUTION_DRIVER_NAME, pid, fd, max_bytes.get(), offset)
                        .map_err(kernel_host_error)?,
                    None => match deadline_ms {
                        Some(timeout) => complete_timed_fd_read(
                            kernel
                                .fd_read_with_timeout_result(
                                    EXECUTION_DRIVER_NAME,
                                    pid,
                                    fd,
                                    max_bytes.get(),
                                    Some(Duration::from_millis(timeout)),
                                )
                                .map_err(kernel_host_error)?,
                        )?,
                        None => kernel
                            .fd_read(EXECUTION_DRIVER_NAME, pid, fd, max_bytes.get())
                            .map_err(kernel_host_error)?,
                    },
                };
                host_bytes_value(&data)
            }
            FilesystemOperation::Write {
                fd,
                bytes,
                offset,
                nonblocking,
                ..
            } => {
                let written = match offset {
                    Some(offset) => {
                        kernel.fd_pwrite(EXECUTION_DRIVER_NAME, pid, fd, bytes.as_slice(), offset)
                    }
                    None if nonblocking => kernel.fd_write_nonblocking(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        bytes.as_slice(),
                    ),
                    None => kernel.fd_write(EXECUTION_DRIVER_NAME, pid, fd, bytes.as_slice()),
                }
                .map_err(kernel_host_error)?;
                Value::from(written)
            }
            FilesystemOperation::Seek { fd, offset, whence } => {
                let whence = match whence {
                    DescriptorWhence::Set => agentos_kernel::kernel::SEEK_SET,
                    DescriptorWhence::Current => agentos_kernel::kernel::SEEK_CUR,
                    DescriptorWhence::End => agentos_kernel::kernel::SEEK_END,
                    DescriptorWhence::Data => agentos_kernel::kernel::SEEK_DATA,
                    DescriptorWhence::Hole => agentos_kernel::kernel::SEEK_HOLE,
                };
                Value::String(
                    kernel
                        .fd_seek(EXECUTION_DRIVER_NAME, pid, fd, offset, whence)
                        .map_err(kernel_host_error)?
                        .to_string(),
                )
            }
            FilesystemOperation::Sync { fd, .. } => {
                kernel
                    .fd_sync(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::DescriptorStatus { fd } => {
                let stat = kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                json!({
                    "filetype": stat.filetype,
                    "flags": stat.flags,
                    "rightsBase": stat.rights,
                    "rightsInheriting": stat.rights_inheriting,
                    "preopenPath": stat.wasi_preopen_path,
                })
            }
            FilesystemOperation::DescriptorFileStat { fd } => {
                let fd_stat = kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                let stat = kernel
                    .dev_fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                wasi_stat_value(stat, fd_stat.filetype)
            }
            FilesystemOperation::DescriptorPath {
                fd,
                require_directory,
            } => {
                if require_directory {
                    let stat = kernel
                        .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_host_error)?;
                    if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                        return Err(HostServiceError::new(
                            "ENOTDIR",
                            format!("file descriptor {fd} is not a directory"),
                        ));
                    }
                }
                Value::String(
                    kernel
                        .fd_path(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::DescriptorFdFlags { fd } => Value::from(
                kernel
                    .fd_fcntl(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        agentos_kernel::fd_table::F_GETFD,
                        0,
                    )
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::SetDescriptorFdFlags { fd, flags } => Value::from(
                kernel
                    .fd_fcntl(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        agentos_kernel::fd_table::F_SETFD,
                        flags,
                    )
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::SetDescriptorFlags { fd, flags } => Value::from(
                kernel
                    .fd_fcntl(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        agentos_kernel::fd_table::F_SETFL,
                        flags,
                    )
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::SetLength { fd, length } => {
                kernel
                    .fd_truncate(EXECUTION_DRIVER_NAME, pid, fd, length)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::SetPathLength {
                dir_fd,
                path,
                length,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .truncate_for_process(EXECUTION_DRIVER_NAME, pid, &path, length)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::AdvisoryLock { fd, operation } => {
                kernel
                    .fd_flock(EXECUTION_DRIVER_NAME, pid, fd, operation)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::RecordLock {
                fd,
                command,
                kind,
                start,
                length,
            } => {
                let kind = match kind {
                    FilesystemRecordLockKind::Read => {
                        agentos_kernel::fd_table::RecordLockType::Read
                    }
                    FilesystemRecordLockKind::Write => {
                        agentos_kernel::fd_table::RecordLockType::Write
                    }
                    FilesystemRecordLockKind::Unlock => {
                        agentos_kernel::fd_table::RecordLockType::Unlock
                    }
                };
                let conflict = match command {
                    RecordLockCommand::Query => kernel.fd_record_lock(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        kind,
                        start,
                        length,
                        true,
                    ),
                    RecordLockCommand::Set => kernel.fd_record_lock(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        kind,
                        start,
                        length,
                        false,
                    ),
                    RecordLockCommand::Wait => kernel
                        .fd_record_lock_wait(EXECUTION_DRIVER_NAME, pid, fd, kind, start, length)
                        .map(|()| None),
                }
                .map_err(kernel_host_error)?;
                conflict.map_or_else(
                    || json!({ "type": 2, "pid": 0, "start": start.to_string(), "length": length.to_string() }),
                    |lock| json!({
                        "type": match lock.lock_type {
                            agentos_kernel::fd_table::RecordLockType::Read => 0,
                            agentos_kernel::fd_table::RecordLockType::Write => 1,
                            agentos_kernel::fd_table::RecordLockType::Unlock => 2,
                        },
                        "pid": lock.pid,
                        "start": lock.start.to_string(),
                        "length": lock.length().to_string(),
                    }),
                )
            }
            FilesystemOperation::CancelRecordLocks => {
                kernel
                    .fd_record_lock_cancel(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::NamedPipePeerReady { fd } => Value::Bool(
                kernel
                    .fd_named_pipe_peer_ready(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?,
            ),
            FilesystemOperation::ReadDirectory {
                fd,
                cookie,
                max_entries,
                ..
            } => {
                let cookie = usize::try_from(cookie).map_err(|_| {
                    HostServiceError::new("EINVAL", "fd_readdir cookie exceeds usize")
                })?;
                let entries = kernel
                    .fd_read_dir_page_with_types(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        cookie,
                        max_entries.get(),
                    )
                    .map_err(kernel_host_error)?;
                Value::Array(
                    entries
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            json!({
                                "name": entry.name,
                                "ino": entry.ino.to_string(),
                                "filetype": entry.filetype,
                                "next": cookie.saturating_add(index).saturating_add(1).to_string(),
                            })
                        })
                        .collect(),
                )
            }
            FilesystemOperation::ReadDirectoryAt {
                dir_fd,
                path,
                max_entries,
                max_reply_bytes,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                let entries = kernel
                    .read_dir_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    .map_err(kernel_host_error)?;
                if entries.len() > max_entries.get() {
                    return Err(HostServiceError::limit(
                        "ERR_AGENTOS_RESOURCE_LIMIT",
                        "runtime.filesystem.maxReaddirEntries",
                        max_entries.get() as u64,
                        entries.len() as u64,
                    ));
                }
                PayloadLimit::new(
                    "limits.reactor.maxBridgeResponseBytes",
                    max_reply_bytes.get(),
                )?
                .admit_json(&entries)?;
                json!(entries)
            }
            FilesystemOperation::Stat { target, path } => {
                let MetadataTarget::Path {
                    dir_fd,
                    follow_symlinks,
                } = target
                else {
                    return Err(unsupported("filesystem stat target", target));
                };
                let path = resolve_path(kernel, process, dir_fd, required_path(path.as_ref())?)?;
                let stat = if follow_symlinks {
                    kernel.stat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                } else {
                    kernel.lstat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                }
                .map_err(kernel_host_error)?;
                wasi_path_stat_value(stat)
            }
            FilesystemOperation::NodeStatAt { dir_fd, path } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                node_stat_value(
                    kernel
                        .stat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::NodeLstatAt { dir_fd, path } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                node_stat_value(
                    kernel
                        .lstat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::SetAttributesAt {
                dir_fd,
                path,
                update,
                follow_symlinks,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                if let Some(mode) = update.mode {
                    kernel
                        .chmod_for_process(EXECUTION_DRIVER_NAME, pid, &path, mode)
                        .map_err(kernel_host_error)?;
                }
                if update.uid.is_some() || update.gid.is_some() {
                    let current = if follow_symlinks {
                        kernel.stat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    } else {
                        kernel.lstat_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    }
                    .map_err(kernel_host_error)?;
                    kernel
                        .chown_for_process(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            update.uid.unwrap_or(current.uid),
                            update.gid.unwrap_or(current.gid),
                            follow_symlinks,
                        )
                        .map_err(kernel_host_error)?;
                }
                if let (Some(atime_ms), Some(mtime_ms)) = (update.atime_ms, update.mtime_ms) {
                    kernel
                        .utimes_spec_for_process(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            agentos_kernel::vfs::VirtualUtimeSpec::Set(
                                agentos_kernel::vfs::VirtualTimeSpec::from_millis(atime_ms),
                            ),
                            agentos_kernel::vfs::VirtualUtimeSpec::Set(
                                agentos_kernel::vfs::VirtualTimeSpec::from_millis(mtime_ms),
                            ),
                            follow_symlinks,
                        )
                        .map_err(kernel_host_error)?;
                }
                Value::Null
            }
            FilesystemOperation::SetTimes {
                target,
                path,
                update,
            } => {
                let atime = time_spec(update.atime_ns, update.atime_now)?;
                let mtime = time_spec(update.mtime_ns, update.mtime_now)?;
                match target {
                    MetadataTarget::Descriptor(fd) => kernel
                        .futimes(EXECUTION_DRIVER_NAME, pid, fd, atime, mtime)
                        .map_err(kernel_host_error)?,
                    MetadataTarget::Path {
                        dir_fd,
                        follow_symlinks,
                    } => {
                        let path =
                            resolve_path(kernel, process, dir_fd, required_path(path.as_ref())?)?;
                        kernel
                            .utimes_spec_for_process(
                                EXECUTION_DRIVER_NAME,
                                pid,
                                &path,
                                atime,
                                mtime,
                                follow_symlinks,
                            )
                            .map_err(kernel_host_error)?;
                    }
                }
                Value::Null
            }
            FilesystemOperation::SetMode { target, path, mode } => {
                match target {
                    MetadataTarget::Descriptor(fd) => {
                        kernel.fd_chmod_for_process(EXECUTION_DRIVER_NAME, pid, fd, mode)
                    }
                    MetadataTarget::Path { dir_fd, .. } => {
                        let path =
                            resolve_path(kernel, process, dir_fd, required_path(path.as_ref())?)?;
                        kernel.chmod_for_process(EXECUTION_DRIVER_NAME, pid, &path, mode)
                    }
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::SetOwner {
                target,
                path,
                uid,
                gid,
            } => {
                let uid = uid.ok_or_else(|| HostServiceError::new("EINVAL", "uid is required"))?;
                let gid = gid.ok_or_else(|| HostServiceError::new("EINVAL", "gid is required"))?;
                match target {
                    MetadataTarget::Descriptor(fd) => {
                        kernel.fd_chown_for_process(EXECUTION_DRIVER_NAME, pid, fd, uid, gid)
                    }
                    MetadataTarget::Path {
                        dir_fd,
                        follow_symlinks,
                    } => {
                        let path =
                            resolve_path(kernel, process, dir_fd, required_path(path.as_ref())?)?;
                        kernel.chown_for_process(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            uid,
                            gid,
                            follow_symlinks,
                        )
                    }
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::AccessAt {
                dir_fd,
                path,
                mode,
                effective_ids,
            } => {
                let valid = libc::R_OK as u32 | libc::W_OK as u32 | libc::X_OK as u32;
                if mode & !valid != 0 {
                    return Err(HostServiceError::new(
                        "EINVAL",
                        format!("invalid filesystem access mode {mode:o}"),
                    ));
                }
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .access_for_process(EXECUTION_DRIVER_NAME, pid, &path, mode, effective_ids)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::CreateDirectoryAt { dir_fd, path, mode } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .mkdir_for_process(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &path,
                        false,
                        (mode != 0o777).then_some(mode),
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::CreateDirectoriesAt { dir_fd, path, mode } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .mkdir_for_process(EXECUTION_DRIVER_NAME, pid, &path, true, mode)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::MakeNodeAt {
                dir_fd,
                path,
                mode,
                device,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .mknod_for_process(EXECUTION_DRIVER_NAME, pid, &path, mode, device)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::LinkAt {
                old_dir_fd,
                old_path,
                follow_old,
                new_dir_fd,
                new_path,
            } => {
                let mut old_path = resolve_path(kernel, process, old_dir_fd, old_path.as_str())?;
                let new_path = resolve_path(kernel, process, new_dir_fd, new_path.as_str())?;
                if follow_old {
                    old_path = kernel
                        .realpath_for_process(EXECUTION_DRIVER_NAME, pid, &old_path)
                        .map_err(kernel_host_error)?;
                }
                kernel
                    .link_for_process(EXECUTION_DRIVER_NAME, pid, &old_path, &new_path)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::LinkDescriptorAt { fd, dir_fd, path } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .fd_link_tmpfile_for_process(EXECUTION_DRIVER_NAME, pid, fd, &path)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::RenameAt {
                old_dir_fd,
                old_path,
                new_dir_fd,
                new_path,
                flags,
            } => {
                let old_path = resolve_path(kernel, process, old_dir_fd, old_path.as_str())?;
                let new_path = resolve_path(kernel, process, new_dir_fd, new_path.as_str())?;
                if flags == 0 {
                    kernel.rename_for_process(EXECUTION_DRIVER_NAME, pid, &old_path, &new_path)
                } else {
                    kernel.rename_at2_for_process(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &old_path,
                        &new_path,
                        flags,
                    )
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::SymlinkAt {
                target,
                dir_fd,
                path,
            } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                kernel
                    .symlink_for_process(EXECUTION_DRIVER_NAME, pid, target.as_str(), &path)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::ReadLinkAt { dir_fd, path, .. } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                Value::String(
                    kernel
                        .read_link_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                        .map_err(kernel_host_error)?,
                )
            }
            FilesystemOperation::UnlinkAt {
                dir_fd,
                path,
                remove_directory,
            } => {
                if remove_directory {
                    kernel
                        .validate_remove_directory_pathname(path.as_str())
                        .map_err(kernel_host_error)?;
                }
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                if remove_directory {
                    kernel.remove_dir_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                } else {
                    kernel.remove_file_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::Range {
                fd,
                operation,
                offset,
                length,
                keep_size,
            } => {
                match operation {
                    FileRangeOperation::Allocate => {
                        kernel.fd_allocate(EXECUTION_DRIVER_NAME, pid, fd, offset, length)
                    }
                    FileRangeOperation::PunchHole => {
                        kernel.fd_punch_hole(EXECUTION_DRIVER_NAME, pid, fd, offset, length)
                    }
                    FileRangeOperation::Zero => kernel.fd_zero_range(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        fd,
                        offset,
                        length,
                        keep_size,
                    ),
                    FileRangeOperation::Insert => {
                        kernel.fd_insert_range(EXECUTION_DRIVER_NAME, pid, fd, offset, length)
                    }
                    FileRangeOperation::Collapse => {
                        kernel.fd_collapse_range(EXECUTION_DRIVER_NAME, pid, fd, offset, length)
                    }
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::Extents { fd, max_entries } => {
                let allocated = kernel
                    .fd_allocated_ranges(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                let unwritten = kernel
                    .fd_unwritten_ranges(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                Value::Array(classify_extents(allocated, &unwritten).into_iter().take(max_entries.get()).map(
                    |(start, end, unwritten)| json!({ "start": start, "end": end, "unwritten": unwritten })
                ).collect())
            }
            FilesystemOperation::ExtentAt { fd, index } => kernel
                .fd_extent_at(EXECUTION_DRIVER_NAME, pid, fd, index)
                .map_err(kernel_host_error)?
                .map(|extent| {
                    json!({
                        "start": extent.start,
                        "end": extent.end,
                        "unwritten": extent.unwritten,
                    })
                })
                .unwrap_or(Value::Null),
            FilesystemOperation::Xattr {
                target,
                path,
                name,
                value,
                operation,
                max_result_bytes,
            } => execute_xattr(
                kernel,
                process,
                target,
                path.as_ref(),
                name.as_ref(),
                value.as_ref(),
                operation,
                max_result_bytes,
            )?,
            FilesystemOperation::FilesystemStatsAt { dir_fd, path } => {
                let path = resolve_path(kernel, process, dir_fd, path.as_str())?;
                let stats = kernel
                    .filesystem_stats_for_process(EXECUTION_DRIVER_NAME, pid, &path)
                    .map_err(kernel_host_error)?;
                json!({
                    "totalBytes": stats.total_bytes,
                    "usedBytes": stats.used_bytes,
                    "availableBytes": stats.available_bytes,
                    "totalInodes": stats.total_inodes,
                    "freeInodes": stats.free_inodes,
                })
            }
            FilesystemOperation::DescriptorFilesystemStats { fd } => {
                let stats = kernel
                    .filesystem_stats_for_fd_process(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_host_error)?;
                json!({
                    "totalBytes": stats.total_bytes,
                    "usedBytes": stats.used_bytes,
                    "availableBytes": stats.available_bytes,
                    "totalInodes": stats.total_inodes,
                    "freeInodes": stats.free_inodes,
                })
            }
            FilesystemOperation::Remount { path, options } => {
                let path = resolve_path(kernel, process, NODE_CWD_FD, path.as_str())?;
                kernel
                    .remount_filesystem_for_process(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &path,
                        options.as_str(),
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            FilesystemOperation::StdinRead {
                max_bytes,
                timeout_ms,
            } => {
                if max_bytes.get() > process.limits.wasm.sync_read_limit_bytes {
                    return Err(HostServiceError::limit(
                        "E2BIG",
                        "limits.wasm.syncReadLimitBytes",
                        process.limits.wasm.sync_read_limit_bytes as u64,
                        max_bytes.get() as u64,
                    ));
                }
                typed_kernel_stdin_read(kernel, process, max_bytes.get(), timeout_ms)
                    .map_err(sidecar_host_error)?
            }
            FilesystemOperation::StdioWrite { fd, bytes } => {
                typed_kernel_stdio_write(kernel, process, fd, bytes.into_vec())
                    .map_err(sidecar_host_error)?
            }
            other => return Err(unsupported("filesystem", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}

fn sidecar_host_error(error: SidecarError) -> HostServiceError {
    match error {
        SidecarError::Host(error) => error,
        other => HostServiceError::new("EIO", other.to_string()),
    }
}

fn required_path(path: Option<&BoundedString>) -> Result<&str, HostServiceError> {
    path.map(BoundedString::as_str)
        .ok_or_else(|| HostServiceError::new("EINVAL", "filesystem path is required"))
}

fn resolve_path(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    dir_fd: u32,
    path: &str,
) -> Result<String, HostServiceError> {
    let dir_fd = super::canonical_path_dir_fd(dir_fd);
    // agentOS extension imports use NODE_CWD_FD for an ordinary POSIX path.
    // Patched libc may use either supported AT_FDCWD encoding. Hidden preopen
    // tags remain intact and retain the preopen's capability-root semantics.
    if dir_fd == NODE_CWD_FD {
        let path = if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!(
                "{}/{}",
                process.guest_cwd.trim_end_matches('/'),
                path
            ))
        };
        if path
            .split('/')
            .any(agentos_kernel::kernel::is_internal_unnamed_file_name)
        {
            return Err(HostServiceError::new(
                "ENOENT",
                format!("no such file or directory: {path}"),
            ));
        }
        return Ok(path);
    }
    if path.starts_with('/') {
        return Err(HostServiceError::new(
            "EACCES",
            format!("absolute path '{path}' cannot bypass directory fd {dir_fd}"),
        ));
    }
    let stat = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, dir_fd)
        .map_err(kernel_host_error)?;
    if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
        return Err(HostServiceError::new(
            "ENOTDIR",
            format!("file descriptor {dir_fd} is not a directory"),
        ));
    }
    let base = kernel
        .fd_path(EXECUTION_DRIVER_NAME, process.kernel_pid, dir_fd)
        .map_err(kernel_host_error)?;
    Ok(normalize_path(&format!("{base}/{path}")))
}

fn preopen_value(preopen: agentos_kernel::kernel::ProcessWasiPreopen) -> Value {
    json!({
        "fd": preopen.fd,
        "guestPath": preopen.guest_path,
        "rightsBase": preopen.rights_base,
        "rightsInheriting": preopen.rights_inheriting,
    })
}

fn node_stat_value(stat: agentos_kernel::vfs::VirtualStat) -> Value {
    json!({
        "mode": stat.mode,
        "size": stat.size,
        "blocks": stat.blocks,
        "dev": stat.dev,
        "rdev": stat.rdev,
        "isDirectory": stat.is_directory,
        "isSymbolicLink": stat.is_symbolic_link,
        "atimeMs": stat.atime_ms,
        "atimeNsec": stat.atime_nsec,
        "mtimeMs": stat.mtime_ms,
        "mtimeNsec": stat.mtime_nsec,
        "ctimeMs": stat.ctime_ms,
        "ctimeNsec": stat.ctime_nsec,
        "birthtimeMs": stat.birthtime_ms,
        "ino": stat.ino,
        "nlink": stat.nlink,
        "uid": stat.uid,
        "gid": stat.gid,
    })
}

fn wasi_path_stat_value(stat: agentos_kernel::vfs::VirtualStat) -> Value {
    let filetype = if stat.is_directory {
        agentos_kernel::fd_table::FILETYPE_DIRECTORY
    } else if stat.is_symbolic_link {
        agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
    } else {
        agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
    };
    wasi_stat_value(stat, filetype)
}

fn wasi_stat_value(stat: agentos_kernel::vfs::VirtualStat, filetype: u8) -> Value {
    json!({
        "dev": stat.dev,
        "ino": stat.ino,
        "filetype": filetype,
        "nlink": stat.nlink,
        "mode": stat.mode,
        "uid": stat.uid,
        "gid": stat.gid,
        "size": stat.size,
        "blocks": stat.blocks,
        "rdev": stat.rdev,
        "atimeMs": stat.atime_ms,
        "mtimeMs": stat.mtime_ms,
        "ctimeMs": stat.ctime_ms,
    })
}

fn time_spec(
    nanoseconds: Option<u64>,
    now: bool,
) -> Result<agentos_kernel::vfs::VirtualUtimeSpec, HostServiceError> {
    use agentos_kernel::vfs::{VirtualTimeSpec, VirtualUtimeSpec};
    if now {
        return Ok(VirtualUtimeSpec::Now);
    }
    let Some(nanoseconds) = nanoseconds else {
        return Ok(VirtualUtimeSpec::Omit);
    };
    let seconds = i64::try_from(nanoseconds / 1_000_000_000)
        .map_err(|_| HostServiceError::new("EINVAL", "timestamp exceeds i64 seconds"))?;
    VirtualTimeSpec::new(seconds, (nanoseconds % 1_000_000_000) as u32)
        .map(VirtualUtimeSpec::Set)
        .map_err(|error| HostServiceError::new("EINVAL", error.to_string()))
}

fn classify_extents(allocated: Vec<(u64, u64)>, unwritten: &[(u64, u64)]) -> Vec<(u64, u64, bool)> {
    let mut classified = Vec::new();
    for (start, end) in allocated {
        let mut cursor = start;
        for &(unwritten_start, unwritten_end) in unwritten {
            if unwritten_end <= cursor || unwritten_start >= end {
                continue;
            }
            if cursor < unwritten_start {
                classified.push((cursor, unwritten_start.min(end), false));
            }
            let overlap_start = cursor.max(unwritten_start);
            let overlap_end = end.min(unwritten_end);
            if overlap_start < overlap_end {
                classified.push((overlap_start, overlap_end, true));
                cursor = overlap_end;
            }
            if cursor == end {
                break;
            }
        }
        if cursor < end {
            classified.push((cursor, end, false));
        }
    }
    classified
}

#[allow(clippy::too_many_arguments)]
fn execute_xattr(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    target: MetadataTarget,
    path: Option<&BoundedString>,
    name: Option<&BoundedString>,
    value: Option<&BoundedBytes>,
    operation: XattrOperation,
    max_result_bytes: BoundedUsize,
) -> Result<Value, HostServiceError> {
    let pid = process.kernel_pid;
    let name = || {
        name.map(BoundedString::as_str)
            .ok_or_else(|| HostServiceError::new("EINVAL", "xattr name is required"))
    };
    let mut path_target = |dir_fd, follow| -> Result<(String, bool), HostServiceError> {
        Ok((
            resolve_path(kernel, process, dir_fd, required_path(path)?)?,
            follow,
        ))
    };
    match (target, operation) {
        (MetadataTarget::Descriptor(fd), XattrOperation::Get) => {
            let bytes = kernel
                .fd_get_xattr_for_process(EXECUTION_DRIVER_NAME, pid, fd, name()?)
                .map_err(kernel_host_error)?;
            if bytes.len() > max_result_bytes.get() {
                return Err(HostServiceError::limit(
                    "E2BIG",
                    "limits.reactor.maxBridgeResponseBytes",
                    max_result_bytes.get() as u64,
                    bytes.len() as u64,
                ));
            }
            Ok(host_bytes_value(&bytes))
        }
        (MetadataTarget::Descriptor(fd), XattrOperation::List) => Ok(json!(kernel
            .fd_list_xattrs_for_process(EXECUTION_DRIVER_NAME, pid, fd)
            .map_err(kernel_host_error)?)),
        (MetadataTarget::Descriptor(fd), XattrOperation::Set { flags }) => {
            kernel
                .fd_set_xattr_for_process(
                    EXECUTION_DRIVER_NAME,
                    pid,
                    fd,
                    name()?,
                    value
                        .ok_or_else(|| HostServiceError::new("EINVAL", "xattr value is required"))?
                        .as_slice()
                        .to_vec(),
                    flags,
                )
                .map_err(kernel_host_error)?;
            Ok(Value::Null)
        }
        (MetadataTarget::Descriptor(fd), XattrOperation::Remove) => {
            kernel
                .fd_remove_xattr_for_process(EXECUTION_DRIVER_NAME, pid, fd, name()?)
                .map_err(kernel_host_error)?;
            Ok(Value::Null)
        }
        (
            MetadataTarget::Path {
                dir_fd,
                follow_symlinks,
            },
            operation,
        ) => {
            let (path, follow) = path_target(dir_fd, follow_symlinks)?;
            match operation {
                XattrOperation::Get => {
                    let bytes = kernel
                        .get_xattr_for_process(EXECUTION_DRIVER_NAME, pid, &path, name()?, follow)
                        .map_err(kernel_host_error)?;
                    if bytes.len() > max_result_bytes.get() {
                        return Err(HostServiceError::limit(
                            "E2BIG",
                            "limits.reactor.maxBridgeResponseBytes",
                            max_result_bytes.get() as u64,
                            bytes.len() as u64,
                        ));
                    }
                    Ok(host_bytes_value(&bytes))
                }
                XattrOperation::List => Ok(json!(kernel
                    .list_xattrs_for_process(EXECUTION_DRIVER_NAME, pid, &path, follow,)
                    .map_err(kernel_host_error)?)),
                XattrOperation::Set { flags } => {
                    kernel
                        .set_xattr_for_process(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            name()?,
                            value
                                .ok_or_else(|| {
                                    HostServiceError::new("EINVAL", "xattr value is required")
                                })?
                                .as_slice()
                                .to_vec(),
                            flags,
                            follow,
                        )
                        .map_err(kernel_host_error)?;
                    Ok(Value::Null)
                }
                XattrOperation::Remove => {
                    kernel
                        .remove_xattr_for_process(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            &path,
                            name()?,
                            follow,
                        )
                        .map_err(kernel_host_error)?;
                    Ok(Value::Null)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::host_dispatch::inventory::{
        capability_family, semantic_rpc_inventory, HostCapabilityFamily,
    };
    use agentos_execution::backend::{
        DirectHostReplyTarget, HostCallIdentity, HostCallReply, PayloadLimit,
    };
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::socket_table::SocketType;
    use agentos_kernel::vfs::MemoryFileSystem;
    use base64::Engine as _;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingTarget {
        replies: Mutex<Vec<Result<HostCallReply, HostServiceError>>>,
    }

    impl DirectHostReplyTarget for RecordingTarget {
        fn claim(&self, _call_id: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _call_id: u64,
            _claimed: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            self.replies.lock().expect("reply lock").push(result);
            Ok(())
        }
    }

    fn test_runtime_context() -> agentos_runtime::RuntimeContext {
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .expect("create test runtime")
            .context()
    }

    fn direct_reply(
        target: Arc<RecordingTarget>,
        generation: u64,
        pid: u32,
        call_id: u64,
    ) -> DirectHostReplyHandle {
        DirectHostReplyHandle::new(
            HostCallIdentity {
                generation,
                pid,
                call_id,
            },
            target,
            64 * 1024,
        )
        .expect("direct reply")
    }

    fn bounded_string(value: &str) -> BoundedString {
        BoundedString::try_new(
            value.to_owned(),
            &PayloadLimit::new("testStringBytes", 4096).expect("string limit"),
        )
        .expect("bounded string")
    }

    fn kernel_process_at_tier(tier: ProcessPermissionTier) -> (SidecarKernel, KernelProcessHandle) {
        let mut config = KernelVmConfig::new(format!("vm-tier-{tier:?}"));
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register WASM driver");
        let handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    permission_tier: Some(tier),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn tier process");
        (kernel, handle)
    }

    #[test]
    fn centralized_tier_matrix_separates_preview1_and_host_process_operations() {
        let (isolated_kernel, isolated) = kernel_process_at_tier(ProcessPermissionTier::Isolated);
        let isolated_pid = isolated.pid();
        for operation in [
            HostOperation::Filesystem(FilesystemOperation::Pipe),
            HostOperation::Filesystem(FilesystemOperation::Duplicate { fd: 0 }),
            HostOperation::Filesystem(FilesystemOperation::DuplicateTo {
                fd: 0,
                target_fd: 3,
            }),
            HostOperation::Filesystem(FilesystemOperation::DuplicateMin { fd: 0, min_fd: 3 }),
            HostOperation::Filesystem(FilesystemOperation::DescriptorFdFlags { fd: 0 }),
            HostOperation::Clock(ClockOperation::Sleep { duration_ms: 1 }),
            HostOperation::Terminal(TerminalOperation::OpenPty),
            HostOperation::Signal(SignalOperation::Pending),
        ] {
            assert_eq!(
                super::authorize_host_operation(&isolated_kernel, isolated_pid, &operation)
                    .expect_err("isolated host-process operation must fail")
                    .code,
                "EACCES"
            );
        }
        for operation in [
            HostOperation::Filesystem(FilesystemOperation::Preopens),
            HostOperation::Filesystem(FilesystemOperation::Preopen { fd: 3 }),
            HostOperation::Filesystem(FilesystemOperation::Move {
                fd: 0,
                replaced_fd: None,
            }),
            HostOperation::Process(ProcessOperation::GetResourceLimit {
                kind: ResourceLimitKind::OpenFiles,
            }),
            HostOperation::Clock(ClockOperation::Resolution {
                clock: GuestClockId::Monotonic,
            }),
            HostOperation::Signal(SignalOperation::UpdateMask {
                how: SignalMaskHow::Block,
                set: SignalSetValue(0),
            }),
            HostOperation::Signal(SignalOperation::BeginDelivery),
        ] {
            super::authorize_host_operation(&isolated_kernel, isolated_pid, &operation)
                .expect("Preview1/internal operation remains available");
        }

        for tier in [
            ProcessPermissionTier::ReadOnly,
            ProcessPermissionTier::ReadWrite,
        ] {
            let (kernel, handle) = kernel_process_at_tier(tier);
            let pid = handle.pid();
            super::authorize_host_operation(
                &kernel,
                pid,
                &HostOperation::Filesystem(FilesystemOperation::DuplicateMin { fd: 0, min_fd: 3 }),
            )
            .expect("limited fd_dup_min remains available");
            for operation in [
                HostOperation::Filesystem(FilesystemOperation::Duplicate { fd: 0 }),
                HostOperation::Filesystem(FilesystemOperation::DuplicateTo {
                    fd: 0,
                    target_fd: 3,
                }),
                HostOperation::Filesystem(FilesystemOperation::Pipe),
                HostOperation::Clock(ClockOperation::RealIntervalGet),
                HostOperation::Terminal(TerminalOperation::OpenPty),
            ] {
                assert_eq!(
                    super::authorize_host_operation(&kernel, pid, &operation)
                        .expect_err("full-only operation must fail")
                        .code,
                    "EACCES"
                );
            }
        }
    }

    #[test]
    fn real_wasi_dirfd_and_explicit_zero_rights_cannot_be_waived_by_absolute_paths() {
        let (mut kernel, handle) = kernel_process_at_tier(ProcessPermissionTier::Full);
        let pid = handle.pid();
        kernel
            .mkdir("/workspace", true)
            .expect("create default workspace preopen");
        kernel
            .mkdir("/cap", true)
            .expect("create capability directory");
        kernel
            .mkdir("/outside", true)
            .expect("create outside directory");
        let root = kernel
            .initialize_wasi_preopens(EXECUTION_DRIVER_NAME, pid)
            .expect("initialize preopens")
            .into_iter()
            .find(|entry| entry.guest_path == "/")
            .expect("root preopen");
        let zero_dir = kernel
            .fd_open_with_rights(
                EXECUTION_DRIVER_NAME,
                pid,
                Some(root.fd),
                "/cap",
                agentos_kernel::fd_table::O_DIRECTORY,
                None,
                Some((0, 0)),
            )
            .expect("open explicit-zero directory");
        let open = HostOperation::Filesystem(FilesystemOperation::OpenAt {
            dir_fd: zero_dir,
            path: bounded_string("/outside"),
            options: GuestOpenSpec {
                flags: agentos_kernel::fd_table::O_DIRECTORY,
                mode: None,
                rights: GuestOpenRights::Explicit {
                    base: 0,
                    inheriting: 0,
                },
            },
        });
        assert_eq!(
            super::authorize_host_operation(&kernel, pid, &open)
                .expect_err("absolute spelling cannot waive dirfd rights")
                .code,
            "EACCES"
        );

        let metadata = HostOperation::Filesystem(FilesystemOperation::SetMode {
            target: MetadataTarget::Descriptor(zero_dir),
            path: None,
            mode: 0o700,
        });
        assert_eq!(
            super::authorize_host_operation(&kernel, pid, &metadata)
                .expect_err("explicit-zero fd cannot mutate metadata")
                .code,
            "EACCES"
        );
    }

    #[test]
    fn pipe_and_socket_metadata_rights_survive_dup_and_process_inheritance() {
        let (mut kernel, parent) = kernel_process_at_tier(ProcessPermissionTier::Full);
        let parent_pid = parent.pid();
        let (pipe_read, _pipe_write) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, parent_pid)
            .expect("create pipe");
        let (socket_left, _socket_right) = kernel
            .fd_socketpair(
                EXECUTION_DRIVER_NAME,
                parent_pid,
                SocketType::Stream,
                false,
                false,
            )
            .expect("create socket pair");
        let pipe_alias = kernel
            .fd_dup(EXECUTION_DRIVER_NAME, parent_pid, pipe_read)
            .expect("duplicate pipe");
        let socket_alias = kernel
            .fd_dup(EXECUTION_DRIVER_NAME, parent_pid, socket_left)
            .expect("duplicate socket");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent_pid),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn child with inherited descriptors");

        for (pid, fd, resource) in [
            (parent_pid, pipe_read, "pipe"),
            (parent_pid, pipe_alias, "duplicated pipe"),
            (parent_pid, socket_left, "socket"),
            (parent_pid, socket_alias, "duplicated socket"),
            (child.pid(), pipe_read, "inherited pipe"),
            (child.pid(), socket_left, "inherited socket"),
        ] {
            for operation in [
                HostOperation::Filesystem(FilesystemOperation::SetMode {
                    target: MetadataTarget::Descriptor(fd),
                    path: None,
                    mode: 0o640,
                }),
                HostOperation::Filesystem(FilesystemOperation::SetOwner {
                    target: MetadataTarget::Descriptor(fd),
                    path: None,
                    uid: None,
                    gid: None,
                }),
            ] {
                super::authorize_host_operation(&kernel, pid, &operation)
                    .unwrap_or_else(|error| panic!("authorize {resource} metadata: {error}"));
            }

            let stat = kernel
                .dev_fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                .unwrap_or_else(|error| panic!("stat {resource}: {error}"));
            kernel
                .fd_chmod_for_process(EXECUTION_DRIVER_NAME, pid, fd, 0o640)
                .unwrap_or_else(|error| panic!("fchmod {resource}: {error}"));
            kernel
                .fd_chown_for_process(EXECUTION_DRIVER_NAME, pid, fd, stat.uid, stat.gid)
                .unwrap_or_else(|error| panic!("fchown {resource}: {error}"));
        }
    }

    #[test]
    fn limited_tiers_cannot_use_synthesized_pipe_or_socket_metadata_rights() {
        for tier in [
            ProcessPermissionTier::ReadOnly,
            ProcessPermissionTier::Isolated,
        ] {
            let (mut kernel, process) = kernel_process_at_tier(tier);
            let pid = process.pid();
            let (pipe_read, _pipe_write) = kernel
                .open_pipe(EXECUTION_DRIVER_NAME, pid)
                .expect("create pipe directly for authorization test");
            let (socket_left, _socket_right) = kernel
                .fd_socketpair(EXECUTION_DRIVER_NAME, pid, SocketType::Stream, false, false)
                .expect("create socket pair directly for authorization test");

            for fd in [pipe_read, socket_left] {
                for operation in [
                    HostOperation::Filesystem(FilesystemOperation::SetMode {
                        target: MetadataTarget::Descriptor(fd),
                        path: None,
                        mode: 0o640,
                    }),
                    HostOperation::Filesystem(FilesystemOperation::SetOwner {
                        target: MetadataTarget::Descriptor(fd),
                        path: None,
                        uid: None,
                        gid: None,
                    }),
                ] {
                    assert_eq!(
                        super::authorize_host_operation(&kernel, pid, &operation)
                            .expect_err("limited tier must deny descriptor metadata mutation")
                            .code,
                        "EROFS"
                    );
                }
            }
        }
    }

    #[test]
    fn sufficient_wasi_dirfd_rights_still_confine_absolute_paths() {
        let (mut kernel, handle) = kernel_process_at_tier(ProcessPermissionTier::Full);
        let pid = handle.pid();
        kernel
            .mkdir("/workspace", true)
            .expect("create workspace capability directory");
        kernel
            .mkdir("/outside", true)
            .expect("create outside directory");
        let cap_fd = kernel
            .initialize_canonical_wasi_preopens(EXECUTION_DRIVER_NAME, pid)
            .expect("initialize canonical preopens")
            .into_iter()
            .find(|entry| entry.guest_path == "/workspace")
            .expect("workspace preopen")
            .fd;
        let process = ActiveProcess::new(
            pid,
            handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        )
        .with_guest_cwd(String::from("/workspace"));
        let operation = HostOperation::Filesystem(FilesystemOperation::OpenAt {
            dir_fd: cap_fd,
            path: bounded_string("/outside"),
            options: GuestOpenSpec {
                flags: agentos_kernel::fd_table::O_DIRECTORY,
                mode: None,
                rights: GuestOpenRights::Explicit {
                    base: 0,
                    inheriting: 0,
                },
            },
        });
        super::authorize_host_operation(&kernel, pid, &operation)
            .expect("directory has sufficient PATH_OPEN rights");
        assert_eq!(
            resolve_path(&mut kernel, &process, cap_fd, "/outside")
                .expect_err("absolute path cannot escape real WASI dirfd")
                .code,
            "EACCES"
        );
        assert_eq!(
            resolve_path(&mut kernel, &process, cap_fd, "child")
                .expect("relative path remains confined"),
            "/workspace/child"
        );
        assert_eq!(
            resolve_path(
                &mut kernel,
                &process,
                agentos_kernel::fd_table::WASI_HIDDEN_PREOPEN_FD_TAG | cap_fd,
                "/outside"
            )
            .expect_err("private libc preopen retains capability confinement")
            .code,
            "EACCES"
        );
        assert_eq!(
            resolve_path(
                &mut kernel,
                &process,
                agentos_kernel::fd_table::WASI_HIDDEN_PREOPEN_FD_TAG | cap_fd,
                "child",
            )
            .expect("private libc preopen resolves from its capability root"),
            "/workspace/child"
        );
        assert_eq!(
            resolve_path(&mut kernel, &process, NODE_CWD_FD, "child")
                .expect("extension cwd sentinel resolves from process cwd"),
            "/workspace/child"
        );
        for at_fdcwd in [(-2_i32) as u32, (-100_i32) as u32] {
            assert_eq!(
                resolve_path(&mut kernel, &process, at_fdcwd, "child")
                    .expect("libc AT_FDCWD resolves from process cwd"),
                "/workspace/child"
            );
        }
        let cwd_metadata = HostOperation::Filesystem(FilesystemOperation::SetMode {
            target: MetadataTarget::Path {
                dir_fd: NODE_CWD_FD,
                follow_symlinks: true,
            },
            path: Some(bounded_string("child")),
            mode: 0o600,
        });
        super::authorize_host_operation(&kernel, pid, &cwd_metadata)
            .expect("cwd sentinel bypasses descriptor-right lookup");
    }

    fn request(method: &str) -> HostRpcRequest {
        let args = match method {
            "__kernel_stdin_read" => vec![json!(1), json!(0)],
            "__kernel_stdio_write" => vec![json!(1), json!("x")],
            "fs.accessSync" => vec![json!("/"), json!(0), json!(false)],
            "fs.chmodForProcessSync" => vec![json!("/x"), json!(0o644)],
            "fs.chownSync" | "fs.lchownSync" => {
                vec![json!("/x"), json!(1000), json!(1000)]
            }
            "fs.truncateForProcessSync" => vec![json!("/x"), json!(0)],
            "fs.fallocateSync"
            | "fs.insertRangeSync"
            | "fs.collapseRangeSync"
            | "fs.punchHoleSync" => vec![json!(3), json!(0), json!(1)],
            "fs.zeroRangeSync" => vec![json!(3), json!(0), json!(1), json!(1)],
            "fs.fgetxattrSync" | "fs.fremovexattrSync" => vec![json!(3), json!("user.x")],
            "fs.flistxattrSync" | "fs.fiemapSync" | "fs.namedFifoPeerReadySync" => vec![json!(3)],
            "fs.fiemapAtSync" => vec![json!(3), json!(0)],
            "fs.fsetxattrSync" => vec![json!(3), json!("user.x"), json!("x"), json!(0)],
            "fs.getxattrSync" | "fs.removexattrSync" => {
                vec![json!("/x"), json!("user.x"), json!(true)]
            }
            "fs.listxattrSync" => vec![json!("/x"), json!(true)],
            "fs.setxattrSync" => vec![
                json!("/x"),
                json!("user.x"),
                json!("x"),
                json!(0),
                json!(true),
            ],
            "fs.linkFdSync" => vec![json!(3), json!("/x")],
            "fs.mknodSync" => vec![json!("/x"), json!(0o644), json!(0)],
            "fs.openTmpfileSync" => {
                vec![json!("/tmp"), json!(0), json!(0o600), json!(true)]
            }
            "fs.remountSync" => vec![json!("/"), json!("rw")],
            "fs.renameAt2Sync" => vec![json!("/a"), json!("/b"), json!(0)],
            "fs.statSync" | "fs.statfsSync" => vec![json!("/")],
            "process.fd_chdir_path"
            | "process.fd_close"
            | "process.fd_closefrom"
            | "process.fd_dup"
            | "process.fd_filestat"
            | "process.fd_getfd"
            | "process.fd_move"
            | "process.fd_path"
            | "process.fd_preopen"
            | "process.fd_stat" => vec![json!(3)],
            "process.fd_dup_min" => vec![json!(3), json!(4)],
            "process.fd_dup2" => vec![json!(3), json!(4)],
            "process.fd_chmod"
            | "process.fd_flock"
            | "process.fd_set_flags"
            | "process.fd_setfd" => vec![json!(3), json!(0)],
            "process.fd_chown" => vec![json!(3), json!(1), json!(1)],
            "process.fd_datasync" | "process.fd_sync" => vec![json!(3)],
            "process.fd_open" => vec![json!("/x"), json!(0), Value::Null, json!("0"), json!("0")],
            "process.fd_pipe" | "process.fd_preopens" | "process.fd_record_lock_cancel" => vec![],
            "process.fd_pread" => vec![json!(3), json!(1), json!("0")],
            "process.fd_pwrite" => vec![json!(3), json!("x"), json!("0")],
            "process.fd_read" => vec![json!(3), json!(1), json!(0)],
            "process.fd_readdir" => vec![json!(3), json!("0"), json!(1)],
            "process.fd_record_lock" => {
                vec![json!(3), json!(12), json!(0), json!("0"), json!("1")]
            }
            "process.fd_seek" => vec![json!(3), json!("0"), json!(0)],
            "process.fd_truncate" => vec![json!(3), json!("0")],
            "process.fd_utimes" => vec![json!(3), json!("0"), json!("0"), json!(5)],
            "process.fd_write" => vec![json!(3), json!("x")],
            "process.path_chmod_at" => vec![json!(3), json!("x"), json!(0o644)],
            "process.path_chown_at" => {
                vec![json!(3), json!("x"), json!(1), json!(1), json!(true)]
            }
            "process.path_link_at" => {
                vec![json!(3), json!("a"), json!(3), json!("b"), json!(false)]
            }
            "process.path_mkdir_at"
            | "process.path_readlink_at"
            | "process.path_remove_dir_at"
            | "process.path_unlink_at" => vec![json!(3), json!("x")],
            "process.path_open_at" => vec![
                json!(3),
                json!("x"),
                json!(0),
                Value::Null,
                json!("0"),
                json!("0"),
            ],
            "process.path_rename_at" => {
                vec![json!(3), json!("a"), json!(3), json!("b")]
            }
            "process.path_stat_at" => vec![json!(3), json!("x"), json!(true)],
            "process.path_statfs_at" => vec![json!(3), json!("x")],
            "process.path_symlink_at" => vec![json!("a"), json!(3), json!("b")],
            "process.path_utimes_at" => vec![
                json!(3),
                json!("x"),
                json!(true),
                json!("0"),
                json!("0"),
                json!(5),
            ],
            other => panic!("missing filesystem fixture for {other}"),
        };
        HostRpcRequest {
            id: 1,
            method: method.to_owned(),
            args,
            raw_bytes_args: HashMap::new(),
        }
    }

    #[test]
    fn every_frozen_filesystem_rpc_decodes_to_a_typed_filesystem_operation() {
        for method in semantic_rpc_inventory()
            .into_iter()
            .filter(|method| capability_family(method) == Some(HostCapabilityFamily::Filesystem))
        {
            let decoded = super::super::decode_host_operation(&request(method), true, 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {method}: {error}"));
            assert!(
                matches!(decoded, Some(HostOperation::Filesystem(_))),
                "{method} must not fall through to the legacy bridge"
            );
        }
    }

    #[test]
    fn closefrom_decoder_preserves_exact_canonical_targets() {
        let mut request = request("process.fd_closefrom");
        request.args[0] = json!(64);
        request.args.push(json!([3, 7, 3]));
        assert!(matches!(
            super::super::decode_host_operation(&request, true, 1024)
                .expect("decode closefrom")
                .expect("typed closefrom"),
            HostOperation::Filesystem(FilesystemOperation::CloseFrom {
                min_fd: 64,
                exact_fds: Some(fds),
            }) if fds.as_slice() == [3, 7, 3]
        ));
    }

    #[test]
    fn wrapped_only_filesystem_aliases_preserve_linux_semantics() {
        let decode = |method| {
            super::super::decode_host_operation(&request(method), true, 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {method}: {error}"))
                .unwrap_or_else(|| panic!("{method} fell through to the legacy bridge"))
        };

        assert!(matches!(
            decode("fs.chmodForProcessSync"),
            HostOperation::Filesystem(FilesystemOperation::SetMode {
                target: MetadataTarget::Path {
                    dir_fd: NODE_CWD_FD,
                    follow_symlinks: true,
                },
                mode: 0o644,
                ..
            })
        ));
        assert!(matches!(
            decode("fs.chownSync"),
            HostOperation::Filesystem(FilesystemOperation::SetOwner {
                target: MetadataTarget::Path {
                    dir_fd: NODE_CWD_FD,
                    follow_symlinks: true,
                },
                uid: Some(1000),
                gid: Some(1000),
                ..
            })
        ));
        assert!(matches!(
            decode("fs.lchownSync"),
            HostOperation::Filesystem(FilesystemOperation::SetOwner {
                target: MetadataTarget::Path {
                    dir_fd: NODE_CWD_FD,
                    follow_symlinks: false,
                },
                uid: Some(1000),
                gid: Some(1000),
                ..
            })
        ));
        assert!(matches!(
            decode("fs.truncateForProcessSync"),
            HostOperation::Filesystem(FilesystemOperation::SetPathLength {
                dir_fd: NODE_CWD_FD,
                length: 0,
                ..
            })
        ));
        assert!(matches!(
            decode("process.fd_dup2"),
            HostOperation::Filesystem(FilesystemOperation::DuplicateTo {
                fd: 3,
                target_fd: 4,
            })
        ));
    }

    #[test]
    fn decoder_rejects_paths_and_payloads_before_constructing_operations() {
        let mut overlong_path = request("fs.statSync");
        overlong_path.args[0] = json!("x".repeat(MAX_PATH_BYTES + 1));
        assert_eq!(
            super::super::decode_host_operation(&overlong_path, true, 1024)
                .expect_err("overlong path")
                .code(),
            Some("ENAMETOOLONG")
        );

        let oversized_read = HostRpcRequest {
            args: vec![json!(3), json!(9), json!(0)],
            ..request("process.fd_read")
        };
        assert_eq!(
            super::super::decode_host_operation(&oversized_read, true, 8)
                .expect_err("oversized read")
                .code(),
            Some("E2BIG")
        );

        let oversized_write = HostRpcRequest {
            args: vec![json!(3), json!("123456789")],
            ..request("process.fd_write")
        };
        assert_eq!(
            super::super::decode_host_operation(&oversized_write, true, 8)
                .expect_err("oversized write")
                .code(),
            Some("E2BIG")
        );
    }

    #[test]
    fn fd_write_decodes_the_v8_cbor_buffer_projection() {
        let request = HostRpcRequest {
            args: vec![json!(7), json!({ "__type": "Buffer", "data": "aGVsbG8=" })],
            ..request("process.fd_write")
        };
        let Some(HostOperation::Filesystem(FilesystemOperation::Write {
            fd, bytes, offset, ..
        })) = super::super::decode_host_operation(&request, true, 1024)
            .expect("canonical V8 buffer must decode")
        else {
            panic!("fd_write did not decode as a filesystem write")
        };

        assert_eq!(fd, 7);
        assert_eq!(bytes.as_slice(), b"hello");
        assert_eq!(offset, None);
    }

    #[test]
    fn fd_write_rejects_malformed_v8_cbor_buffer_base64_with_typed_einval() {
        let request = HostRpcRequest {
            args: vec![
                json!(7),
                json!({ "__type": "Buffer", "data": "not base64!" }),
            ],
            ..request("process.fd_write")
        };

        assert_eq!(
            super::super::decode_host_operation(&request, true, 1024)
                .expect_err("malformed canonical V8 buffer")
                .code(),
            Some("EINVAL")
        );
    }

    #[test]
    fn completed_timed_fd_read_preserves_kernel_eof() {
        assert_eq!(
            complete_timed_fd_read(None).expect("kernel EOF must remain successful"),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn typed_stdin_read_refills_accepted_input_beyond_pipe_capacity() {
        let mut config = KernelVmConfig::new("vm-typed-stdin-refill");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register WASM driver");
        let kernel_handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn typed-stdin process");
        let pid = kernel_handle.pid();
        let identity = kernel_handle.runtime_identity();
        let writer_fd = install_kernel_stdin_pipe(&mut kernel, pid).expect("install stdin pipe");
        let mut process = ActiveProcess::new(
            pid,
            kernel_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        )
        .with_kernel_stdin_writer_fd(writer_fd);

        // Three pipe capacities ensure that correctness depends on multiple
        // typed read admissions refilling the bounded owner-side backlog.
        let payload = (0..(3 * 64 * 1024 + 37))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        write_kernel_process_stdin(&mut kernel, &mut process, &payload)
            .expect("accept oversized stdin payload");
        assert!(
            process.pending_kernel_stdin.total > 0,
            "payload must exceed the live kernel pipe and exercise owner backlog refill"
        );
        close_kernel_process_stdin(&mut kernel, &mut process)
            .expect("defer stdin close until accepted bytes drain");
        assert!(process.pending_kernel_stdin.close_requested);

        let runtime = process.runtime_context.clone();
        let notify = Arc::new(tokio::sync::Notify::new());
        let target = Arc::new(RecordingTarget::default());
        let response_limit =
            PayloadLimit::new("testStdinReadBytes", 64 * 1024).expect("stdin response limit");
        let maximum =
            BoundedUsize::try_new(16 * 1024, &response_limit).expect("bounded stdin read");
        let stdin_reader_fd = process.kernel_stdin_reader_fd;
        let mut observed = Vec::with_capacity(payload.len());
        let mut call_id = 1_u64;

        loop {
            let reply_count = target.replies.lock().expect("stdin reply lock").len();
            service_deferred_kernel_read_with_response(
                identity.generation,
                &runtime,
                kernel.poll_wait_handle(),
                Arc::clone(&notify),
                &mut kernel,
                &mut process,
                Some((
                    stdin_reader_fd,
                    maximum,
                    DeferredKernelReadResponse::KernelStdin,
                    Instant::now(),
                    direct_reply(Arc::clone(&target), identity.generation, pid, call_id),
                )),
            )
            .expect("service typed stdin read");

            let replies = target.replies.lock().expect("stdin reply lock");
            assert_eq!(
                replies.len(),
                reply_count + 1,
                "each ready HostCall must be pumped and settled exactly once"
            );
            let HostCallReply::Json(value) = replies
                .last()
                .expect("every ready stdin admission replies")
                .as_ref()
                .expect("stdin read succeeds")
            else {
                panic!("stdin reply must be JSON")
            };
            if value.get("done").and_then(Value::as_bool) == Some(true) {
                break;
            }
            let encoded = value
                .get("dataBase64")
                .and_then(Value::as_str)
                .expect("accepted input must be readable before EOF");
            observed.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .expect("decode typed stdin response"),
            );
            drop(replies);
            call_id += 1;
            assert!(
                call_id < 64,
                "bounded refill state machine must reach EOF without spinning"
            );
        }

        assert!(call_id > 2, "the regression must cross multiple HostCalls");
        assert_eq!(observed, payload);
        assert_eq!(process.pending_kernel_stdin.total, 0);
        assert!(!process.pending_kernel_stdin.close_requested);
        assert!(process.kernel_stdin_writer_fd.is_none());
        process.kernel_handle.finish(0);
    }

    #[test]
    fn managed_parent_read_parks_until_inherited_child_writer_progresses() {
        let mut config = KernelVmConfig::new("vm-deferred-managed-parent-read");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register WASM driver");
        let parent_handle = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn managed parent");
        let parent_pid = parent_handle.pid();
        let identity = parent_handle.runtime_identity();
        let mut parent = ActiveProcess::new(
            parent_pid,
            parent_handle,
            test_runtime_context(),
            crate::limits::VmLimits::default(),
            agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
            GuestRuntimeKind::WebAssembly,
            ActiveExecution::Binding(BindingExecution::default()),
        );
        let (read_fd, write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, parent_pid)
            .expect("open parent pipe");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent_pid),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn child inheriting pipe descriptions");
        let child_pid = child.pid();
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent_pid, write_fd)
            .expect("parent closes writer before reading");

        let runtime = parent.runtime_context.clone();
        let notify = Arc::new(tokio::sync::Notify::new());
        let target = Arc::new(RecordingTarget::default());
        let read_limit = PayloadLimit::new("testReadBytes", 64).expect("read limit");
        let maximum = BoundedUsize::try_new(64, &read_limit).expect("bounded read");
        service_deferred_kernel_read(
            identity.generation,
            &runtime,
            kernel.poll_wait_handle(),
            Arc::clone(&notify),
            &mut kernel,
            &mut parent,
            Some((
                read_fd,
                maximum,
                Instant::now() + Duration::from_secs(1),
                direct_reply(Arc::clone(&target), identity.generation, parent_pid, 1),
            )),
        )
        .expect("park parent read");
        assert!(parent.deferred_kernel_read.is_some());
        assert!(target.replies.lock().expect("reply lock").is_empty());

        kernel
            .fd_write(EXECUTION_DRIVER_NAME, child_pid, write_fd, b"child-payload")
            .expect("child writes inherited pipe");
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, child_pid, write_fd)
            .expect("child closes inherited writer");
        if let Some(task) = parent
            .deferred_kernel_read
            .as_mut()
            .and_then(|read| read.wake_task.take())
        {
            task.abort();
        }
        service_deferred_kernel_read(
            identity.generation,
            &runtime,
            kernel.poll_wait_handle(),
            Arc::clone(&notify),
            &mut kernel,
            &mut parent,
            None,
        )
        .expect("complete parent read after child progress");

        let replies = target.replies.lock().expect("reply lock");
        assert_eq!(replies.len(), 1);
        let HostCallReply::Json(payload) = replies[0].as_ref().expect("successful read reply")
        else {
            panic!("read reply must be JSON")
        };
        assert_eq!(
            javascript_sync_rpc_bytes_arg(std::slice::from_ref(payload), 0, "read reply")
                .expect("decode read reply"),
            b"child-payload"
        );
        drop(replies);

        let eof_target = Arc::new(RecordingTarget::default());
        service_deferred_kernel_read(
            identity.generation,
            &runtime,
            kernel.poll_wait_handle(),
            notify,
            &mut kernel,
            &mut parent,
            Some((
                read_fd,
                maximum,
                Instant::now() + Duration::from_secs(1),
                direct_reply(Arc::clone(&eof_target), identity.generation, parent_pid, 2),
            )),
        )
        .expect("observe EOF after child close");
        let replies = eof_target.replies.lock().expect("EOF reply lock");
        let HostCallReply::Json(payload) = replies[0].as_ref().expect("successful EOF reply")
        else {
            panic!("EOF reply must be JSON")
        };
        assert!(
            javascript_sync_rpc_bytes_arg(std::slice::from_ref(payload), 0, "EOF reply")
                .expect("decode EOF reply")
                .is_empty()
        );
        drop(replies);

        let (timeout_read_fd, timeout_write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, parent_pid)
            .expect("open timeout pipe");
        let timeout_target = Arc::new(RecordingTarget::default());
        service_deferred_kernel_read(
            identity.generation,
            &runtime,
            kernel.poll_wait_handle(),
            Arc::new(tokio::sync::Notify::new()),
            &mut kernel,
            &mut parent,
            Some((
                timeout_read_fd,
                BoundedUsize::try_new(64, &read_limit).expect("bounded timeout read"),
                Instant::now(),
                direct_reply(
                    Arc::clone(&timeout_target),
                    identity.generation,
                    parent_pid,
                    3,
                ),
            )),
        )
        .expect("settle expired not-ready read");
        let replies = timeout_target.replies.lock().expect("timeout reply lock");
        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0]
                .as_ref()
                .expect_err("expired not-ready read must fail")
                .code,
            "EAGAIN"
        );
        drop(replies);
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent_pid, timeout_read_fd)
            .expect("close timeout reader");
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent_pid, timeout_write_fd)
            .expect("close timeout writer");

        let (nonblocking_read_fd, nonblocking_write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, parent_pid)
            .expect("open nonblocking pipe");
        kernel
            .fd_fcntl(
                EXECUTION_DRIVER_NAME,
                parent_pid,
                nonblocking_read_fd,
                agentos_kernel::fd_table::F_SETFL,
                agentos_kernel::fd_table::O_NONBLOCK,
            )
            .expect("mark pipe reader nonblocking");
        let nonblocking_target = Arc::new(RecordingTarget::default());
        service_deferred_kernel_read(
            identity.generation,
            &runtime,
            kernel.poll_wait_handle(),
            Arc::new(tokio::sync::Notify::new()),
            &mut kernel,
            &mut parent,
            Some((
                nonblocking_read_fd,
                BoundedUsize::try_new(64, &read_limit).expect("bounded nonblocking read"),
                Instant::now() + Duration::from_secs(1),
                direct_reply(
                    Arc::clone(&nonblocking_target),
                    identity.generation,
                    parent_pid,
                    4,
                ),
            )),
        )
        .expect("settle nonblocking not-ready read");
        assert!(
            parent.deferred_kernel_read.is_none(),
            "O_NONBLOCK must never park a descriptor read"
        );
        let replies = nonblocking_target
            .replies
            .lock()
            .expect("nonblocking reply lock");
        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0]
                .as_ref()
                .expect_err("nonblocking not-ready read must fail")
                .code,
            "EAGAIN"
        );
        drop(replies);
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent_pid, nonblocking_read_fd)
            .expect("close nonblocking reader");
        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent_pid, nonblocking_write_fd)
            .expect("close nonblocking writer");

        child.finish(0);
        parent.kernel_handle.finish(0);
    }

    #[test]
    fn every_side_effecting_filesystem_rpc_requires_reply_claim_first() {
        let side_effecting = [
            "__kernel_stdin_read",
            "__kernel_stdio_write",
            "fs.collapseRangeSync",
            "fs.chmodForProcessSync",
            "fs.chownSync",
            "fs.fallocateSync",
            "fs.fremovexattrSync",
            "fs.fsetxattrSync",
            "fs.insertRangeSync",
            "fs.lchownSync",
            "fs.linkFdSync",
            "fs.mknodSync",
            "fs.openTmpfileSync",
            "fs.punchHoleSync",
            "fs.remountSync",
            "fs.removexattrSync",
            "fs.renameAt2Sync",
            "fs.setxattrSync",
            "fs.truncateForProcessSync",
            "fs.zeroRangeSync",
            "process.fd_chmod",
            "process.fd_chown",
            "process.fd_close",
            "process.fd_closefrom",
            "process.fd_datasync",
            "process.fd_dup",
            "process.fd_dup2",
            "process.fd_dup_min",
            "process.fd_flock",
            "process.fd_move",
            "process.fd_open",
            "process.fd_pipe",
            "process.fd_preopen",
            "process.fd_preopens",
            "process.fd_pwrite",
            "process.fd_read",
            "process.fd_pread",
            "process.fd_record_lock",
            "process.fd_record_lock_cancel",
            "process.fd_seek",
            "process.fd_set_flags",
            "process.fd_setfd",
            "process.fd_sync",
            "process.fd_truncate",
            "process.fd_utimes",
            "process.fd_write",
            "process.path_chmod_at",
            "process.path_chown_at",
            "process.path_link_at",
            "process.path_mkdir_at",
            "process.path_open_at",
            "process.path_remove_dir_at",
            "process.path_rename_at",
            "process.path_symlink_at",
            "process.path_unlink_at",
            "process.path_utimes_at",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        for method in semantic_rpc_inventory()
            .into_iter()
            .filter(|method| capability_family(method) == Some(HostCapabilityFamily::Filesystem))
        {
            let Some(HostOperation::Filesystem(operation)) =
                super::super::decode_host_operation(&request(method), true, 1024 * 1024)
                    .expect("decode")
            else {
                panic!("{method} did not decode as filesystem")
            };
            assert_eq!(
                FilesystemCapability::requires_claim(&operation),
                side_effecting.contains(method),
                "claim classification drift for {method}"
            );
        }
    }
}
