use super::*;
use agentos_kernel::kernel::{WaitPidEvent, WaitPidFlags};
use agentos_kernel::process_runtime::ProcessExit;
use agentos_kernel::process_table::{ProcessResourceLimit, ProcessResourceLimitKind};
use agentos_kernel::resource_accounting::{
    DEFAULT_MAX_PROCESS_ARGV_BYTES, DEFAULT_MAX_PROCESS_ENV_BYTES,
};

pub(super) struct ProcessCapability;

impl SidecarHostCapability<ProcessOperation> for ProcessCapability {
    fn requires_claim(operation: &ProcessOperation) -> bool {
        matches!(
            operation,
            ProcessOperation::Spawn(_)
                | ProcessOperation::RunCaptured { .. }
                | ProcessOperation::Exec(_)
                | ProcessOperation::OpenExecutableImage { .. }
                | ProcessOperation::CloseExecutableImage { .. }
                | ProcessOperation::PollChild { .. }
                | ProcessOperation::WriteChildStdin { .. }
                | ProcessOperation::CloseChildStdin { .. }
                | ProcessOperation::SetResourceLimit { .. }
                | ProcessOperation::Umask { new_mask: Some(_) }
                | ProcessOperation::SetProcessGroup { .. }
                | ProcessOperation::Kill { .. }
                | ProcessOperation::Wait { .. }
                | ProcessOperation::WaitTransition { .. }
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: ProcessOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let value = match operation {
            ProcessOperation::OpenExecutableImage { source } => {
                if process.executable_image.is_some() {
                    return Err(HostServiceError::new(
                        "EBUSY",
                        "this process already owns an executable-image snapshot",
                    ));
                }
                let image = match source {
                    ExecutableImageSource::TrustedInitialPath(path) => kernel
                        .load_trusted_initial_runtime_image(
                            path.as_str(),
                            process.limits.wasm.max_module_file_bytes,
                        ),
                    ExecutableImageSource::Path(path) => {
                        let path = if path.as_str().starts_with('/') {
                            normalize_path(path.as_str())
                        } else {
                            normalize_path(&format!("{}/{}", process.guest_cwd, path.as_str()))
                        };
                        kernel.load_process_runtime_image(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            &path,
                            process.limits.wasm.max_module_file_bytes,
                        )
                    }
                    ExecutableImageSource::Descriptor(fd) => kernel
                        .load_process_runtime_image_from_fd(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            fd,
                            process.limits.wasm.max_module_file_bytes,
                        ),
                }
                .map_err(kernel_host_error)?;
                let size = image.bytes.len();
                let mode = image.mode;
                let canonical_path = image.canonical_path;
                let retained_bytes = process
                    .runtime_context
                    .resources()
                    .reserve(ResourceClass::ExecutorBytes, size)
                    .map_err(executable_image_limit_error)?;
                let handle = process.install_executable_image(image.bytes, retained_bytes)?;
                json!({
                    "handle": handle.to_string(),
                    "canonicalPath": canonical_path,
                    "size": size,
                    "mode": mode,
                })
            }
            ProcessOperation::ReadExecutableImage {
                handle,
                offset,
                max_bytes,
            } => {
                let bytes = process.read_executable_image(handle, offset, max_bytes.get())?;
                return Ok(HostCallReply::Json(host_bytes_value(bytes)));
            }
            ProcessOperation::CloseExecutableImage { handle } => {
                process.close_executable_image(handle)?;
                Value::Null
            }
            ProcessOperation::GetImage { max_reply_bytes } => {
                let image = bounded_committed_process_image(
                    kernel,
                    process.kernel_pid,
                    max_reply_bytes.get(),
                )?;
                json!({
                    "argv": image
                        .argv
                        .into_vec()
                        .into_iter()
                        .map(BoundedString::into_string)
                        .collect::<Vec<_>>(),
                    "env": image
                        .env
                        .into_vec()
                        .into_iter()
                        .map(|(key, value)| {
                            vec![key.into_string(), value.into_string()]
                        })
                        .collect::<Vec<_>>(),
                })
            }
            ProcessOperation::GetPid => json!(kernel
                .getpid(EXECUTION_DRIVER_NAME, process.kernel_pid)
                .map_err(kernel_host_error)?),
            ProcessOperation::GetParentPid => json!(kernel
                .getppid(EXECUTION_DRIVER_NAME, process.kernel_pid)
                .map_err(kernel_host_error)?),
            ProcessOperation::GetProcessGroup { pid } => json!(kernel
                .getpgid(EXECUTION_DRIVER_NAME, pid.unwrap_or(process.kernel_pid),)
                .map_err(kernel_host_error)?),
            ProcessOperation::SetProcessGroup { pid, pgid } => {
                let pid = pid.unwrap_or(process.kernel_pid);
                kernel
                    .setpgid(EXECUTION_DRIVER_NAME, pid, pgid.unwrap_or(pid))
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            ProcessOperation::Kill { target, signal } => {
                // Linux permits signaling any same-credential process. The
                // per-VM kernel and requester-driver ownership checks are the
                // isolation boundary; restricting this to self/direct children
                // incorrectly rejects ordinary shell jobs signaling a parent
                // or sibling process.
                kernel
                    .signal_process(EXECUTION_DRIVER_NAME, target, signal)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            ProcessOperation::Wait {
                target,
                options,
                deadline_ms,
                temporary_mask,
            } => {
                if deadline_ms.is_some() || temporary_mask.is_some() {
                    return Err(HostServiceError::new(
                        "EINVAL",
                        "synchronous waitpid does not accept a deadline or temporary mask",
                    ));
                }
                probe_process_wait(kernel, process.kernel_pid, target, options)?
            }
            ProcessOperation::WaitTransition { target, options } => {
                let selector = wait_selector(kernel, process.kernel_pid, target)?;
                match kernel
                    .take_nonterminal_wait_event(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        selector,
                        wait_flags(options),
                    )
                    .map_err(kernel_host_error)?
                {
                    Some(event) => {
                        let status = match event.event {
                            WaitPidEvent::Stopped => ((event.status as u32 & 0xff) << 8) | 0x7f,
                            WaitPidEvent::Continued => 0xffff,
                            WaitPidEvent::Exited => {
                                return Err(HostServiceError::new(
                                    "EIO",
                                    "terminal wait event escaped nonterminal query",
                                ));
                            }
                        };
                        json!({ "pid": event.pid, "status": status })
                    }
                    None => Value::Null,
                }
            }
            ProcessOperation::Umask { new_mask } => json!(kernel
                .umask(EXECUTION_DRIVER_NAME, process.kernel_pid, new_mask)
                .map_err(kernel_host_error)?),
            ProcessOperation::GetResourceLimit { kind } => {
                let limit = kernel
                    .get_resource_limit(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        resource_kind(kind),
                    )
                    .map_err(kernel_host_error)?;
                json!({
                    "soft": limit.soft.unwrap_or(u64::MAX).to_string(),
                    "hard": limit.hard.unwrap_or(u64::MAX).to_string(),
                })
            }
            ProcessOperation::SetResourceLimit { kind, value } => {
                kernel
                    .set_resource_limit(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        resource_kind(kind),
                        ProcessResourceLimit {
                            soft: value.soft,
                            hard: value.hard,
                        },
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            ProcessOperation::SystemIdentity => {
                let identity = kernel.system_identity();
                json!({
                    "hostname": identity.hostname,
                    "type": identity.os_type,
                    "release": identity.os_release,
                    "version": identity.os_version,
                    "machine": identity.machine,
                    "domainName": identity.domain_name,
                })
            }
            other => return Err(unsupported("process", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}

fn bounded_committed_process_image(
    kernel: &SidecarKernel,
    pid: u32,
    max_reply_bytes: usize,
) -> Result<CommittedProcessImage, HostServiceError> {
    #[derive(serde::Serialize)]
    struct Reply<'a> {
        argv: &'a [String],
        env: &'a [(String, String)],
    }

    let reply_limit = PayloadLimit::new("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)?;
    let limits = kernel.resource_limits();
    let argv_maximum = limits
        .max_process_argv_bytes
        .unwrap_or(DEFAULT_MAX_PROCESS_ARGV_BYTES)
        .max(1);
    let env_maximum = limits
        .max_process_env_bytes
        .unwrap_or(DEFAULT_MAX_PROCESS_ENV_BYTES)
        .max(1);
    let image = kernel
        .process_image(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_host_error)?;
    // Count the exact wire JSON with a bounded streaming writer before
    // constructing a serde_json::Value. This remains safe when a trusted VM
    // raises the kernel argv/env limits above the bridge response limit.
    reply_limit.admit_json(&Reply {
        argv: &image.argv,
        env: &image.env,
    })?;
    let argv_limit = PayloadLimit::new("limits.resources.maxProcessArgvBytes", argv_maximum)?;
    let env_limit = PayloadLimit::new("limits.resources.maxProcessEnvBytes", env_maximum)?;
    let argv = image
        .argv
        .into_iter()
        .map(|value| BoundedString::try_new(value, &argv_limit))
        .collect::<Result<Vec<_>, _>>()?;
    let env = image
        .env
        .into_iter()
        .map(|(key, value)| {
            Ok((
                BoundedString::try_new(key, &env_limit)?,
                BoundedString::try_new(value, &env_limit)?,
            ))
        })
        .collect::<Result<Vec<_>, HostServiceError>>()?;
    Ok(CommittedProcessImage {
        argv: BoundedVec::try_new(argv, &argv_limit)?,
        env: BoundedVec::try_new(env, &env_limit)?,
    })
}

pub(super) fn probe_process_wait(
    kernel: &mut SidecarKernel,
    caller_pid: u32,
    target: WaitTarget,
    options: u32,
) -> Result<Value, HostServiceError> {
    let selector = wait_selector(kernel, caller_pid, target)?;
    wait_result_value(
        kernel
            .waitpid_detailed_with_options(
                EXECUTION_DRIVER_NAME,
                caller_pid,
                selector,
                wait_flags(options),
            )
            .map_err(kernel_host_error)?,
    )
}

fn executable_image_limit_error(error: LimitError) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_RESOURCE_LIMIT", error.to_string()).with_details(json!({
        "scope": error.scope,
        "resource": error.resource.name(),
        "used": error.used,
        "requested": error.requested,
        "limit": error.limit,
        "limitName": error.config_path,
    }))
}

fn wait_flags(options: u32) -> WaitPidFlags {
    let mut flags = WaitPidFlags::WNOHANG;
    if options & 2 != 0 {
        flags |= WaitPidFlags::WUNTRACED;
    }
    if options & 8 != 0 {
        flags |= WaitPidFlags::WCONTINUED;
    }
    flags
}

fn wait_selector(
    _kernel: &SidecarKernel,
    _caller_pid: u32,
    target: WaitTarget,
) -> Result<i32, HostServiceError> {
    match target {
        WaitTarget::Any => Ok(-1),
        WaitTarget::Pid(pid) => i32::try_from(pid)
            .map_err(|_| HostServiceError::new("EINVAL", "waitpid PID exceeds i32")),
        // waitpid(0, ...) is relative to the caller's process group. Preserve
        // selector zero so the kernel evaluates it with the caller identity;
        // converting pgid 1 to -1 would incorrectly mean "any child".
        WaitTarget::ProcessGroup(0) => Ok(0),
        WaitTarget::ProcessGroup(pgid) => i32::try_from(pgid)
            .map(|pgid| -pgid)
            .map_err(|_| HostServiceError::new("EINVAL", "process group exceeds i32")),
    }
}

fn wait_result_value(
    transition: Option<agentos_kernel::kernel::WaitPidDetailedResult>,
) -> Result<Value, HostServiceError> {
    let Some(transition) = transition else {
        return Ok(Value::Null);
    };
    let (event, raw_status, exit_code, signal, core_dumped) =
        match (transition.event, transition.termination) {
            (WaitPidEvent::Exited, Some(ProcessExit::Exited(code))) => (
                "exit",
                (code as u32 & 0xff) << 8,
                code as u32 & 0xff,
                0,
                false,
            ),
            (
                WaitPidEvent::Exited,
                Some(ProcessExit::Signaled {
                    signal,
                    core_dumped,
                }),
            ) => (
                "exit",
                (signal as u32 & 0x7f) | if core_dumped { 0x80 } else { 0 },
                0,
                signal as u32 & 0x7f,
                core_dumped,
            ),
            (WaitPidEvent::Stopped, _) => (
                "stopped",
                ((transition.status as u32 & 0xff) << 8) | 0x7f,
                0,
                transition.status as u32 & 0xff,
                false,
            ),
            (WaitPidEvent::Continued, _) => ("continued", 0xffff, 0, 0, false),
            (WaitPidEvent::Exited, None) => {
                return Err(HostServiceError::new(
                    "EIO",
                    "kernel terminal wait transition omitted exact termination",
                ));
            }
        };
    Ok(json!({
        "pid": transition.pid,
        "event": event,
        "status": transition.status,
        "rawStatus": raw_status,
        "exitCode": exit_code,
        "signal": signal,
        "coreDumped": core_dumped,
    }))
}

fn resource_kind(kind: ResourceLimitKind) -> ProcessResourceLimitKind {
    match kind {
        ResourceLimitKind::AddressSpace => ProcessResourceLimitKind::AddressSpace,
        ResourceLimitKind::Core => ProcessResourceLimitKind::Core,
        ResourceLimitKind::Cpu => ProcessResourceLimitKind::Cpu,
        ResourceLimitKind::Data => ProcessResourceLimitKind::Data,
        ResourceLimitKind::FileSize => ProcessResourceLimitKind::FileSize,
        ResourceLimitKind::LockedMemory => ProcessResourceLimitKind::LockedMemory,
        ResourceLimitKind::OpenFiles => ProcessResourceLimitKind::OpenFiles,
        ResourceLimitKind::Processes => ProcessResourceLimitKind::Processes,
        ResourceLimitKind::ResidentSet => ProcessResourceLimitKind::ResidentSet,
        ResourceLimitKind::Stack => ProcessResourceLimitKind::Stack,
    }
}
