//! Production runtime-neutral host-operation dispatch.
//!
//! Execution adapters decode only calls whose wire contract is already an
//! exact match for a [`HostOperation`]. Everything below this router is split
//! by capability family and invokes the existing kernel semantic operation;
//! no Linux state is mirrored here.

mod clock;
mod entropy;
mod filesystem;
pub(super) use filesystem::{service_deferred_kernel_read, service_deferred_kernel_stdin_read};
mod identity;
#[allow(dead_code)]
mod inventory;

#[cfg(test)]
pub(crate) fn is_wasm_adapter_only_rpc(method: &str) -> bool {
    inventory::WASM_ADAPTER_ONLY_RPCS.contains(&method)
}
mod network;
pub(super) use network::service_deferred_kernel_poll;
mod network_compat;
pub(in crate::execution) use network_compat::managed_socket_address_from_info;
pub(in crate::execution) use network_compat::{
    close_with_managed_retirement, closefrom_with_managed_retirement,
    deferred_posix_poll_wake_lane, dispatch_claimed_context_stream_read,
    dispatch_claimed_context_udp_poll, dispatch_descendant_context_stream_read,
    dispatch_descendant_context_udp_poll, fd_snapshot_with_managed_routes,
    prune_managed_process_routes_without_aliases, replace_descriptor_with_managed_retirement,
    retire_managed_process_routes, retire_orphaned_managed_descriptions,
    service_deferred_posix_poll, service_descendant_managed_fd_network_operation,
};
mod process;
mod signal;
mod terminal;

use super::*;
use agentos_execution::backend::{ExecutionEvent, HostCallReply, PayloadLimit};
use agentos_execution::host::{
    BoundedBytes, BoundedExecutableImageResolutionRequest, BoundedProcessLaunchRequest,
    BoundedString, BoundedUsize, BoundedVec, ClockOperation, CommittedProcessImage,
    DescriptorSyncKind, DescriptorWhence, DnsAddressFamily, EntropyOperation,
    ExecutableImageResolutionRequest, ExecutableImageSource, FileRangeOperation, FileTimeUpdate,
    FilesystemOperation, FilesystemRecordLockKind, GuestClockId, GuestOpenRights, GuestOpenSpec,
    HostOperation, IdentityIdKind, IdentityOperation, KernelPollInterest, ManagedTcpEndpoint,
    ManagedUdpFamily, ManagedUnixAddress, MetadataTarget, NetworkOperation, PollInterest,
    ProcessLaunchOptions, ProcessLaunchRequest, ProcessOperation, RecordLockCommand,
    ResourceLimitKind, ResourceLimitValue, SignalActionValue, SignalDispositionValue,
    SignalMaskHow, SignalOperation, SignalSetValue, SocketAddress,
    SocketDomain as HostSocketDomain, SocketKind, SocketOptionName, SocketOptionValue,
    SocketShutdown, SocketValidationRequirement, TerminalAttributes, TerminalOperation,
    TerminalWindowSize, WaitTarget, XattrOperation,
};
use agentos_kernel::fd_table::{
    WASI_RIGHT_FD_ALLOCATE, WASI_RIGHT_FD_DATASYNC, WASI_RIGHT_FD_FDSTAT_SET_FLAGS,
    WASI_RIGHT_FD_FILESTAT_GET, WASI_RIGHT_FD_FILESTAT_SET_SIZE, WASI_RIGHT_FD_FILESTAT_SET_TIMES,
    WASI_RIGHT_FD_READ, WASI_RIGHT_FD_READDIR, WASI_RIGHT_FD_SEEK, WASI_RIGHT_FD_SYNC,
    WASI_RIGHT_FD_WRITE, WASI_RIGHT_PATH_CREATE_DIRECTORY, WASI_RIGHT_PATH_FILESTAT_GET,
    WASI_RIGHT_PATH_FILESTAT_SET_SIZE, WASI_RIGHT_PATH_FILESTAT_SET_TIMES,
    WASI_RIGHT_PATH_LINK_SOURCE, WASI_RIGHT_PATH_LINK_TARGET, WASI_RIGHT_PATH_OPEN,
    WASI_RIGHT_PATH_READLINK, WASI_RIGHT_PATH_REMOVE_DIRECTORY, WASI_RIGHT_PATH_RENAME_SOURCE,
    WASI_RIGHT_PATH_RENAME_TARGET, WASI_RIGHT_PATH_SYMLINK, WASI_RIGHT_PATH_UNLINK_FILE,
};
use agentos_kernel::kernel::KernelError;

const MAX_ACCOUNT_NAME_BYTES: usize = 4 * 1024;
const MAX_ACCOUNT_RECORD_BYTES: usize = 4 * 1024;
const MAX_CHILD_PROCESS_ID_BYTES: usize = 4 * 1024;
const MAX_SUPPLEMENTARY_GROUPS: usize = 64;
const MAX_SIGNAL_SET_ENTRIES: usize = 64;
const MAX_SIGNAL_STATE_MASK_JSON_BYTES: usize = 4 * 1024;
const MAX_ENTROPY_CHUNK_BYTES: usize = 64 * 1024;
const MAX_DEFERRED_GUEST_WAIT_MS: u64 = u32::MAX as u64;
fn canonical_path_dir_fd(fd: u32) -> u32 {
    // wasi-libc historically defines AT_FDCWD as -2 while Linux defines it as
    // -100. Extension imports carry either value through the unsigned WebAssembly
    // ABI. Normalize both spellings to the executor-independent cwd sentinel.
    const WASI_LIBC_AT_FDCWD: u32 = (-2_i32) as u32;
    const LINUX_AT_FDCWD: u32 = (-100_i32) as u32;
    if matches!(fd, WASI_LIBC_AT_FDCWD | LINUX_AT_FDCWD) {
        return u32::MAX;
    }

    // Hidden preopen aliases are real kernel-owned descriptor identities. Keep
    // the tag so ordinary pathname resolution continues to use the capability
    // root even after the guest closes or replaces the same-numbered visible
    // preopen descriptor.
    fd
}

pub(crate) fn checked_deferred_guest_wait_deadline(
    delay_ms: u64,
) -> Result<Instant, HostServiceError> {
    if delay_ms > MAX_DEFERRED_GUEST_WAIT_MS {
        return Err(HostServiceError::new(
            "EINVAL",
            format!(
                "guest wait duration {delay_ms} ms exceeds guestWaitDurationMs ({MAX_DEFERRED_GUEST_WAIT_MS} ms)"
            ),
        )
        .with_details(json!({
            "limitName": "guestWaitDurationMs",
            "limit": MAX_DEFERRED_GUEST_WAIT_MS,
            "observed": delay_ms,
        })));
    }
    Instant::now()
        .checked_add(Duration::from_millis(delay_ms))
        .ok_or_else(|| {
            HostServiceError::new("EINVAL", "guest wait deadline exceeds the host clock range")
                .with_details(json!({
                    "limitName": "guestWaitDurationMs",
                    "limit": MAX_DEFERRED_GUEST_WAIT_MS,
                    "observed": delay_ms,
                }))
        })
}

trait SidecarHostCapability<Operation> {
    fn requires_claim(operation: &Operation) -> bool;

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: Operation,
    ) -> Result<HostCallReply, HostServiceError>;
}

fn permission_tier_label(tier: ProcessPermissionTier) -> &'static str {
    match tier {
        ProcessPermissionTier::Isolated => "isolated",
        ProcessPermissionTier::ReadOnly => "read-only",
        ProcessPermissionTier::ReadWrite => "read-write",
        ProcessPermissionTier::Full => "full",
    }
}

fn tier_denied(
    code: &'static str,
    tier: ProcessPermissionTier,
    family: &'static str,
    operation: &impl fmt::Debug,
) -> HostServiceError {
    HostServiceError::new(
        code,
        format!(
            "{} process tier denies {family} operation {operation:?}",
            permission_tier_label(tier)
        ),
    )
    .with_details(json!({
        "permissionTier": permission_tier_label(tier),
        "capabilityFamily": family,
        "operation": format!("{operation:?}"),
    }))
}

fn filesystem_operation_is_path_based(operation: &FilesystemOperation) -> bool {
    matches!(
        operation,
        FilesystemOperation::ReadFileAt { .. }
            | FilesystemOperation::WriteFileAt { .. }
            | FilesystemOperation::OpenAt { .. }
            | FilesystemOperation::OpenTmpfileAt { .. }
            | FilesystemOperation::NodeStatAt { .. }
            | FilesystemOperation::NodeLstatAt { .. }
            | FilesystemOperation::ReadDirectoryAt { .. }
            | FilesystemOperation::AccessAt { .. }
            | FilesystemOperation::CreateDirectoryAt { .. }
            | FilesystemOperation::CreateDirectoriesAt { .. }
            | FilesystemOperation::MakeNodeAt { .. }
            | FilesystemOperation::LinkAt { .. }
            | FilesystemOperation::LinkDescriptorAt { .. }
            | FilesystemOperation::RenameAt { .. }
            | FilesystemOperation::SymlinkAt { .. }
            | FilesystemOperation::ReadLinkAt { .. }
            | FilesystemOperation::UnlinkAt { .. }
            | FilesystemOperation::FilesystemStatsAt { .. }
            | FilesystemOperation::Remount { .. }
            | FilesystemOperation::Stat {
                target: MetadataTarget::Path { .. },
                ..
            }
            | FilesystemOperation::SetTimes {
                target: MetadataTarget::Path { .. },
                ..
            }
            | FilesystemOperation::SetMode {
                target: MetadataTarget::Path { .. },
                ..
            }
            | FilesystemOperation::SetOwner {
                target: MetadataTarget::Path { .. },
                ..
            }
            | FilesystemOperation::SetAttributesAt { .. }
            | FilesystemOperation::SetPathLength { .. }
            | FilesystemOperation::Xattr {
                target: MetadataTarget::Path { .. },
                ..
            }
    )
}

fn filesystem_operation_is_read_only_mutation(operation: &FilesystemOperation) -> bool {
    use agentos_kernel::fd_table::{O_CREAT, O_RDWR, O_TRUNC, O_WRONLY};

    match operation {
        FilesystemOperation::WriteFileAt { .. }
        | FilesystemOperation::SetAttributesAt { .. }
        | FilesystemOperation::CreateDirectoriesAt { .. }
        | FilesystemOperation::CreateDirectoryAt { .. } => true,
        FilesystemOperation::OpenAt { options, .. } => {
            let requested_mutation_rights = match options.rights {
                GuestOpenRights::Explicit { base, inheriting } => {
                    (base | inheriting)
                        & (WASI_RIGHT_FD_DATASYNC
                            | WASI_RIGHT_FD_SYNC
                            | WASI_RIGHT_FD_WRITE
                            | WASI_RIGHT_FD_ALLOCATE
                            | WASI_RIGHT_FD_FILESTAT_SET_SIZE
                            | WASI_RIGHT_FD_FILESTAT_SET_TIMES
                            | WASI_RIGHT_PATH_CREATE_DIRECTORY
                            | WASI_RIGHT_PATH_FILESTAT_SET_SIZE
                            | WASI_RIGHT_PATH_FILESTAT_SET_TIMES
                            | WASI_RIGHT_PATH_LINK_SOURCE
                            | WASI_RIGHT_PATH_LINK_TARGET
                            | WASI_RIGHT_PATH_RENAME_SOURCE
                            | WASI_RIGHT_PATH_RENAME_TARGET
                            | WASI_RIGHT_PATH_SYMLINK
                            | WASI_RIGHT_PATH_REMOVE_DIRECTORY
                            | WASI_RIGHT_PATH_UNLINK_FILE)
                        != 0
                }
                GuestOpenRights::Synthesized => false,
            };
            options.flags & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC) != 0
                || requested_mutation_rights
        }
        FilesystemOperation::OpenTmpfileAt { .. }
        | FilesystemOperation::SetLength { .. }
        | FilesystemOperation::SetPathLength { .. }
        | FilesystemOperation::SetTimes { .. }
        | FilesystemOperation::SetMode { .. }
        | FilesystemOperation::SetOwner { .. }
        | FilesystemOperation::MakeNodeAt { .. }
        | FilesystemOperation::LinkAt { .. }
        | FilesystemOperation::LinkDescriptorAt { .. }
        | FilesystemOperation::RenameAt { .. }
        | FilesystemOperation::SymlinkAt { .. }
        | FilesystemOperation::UnlinkAt { .. }
        | FilesystemOperation::Range { .. }
        | FilesystemOperation::Remount { .. } => true,
        FilesystemOperation::Xattr { operation, .. } => {
            matches!(
                operation,
                XattrOperation::Set { .. } | XattrOperation::Remove
            )
        }
        _ => false,
    }
}

fn descriptor_write_targets_file(
    kernel: &SidecarKernel,
    pid: u32,
    fd: u32,
) -> Result<bool, HostServiceError> {
    let entry = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
        .map_err(kernel_host_error)?;
    Ok(matches!(
        entry.filetype,
        agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
            | agentos_kernel::fd_table::FILETYPE_DIRECTORY
            | agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
    ))
}

fn require_descriptor_right(
    kernel: &SidecarKernel,
    pid: u32,
    fd: u32,
    required: u64,
    operation: &FilesystemOperation,
) -> Result<(), HostServiceError> {
    let entry = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
        .map_err(kernel_host_error)?;
    if entry.rights & required == required {
        return Ok(());
    }
    Err(HostServiceError::new(
        "EACCES",
        format!("descriptor {fd} lacks required WASI rights {required:#x} for {operation:?}"),
    )
    .with_details(json!({
        "fd": fd,
        "requiredRights": required,
        "descriptorRights": entry.rights,
        "operation": format!("{operation:?}"),
    })))
}

fn require_path_right(
    kernel: &SidecarKernel,
    pid: u32,
    dir_fd: u32,
    _path: &BoundedString,
    required: u64,
    operation: &FilesystemOperation,
) -> Result<(), HostServiceError> {
    let dir_fd = canonical_path_dir_fd(dir_fd);
    if dir_fd == u32::MAX {
        return Ok(());
    }
    require_descriptor_right(kernel, pid, dir_fd, required, operation)
}

fn authorize_filesystem_rights(
    kernel: &SidecarKernel,
    pid: u32,
    operation: &FilesystemOperation,
) -> Result<(), HostServiceError> {
    match operation {
        FilesystemOperation::ReadFileAt { dir_fd, path, .. }
        | FilesystemOperation::WriteFileAt { dir_fd, path, .. }
        | FilesystemOperation::ReadDirectoryAt { dir_fd, path, .. } => {
            require_path_right(kernel, pid, *dir_fd, path, WASI_RIGHT_PATH_OPEN, operation)
        }
        FilesystemOperation::OpenAt { dir_fd, path, .. } => {
            require_path_right(kernel, pid, *dir_fd, path, WASI_RIGHT_PATH_OPEN, operation)
        }
        FilesystemOperation::OpenTmpfileAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_OPEN | WASI_RIGHT_PATH_CREATE_DIRECTORY,
            operation,
        ),
        FilesystemOperation::Read { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_READ, operation)
        }
        FilesystemOperation::Write { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_WRITE, operation)
        }
        FilesystemOperation::Seek { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_SEEK, operation)
        }
        FilesystemOperation::Sync { fd, kind } => require_descriptor_right(
            kernel,
            pid,
            *fd,
            match kind {
                DescriptorSyncKind::Data => WASI_RIGHT_FD_DATASYNC,
                DescriptorSyncKind::All => WASI_RIGHT_FD_SYNC,
            },
            operation,
        ),
        FilesystemOperation::DescriptorFileStat { fd }
        | FilesystemOperation::DescriptorPath { fd, .. }
        | FilesystemOperation::DescriptorFilesystemStats { fd }
        | FilesystemOperation::Extents { fd, .. }
        | FilesystemOperation::ExtentAt { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_FILESTAT_GET, operation)
        }
        FilesystemOperation::SetDescriptorFlags { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_FDSTAT_SET_FLAGS, operation)
        }
        FilesystemOperation::SetLength { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_FILESTAT_SET_SIZE, operation)
        }
        FilesystemOperation::SetPathLength { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_FILESTAT_SET_SIZE,
            operation,
        ),
        FilesystemOperation::ReadDirectory { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_READDIR, operation)
        }
        FilesystemOperation::Stat { target, path } => match (target, path) {
            (MetadataTarget::Descriptor(fd), _) => {
                require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_FILESTAT_GET, operation)
            }
            (MetadataTarget::Path { dir_fd, .. }, Some(path)) => require_path_right(
                kernel,
                pid,
                *dir_fd,
                path,
                WASI_RIGHT_PATH_FILESTAT_GET,
                operation,
            ),
            _ => Ok(()),
        },
        FilesystemOperation::NodeStatAt { dir_fd, path }
        | FilesystemOperation::NodeLstatAt { dir_fd, path }
        | FilesystemOperation::AccessAt { dir_fd, path, .. }
        | FilesystemOperation::FilesystemStatsAt { dir_fd, path } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_FILESTAT_GET,
            operation,
        ),
        FilesystemOperation::SetTimes { target, path, .. } => match (target, path) {
            (MetadataTarget::Descriptor(fd), _) => require_descriptor_right(
                kernel,
                pid,
                *fd,
                WASI_RIGHT_FD_FILESTAT_SET_TIMES,
                operation,
            ),
            (MetadataTarget::Path { dir_fd, .. }, Some(path)) => require_path_right(
                kernel,
                pid,
                *dir_fd,
                path,
                WASI_RIGHT_PATH_FILESTAT_SET_TIMES,
                operation,
            ),
            _ => Ok(()),
        },
        FilesystemOperation::SetMode { target, path, .. }
        | FilesystemOperation::SetOwner { target, path, .. } => match (target, path) {
            (MetadataTarget::Descriptor(fd), _) => require_descriptor_right(
                kernel,
                pid,
                *fd,
                WASI_RIGHT_FD_FILESTAT_SET_TIMES,
                operation,
            ),
            (MetadataTarget::Path { dir_fd, .. }, Some(path)) => require_path_right(
                kernel,
                pid,
                *dir_fd,
                path,
                WASI_RIGHT_PATH_FILESTAT_SET_TIMES,
                operation,
            ),
            _ => Ok(()),
        },
        FilesystemOperation::SetAttributesAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_FILESTAT_SET_TIMES,
            operation,
        ),
        FilesystemOperation::CreateDirectoryAt { dir_fd, path, .. }
        | FilesystemOperation::CreateDirectoriesAt { dir_fd, path, .. }
        | FilesystemOperation::MakeNodeAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_CREATE_DIRECTORY,
            operation,
        ),
        FilesystemOperation::LinkAt {
            old_dir_fd,
            old_path,
            new_dir_fd,
            new_path,
            ..
        } => {
            require_path_right(
                kernel,
                pid,
                *old_dir_fd,
                old_path,
                WASI_RIGHT_PATH_LINK_SOURCE,
                operation,
            )?;
            require_path_right(
                kernel,
                pid,
                *new_dir_fd,
                new_path,
                WASI_RIGHT_PATH_LINK_TARGET,
                operation,
            )
        }
        FilesystemOperation::LinkDescriptorAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_LINK_TARGET,
            operation,
        ),
        FilesystemOperation::RenameAt {
            old_dir_fd,
            old_path,
            new_dir_fd,
            new_path,
            ..
        } => {
            require_path_right(
                kernel,
                pid,
                *old_dir_fd,
                old_path,
                WASI_RIGHT_PATH_RENAME_SOURCE,
                operation,
            )?;
            require_path_right(
                kernel,
                pid,
                *new_dir_fd,
                new_path,
                WASI_RIGHT_PATH_RENAME_TARGET,
                operation,
            )
        }
        FilesystemOperation::SymlinkAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_SYMLINK,
            operation,
        ),
        FilesystemOperation::ReadLinkAt { dir_fd, path, .. } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            WASI_RIGHT_PATH_READLINK,
            operation,
        ),
        FilesystemOperation::UnlinkAt {
            dir_fd,
            path,
            remove_directory,
        } => require_path_right(
            kernel,
            pid,
            *dir_fd,
            path,
            if *remove_directory {
                WASI_RIGHT_PATH_REMOVE_DIRECTORY
            } else {
                WASI_RIGHT_PATH_UNLINK_FILE
            },
            operation,
        ),
        FilesystemOperation::Range { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_ALLOCATE, operation)
        }
        FilesystemOperation::Xattr {
            target,
            path,
            operation: xattr_operation,
            ..
        } => {
            let mutation = matches!(
                xattr_operation,
                XattrOperation::Set { .. } | XattrOperation::Remove
            );
            match (target, path) {
                (MetadataTarget::Descriptor(fd), _) => require_descriptor_right(
                    kernel,
                    pid,
                    *fd,
                    if mutation {
                        WASI_RIGHT_FD_FILESTAT_SET_TIMES
                    } else {
                        WASI_RIGHT_FD_FILESTAT_GET
                    },
                    operation,
                ),
                (MetadataTarget::Path { dir_fd, .. }, Some(path)) => require_path_right(
                    kernel,
                    pid,
                    *dir_fd,
                    path,
                    if mutation {
                        WASI_RIGHT_PATH_FILESTAT_SET_TIMES
                    } else {
                        WASI_RIGHT_PATH_FILESTAT_GET
                    },
                    operation,
                ),
                _ => Ok(()),
            }
        }
        FilesystemOperation::StdinRead { .. } => {
            require_descriptor_right(kernel, pid, 0, WASI_RIGHT_FD_READ, operation)
        }
        FilesystemOperation::StdioWrite { fd, .. } => {
            require_descriptor_right(kernel, pid, *fd, WASI_RIGHT_FD_WRITE, operation)
        }
        _ => Ok(()),
    }
}

pub(super) fn authorize_host_operation(
    kernel: &SidecarKernel,
    pid: u32,
    operation: &HostOperation,
) -> Result<(), HostServiceError> {
    if let HostOperation::Filesystem(operation) = operation {
        authorize_filesystem_rights(kernel, pid, operation)?;
    }
    let tier = kernel
        .process_permission_tier(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_host_error)?;
    if tier == ProcessPermissionTier::Full {
        return Ok(());
    }

    match operation {
        HostOperation::Filesystem(operation) => {
            if matches!(
                operation,
                FilesystemOperation::Pipe
                    | FilesystemOperation::Duplicate { .. }
                    | FilesystemOperation::DuplicateTo { .. }
                    | FilesystemOperation::Remount { .. }
            ) {
                return Err(tier_denied("EACCES", tier, "filesystem", operation));
            }
            if tier == ProcessPermissionTier::Isolated
                && matches!(
                    operation,
                    FilesystemOperation::DuplicateMin { .. }
                        | FilesystemOperation::DescriptorFdFlags { .. }
                        | FilesystemOperation::SetDescriptorFdFlags { .. }
                        | FilesystemOperation::AdvisoryLock { .. }
                        | FilesystemOperation::RecordLock { .. }
                        | FilesystemOperation::CancelRecordLocks
                )
            {
                return Err(tier_denied("EACCES", tier, "filesystem", operation));
            }
            if tier == ProcessPermissionTier::Isolated
                && filesystem_operation_is_path_based(operation)
            {
                return Err(tier_denied("EACCES", tier, "filesystem", operation));
            }
            if matches!(
                tier,
                ProcessPermissionTier::ReadOnly | ProcessPermissionTier::Isolated
            ) {
                let descriptor_file_write = match operation {
                    FilesystemOperation::Write { fd, .. } if *fd > 2 => {
                        descriptor_write_targets_file(kernel, pid, *fd)?
                    }
                    _ => false,
                };
                if descriptor_file_write || filesystem_operation_is_read_only_mutation(operation) {
                    return Err(tier_denied("EROFS", tier, "filesystem", operation));
                }
            }
            if tier == ProcessPermissionTier::ReadWrite
                && matches!(
                    operation,
                    FilesystemOperation::MakeNodeAt { .. }
                        | FilesystemOperation::LinkAt { .. }
                        | FilesystemOperation::LinkDescriptorAt { .. }
                        | FilesystemOperation::RenameAt { .. }
                        | FilesystemOperation::SymlinkAt { .. }
                        | FilesystemOperation::UnlinkAt { .. }
                )
            {
                return Err(tier_denied("EACCES", tier, "filesystem", operation));
            }
            Ok(())
        }
        HostOperation::Network(
            NetworkOperation::KernelPoll { .. } | NetworkOperation::PosixPoll { .. },
        ) => Ok(()),
        HostOperation::Network(operation) => Err(tier_denied("EACCES", tier, "network", operation)),
        HostOperation::Process(
            ProcessOperation::GetImage { .. }
            | ProcessOperation::SystemIdentity
            | ProcessOperation::OpenExecutableImage { .. }
            | ProcessOperation::ReadExecutableImage { .. }
            | ProcessOperation::CloseExecutableImage { .. },
        ) => Ok(()),
        HostOperation::Process(ProcessOperation::GetResourceLimit { .. }) => Ok(()),
        HostOperation::Process(
            ProcessOperation::SetResourceLimit { .. } | ProcessOperation::Umask { .. },
        ) if tier != ProcessPermissionTier::Isolated => Ok(()),
        HostOperation::Process(operation) => Err(tier_denied("EACCES", tier, "process", operation)),
        HostOperation::Clock(ClockOperation::Time { .. } | ClockOperation::Resolution { .. }) => {
            Ok(())
        }
        HostOperation::Clock(operation) => Err(tier_denied("EACCES", tier, "clock", operation)),
        HostOperation::Terminal(TerminalOperation::OpenPty) => Err(tier_denied(
            "EACCES",
            tier,
            "terminal",
            &TerminalOperation::OpenPty,
        )),
        HostOperation::Terminal(_) => Ok(()),
        HostOperation::Signal(
            SignalOperation::RegisterThread { .. }
            | SignalOperation::UnregisterThread { .. }
            | SignalOperation::UpdateMask {
                how: SignalMaskHow::Block,
                set: SignalSetValue(0),
            }
            | SignalOperation::UpdateMaskForThread {
                how: SignalMaskHow::Block,
                set: SignalSetValue(0),
                ..
            }
            | SignalOperation::BeginDelivery
            | SignalOperation::BeginDeliveryForThread { .. }
            | SignalOperation::TakePublishedDelivery
            | SignalOperation::TakePublishedDeliveryForThread { .. }
            | SignalOperation::EndDelivery { .. }
            | SignalOperation::EndDeliveryForThread { .. },
        ) => Ok(()),
        HostOperation::Signal(operation) => Err(tier_denied("EACCES", tier, "signal", operation)),
        HostOperation::Identity(_) | HostOperation::Entropy(_) => Ok(()),
        _ => Err(HostServiceError::new(
            "EACCES",
            "process tier denies unknown host operation",
        )),
    }
}

pub(super) fn decode_compatibility_host_call(
    call: ExecutionHostCall,
    full_filesystem: bool,
    max_reply_bytes: usize,
) -> Result<ActiveExecutionEvent, SidecarError> {
    let remapped = remap_wasm_process_sync_rpc(&call.request)?;
    let request = remapped.as_ref().unwrap_or(&call.request);
    let Some(operation) = decode_host_operation(request, full_filesystem, max_reply_bytes)? else {
        return Ok(ActiveExecutionEvent::HostRpcRequest(call));
    };
    Ok(ActiveExecutionEvent::Common(ExecutionEvent::HostCall {
        operation,
        reply: call.reply,
    }))
}

fn decode_host_operation(
    request: &HostRpcRequest,
    full_filesystem: bool,
    max_reply_bytes: usize,
) -> Result<Option<HostOperation>, SidecarError> {
    let operation = match request.method.as_str() {
        "process.fd_preopens" => HostOperation::Filesystem(FilesystemOperation::Preopens),
        "process.fd_close" => HostOperation::Filesystem(FilesystemOperation::Close {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_close fd")?,
        }),
        "process.fd_seek" => {
            let raw_whence = javascript_sync_rpc_arg_u32(&request.args, 2, "fd_seek whence")?;
            let whence = match raw_whence {
                0 => DescriptorWhence::Set,
                1 => DescriptorWhence::Current,
                2 => DescriptorWhence::End,
                3 => DescriptorWhence::Data,
                4 => DescriptorWhence::Hole,
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported fd_seek whence {raw_whence}"),
                    ));
                }
            };
            let offset = javascript_sync_rpc_arg_str(&request.args, 1, "fd_seek offset")?
                .parse::<i64>()
                .map_err(|_| SidecarError::host("EINVAL", "fd_seek offset must be i64"))?;
            HostOperation::Filesystem(FilesystemOperation::Seek {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "fd_seek fd")?,
                offset,
                whence,
            })
        }
        "process.fd_socketpair" => {
            let raw_kind = javascript_sync_rpc_arg_u32(&request.args, 0, "socketpair kind")?;
            let kind = match raw_kind {
                1 => SocketKind::Stream,
                2 => SocketKind::Datagram,
                3 => SocketKind::SeqPacket,
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported socketpair kind {raw_kind}"),
                    ));
                }
            };
            HostOperation::Network(NetworkOperation::SocketPair {
                kind,
                nonblocking: javascript_sync_rpc_arg_bool(
                    &request.args,
                    1,
                    "socketpair nonblocking",
                )?,
                close_on_exec: javascript_sync_rpc_arg_bool(
                    &request.args,
                    2,
                    "socketpair close-on-exec",
                )?,
            })
        }
        "process.hostnet_fd_open" => {
            let (domain, kind_index, nonblocking_index, close_on_exec_index) =
                if request.args.len() >= 4 {
                    let domain = match javascript_sync_rpc_arg_u32(
                        &request.args,
                        0,
                        "host-network socket domain",
                    )? {
                        1 => HostSocketDomain::Inet4,
                        2 => HostSocketDomain::Inet6,
                        3 => HostSocketDomain::Unix,
                        other => {
                            return Err(SidecarError::host(
                                "EAFNOSUPPORT",
                                format!("unsupported host-network socket domain {other}"),
                            ));
                        }
                    };
                    (domain, 1, 2, 3)
                } else {
                    // Compatibility with already-built V8 runners. The legacy
                    // request carried only a datagram boolean and had no
                    // executor-neutral address-family metadata.
                    (HostSocketDomain::Inet4, 0, 1, 2)
                };
            let raw_kind = request.args.get(kind_index).ok_or_else(|| {
                SidecarError::host("EINVAL", "host-network socket kind is required")
            })?;
            let kind = if request.args.len() >= 4 {
                match raw_kind
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                {
                    Some(5) => SocketKind::Datagram,
                    Some(6) => SocketKind::Stream,
                    _ => {
                        return Err(SidecarError::host(
                            "EPROTONOSUPPORT",
                            "unsupported host-network socket kind",
                        ));
                    }
                }
            } else if raw_kind.as_bool().ok_or_else(|| {
                SidecarError::host(
                    "EINVAL",
                    "legacy host-network datagram flag must be boolean",
                )
            })? {
                SocketKind::Datagram
            } else {
                SocketKind::Stream
            };
            HostOperation::Network(NetworkOperation::Socket {
                domain,
                kind,
                nonblocking: javascript_sync_rpc_arg_bool(
                    &request.args,
                    nonblocking_index,
                    "host-network nonblocking flag",
                )?,
                close_on_exec: javascript_sync_rpc_arg_bool(
                    &request.args,
                    close_on_exec_index,
                    "host-network close-on-exec flag",
                )?,
            })
        }
        "process.hostnet_bind" => HostOperation::Network(NetworkOperation::Bind {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network bind fd")?,
            address: decode_hostnet_socket_address(
                request.args.get(1),
                "host-network bind address",
            )?,
        }),
        "process.hostnet_connect" => HostOperation::Network(NetworkOperation::Connect {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network connect fd")?,
            address: decode_hostnet_socket_address(
                request.args.get(1),
                "host-network connect address",
            )?,
            deadline_ms: javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "host-network connect deadline",
            )?,
        }),
        "process.hostnet_listen" => HostOperation::Network(NetworkOperation::Listen {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network listen fd")?,
            backlog: javascript_sync_rpc_arg_u32(&request.args, 1, "host-network listen backlog")?,
        }),
        "process.hostnet_accept" => HostOperation::Network(NetworkOperation::Accept {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network accept fd")?,
            nonblocking: javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "host-network accepted nonblocking flag",
            )?,
            close_on_exec: javascript_sync_rpc_arg_bool(
                &request.args,
                2,
                "host-network accepted close-on-exec flag",
            )?,
            deadline_ms: javascript_sync_rpc_arg_u64_optional(
                &request.args,
                3,
                "host-network accept deadline",
            )?,
        }),
        "process.hostnet_validate" => HostOperation::Network(NetworkOperation::Validate {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network validation fd")?,
            requirement: if javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "host-network listening requirement",
            )? {
                SocketValidationRequirement::Listening
            } else {
                SocketValidationRequirement::Socket
            },
        }),
        "process.hostnet_recv" => {
            let requested = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "host-network receive byte length",
            )?)
            .map_err(|_| SidecarError::host("E2BIG", "receive length exceeds usize"))?;
            let limit = PayloadLimit::new("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)
                .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::Receive {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network receive fd")?,
                max_bytes: BoundedUsize::try_new(requested, &limit).map_err(SidecarError::from)?,
                flags: javascript_sync_rpc_arg_u32(&request.args, 2, "receive flags")?,
                deadline_ms: javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    3,
                    "host-network receive deadline",
                )?,
            })
        }
        "process.hostnet_send" => {
            let bytes =
                javascript_sync_rpc_request_bytes_arg(request, 1, "host-network send bytes")?;
            let limit = PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_reply_bytes)
                .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::Send {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "host-network send fd")?,
                bytes: BoundedBytes::try_new(bytes, &limit).map_err(SidecarError::from)?,
                flags: javascript_sync_rpc_arg_u32(&request.args, 2, "send flags")?,
                address: request
                    .args
                    .get(3)
                    .filter(|value| !value.is_null())
                    .map(|value| decode_hostnet_socket_address(Some(value), "send address"))
                    .transpose()?,
                deadline_ms: javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    4,
                    "host-network send deadline",
                )?,
            })
        }
        "process.hostnet_local_address" => HostOperation::Network(NetworkOperation::LocalAddress {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "local-address fd")?,
        }),
        "process.hostnet_peer_address" => HostOperation::Network(NetworkOperation::PeerAddress {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "peer-address fd")?,
        }),
        "process.hostnet_get_option" => HostOperation::Network(NetworkOperation::GetOption {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "get-option fd")?,
            name: decode_hostnet_socket_option(javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "socket option",
            )?)?,
        }),
        "process.hostnet_set_option" => HostOperation::Network(NetworkOperation::SetOption {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "set-option fd")?,
            name: decode_hostnet_socket_option(javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "socket option",
            )?)?,
            value: decode_hostnet_socket_option_value(request.args.get(2), "socket option value")?,
        }),
        "process.hostnet_poll" => {
            let raw = request
                .args
                .first()
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", "host-network poll interests must be an array")
                })?;
            let interests = raw
                .iter()
                .map(|value| {
                    let entry = value.as_object().ok_or_else(|| {
                        SidecarError::host("EINVAL", "poll interest must be an object")
                    })?;
                    Ok(PollInterest {
                        fd: entry
                            .get("fd")
                            .and_then(Value::as_u64)
                            .and_then(|fd| u32::try_from(fd).ok())
                            .ok_or_else(|| SidecarError::host("EINVAL", "poll fd must be u32"))?,
                        readable: entry
                            .get("readable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        writable: entry
                            .get("writable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<Result<Vec<_>, SidecarError>>()?;
            let limit = PayloadLimit::new("limits.kernel.maxPollDescriptors", 1024)
                .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::Poll {
                interests: BoundedVec::try_new(interests, &limit).map_err(SidecarError::from)?,
                deadline_ms: javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "host-network poll deadline",
                )?,
            })
        }
        "process.hostnet_tls_connect" => {
            let name_limit = PayloadLimit::new("runtime.network.maxHostBytes", 253)
                .map_err(SidecarError::from)?;
            let alpn_limit = PayloadLimit::new("runtime.network.maxAlpnProtocols", 32)
                .map_err(SidecarError::from)?;
            let alpn_bytes_limit = PayloadLimit::new("runtime.network.maxAlpnProtocolBytes", 255)
                .map_err(SidecarError::from)?;
            let alpn = request
                .args
                .get(2)
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| {
                            BoundedBytes::try_new(
                                javascript_sync_rpc_bytes_arg(
                                    std::slice::from_ref(value),
                                    0,
                                    "ALPN protocol",
                                )?,
                                &alpn_bytes_limit,
                            )
                            .map_err(SidecarError::from)
                        })
                        .collect::<Result<Vec<_>, SidecarError>>()
                })
                .transpose()?
                .unwrap_or_default();
            HostOperation::Network(NetworkOperation::TlsConnect {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "TLS socket fd")?,
                server_name: BoundedString::try_new(
                    javascript_sync_rpc_arg_str(&request.args, 1, "TLS server name")?.to_owned(),
                    &name_limit,
                )
                .map_err(SidecarError::from)?,
                alpn: BoundedVec::try_new(alpn, &alpn_limit).map_err(SidecarError::from)?,
                deadline_ms: javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    3,
                    "TLS connect deadline",
                )?,
                reject_unauthorized: request.args.get(4).and_then(Value::as_bool).unwrap_or(true),
            })
        }
        "process.fd_socket_shutdown" => {
            let how = match javascript_sync_rpc_arg_u32(&request.args, 1, "shutdown mode")? {
                0 => SocketShutdown::Read,
                1 => SocketShutdown::Write,
                2 => SocketShutdown::Both,
                other => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("invalid shutdown mode {other}"),
                    ));
                }
            };
            HostOperation::Network(NetworkOperation::Shutdown {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "shutdown socket fd")?,
                how,
            })
        }
        "process.fd_sendmsg_rights" => {
            let raw_rights = request
                .args
                .get(2)
                .and_then(Value::as_array)
                .ok_or_else(|| SidecarError::host("EINVAL", "sendmsg rights must be an array"))?;
            // Compatibility V8 host-network descriptions remain an explicit
            // adapter projection. Plain kernel descriptor transfers are fully
            // typed and are the contract used by engine-neutral executors.
            if raw_rights.iter().any(|value| !value.is_u64()) {
                return Ok(None);
            }
            let rights = raw_rights
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|fd| u32::try_from(fd).ok())
                        .ok_or_else(|| SidecarError::host("EBADF", "sendmsg right must be u32"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let rights_limit = PayloadLimit::new("limits.network.maxScmRights", 253)
                .map_err(SidecarError::from)?;
            let bytes_limit =
                PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_reply_bytes)
                    .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::SendDescriptorRights {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "sendmsg socket fd")?,
                bytes: BoundedBytes::try_new(
                    javascript_sync_rpc_request_bytes_arg(request, 1, "sendmsg data")?,
                    &bytes_limit,
                )
                .map_err(SidecarError::from)?,
                rights: BoundedVec::try_new(rights, &rights_limit).map_err(SidecarError::from)?,
                flags: javascript_sync_rpc_arg_u32_optional(&request.args, 3, "sendmsg flags")?
                    .unwrap_or_default(),
            })
        }
        "process.fd_recvmsg_rights" => {
            let byte_limit =
                PayloadLimit::new("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)
                    .map_err(SidecarError::from)?;
            let rights_limit = PayloadLimit::new("limits.network.maxScmRights", 253)
                .map_err(SidecarError::from)?;
            let max_bytes = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "recvmsg maximum bytes",
            )?)
            .map_err(|_| SidecarError::host("E2BIG", "recvmsg byte limit exceeds usize"))?;
            let max_rights = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "recvmsg maximum rights",
            )?)
            .map_err(|_| SidecarError::host("E2BIG", "recvmsg rights limit exceeds usize"))?;
            HostOperation::Network(NetworkOperation::ReceiveDescriptorRights {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "recvmsg socket fd")?,
                max_bytes: BoundedUsize::try_new(max_bytes, &byte_limit)
                    .map_err(SidecarError::from)?,
                max_rights: BoundedUsize::try_new(max_rights, &rights_limit)
                    .map_err(SidecarError::from)?,
                close_on_exec: javascript_sync_rpc_arg_bool(
                    &request.args,
                    3,
                    "recvmsg close-on-exec",
                )?,
                peek: request
                    .args
                    .get(4)
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                dontwait: request
                    .args
                    .get(5)
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                waitall: request
                    .args
                    .get(6)
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        "__kernel_poll" => {
            let (fd_requests, timeout_ms) = parse_kernel_poll_args(request)?;
            let limit = PayloadLimit::new("limits.kernel.maxPollDescriptors", 1024)
                .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::KernelPoll {
                interests: BoundedVec::try_new(
                    fd_requests
                        .into_iter()
                        .map(|entry| KernelPollInterest {
                            fd: entry.fd,
                            events: entry.events,
                        })
                        .collect(),
                    &limit,
                )
                .map_err(SidecarError::from)?,
                timeout_ms: (timeout_ms >= 0).then_some(timeout_ms as u64),
            })
        }
        "process.posix_poll" => {
            let (fd_requests, timeout_ms) = parse_kernel_poll_args(request)?;
            let limit = PayloadLimit::new("limits.kernel.maxPollDescriptors", 1024)
                .map_err(SidecarError::from)?;
            HostOperation::Network(NetworkOperation::PosixPoll {
                interests: BoundedVec::try_new(
                    fd_requests
                        .into_iter()
                        .map(|entry| KernelPollInterest {
                            fd: entry.fd,
                            events: entry.events,
                        })
                        .collect(),
                    &limit,
                )
                .map_err(SidecarError::from)?,
                timeout_ms: (timeout_ms >= 0).then_some(timeout_ms as u64),
                signal_mask: request
                    .args
                    .get(2)
                    .filter(|value| !value.is_null())
                    .map(|value| decode_signal_set(Some(value)))
                    .transpose()?,
                signal_thread_id: request
                    .args
                    .get(3)
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        value
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or_else(|| {
                                SidecarError::host(
                                    "EINVAL",
                                    "POSIX poll signal thread id is invalid",
                                )
                            })
                    })
                    .transpose()?,
            })
        }
        "dns.lookup" => {
            let payload = request
                .args
                .first()
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", "dns.lookup requires an object payload")
                })?;
            let hostname = payload
                .get("hostname")
                .and_then(Value::as_str)
                .ok_or_else(|| SidecarError::host("EINVAL", "dns.lookup hostname is required"))?;
            let family = match payload.get("family").and_then(Value::as_u64).unwrap_or(0) {
                0 => DnsAddressFamily::Any,
                4 => DnsAddressFamily::Inet4,
                6 => DnsAddressFamily::Inet6,
                other => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported dns family {other}"),
                    ));
                }
            };
            HostOperation::Network(NetworkOperation::ResolveDns {
                host: bounded_dns_value(hostname, "maxDnsNameBytes", 253)?,
                port: None,
                family,
                max_results: BoundedUsize::try_new(
                    64,
                    &PayloadLimit::new("runtime.network.maxDnsResults", 64)
                        .map_err(SidecarError::from)?,
                )
                .map_err(SidecarError::from)?,
            })
        }
        "dns.resolveRawRr" => {
            let payload = request
                .args
                .first()
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", "dns.resolveRawRr requires an object payload")
                })?;
            let hostname = payload
                .get("hostname")
                .and_then(Value::as_str)
                .ok_or_else(|| SidecarError::host("EINVAL", "DNS hostname is required"))?;
            let record_type = payload.get("rrtype").and_then(Value::as_str).unwrap_or("A");
            HostOperation::Network(NetworkOperation::ResolveDnsRecord {
                host: bounded_dns_value(hostname, "maxDnsNameBytes", 253)?,
                record_type: bounded_dns_value(record_type, "maxDnsRecordTypeBytes", 16)?,
                raw: true,
                max_results: BoundedUsize::try_new(
                    64,
                    &PayloadLimit::new("runtime.network.maxDnsResults", 64)
                        .map_err(SidecarError::from)?,
                )
                .map_err(SidecarError::from)?,
            })
        }
        "child_process.spawn" => HostOperation::Process(ProcessOperation::Spawn(
            decode_process_launch_request(&request.args, max_reply_bytes)?,
        )),
        "child_process.poll" => HostOperation::Process(ProcessOperation::PollChild {
            child_id: bounded_child_process_id(javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "child_process.poll child id",
            )?)?,
            wait_ms: javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "child_process.poll wait ms",
            )?
            .unwrap_or_default(),
        }),
        "child_process.write_stdin" => {
            let request_limit =
                PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_reply_bytes)
                    .map_err(SidecarError::from)?;
            HostOperation::Process(ProcessOperation::WriteChildStdin {
                child_id: bounded_child_process_id(javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.write_stdin child id",
                )?)?,
                chunk: BoundedBytes::try_new(
                    javascript_sync_rpc_request_bytes_arg(
                        request,
                        1,
                        "child_process.write_stdin chunk",
                    )?,
                    &request_limit,
                )
                .map_err(SidecarError::from)?,
            })
        }
        "child_process.close_stdin" => HostOperation::Process(ProcessOperation::CloseChildStdin {
            child_id: bounded_child_process_id(javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "child_process.close_stdin child id",
            )?)?,
        }),
        "process.exec" => HostOperation::Process(ProcessOperation::Exec(
            decode_process_exec_request(&request.args, max_reply_bytes, false)?,
        )),
        "process.exec_fd_image_commit" => HostOperation::Process(ProcessOperation::Exec(
            decode_process_exec_request(&request.args, max_reply_bytes, true)?,
        )),
        "process.exec_image_open" => {
            let path_limit = PayloadLimit::new("runtime.filesystem.maxPathBytes", 4096)
                .map_err(SidecarError::from)?;
            HostOperation::Process(ProcessOperation::OpenExecutableImage {
                source: ExecutableImageSource::Path(
                    BoundedString::try_new(
                        javascript_sync_rpc_arg_str(
                            &request.args,
                            0,
                            "process.exec_image_open path",
                        )?
                        .to_owned(),
                        &path_limit,
                    )
                    .map_err(SidecarError::from)?,
                ),
                resolution: decode_executable_image_resolution(&request.args, 1, max_reply_bytes)?,
            })
        }
        "process.exec_image_open_fd" => {
            HostOperation::Process(ProcessOperation::OpenExecutableImage {
                source: ExecutableImageSource::Descriptor(javascript_sync_rpc_arg_u32(
                    &request.args,
                    0,
                    "process.exec_image_open_fd fd",
                )?),
                resolution: decode_executable_image_resolution(&request.args, 1, max_reply_bytes)?,
            })
        }
        "process.exec_image_read" => {
            let handle =
                javascript_sync_rpc_arg_str(&request.args, 0, "process.exec_image_read handle")?
                    .parse::<u64>()
                    .map_err(|_| SidecarError::host("EINVAL", "exec image handle must be u64"))?;
            let offset =
                javascript_sync_rpc_arg_str(&request.args, 1, "process.exec_image_read offset")?
                    .parse::<u64>()
                    .map_err(|_| SidecarError::host("EINVAL", "exec image offset must be u64"))?;
            let requested = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "process.exec_image_read byte length",
            )?)
            .map_err(|_| SidecarError::host("E2BIG", "exec image read length exceeds usize"))?;
            let encoded_reply = requested
                .checked_add(2)
                .and_then(|value| value.checked_div(3))
                .and_then(|value| value.checked_mul(4))
                .and_then(|value| value.checked_add(37))
                .ok_or_else(|| {
                    SidecarError::host("E2BIG", "exec image encoded reply size overflows usize")
                })?;
            PayloadLimit::new("limits.reactor.maxBridgeResponseBytes", max_reply_bytes)
                .map_err(SidecarError::from)?
                .admit(encoded_reply)
                .map_err(SidecarError::from)?;
            let raw_limit = PayloadLimit::new("maxExecImageReadBytes", max_reply_bytes)
                .map_err(SidecarError::from)?;
            HostOperation::Process(ProcessOperation::ReadExecutableImage {
                handle,
                offset,
                max_bytes: BoundedUsize::try_new(requested, &raw_limit)
                    .map_err(SidecarError::from)?,
            })
        }
        "process.exec_image_close" => {
            let handle =
                javascript_sync_rpc_arg_str(&request.args, 0, "process.exec_image_close handle")?
                    .parse::<u64>()
                    .map_err(|_| SidecarError::host("EINVAL", "exec image handle must be u64"))?;
            HostOperation::Process(ProcessOperation::CloseExecutableImage { handle })
        }
        "process.umask" => HostOperation::Process(ProcessOperation::Umask {
            new_mask: javascript_sync_rpc_arg_u32_optional(&request.args, 0, "process umask")?,
        }),
        "process.getrlimit" => {
            let raw = javascript_sync_rpc_arg_u32(&request.args, 0, "process.getrlimit resource")?;
            HostOperation::Process(ProcessOperation::GetResourceLimit {
                kind: decode_resource_limit(raw)?,
            })
        }
        "process.setrlimit" => {
            let raw = javascript_sync_rpc_arg_u32(&request.args, 0, "process.setrlimit resource")?;
            let soft = decode_rlim(&request.args, 1, "process.setrlimit soft value")?;
            let hard = decode_rlim(&request.args, 2, "process.setrlimit hard value")?;
            HostOperation::Process(ProcessOperation::SetResourceLimit {
                kind: decode_resource_limit(raw)?,
                value: ResourceLimitValue { soft, hard },
            })
        }
        "process.getpgid" => {
            let pid = javascript_sync_rpc_arg_u32(&request.args, 0, "process getpgid pid")?;
            HostOperation::Process(ProcessOperation::GetProcessGroup {
                pid: (pid != 0).then_some(pid),
            })
        }
        "process.setpgid" => {
            let pid = javascript_sync_rpc_arg_u32(&request.args, 0, "process setpgid pid")?;
            let pgid =
                javascript_sync_rpc_arg_u32(&request.args, 1, "process setpgid process group")?;
            HostOperation::Process(ProcessOperation::SetProcessGroup {
                pid: (pid != 0).then_some(pid),
                pgid: (pgid != 0).then_some(pgid),
            })
        }
        "process.kill" => HostOperation::Process(ProcessOperation::Kill {
            target: javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?,
            signal: parse_signal(javascript_sync_rpc_arg_str(
                &request.args,
                1,
                "process.kill signal",
            )?)?,
        }),
        "process.waitpid" => HostOperation::Process(ProcessOperation::Wait {
            target: decode_wait_target(javascript_sync_rpc_arg_i32(
                &request.args,
                0,
                "waitpid selector",
            )?)?,
            options: decode_wait_options(javascript_sync_rpc_arg_u32(
                &request.args,
                1,
                "waitpid options",
            )?)?,
            deadline_ms: javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "waitpid deadline ms",
            )?,
            temporary_mask: None,
        }),
        "process.waitpid_transition" => HostOperation::Process(ProcessOperation::WaitTransition {
            target: decode_wait_target(javascript_sync_rpc_arg_i32(
                &request.args,
                0,
                "waitpid selector",
            )?)?,
            options: decode_wait_options(javascript_sync_rpc_arg_u32(
                &request.args,
                1,
                "waitpid options",
            )?)?,
        }),
        "process.system_identity" => HostOperation::Process(ProcessOperation::SystemIdentity),
        "process.image" => HostOperation::Process(ProcessOperation::GetImage {
            max_reply_bytes: BoundedUsize::try_new(
                max_reply_bytes,
                &PayloadLimit::with_warning_hook(
                    "limits.reactor.maxBridgeResponseBytes",
                    max_reply_bytes,
                    None,
                )
                .map_err(SidecarError::from)?,
            )
            .map_err(SidecarError::from)?,
        }),
        "process.getuid" => identity_id(IdentityIdKind::RealUser),
        "process.geteuid" => identity_id(IdentityIdKind::EffectiveUser),
        "process.getgid" => identity_id(IdentityIdKind::RealGroup),
        "process.getegid" => identity_id(IdentityIdKind::EffectiveGroup),
        "process.getresuid" => HostOperation::Identity(IdentityOperation::GetUserIds),
        "process.getresgid" => HostOperation::Identity(IdentityOperation::GetGroupIds),
        "process.getgroups" => HostOperation::Identity(IdentityOperation::GetSupplementaryGroups),
        "process.getpwuid" => HostOperation::Identity(IdentityOperation::PasswdById {
            uid: javascript_sync_rpc_arg_u32(&request.args, 0, "passwd uid")?,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.getpwnam" => HostOperation::Identity(IdentityOperation::PasswdByName {
            name: bounded_account_name(javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "passwd name",
            )?)?,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.getpwent" => HostOperation::Identity(IdentityOperation::NextPasswd {
            index: javascript_sync_rpc_arg_u32(&request.args, 0, "passwd index")? as usize,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.getgrgid" => HostOperation::Identity(IdentityOperation::GroupById {
            gid: javascript_sync_rpc_arg_u32(&request.args, 0, "group gid")?,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.getgrnam" => HostOperation::Identity(IdentityOperation::GroupByName {
            name: bounded_account_name(javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "group name",
            )?)?,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.getgrent" => HostOperation::Identity(IdentityOperation::NextGroup {
            index: javascript_sync_rpc_arg_u32(&request.args, 0, "group index")? as usize,
            max_record_bytes: account_record_limit(max_reply_bytes)?,
        }),
        "process.setuid" => HostOperation::Identity(IdentityOperation::SetId {
            kind: IdentityIdKind::RealUser,
            value: Some(javascript_sync_rpc_arg_u32(&request.args, 0, "setuid uid")?),
        }),
        "process.seteuid" => HostOperation::Identity(IdentityOperation::SetId {
            kind: IdentityIdKind::EffectiveUser,
            value: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "seteuid uid",
            )?),
        }),
        "process.setreuid" => HostOperation::Identity(IdentityOperation::SetRealEffectiveUserIds {
            real: optional_identity_id(&request.args, 0, "setreuid uid")?,
            effective: optional_identity_id(&request.args, 1, "setreuid euid")?,
        }),
        "process.setresuid" => HostOperation::Identity(IdentityOperation::SetUserIds {
            real: optional_identity_id(&request.args, 0, "setresuid uid")?,
            effective: optional_identity_id(&request.args, 1, "setresuid euid")?,
            saved: optional_identity_id(&request.args, 2, "setresuid suid")?,
        }),
        "process.setgid" => HostOperation::Identity(IdentityOperation::SetId {
            kind: IdentityIdKind::RealGroup,
            value: Some(javascript_sync_rpc_arg_u32(&request.args, 0, "setgid gid")?),
        }),
        "process.setegid" => HostOperation::Identity(IdentityOperation::SetId {
            kind: IdentityIdKind::EffectiveGroup,
            value: Some(javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "setegid gid",
            )?),
        }),
        "process.setregid" => {
            HostOperation::Identity(IdentityOperation::SetRealEffectiveGroupIds {
                real: optional_identity_id(&request.args, 0, "setregid gid")?,
                effective: optional_identity_id(&request.args, 1, "setregid egid")?,
            })
        }
        "process.setresgid" => HostOperation::Identity(IdentityOperation::SetGroupIds {
            real: optional_identity_id(&request.args, 0, "setresgid gid")?,
            effective: optional_identity_id(&request.args, 1, "setresgid egid")?,
            saved: optional_identity_id(&request.args, 2, "setresgid sgid")?,
        }),
        "process.setgroups" => {
            let values = request
                .args
                .first()
                .and_then(Value::as_array)
                .ok_or_else(|| SidecarError::host("EINVAL", "setgroups requires an array"))?;
            if values.len() > MAX_SUPPLEMENTARY_GROUPS {
                return Err(payload_limit_error(
                    "limits.process.maxSupplementaryGroups",
                    MAX_SUPPLEMENTARY_GROUPS,
                    values.len(),
                ));
            }
            let groups = values
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| {
                            SidecarError::host("EINVAL", "setgroups entries must be u32")
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let limit = PayloadLimit::new(
                "limits.process.maxSupplementaryGroups",
                MAX_SUPPLEMENTARY_GROUPS,
            )
            .map_err(SidecarError::from)?;
            HostOperation::Identity(IdentityOperation::SetSupplementaryGroups {
                groups: BoundedVec::try_new(groups, &limit).map_err(SidecarError::from)?,
            })
        }
        "process.clock_time" => {
            let clock = decode_clock(javascript_sync_rpc_arg_u32(&request.args, 0, "clock id")?)?;
            let precision_ns = request
                .args
                .get(1)
                .and_then(Value::as_str)
                .map(|value| value.parse::<u64>())
                .transpose()
                .map_err(|_| SidecarError::host("EINVAL", "clock precision must be u64"))?
                .unwrap_or_default();
            let deterministic_realtime_ns = if clock == GuestClockId::Realtime {
                request
                    .args
                    .get(2)
                    .and_then(Value::as_str)
                    .map(str::parse::<u64>)
                    .transpose()
                    .map_err(|_| {
                        SidecarError::host(
                            "EINVAL",
                            "deterministic realtime must be u64 nanoseconds",
                        )
                    })?
            } else {
                None
            };
            HostOperation::Clock(ClockOperation::Time {
                clock,
                precision_ns,
                deterministic_realtime_ns,
            })
        }
        "process.clock_resolution" => HostOperation::Clock(ClockOperation::Resolution {
            clock: decode_clock(javascript_sync_rpc_arg_u32(&request.args, 0, "clock id")?)?,
        }),
        "process.sleep" => HostOperation::Clock(ClockOperation::Sleep {
            duration_ms: javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sleep duration milliseconds",
            )?,
        }),
        "process.itimer_real" => {
            match javascript_sync_rpc_arg_u32(&request.args, 0, "ITIMER_REAL operation")? {
                0 => HostOperation::Clock(ClockOperation::RealIntervalGet),
                1 => HostOperation::Clock(ClockOperation::RealIntervalSet {
                    initial_us: javascript_sync_rpc_arg_u64(
                        &request.args,
                        1,
                        "ITIMER_REAL value microseconds",
                    )?,
                    interval_us: javascript_sync_rpc_arg_u64(
                        &request.args,
                        2,
                        "ITIMER_REAL interval microseconds",
                    )?,
                }),
                other => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("invalid ITIMER_REAL operation {other}"),
                    ));
                }
            }
        }
        "process.take_signal" => HostOperation::Signal(SignalOperation::TakePublishedDelivery),
        "process.signal_begin" => HostOperation::Signal(SignalOperation::BeginDelivery),
        "process.signal_end" => HostOperation::Signal(SignalOperation::EndDelivery {
            token: javascript_sync_rpc_arg_u64(&request.args, 0, "signal token")?,
        }),
        "process.signal_state" => {
            let mask_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "process.signal_state mask")?;
            if mask_json.len() > MAX_SIGNAL_STATE_MASK_JSON_BYTES {
                return Err(payload_limit_error(
                    "maxSignalStateMaskJsonBytes",
                    MAX_SIGNAL_STATE_MASK_JSON_BYTES,
                    mask_json.len(),
                ));
            }
            let (signal, registration) =
                parse_process_signal_state_request(&request.args).map_err(SidecarError::from)?;
            if registration.mask.len() > MAX_SIGNAL_SET_ENTRIES {
                return Err(payload_limit_error(
                    "maxSignalSetEntries",
                    MAX_SIGNAL_SET_ENTRIES,
                    registration.mask.len(),
                ));
            }
            HostOperation::Signal(SignalOperation::SetAction {
                signal: i32::try_from(signal).map_err(|_| {
                    SidecarError::host("EINVAL", "process.signal_state signal exceeds i32")
                })?,
                action: SignalActionValue {
                    disposition: match registration.action {
                        SignalDispositionAction::Default => SignalDispositionValue::Default,
                        SignalDispositionAction::Ignore => SignalDispositionValue::Ignore,
                        SignalDispositionAction::User => SignalDispositionValue::User,
                    },
                    flags: registration.flags,
                    mask: signal_set_from_u32(registration.mask)?,
                },
            })
        }
        "process.signal_mask" => {
            let raw_how = javascript_sync_rpc_arg_u32(&request.args, 0, "signal-mask operation")?;
            let set = decode_signal_set(request.args.get(1))?;
            let how = match raw_how {
                0 => SignalMaskHow::Block,
                1 => SignalMaskHow::Unblock,
                2 => SignalMaskHow::Set,
                // The compatibility query convention is a no-op block.
                3 if set.0 == 0 => SignalMaskHow::Block,
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("invalid signal-mask operation {raw_how}"),
                    ));
                }
            };
            HostOperation::Signal(SignalOperation::UpdateMask { how, set })
        }
        "process.signal_mask_scope_begin" => {
            HostOperation::Signal(SignalOperation::BeginTemporaryMask {
                mask: decode_signal_set(request.args.first())?,
            })
        }
        "process.signal_mask_scope_end" => {
            HostOperation::Signal(SignalOperation::EndTemporaryMask {
                token: javascript_sync_rpc_arg_u64(
                    &request.args,
                    0,
                    "temporary signal-mask token",
                )?,
            })
        }
        "process.random_get" => {
            let requested =
                javascript_sync_rpc_arg_u64(&request.args, 0, "process.random_get byte length")?;
            let requested = usize::try_from(requested).map_err(|_| {
                SidecarError::host("E2BIG", "process.random_get byte length exceeds usize")
            })?;
            let maximum = MAX_ENTROPY_CHUNK_BYTES.min(max_reply_bytes);
            let limit =
                PayloadLimit::new("maxEntropyChunkBytes", maximum).map_err(SidecarError::from)?;
            HostOperation::Entropy(EntropyOperation {
                length: BoundedUsize::try_new(requested, &limit).map_err(SidecarError::from)?,
            })
        }
        "__kernel_isatty" => HostOperation::Terminal(TerminalOperation::IsTerminal {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_isatty fd")?,
        }),
        "__kernel_tty_size" => HostOperation::Terminal(TerminalOperation::GetWindowSize {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tty_size fd")?,
        }),
        "__kernel_tty_set_size" => {
            let columns = u16::try_from(javascript_sync_rpc_arg_u32(
                &request.args,
                1,
                "__kernel_tty_set_size cols",
            )?)
            .map_err(|_| SidecarError::host("EINVAL", "TTY columns exceed u16"))?;
            let rows = u16::try_from(javascript_sync_rpc_arg_u32(
                &request.args,
                2,
                "__kernel_tty_set_size rows",
            )?)
            .map_err(|_| SidecarError::host("EINVAL", "TTY rows exceed u16"))?;
            HostOperation::Terminal(TerminalOperation::SetWindowSize {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tty_set_size fd")?,
                size: TerminalWindowSize {
                    rows,
                    columns,
                    x_pixels: 0,
                    y_pixels: 0,
                },
            })
        }
        "__kernel_tcgetattr" => HostOperation::Terminal(TerminalOperation::GetAttributes {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetattr fd")?,
        }),
        "__kernel_tcsetattr" => {
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "__kernel_tcsetattr flags")?;
            let cc = request
                .args
                .get(2)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", "TTY control characters must be an array")
                })?;
            if cc.len() != 7 {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!(
                        "TTY control character array must contain 7 bytes, observed {}",
                        cc.len()
                    ),
                ));
            }
            let mut control_characters = [0_u8; 32];
            for (index, value) in cc.iter().enumerate() {
                control_characters[index] = value
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| {
                        SidecarError::host("EINVAL", "TTY control character must be a byte")
                    })?;
            }
            HostOperation::Terminal(TerminalOperation::SetAttributes {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcsetattr fd")?,
                attributes: TerminalAttributes {
                    input_flags: flags & (1 << 0),
                    output_flags: flags & ((1 << 1) | (1 << 2)),
                    control_flags: 0,
                    local_flags: flags & ((1 << 3) | (1 << 4) | (1 << 5)),
                    line_discipline: 0,
                    control_characters,
                    input_speed: 0,
                    output_speed: 0,
                },
            })
        }
        "__kernel_tcgetpgrp" => {
            HostOperation::Terminal(TerminalOperation::GetForegroundProcessGroup {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetpgrp fd")?,
            })
        }
        "__kernel_tcsetpgrp" => {
            HostOperation::Terminal(TerminalOperation::SetForegroundProcessGroup {
                fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcsetpgrp fd")?,
                pgid: javascript_sync_rpc_arg_u32(&request.args, 1, "__kernel_tcsetpgrp pgid")?,
            })
        }
        "__kernel_tcgetsid" => HostOperation::Terminal(TerminalOperation::GetSession {
            fd: javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_tcgetsid fd")?,
        }),
        "__pty_set_raw_mode" => HostOperation::Terminal(TerminalOperation::SetRawMode {
            fd: 0,
            enabled: javascript_sync_rpc_arg_bool(&request.args, 0, "__pty_set_raw_mode enabled")?,
        }),
        "process.pty_open" => HostOperation::Terminal(TerminalOperation::OpenPty),
        _ if full_filesystem => {
            if let Some(operation) = filesystem::decode(request, full_filesystem, max_reply_bytes)?
            {
                HostOperation::Filesystem(operation)
            } else if let Some(operation) =
                network_compat::decode_managed(request, max_reply_bytes)?
            {
                HostOperation::Network(operation)
            } else {
                return Ok(None);
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(operation))
}

fn identity_id(kind: IdentityIdKind) -> HostOperation {
    HostOperation::Identity(IdentityOperation::GetId { kind })
}

fn optional_identity_id(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<u32>, SidecarError> {
    if args.get(index).is_some_and(Value::is_null) {
        return Ok(None);
    }
    let value = javascript_sync_rpc_arg_u32(args, index, label)?;
    Ok((value != u32::MAX).then_some(value))
}

fn account_record_limit(max_reply_bytes: usize) -> Result<BoundedUsize, SidecarError> {
    let maximum = max_reply_bytes.min(MAX_ACCOUNT_RECORD_BYTES);
    // `maximum` is carried to the kernel as a bound. It is not an observed
    // account-record size, so registering it must not emit a near-limit warning.
    let limit = PayloadLimit::with_warning_hook("maxAccountRecordBytes", maximum, None)
        .map_err(SidecarError::from)?;
    BoundedUsize::try_new(maximum, &limit).map_err(SidecarError::from)
}

fn bounded_account_name(name: &str) -> Result<BoundedString, SidecarError> {
    let limit = PayloadLimit::new("maxAccountNameBytes", MAX_ACCOUNT_NAME_BYTES)
        .map_err(SidecarError::from)?;
    BoundedString::try_new(name.to_owned(), &limit).map_err(SidecarError::from)
}

fn bounded_child_process_id(value: &str) -> Result<BoundedString, SidecarError> {
    let limit = PayloadLimit::new("maxChildProcessIdBytes", MAX_CHILD_PROCESS_ID_BYTES)
        .map_err(SidecarError::from)?;
    BoundedString::try_new(value.to_owned(), &limit).map_err(SidecarError::from)
}

#[derive(Debug, serde::Deserialize)]
struct LegacyProcessLaunchOptions {
    #[serde(flatten)]
    options: ProcessLaunchOptions,
    #[serde(default, rename = "maxBuffer")]
    _max_buffer: Option<usize>,
}

fn decode_process_launch_request(
    args: &[Value],
    max_request_bytes: usize,
) -> Result<BoundedProcessLaunchRequest, SidecarError> {
    let request_limit =
        PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_request_bytes)
            .map_err(SidecarError::from)?;
    request_limit.admit_json(args).map_err(SidecarError::from)?;

    if let Some(value) = args.first().cloned() {
        if let Ok(request) = serde_json::from_value::<ProcessLaunchRequest>(value) {
            return BoundedProcessLaunchRequest::try_new(request, &request_limit)
                .map_err(SidecarError::from);
        }
    }

    let command = javascript_sync_rpc_arg_str(args, 0, "process launch command")?.to_owned();
    let raw_args = javascript_sync_rpc_arg_str(args, 1, "process launch args")?;
    let raw_options = javascript_sync_rpc_arg_str(args, 2, "process launch options")?;
    let parsed_args = serde_json::from_str::<Vec<String>>(raw_args).map_err(|error| {
        SidecarError::host(
            "EINVAL",
            format!("invalid process launch args payload: {error}"),
        )
    })?;
    let parsed_options =
        serde_json::from_str::<LegacyProcessLaunchOptions>(raw_options).map_err(|error| {
            SidecarError::host(
                "EINVAL",
                format!("invalid process launch options payload: {error}"),
            )
        })?;
    BoundedProcessLaunchRequest::try_new(
        ProcessLaunchRequest {
            command,
            args: parsed_args,
            options: parsed_options.options,
        },
        &request_limit,
    )
    .map_err(SidecarError::from)
}

fn decode_process_exec_request(
    args: &[Value],
    max_request_bytes: usize,
    fd_image_commit: bool,
) -> Result<BoundedProcessLaunchRequest, SidecarError> {
    let request = decode_process_launch_request(args, max_request_bytes)?;
    let has_executable_fd = request.as_request().options.executable_fd.is_some();
    if has_executable_fd != fd_image_commit {
        return Err(SidecarError::host(
            "EINVAL",
            if fd_image_commit {
                "process.exec_fd_image_commit requires executableFd"
            } else {
                "executableFd is only valid for process.exec_fd_image_commit"
            },
        ));
    }
    if fd_image_commit {
        validate_wasm_fd_image_commit_request(request.as_request())?;
    }
    Ok(request)
}

fn decode_executable_image_resolution(
    args: &[Value],
    argv_index: usize,
    max_request_bytes: usize,
) -> Result<Option<BoundedExecutableImageResolutionRequest>, SidecarError> {
    let Some(argv_value) = args.get(argv_index) else {
        return Ok(None);
    };
    let argv = serde_json::from_value::<Vec<String>>(argv_value.clone()).map_err(|error| {
        SidecarError::host(
            "EINVAL",
            format!("invalid exec image resolution argv: {error}"),
        )
    })?;
    let close_on_exec_fds = serde_json::from_value::<Vec<u32>>(
        args.get(argv_index + 1)
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(|error| {
        SidecarError::host(
            "EINVAL",
            format!("invalid exec image close-on-exec descriptors: {error}"),
        )
    })?;
    let limit = PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", max_request_bytes)
        .map_err(SidecarError::from)?;
    BoundedExecutableImageResolutionRequest::try_new(
        ExecutableImageResolutionRequest {
            argv,
            close_on_exec_fds,
        },
        &limit,
    )
    .map(Some)
    .map_err(SidecarError::from)
}

fn bounded_dns_value(
    value: &str,
    limit_name: &'static str,
    maximum: usize,
) -> Result<BoundedString, SidecarError> {
    let limit = PayloadLimit::new(limit_name, maximum).map_err(SidecarError::from)?;
    BoundedString::try_new(value.to_owned(), &limit).map_err(SidecarError::from)
}

fn decode_hostnet_socket_address(
    value: Option<&Value>,
    label: &str,
) -> Result<SocketAddress, SidecarError> {
    let value = value
        .and_then(Value::as_object)
        .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} must be an object")))?;
    let path_limit =
        PayloadLimit::new("runtime.filesystem.maxPathBytes", 4096).map_err(SidecarError::from)?;
    let host_limit =
        PayloadLimit::new("runtime.network.maxHostBytes", 253).map_err(SidecarError::from)?;
    match value.get("type").and_then(Value::as_str) {
        Some("inet") => Ok(SocketAddress::Inet {
            host: BoundedString::try_new(
                value
                    .get("host")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::host("EINVAL", format!("{label} host is required"))
                    })?
                    .to_owned(),
                &host_limit,
            )
            .map_err(SidecarError::from)?,
            port: value
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} port must be u16")))?,
        }),
        Some("unix-path") => Ok(SocketAddress::UnixPath(
            BoundedString::try_new(
                value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::host("EINVAL", format!("{label} path is required"))
                    })?
                    .to_owned(),
                &path_limit,
            )
            .map_err(SidecarError::from)?,
        )),
        Some("unix-abstract") => {
            let bytes = value
                .get("hex")
                .and_then(Value::as_str)
                .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} hex is required")))?;
            if !bytes.len().is_multiple_of(2) {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!("{label} hex is invalid"),
                ));
            }
            let decoded = bytes
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let pair = std::str::from_utf8(pair).map_err(|_| {
                        SidecarError::host("EINVAL", format!("{label} hex is invalid"))
                    })?;
                    u8::from_str_radix(pair, 16).map_err(|_| {
                        SidecarError::host("EINVAL", format!("{label} hex is invalid"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SocketAddress::UnixAbstract(
                BoundedBytes::try_new(decoded, &path_limit).map_err(SidecarError::from)?,
            ))
        }
        Some("unix-autobind") => Ok(SocketAddress::UnixAutobind),
        _ => Err(SidecarError::host(
            "EINVAL",
            format!("{label} has an unsupported type"),
        )),
    }
}

fn decode_hostnet_socket_option(value: &str) -> Result<SocketOptionName, SidecarError> {
    match value {
        "error" => Ok(SocketOptionName::Error),
        "reuse-address" => Ok(SocketOptionName::ReuseAddress),
        "reuse-port" => Ok(SocketOptionName::ReusePort),
        "keep-alive" => Ok(SocketOptionName::KeepAlive),
        "no-delay" => Ok(SocketOptionName::NoDelay),
        "broadcast" => Ok(SocketOptionName::Broadcast),
        "receive-buffer" => Ok(SocketOptionName::ReceiveBuffer),
        "send-buffer" => Ok(SocketOptionName::SendBuffer),
        "linger" => Ok(SocketOptionName::Linger),
        "receive-timeout" => Ok(SocketOptionName::ReceiveTimeout),
        "send-timeout" => Ok(SocketOptionName::SendTimeout),
        "ipv6-only" => Ok(SocketOptionName::Ipv6Only),
        "multicast-ttl" => Ok(SocketOptionName::MulticastTtl),
        "multicast-loop" => Ok(SocketOptionName::MulticastLoop),
        _ => Err(SidecarError::host(
            "ENOPROTOOPT",
            format!("unsupported socket option {value}"),
        )),
    }
}

fn decode_hostnet_socket_option_value(
    value: Option<&Value>,
    label: &str,
) -> Result<SocketOptionValue, SidecarError> {
    let value =
        value.ok_or_else(|| SidecarError::host("EINVAL", format!("{label} is required")))?;
    if let Some(value) = value.as_bool() {
        return Ok(SocketOptionValue::Bool(value));
    }
    if let Some(value) = value.as_i64() {
        return Ok(SocketOptionValue::Integer(value));
    }
    if value.is_null() {
        return Ok(SocketOptionValue::DurationMs(None));
    }
    let object = value
        .as_object()
        .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} is invalid")))?;
    if let Some(duration) = object.get("durationMs") {
        return Ok(SocketOptionValue::DurationMs(if duration.is_null() {
            None
        } else {
            Some(duration.as_u64().ok_or_else(|| {
                SidecarError::host("EINVAL", format!("{label} durationMs must be u64"))
            })?)
        }));
    }
    if let Some(enabled) = object.get("enabled").and_then(Value::as_bool) {
        let seconds = object
            .get("seconds")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| SidecarError::host("EINVAL", format!("{label} seconds must be u32")))?;
        return Ok(SocketOptionValue::Linger { enabled, seconds });
    }
    Err(SidecarError::host("EINVAL", format!("{label} is invalid")))
}

fn payload_limit_error(limit_name: &'static str, limit: usize, requested: usize) -> SidecarError {
    HostServiceError::new("E2BIG", format!("request exceeds {limit_name} ({limit})"))
        .with_details(json!({
            "limitName": limit_name,
            "limit": limit,
            "requested": requested,
        }))
        .into()
}

fn decode_clock(raw: u32) -> Result<GuestClockId, SidecarError> {
    match raw {
        0 => Ok(GuestClockId::Realtime),
        1 => Ok(GuestClockId::Monotonic),
        2 => Ok(GuestClockId::ProcessCpu),
        3 => Ok(GuestClockId::ThreadCpu),
        _ => Err(SidecarError::host(
            "EINVAL",
            format!("unsupported clock id {raw}"),
        )),
    }
}

fn decode_resource_limit(raw: u32) -> Result<ResourceLimitKind, SidecarError> {
    match raw {
        0 => Ok(ResourceLimitKind::Cpu),
        1 => Ok(ResourceLimitKind::FileSize),
        2 => Ok(ResourceLimitKind::Data),
        3 => Ok(ResourceLimitKind::Stack),
        4 => Ok(ResourceLimitKind::Core),
        5 => Ok(ResourceLimitKind::ResidentSet),
        6 => Ok(ResourceLimitKind::Processes),
        7 => Ok(ResourceLimitKind::OpenFiles),
        8 => Ok(ResourceLimitKind::LockedMemory),
        9 => Ok(ResourceLimitKind::AddressSpace),
        _ => Err(SidecarError::host(
            "EINVAL",
            format!("unsupported resource limit {raw}"),
        )),
    }
}

fn decode_wait_target(selector: i32) -> Result<WaitTarget, SidecarError> {
    match selector {
        -1 => Ok(WaitTarget::Any),
        0 => Ok(WaitTarget::ProcessGroup(0)),
        selector if selector > 0 => Ok(WaitTarget::Pid(selector as u32)),
        selector => selector
            .checked_abs()
            .and_then(|value| u32::try_from(value).ok())
            .map(WaitTarget::ProcessGroup)
            .ok_or_else(|| SidecarError::host("EINVAL", "invalid waitpid selector")),
    }
}

fn decode_wait_options(options: u32) -> Result<u32, SidecarError> {
    let invalid = options & !(1 | 2 | 8);
    if invalid != 0 {
        return Err(SidecarError::host(
            "EINVAL",
            format!("invalid waitpid option bits {invalid:#x}"),
        ));
    }
    Ok(options)
}

fn decode_rlim(args: &[Value], index: usize, label: &str) -> Result<Option<u64>, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::host("EINVAL", format!("{label} is required")));
    };
    let value = if let Some(text) = value.as_str() {
        text.parse::<u64>()
            .map_err(|_| SidecarError::host("EINVAL", format!("{label} must be u64")))?
    } else {
        javascript_sync_rpc_arg_u64(args, index, label)?
    };
    Ok((value != u64::MAX).then_some(value))
}

fn decode_signal_set(value: Option<&Value>) -> Result<SignalSetValue, SidecarError> {
    let signals = value
        .and_then(Value::as_array)
        .ok_or_else(|| SidecarError::host("EINVAL", "signal-mask set must be an array"))?;
    if signals.len() > MAX_SIGNAL_SET_ENTRIES {
        return Err(payload_limit_error(
            "maxSignalSetEntries",
            MAX_SIGNAL_SET_ENTRIES,
            signals.len(),
        ));
    }
    let signals = signals
        .iter()
        .map(|signal| {
            signal
                .as_i64()
                .and_then(|signal| u32::try_from(signal).ok())
                .ok_or_else(|| {
                    SidecarError::host(
                        "EINVAL",
                        "signal-mask entries must be integers between 1 and 64",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    signal_set_from_u32(signals)
}

fn signal_set_from_u32(
    signals: impl IntoIterator<Item = u32>,
) -> Result<SignalSetValue, SidecarError> {
    let mut bits = 0_u64;
    for signal in signals {
        if !(1..=64).contains(&signal) {
            return Err(SidecarError::host(
                "EINVAL",
                "signal-mask entries must be integers between 1 and 64",
            ));
        }
        bits |= 1_u64 << (signal - 1);
    }
    Ok(SignalSetValue(bits))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct HostOperationEffects {
    pub(super) may_make_fd_readable: bool,
    pub(super) may_make_fd_writable: bool,
}

pub(super) fn dispatch_host_operation(
    generation: u64,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    operation: HostOperation,
    reply: DirectHostReplyHandle,
) -> Result<HostOperationEffects, SidecarError> {
    let identity = reply.identity();
    if identity.generation != generation || identity.pid != process.kernel_pid {
        reply
            .fail(
                HostServiceError::new(
                    "ESTALE",
                    "host call identity does not match the active kernel process",
                )
                .with_details(json!({
                    "expectedGeneration": generation,
                    "expectedPid": process.kernel_pid,
                    "observedGeneration": identity.generation,
                    "observedPid": identity.pid,
                })),
            )
            .map_err(SidecarError::from)?;
        return Ok(HostOperationEffects::default());
    }
    if let Err(error) = authorize_host_operation(kernel, process.kernel_pid, &operation) {
        reply.fail(error).map_err(SidecarError::from)?;
        return Ok(HostOperationEffects::default());
    }
    if requires_context_host_dispatch(&operation) {
        reply
            .fail(HostServiceError::new(
                "ERR_AGENTOS_CONTEXT_DISPATCH_REQUIRED",
                "VM-scoped host operation bypassed context dispatch",
            ))
            .map_err(SidecarError::from)?;
        return Ok(HostOperationEffects::default());
    }

    let effects = host_operation_effects(&operation);
    let result = match operation {
        HostOperation::Filesystem(operation) => {
            execute::<filesystem::FilesystemCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Network(operation) => {
            execute::<network::NetworkCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Process(operation) => {
            execute::<process::ProcessCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Terminal(operation) => {
            execute::<terminal::TerminalCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Signal(operation) => {
            execute::<signal::SignalCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Identity(operation) => {
            execute::<identity::IdentityCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Clock(operation) => {
            execute::<clock::ClockCapability, _>(kernel, process, operation, &reply)
        }
        HostOperation::Entropy(operation) => {
            execute::<entropy::EntropyCapability, _>(kernel, process, operation, &reply)
        }
        other => Err(unsupported("host", other)),
    };
    // Host operations such as kill(self), SIGPIPE-producing writes, and timer
    // probes can queue kernel runtime controls. Publish those controls before
    // releasing the direct waiter so every executor observes the checkpoint at
    // the safe point immediately following this import.
    let result = result.and_then(|response| {
        if response.is_some() {
            process
                .apply_runtime_controls()
                .map_err(|error| host_service_error(&error))?;
        }
        Ok(response)
    });
    match result {
        Ok(Some(response)) => {
            reply.succeed(response).map_err(SidecarError::from)?;
            Ok(effects)
        }
        Ok(None) => Ok(HostOperationEffects::default()),
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            Ok(HostOperationEffects::default())
        }
    }
}

/// VM-scoped operations must run before the kernel/process-only fallback.
/// Keeping the classifier next to both dispatchers prevents a new executor or
/// event pump from silently losing DNS, reactor, wait, or process context.
pub(super) fn requires_context_host_dispatch(operation: &HostOperation) -> bool {
    matches!(
        operation,
        HostOperation::Filesystem(
            FilesystemOperation::Snapshot
                | FilesystemOperation::Close { .. }
                | FilesystemOperation::CloseFrom { .. }
                | FilesystemOperation::Renumber { .. }
                | FilesystemOperation::DuplicateTo { .. }
                | FilesystemOperation::Move { .. }
                | FilesystemOperation::Read { offset: None, .. }
                | FilesystemOperation::StdinRead { .. },
        ) | HostOperation::Network(
            NetworkOperation::HttpRequest { .. }
                | NetworkOperation::ResolveDns { .. }
                | NetworkOperation::ResolveDnsRecord { .. }
                | NetworkOperation::Socket { .. }
                | NetworkOperation::Bind { .. }
                | NetworkOperation::Connect { .. }
                | NetworkOperation::Listen { .. }
                | NetworkOperation::Accept { .. }
                | NetworkOperation::Validate { .. }
                | NetworkOperation::Receive { .. }
                | NetworkOperation::Send { .. }
                | NetworkOperation::LocalAddress { .. }
                | NetworkOperation::PeerAddress { .. }
                | NetworkOperation::GetOption { .. }
                | NetworkOperation::SetOption { .. }
                | NetworkOperation::Poll { .. }
                | NetworkOperation::TlsConnect { .. }
                | NetworkOperation::KernelPoll { .. }
                | NetworkOperation::PosixPoll { .. }
                | NetworkOperation::ManagedUdpPoll { .. }
                | NetworkOperation::ManagedBindUnix { .. }
                | NetworkOperation::ManagedBindConnectedUnix { .. }
                | NetworkOperation::ManagedReserveTcpPort { .. }
                | NetworkOperation::ManagedReleaseTcpPort { .. }
                | NetworkOperation::ManagedConnect { .. }
                | NetworkOperation::ManagedListen { .. }
                | NetworkOperation::ManagedPoll { .. }
                | NetworkOperation::ManagedWaitConnect { .. }
                | NetworkOperation::ManagedRead { .. }
                | NetworkOperation::ManagedWrite { .. }
                | NetworkOperation::ManagedDestroy { .. }
                | NetworkOperation::ManagedAccept { .. }
                | NetworkOperation::ManagedCloseListener { .. }
                | NetworkOperation::ManagedTlsUpgrade { .. }
                | NetworkOperation::ManagedUdpCreate { .. }
                | NetworkOperation::ManagedUdpBind { .. }
                | NetworkOperation::ManagedUdpSend { .. }
                | NetworkOperation::ManagedUdpClose { .. }
                | NetworkOperation::SendDescriptorRights { .. }
                | NetworkOperation::ReceiveDescriptorRights { .. }
        ) | HostOperation::Process(
            ProcessOperation::Spawn(_)
                | ProcessOperation::RunCaptured { .. }
                | ProcessOperation::Exec(_)
                | ProcessOperation::PollChild { .. }
                | ProcessOperation::WriteChildStdin { .. }
                | ProcessOperation::CloseChildStdin { .. }
                | ProcessOperation::Wait { .. }
        ) | HostOperation::Clock(ClockOperation::Sleep { .. })
    )
}

/// Dispatch operations that need VM-scoped sidecar capabilities in addition
/// to the kernel/process pair. Waiting variants use this seam as well: they
/// retain only owned operation data and the direct reply capability.
pub(super) async fn dispatch_context_host_operation<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: HostOperation,
    reply: DirectHostReplyHandle,
) -> Result<Option<(HostOperation, DirectHostReplyHandle)>, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(None);
    }
    let vm = sidecar
        .vms
        .get(vm_id)
        .expect("validated host-call VM remains registered");
    if let Err(error) = authorize_host_operation(&vm.kernel, reply.identity().pid, &operation) {
        reply.fail(error).map_err(SidecarError::from)?;
        return Ok(None);
    }
    match operation {
        HostOperation::Network(operation @ NetworkOperation::HttpRequest { .. }) => {
            dispatch_context_http_operation(sidecar, vm_id, process_id, operation, reply)?;
            Ok(None)
        }
        HostOperation::Filesystem(FilesystemOperation::Snapshot) => {
            network_compat::dispatch_context_fd_snapshot(sidecar, vm_id, process_id, reply)?;
            Ok(None)
        }
        HostOperation::Filesystem(FilesystemOperation::Close { fd }) => {
            network_compat::dispatch_context_close_with_managed_retirement(
                sidecar, vm_id, process_id, fd, reply,
            )?;
            Ok(None)
        }
        HostOperation::Filesystem(FilesystemOperation::CloseFrom { min_fd, exact_fds }) => {
            network_compat::dispatch_context_closefrom_with_managed_retirement(
                sidecar, vm_id, process_id, min_fd, exact_fds, reply,
            )?;
            Ok(None)
        }
        HostOperation::Filesystem(
            operation @ (FilesystemOperation::Renumber { .. }
            | FilesystemOperation::DuplicateTo { .. }
            | FilesystemOperation::Move { .. }),
        ) => {
            network_compat::dispatch_context_descriptor_replacement_with_managed_retirement(
                sidecar, vm_id, process_id, operation, reply,
            )?;
            Ok(None)
        }
        HostOperation::Filesystem(
            operation @ (FilesystemOperation::Read { offset: None, .. }
            | FilesystemOperation::StdinRead { .. }),
        ) => {
            filesystem::dispatch_context_deferred_kernel_read(
                sidecar, vm_id, process_id, operation, reply,
            )?;
            Ok(None)
        }
        HostOperation::Network(
            operation @ (NetworkOperation::ResolveDns { .. }
            | NetworkOperation::ResolveDnsRecord { .. }),
        ) => {
            dispatch_context_dns_operation(sidecar, vm_id, process_id, operation, reply)?;
            Ok(None)
        }
        HostOperation::Network(operation @ NetworkOperation::KernelPoll { .. }) => {
            network::dispatch_context_kernel_poll(sidecar, vm_id, process_id, operation, reply)?;
            Ok(None)
        }
        HostOperation::Network(operation @ NetworkOperation::PosixPoll { .. }) => {
            network_compat::dispatch_context_posix_poll(
                sidecar, vm_id, process_id, operation, reply,
            )?;
            Ok(None)
        }
        HostOperation::Network(operation @ NetworkOperation::ManagedUdpPoll { .. }) => {
            network_compat::dispatch_context_udp_poll(
                sidecar, vm_id, process_id, operation, reply,
            )?;
            Ok(None)
        }
        HostOperation::Network(
            operation @ (NetworkOperation::Socket { .. }
            | NetworkOperation::Bind { .. }
            | NetworkOperation::Connect { .. }
            | NetworkOperation::Listen { .. }
            | NetworkOperation::Accept { .. }
            | NetworkOperation::Validate { .. }
            | NetworkOperation::Receive { .. }
            | NetworkOperation::Send { .. }
            | NetworkOperation::LocalAddress { .. }
            | NetworkOperation::PeerAddress { .. }
            | NetworkOperation::GetOption { .. }
            | NetworkOperation::SetOption { .. }
            | NetworkOperation::Poll { .. }
            | NetworkOperation::TlsConnect { .. }
            | NetworkOperation::ManagedBindUnix { .. }
            | NetworkOperation::ManagedBindConnectedUnix { .. }
            | NetworkOperation::ManagedReserveTcpPort { .. }
            | NetworkOperation::ManagedReleaseTcpPort { .. }
            | NetworkOperation::ManagedConnect { .. }
            | NetworkOperation::ManagedListen { .. }
            | NetworkOperation::ManagedPoll { .. }
            | NetworkOperation::ManagedWaitConnect { .. }
            | NetworkOperation::ManagedRead { .. }
            | NetworkOperation::ManagedWrite { .. }
            | NetworkOperation::ManagedDestroy { .. }
            | NetworkOperation::ManagedAccept { .. }
            | NetworkOperation::ManagedCloseListener { .. }
            | NetworkOperation::ManagedTlsUpgrade { .. }
            | NetworkOperation::ManagedUdpCreate { .. }
            | NetworkOperation::ManagedUdpBind { .. }
            | NetworkOperation::ManagedUdpSend { .. }
            | NetworkOperation::ManagedUdpClose { .. }
            | NetworkOperation::SendDescriptorRights { .. }
            | NetworkOperation::ReceiveDescriptorRights { .. }),
        ) => {
            network_compat::dispatch_context_managed_network_operation(
                sidecar, vm_id, process_id, operation, reply,
            )
            .await?;
            Ok(None)
        }
        HostOperation::Process(
            operation @ (ProcessOperation::Spawn(_)
            | ProcessOperation::RunCaptured { .. }
            | ProcessOperation::Exec(_)
            | ProcessOperation::PollChild { .. }
            | ProcessOperation::WriteChildStdin { .. }
            | ProcessOperation::CloseChildStdin { .. }),
        ) => {
            dispatch_context_process_operation(sidecar, vm_id, process_id, operation, reply)
                .await?;
            Ok(None)
        }
        HostOperation::Process(ProcessOperation::Wait {
            target,
            options,
            deadline_ms,
            temporary_mask,
        }) => {
            if temporary_mask.is_some() {
                reply
                    .fail(HostServiceError::new(
                        "EINVAL",
                        "waitpid does not accept a temporary signal mask",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(None);
            }
            let deadline = match deadline_ms
                .map(checked_deferred_guest_wait_deadline)
                .transpose()
            {
                Ok(deadline) => deadline,
                Err(error) => {
                    reply.fail(error).map_err(SidecarError::from)?;
                    return Ok(None);
                }
            };
            dispatch_context_guest_wait(
                sidecar,
                vm_id,
                process_id,
                DeferredGuestWaitKind::Process { target, options },
                deadline,
                reply,
            )?;
            Ok(None)
        }
        HostOperation::Clock(ClockOperation::Sleep { duration_ms }) => {
            let deadline = match checked_deferred_guest_wait_deadline(duration_ms) {
                Ok(deadline) => deadline,
                Err(error) => {
                    reply.fail(error).map_err(SidecarError::from)?;
                    return Ok(None);
                }
            };
            dispatch_context_guest_wait(
                sidecar,
                vm_id,
                process_id,
                DeferredGuestWaitKind::Sleep,
                Some(deadline),
                reply,
            )?;
            Ok(None)
        }
        operation => Ok(Some((operation, reply))),
    }
}

fn dispatch_context_guest_wait<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    kind: DeferredGuestWaitKind,
    deadline: Option<Instant>,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let notify = Arc::clone(&sidecar.process_event_notify);
    let vm = sidecar
        .vms
        .get_mut(vm_id)
        .expect("validated VM must remain borrowed");
    let runtime = vm.runtime_context.clone();
    let wait_handle = vm.kernel.process_wait_handle();
    let generation = vm.generation;
    let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
    let process = active_processes
        .get_mut(process_id)
        .expect("validated process must remain borrowed");
    service_deferred_guest_wait(
        generation,
        &runtime,
        wait_handle,
        notify,
        kernel,
        process,
        Some((kind, deadline, reply)),
    )
}

pub(super) fn service_deferred_guest_wait(
    generation: u64,
    runtime: &agentos_runtime::RuntimeContext,
    wait_handle: agentos_kernel::process_table::ProcessWaitHandle,
    notify: Arc<tokio::sync::Notify>,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    incoming: Option<(
        DeferredGuestWaitKind,
        Option<Instant>,
        DirectHostReplyHandle,
    )>,
) -> Result<(), SidecarError> {
    let newly_admitted = incoming.is_some();
    if let Some((kind, deadline, reply)) = incoming {
        if process.deferred_guest_wait.is_some() {
            reply
                .fail(HostServiceError::new(
                    "EBUSY",
                    "process already owns a deferred wait",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != process.kernel_pid {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "deferred wait identity does not match the active kernel process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }
        process.deferred_guest_wait = Some(DeferredGuestWait {
            kind,
            reply,
            deadline,
            wake_task: None,
        });
    }

    // ITIMER_REAL state remains sidecar-owned. A deferred syscall must wake at
    // the timer deadline as well as its own deadline so the kernel can publish
    // SIGALRM promptly without an executor-local timer or polling loop.
    signal::materialize_real_timer_signal(process);
    process.apply_runtime_controls()?;
    if process.deferred_guest_wait.is_none() {
        return Ok(());
    }

    let now = Instant::now();
    let should_probe = process.deferred_guest_wait.as_ref().is_some_and(|wait| {
        newly_admitted
            || process.deferred_guest_wait_interrupted
            || wait.deadline.is_some_and(|deadline| now >= deadline)
            || wait
                .wake_task
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished)
    });
    if !should_probe {
        return Ok(());
    }

    let mut wait = process
        .deferred_guest_wait
        .take()
        .expect("deferred wait checked above");
    if let Some(task) = wait.wake_task.take() {
        task.abort();
    }
    let interrupted = std::mem::take(&mut process.deferred_guest_wait_interrupted);
    // Snapshot before the destructive readiness probe. A transition racing
    // the probe changes this generation, so the waiter returns immediately
    // instead of absorbing the only wake and sleeping forever.
    let observed = wait_handle.snapshot();
    let result = match wait.kind {
        DeferredGuestWaitKind::Process { target, options } => {
            match process::probe_process_wait(kernel, process.kernel_pid, target, options) {
                Ok(value) if !value.is_null() || options & 1 != 0 => Some(Ok(value)),
                Ok(_) if interrupted => Some(Err(HostServiceError::new(
                    "EINTR",
                    "blocking waitpid interrupted by a caught signal",
                ))),
                Ok(_) if wait.deadline.is_some_and(|deadline| now >= deadline) => {
                    Some(Ok(Value::Null))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        }
        DeferredGuestWaitKind::Sleep => {
            if interrupted {
                Some(Err(HostServiceError::new(
                    "EINTR",
                    "sleep interrupted by a caught signal",
                )))
            } else if wait.deadline.is_none_or(|deadline| now >= deadline) {
                Some(Ok(Value::Null))
            } else {
                None
            }
        }
    };
    if let Some(result) = result {
        return match result {
            Ok(value) => wait
                .reply
                .succeed(HostCallReply::Json(value))
                .map_err(SidecarError::from),
            Err(error) => wait.reply.fail(error).map_err(SidecarError::from),
        };
    }

    let wake_deadline = match (wait.deadline, process.real_interval_timer.next_deadline()) {
        (Some(wait), Some(timer)) => Some(wait.min(timer)),
        (wait @ Some(_), None) => wait,
        (None, timer) => timer,
    };
    let task_class = if wait.kind == DeferredGuestWaitKind::Sleep {
        agentos_runtime::TaskClass::Timer
    } else {
        agentos_runtime::TaskClass::Vm
    };
    let wake_task = runtime.spawn(task_class, async move {
        match wake_deadline {
            Some(deadline) => {
                let delay = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = wait_handle.wait_for_change_async(observed) => {}
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            None => wait_handle.wait_for_change_async(observed).await,
        }
        notify.notify_one();
    });
    match wake_task {
        Ok(task) => {
            wait.wake_task = Some(task);
            process.deferred_guest_wait = Some(wait);
            Ok(())
        }
        Err(error) => wait
            .reply
            .fail(host_service_error(&SidecarError::from(error)))
            .map_err(SidecarError::from),
    }
}

fn validate_context_host_call<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    reply: &DirectHostReplyHandle,
) -> Result<bool, SidecarError> {
    let Some(vm) = sidecar.vms.get(vm_id) else {
        reply
            .fail(HostServiceError::new(
                "ESTALE",
                "host call VM no longer exists",
            ))
            .map_err(SidecarError::from)?;
        return Ok(false);
    };
    let Some(process) = vm.active_processes.get(process_id) else {
        reply
            .fail(HostServiceError::new(
                "ESTALE",
                "host call process no longer exists",
            ))
            .map_err(SidecarError::from)?;
        return Ok(false);
    };
    let identity = reply.identity();
    if identity.generation != vm.generation || identity.pid != process.kernel_pid {
        reply
            .fail(
                HostServiceError::new(
                    "ESTALE",
                    "host call identity does not match the active kernel process",
                )
                .with_details(json!({
                    "expectedGeneration": vm.generation,
                    "expectedPid": process.kernel_pid,
                    "observedGeneration": identity.generation,
                    "observedPid": identity.pid,
                })),
            )
            .map_err(SidecarError::from)?;
        return Ok(false);
    }
    Ok(true)
}

fn claim_context_host_work(reply: &DirectHostReplyHandle) -> Result<bool, SidecarError> {
    match reply.claim() {
        Ok(claimed) => Ok(claimed),
        Err(error) => {
            reply.fail(error).map_err(SidecarError::from)?;
            Ok(false)
        }
    }
}

fn settle_context_preflight<T>(
    reply: &DirectHostReplyHandle,
    result: Result<T, SidecarError>,
) -> Result<Option<T>, SidecarError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            reply
                .fail(host_service_error(&error))
                .map_err(SidecarError::from)?;
            Ok(None)
        }
    }
}

fn dispatch_context_dns_operation<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: NetworkOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    if !claim_context_host_work(&reply)? {
        return Ok(());
    }
    let vm = sidecar
        .vms
        .get(vm_id)
        .expect("validated VM must remain borrowed");
    let runtime = vm.runtime_context.clone();
    let response = service_host_dns_operation(
        sidecar.bridge.clone(),
        &vm.kernel,
        vm_id.to_owned(),
        vm.dns.clone(),
        operation,
    );
    let task_reply = reply.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Dns, async move {
        let settled = match response.await {
            Ok(response) => task_reply.succeed(response),
            Err(error) => task_reply.fail(error),
        };
        if let Err(error) = settled {
            eprintln!("ERR_AGENTOS_DNS_DIRECT_REPLY: {error}");
        }
    }) {
        let error = SidecarError::from(error);
        reply
            .fail(host_service_error(&error))
            .map_err(SidecarError::from)?;
    }
    Ok(())
}

fn dispatch_context_http_operation<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: NetworkOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }
    let NetworkOperation::HttpRequest {
        url,
        method,
        headers,
        body,
        max_response_bytes,
        max_header_bytes,
        max_body_bytes,
    } = operation
    else {
        return Err(SidecarError::host(
            "EINVAL",
            "HTTP dispatcher received a non-HTTP operation",
        ));
    };
    let bridge = sidecar.bridge.clone();
    let preflight = (|| {
        let url = Url::parse(url.as_str())
            .map_err(|error| SidecarError::host("ERR_INVALID_URL", error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(SidecarError::host(
                "ERR_INVALID_URL",
                format!("unsupported outbound HTTP scheme {}", url.scheme()),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| SidecarError::host("ERR_INVALID_URL", "outbound HTTP URL has no host"))?
            .to_owned();
        let port = url.port_or_known_default().ok_or_else(|| {
            SidecarError::host("ERR_INVALID_URL", "outbound HTTP URL has no port")
        })?;
        validate_http_request_metadata(method.as_str(), headers.as_slice())?;
        bridge.require_network_access(
            vm_id,
            crate::execution::NetworkOperation::Http,
            format_tcp_resource(&host, port),
        )?;
        Ok((url, host, port))
    })();
    let Some((url, host, port)) = settle_context_preflight(&reply, preflight)? else {
        return Ok(());
    };
    if !claim_context_host_work(&reply)? {
        return Ok(());
    }
    let prepared = (|| {
        let vm = sidecar
            .vms
            .get_mut(vm_id)
            .expect("validated HTTP host-call VM remains registered");
        let process = vm
            .active_processes
            .get(process_id)
            .expect("validated HTTP host-call process remains registered");
        let kernel_pid = process.kernel_pid;
        let policy_body_limit = process.limits.http.max_fetch_response_bytes;
        let runtime = vm.runtime_context.clone();
        let pinned_addresses = if let Ok(literal_ip) = host.parse::<IpAddr>() {
            filter_dns_safe_ip_addrs(vec![literal_ip], &host)?
        } else {
            filter_dns_safe_ip_addrs(
                resolve_dns_ip_addrs(
                    &bridge,
                    &vm.kernel,
                    vm_id,
                    &vm.dns,
                    &host,
                    DnsLookupPolicy::SkipPermissions,
                )?,
                &host,
            )?
        };
        bridge.require_resolved_network_access(
            vm_id,
            crate::execution::NetworkOperation::Http,
            &format_tcp_resource(&host, port),
            &pinned_addresses
                .iter()
                .map(|ip| format_tcp_resource(&ip.to_string(), port))
                .collect::<Vec<_>>(),
        )?;
        let default_ca_bundle = if url.scheme() == "https" {
            read_vm_default_ca_bundle(&mut vm.kernel, kernel_pid)?
        } else {
            Vec::new()
        };
        Ok((
            runtime,
            policy_body_limit,
            pinned_addresses,
            default_ca_bundle,
        ))
    })();
    let Some((runtime, policy_body_limit, pinned_addresses, default_ca_bundle)) =
        settle_context_preflight(&reply, prepared)?
    else {
        return Ok(());
    };
    let max_response_bytes = max_response_bytes.get();
    let max_header_bytes = max_header_bytes.get().min(max_response_bytes);
    let max_body_bytes = max_body_bytes
        .get()
        .min(policy_body_limit)
        .min(max_response_bytes);
    let reserved_bytes = url
        .as_str()
        .len()
        .saturating_add(method.as_str().len())
        .saturating_add(body.len())
        .saturating_add(default_ca_bundle.len())
        .saturating_add(max_response_bytes)
        .saturating_add(
            headers
                .as_slice()
                .iter()
                .map(|header| {
                    header
                        .name
                        .as_str()
                        .len()
                        .saturating_add(header.value.as_str().len())
                })
                .sum::<usize>(),
        );
    let request = BoundedHttpRequest {
        url,
        method: method.into_string(),
        headers: headers.into_vec(),
        body: body.into_vec(),
        pinned_addresses,
        default_ca_bundle,
        max_response_bytes,
        max_header_bytes,
        max_body_bytes,
    };
    let task_reply = reply.clone();
    let task_runtime = runtime.clone();
    if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Socket, async move {
        let result = task_runtime
            .blocking()
            .run(reserved_bytes, move || issue_bounded_http_request(request))
            .await
            .map_err(|error| host_service_error(&SidecarError::from(error)))
            .and_then(|result| result.map_err(|error| host_service_error(&error)))
            .map(HostCallReply::Json);
        let settled = match result {
            Ok(response) => task_reply.succeed(response),
            Err(error) => task_reply.fail(error),
        };
        if let Err(error) = settled {
            eprintln!("ERR_AGENTOS_HTTP_DIRECT_REPLY: {error}");
        }
    }) {
        let error = SidecarError::from(error);
        reply
            .fail(host_service_error(&error))
            .map_err(SidecarError::from)?;
    }
    Ok(())
}

async fn dispatch_context_process_operation<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    operation: ProcessOperation,
    reply: DirectHostReplyHandle,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !validate_context_host_call(sidecar, vm_id, process_id, &reply)? {
        return Ok(());
    }

    match operation {
        ProcessOperation::Spawn(request) => {
            let mut request = request.into_request();
            if let Err(error) = merge_process_internal_bootstrap_env(sidecar, vm_id, &mut request)
                .and_then(|()| validate_process_launch_request(&request, false))
            {
                settle_context_process_reply(&reply, Err(error))?;
                return Ok(());
            }
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            let result = sidecar
                .spawn_child_process(vm_id, process_id, request)
                .await;
            settle_context_process_reply(&reply, result.map(HostCallReply::Json))?;
        }
        ProcessOperation::RunCaptured {
            request,
            max_buffer,
        } => {
            let mut request = request.into_request();
            if let Err(error) = merge_process_internal_bootstrap_env(sidecar, vm_id, &mut request)
                .and_then(|()| validate_process_launch_request(&request, false))
            {
                settle_context_process_reply(&reply, Err(error))?;
                return Ok(());
            }
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            let completion = PendingChildProcessSyncCompletion::Direct(reply.clone());
            if let Err(error) = sidecar
                .begin_javascript_child_process_sync(
                    vm_id,
                    process_id,
                    request,
                    Some(max_buffer.get()),
                    completion,
                )
                .await
            {
                reply
                    .fail(host_service_error(&error))
                    .map_err(SidecarError::from)?;
            }
        }
        ProcessOperation::PollChild { child_id, wait_ms } => {
            if let Err(error) =
                sidecar.validate_child_poll_target(vm_id, process_id, &[], child_id.as_str())
            {
                settle_context_process_reply(&reply, Err(error))?;
                return Ok(());
            }
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            // Polling is deliberately nonblocking in the sidecar. Settle the
            // claimed reply in this event turn so the guest can re-poll after
            // a concurrent child HostCall without depending on another edge
            // from the shared process broker.
            let result = sidecar
                .poll_child_process(vm_id, process_id, child_id.as_str(), wait_ms)
                .await
                .map(HostCallReply::Json);
            settle_context_process_reply(&reply, result)?;
        }
        ProcessOperation::WriteChildStdin { child_id, chunk } => {
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            let result = sidecar
                .write_child_process_stdin(vm_id, process_id, child_id.as_str(), chunk.as_slice())
                .map(|()| HostCallReply::Json(Value::Null));
            settle_context_process_reply(&reply, result)?;
        }
        ProcessOperation::CloseChildStdin { child_id } => {
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            let result = sidecar
                .close_child_process_stdin(vm_id, process_id, child_id.as_str())
                .map(|()| HostCallReply::Json(Value::Null));
            settle_context_process_reply(&reply, result)?;
        }
        ProcessOperation::Exec(request) => {
            let mut request = request.into_request();
            let fd_image_commit = request.options.executable_fd.is_some();
            let preflight = if fd_image_commit {
                validate_wasm_fd_image_commit_request(&request)
            } else {
                merge_process_internal_bootstrap_env(sidecar, vm_id, &mut request)
                    .and_then(|()| validate_process_launch_request(&request, true))
            };
            if let Err(error) = preflight {
                settle_context_process_reply(&reply, Err(error))?;
                return Ok(());
            }
            if !reply.claim().map_err(SidecarError::from)? {
                return Ok(());
            }
            let local_replacement = request.options.local_replacement;
            let result = if fd_image_commit {
                sidecar.commit_wasm_fd_process_image(vm_id, process_id, &[], request)
            } else {
                sidecar.exec_process_image(vm_id, process_id, &[], request)
            };
            match result {
                Ok(()) if local_replacement => reply
                    .succeed_json(json!({ "committed": true }))
                    .map_err(SidecarError::from)?,
                Ok(()) => reply.dismiss_claimed().map_err(SidecarError::from)?,
                Err(error) => reply
                    .fail(host_service_error(&error))
                    .map_err(SidecarError::from)?,
            }
        }
        other => {
            reply
                .fail(unsupported("process context", other))
                .map_err(SidecarError::from)?;
        }
    }
    Ok(())
}

pub(super) fn merge_process_internal_bootstrap_env<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    request: &mut ProcessLaunchRequest,
) -> Result<(), SidecarError> {
    let vm = sidecar
        .vms
        .get(vm_id)
        .ok_or_else(|| SidecarError::host("ESTALE", "host call VM no longer exists"))?;
    let mut internal = sanitize_javascript_child_process_internal_bootstrap_env(&vm.guest_env);
    internal.extend(sanitize_javascript_child_process_internal_bootstrap_env(
        &request.options.internal_bootstrap_env,
    ));
    request.options.internal_bootstrap_env = internal;
    Ok(())
}

fn settle_context_process_reply(
    reply: &DirectHostReplyHandle,
    result: Result<HostCallReply, SidecarError>,
) -> Result<(), SidecarError> {
    match result {
        Ok(response) => reply.succeed(response),
        Err(error) => reply.fail(host_service_error(&error)),
    }
    .map_err(SidecarError::from)
}

fn host_operation_effects(operation: &HostOperation) -> HostOperationEffects {
    match operation {
        HostOperation::Filesystem(
            FilesystemOperation::Close { .. } | FilesystemOperation::CloseFrom { .. },
        ) => HostOperationEffects {
            may_make_fd_readable: true,
            may_make_fd_writable: false,
        },
        _ => HostOperationEffects::default(),
    }
}

fn execute<C, Operation>(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    operation: Operation,
    reply: &DirectHostReplyHandle,
) -> Result<Option<HostCallReply>, HostServiceError>
where
    C: SidecarHostCapability<Operation>,
{
    if C::requires_claim(&operation) && !reply.claim()? {
        return Ok(None);
    }
    C::execute(kernel, process, operation).map(Some)
}

fn kernel_host_error(error: KernelError) -> HostServiceError {
    HostServiceError::new(error.code(), error.to_string())
}

fn unsupported(family: &str, operation: impl fmt::Debug) -> HostServiceError {
    HostServiceError::new(
        "ENOSYS",
        format!("{family} host operation is not implemented by the shared sidecar: {operation:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingReplyTarget {
        replies: Mutex<Vec<(bool, Result<HostCallReply, HostServiceError>)>>,
    }

    struct RejectingClaimTarget;

    impl agentos_execution::backend::DirectHostReplyTarget for RejectingClaimTarget {
        fn claim(&self, _call_id: u64) -> Result<bool, HostServiceError> {
            Ok(false)
        }

        fn respond(
            &self,
            _call_id: u64,
            _claimed: bool,
            _result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            panic!("a rejected claim must not be settled again")
        }
    }

    impl agentos_execution::backend::DirectHostReplyTarget for RecordingReplyTarget {
        fn claim(&self, _call_id: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _call_id: u64,
            claimed: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            self.replies
                .lock()
                .expect("reply lock")
                .push((claimed, result));
            Ok(())
        }
    }

    fn recording_reply(
        target: Arc<RecordingReplyTarget>,
    ) -> agentos_execution::backend::DirectHostReplyHandle {
        agentos_execution::backend::DirectHostReplyHandle::new(
            agentos_execution::backend::HostCallIdentity {
                generation: 1,
                pid: 2,
                call_id: 3,
            },
            target,
            1024,
        )
        .expect("reply")
    }

    fn request(method: &str, args: Vec<Value>) -> HostRpcRequest {
        HostRpcRequest {
            id: 1,
            method: method.to_owned(),
            args,
            raw_bytes_args: HashMap::new(),
        }
    }

    #[test]
    fn rejected_context_claim_performs_no_dns_or_http_work() {
        let reply = agentos_execution::backend::DirectHostReplyHandle::new(
            agentos_execution::backend::HostCallIdentity {
                generation: 1,
                pid: 2,
                call_id: 9,
            },
            Arc::new(RejectingClaimTarget),
            1024,
        )
        .expect("reply");
        let mut work_started = false;
        if claim_context_host_work(&reply).expect("claim") {
            work_started = true;
        }
        assert!(!work_started);
    }

    #[test]
    fn context_preflight_failure_settles_the_original_typed_error() {
        let target = Arc::new(RecordingReplyTarget::default());
        let reply = recording_reply(Arc::clone(&target));
        assert!(settle_context_preflight::<()>(
            &reply,
            Err(SidecarError::host("ERR_INVALID_URL", "invalid URL")),
        )
        .expect("settle preflight")
        .is_none());
        let replies = target.replies.lock().expect("replies");
        assert!(!replies[0].0);
        assert_eq!(
            replies[0].1.as_ref().expect_err("typed error").code,
            "ERR_INVALID_URL"
        );
    }

    #[test]
    fn deferred_guest_wait_deadlines_are_bounded_and_checked() {
        assert!(checked_deferred_guest_wait_deadline(MAX_DEFERRED_GUEST_WAIT_MS).is_ok());
        let error = checked_deferred_guest_wait_deadline(MAX_DEFERRED_GUEST_WAIT_MS + 1)
            .expect_err("duration above the ABI bound must fail");
        assert_eq!(error.code, "EINVAL");
        let details = error.details.expect("duration limit details");
        assert_eq!(details["limitName"], "guestWaitDurationMs");
        assert_eq!(details["limit"], MAX_DEFERRED_GUEST_WAIT_MS);
        assert_eq!(details["observed"], MAX_DEFERRED_GUEST_WAIT_MS + 1);
    }

    #[test]
    fn typed_waitpid_decodes_an_optional_bounded_probe_deadline() {
        let operation = decode_host_operation(
            &request("process.waitpid", vec![json!(-1), json!(0), json!(10_000)]),
            true,
            1024,
        )
        .expect("decode waitpid")
        .expect("typed waitpid operation");

        assert!(matches!(
            operation,
            HostOperation::Process(ProcessOperation::Wait {
                target: WaitTarget::Any,
                options: 0,
                deadline_ms: Some(10_000),
                temporary_mask: None,
            })
        ));
    }

    #[test]
    fn typed_identity_route_accepts_explicit_unchanged_ids() {
        let operation = decode_host_operation(
            &request(
                "process.setresgid",
                vec![Value::Null, json!(1000), Value::Null],
            ),
            true,
            1024,
        )
        .expect("decode identity operation")
        .expect("typed identity route");

        assert!(matches!(
            operation,
            HostOperation::Identity(IdentityOperation::SetGroupIds {
                real: None,
                effective: Some(1000),
                saved: None,
            })
        ));
    }

    fn operation_family(operation: &HostOperation) -> inventory::HostCapabilityFamily {
        match operation {
            HostOperation::Terminal(_) => inventory::HostCapabilityFamily::Terminal,
            HostOperation::Signal(_) => inventory::HostCapabilityFamily::Signal,
            HostOperation::Identity(_) => inventory::HostCapabilityFamily::Identity,
            HostOperation::Clock(_) => inventory::HostCapabilityFamily::Clock,
            HostOperation::Entropy(_) => inventory::HostCapabilityFamily::Entropy,
            other => panic!("unexpected capability family for assigned RPC: {other:?}"),
        }
    }

    #[test]
    fn every_live_identity_terminal_signal_clock_and_entropy_rpc_has_a_typed_route() {
        use inventory::HostCapabilityFamily::{Clock, Entropy, Identity, Signal, Terminal};

        let cases = vec![
            ("process.getuid", vec![], Identity),
            ("process.geteuid", vec![], Identity),
            ("process.getgid", vec![], Identity),
            ("process.getegid", vec![], Identity),
            ("process.getresuid", vec![], Identity),
            ("process.getresgid", vec![], Identity),
            ("process.getgroups", vec![], Identity),
            ("process.getpwuid", vec![json!(1000)], Identity),
            ("process.getpwnam", vec![json!("agentos")], Identity),
            ("process.getpwent", vec![json!(0)], Identity),
            ("process.getgrgid", vec![json!(1000)], Identity),
            ("process.getgrnam", vec![json!("agentos")], Identity),
            ("process.getgrent", vec![json!(0)], Identity),
            ("process.setuid", vec![json!(1000)], Identity),
            ("process.seteuid", vec![json!(1000)], Identity),
            ("process.setreuid", vec![json!(1000), json!(1000)], Identity),
            (
                "process.setresuid",
                vec![json!(1000), json!(1000), json!(1000)],
                Identity,
            ),
            ("process.setgid", vec![json!(1000)], Identity),
            ("process.setegid", vec![json!(1000)], Identity),
            ("process.setregid", vec![json!(1000), json!(1000)], Identity),
            (
                "process.setresgid",
                vec![json!(1000), json!(1000), json!(1000)],
                Identity,
            ),
            ("process.setgroups", vec![json!([1000])], Identity),
            ("__kernel_isatty", vec![json!(0)], Terminal),
            ("__kernel_tty_size", vec![json!(0)], Terminal),
            (
                "__kernel_tty_set_size",
                vec![json!(0), json!(80), json!(24)],
                Terminal,
            ),
            ("__kernel_tcgetattr", vec![json!(0)], Terminal),
            (
                "__kernel_tcsetattr",
                vec![json!(0), json!(63), json!([1, 2, 3, 4, 5, 6, 7])],
                Terminal,
            ),
            ("__kernel_tcgetpgrp", vec![json!(0)], Terminal),
            ("__kernel_tcsetpgrp", vec![json!(0), json!(42)], Terminal),
            ("__kernel_tcgetsid", vec![json!(0)], Terminal),
            ("__pty_set_raw_mode", vec![json!(true)], Terminal),
            ("process.pty_open", vec![], Terminal),
            ("process.signal_begin", vec![], Signal),
            ("process.signal_end", vec![json!(7)], Signal),
            ("process.signal_mask", vec![json!(3), json!([])], Signal),
            (
                "process.signal_mask_scope_begin",
                vec![json!([2, 15])],
                Signal,
            ),
            ("process.signal_mask_scope_end", vec![json!(7)], Signal),
            (
                "process.signal_state",
                vec![json!(15), json!("user"), json!("[2]"), json!(0)],
                Signal,
            ),
            ("process.take_signal", vec![], Signal),
            (
                "process.clock_time",
                vec![json!(1), json!("1"), Value::Null],
                Clock,
            ),
            ("process.clock_resolution", vec![json!(1)], Clock),
            ("process.itimer_real", vec![json!(0)], Clock),
            ("process.sleep", vec![json!(1)], Clock),
            ("process.random_get", vec![json!(1024)], Entropy),
        ];

        let mut covered = BTreeSet::new();
        for (method, args, expected_family) in cases {
            let operation = decode_host_operation(&request(method, args), true, 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {method}: {error}"))
                .unwrap_or_else(|| panic!("{method} fell through to the legacy dispatcher"));
            assert_eq!(operation_family(&operation), expected_family, "{method}");
            assert!(covered.insert(method), "duplicate coverage for {method}");
        }

        let assigned_families = [Identity, Terminal, Signal, Clock, Entropy];
        let expected = inventory::semantic_rpc_inventory()
            .into_iter()
            .filter(|method| {
                inventory::capability_family(method)
                    .is_some_and(|family| assigned_families.contains(&family))
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(covered, expected);
    }

    #[test]
    fn exec_decoder_keeps_path_and_prepared_fd_commit_modes_distinct() {
        let prepared = json!({
            "command": "/proc/self/fd/3",
            "options": {
                "executableFd": 3,
                "localReplacement": true,
            },
        });
        assert!(
            decode_host_operation(&request("process.exec", vec![prepared]), true, 1024 * 1024,)
                .is_err()
        );
        assert!(decode_host_operation(
            &request(
                "process.exec_fd_image_commit",
                vec![json!({ "command": "/bin/true" })],
            ),
            true,
            1024 * 1024,
        )
        .is_err());
    }

    #[test]
    fn compatibility_decoder_emits_each_shared_capability_family_it_supports() {
        let cases = [
            request("process.fd_preopens", vec![]),
            request(
                "process.fd_socketpair",
                vec![json!(1), json!(false), json!(true)],
            ),
            request("process.umask", vec![Value::Null]),
            request("__kernel_tty_size", vec![json!(1)]),
            request("process.signal_end", vec![json!(7)]),
            request("process.getresuid", vec![]),
            request("process.clock_resolution", vec![json!(1)]),
        ];
        let operations = cases
            .iter()
            .map(|request| {
                decode_host_operation(request, true, 1024 * 1024)
                    .expect("decode")
                    .expect("typed route")
            })
            .collect::<Vec<_>>();
        assert!(matches!(operations[0], HostOperation::Filesystem(_)));
        assert!(matches!(operations[1], HostOperation::Network(_)));
        assert!(matches!(operations[2], HostOperation::Process(_)));
        assert!(matches!(operations[3], HostOperation::Terminal(_)));
        assert!(matches!(operations[4], HostOperation::Signal(_)));
        assert!(matches!(operations[5], HostOperation::Identity(_)));
        assert!(matches!(operations[6], HostOperation::Clock(_)));
    }

    #[test]
    fn compatibility_adapter_selects_unpositioned_write_progress_without_runtime_branching() {
        let write = request("process.fd_write", vec![json!(4), json!("data")]);
        let positioned = request(
            "process.fd_pwrite",
            vec![json!(4), json!("data"), json!("0")],
        );

        assert!(matches!(
            decode_host_operation(&write, true, 1024).expect("decode WASM write"),
            Some(HostOperation::Filesystem(FilesystemOperation::Write {
                nonblocking: true,
                offset: None,
                ..
            }))
        ));
        assert!(
            decode_host_operation(&write, false, 1024)
                .expect("decode non-WASM adapter write")
                .is_none(),
            "non-WASM adapters retain their existing adapter-local write path"
        );
        assert!(matches!(
            decode_host_operation(&positioned, true, 1024).expect("decode positioned write"),
            Some(HostOperation::Filesystem(FilesystemOperation::Write {
                nonblocking: false,
                offset: Some(0),
                ..
            }))
        ));
    }

    #[test]
    fn compatibility_decoder_routes_owned_process_lifecycle_operations() {
        let launch = json!({
            "command": "/opt/agentos/bin/ls",
            "args": ["-la"],
            "options": { "cwd": "/tmp", "stdio": ["pipe", "pipe", "pipe"] },
        });
        let cases = [
            request("child_process.spawn", vec![launch.clone()]),
            request("child_process.poll", vec![json!("child-1"), json!(5000)]),
            request(
                "child_process.write_stdin",
                vec![json!("child-1"), json!("input")],
            ),
            request("child_process.close_stdin", vec![json!("child-1")]),
            request("process.exec", vec![launch]),
        ];
        let operations = cases
            .iter()
            .map(|request| {
                decode_host_operation(request, true, 1024 * 1024)
                    .expect("decode process operation")
                    .expect("typed process route")
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            operations[0],
            HostOperation::Process(ProcessOperation::Spawn(_))
        ));
        assert!(matches!(
            operations[1],
            HostOperation::Process(ProcessOperation::PollChild { wait_ms: 5000, .. })
        ));
        assert!(matches!(
            operations[2],
            HostOperation::Process(ProcessOperation::WriteChildStdin { .. })
        ));
        assert!(matches!(
            operations[3],
            HostOperation::Process(ProcessOperation::CloseChildStdin { .. })
        ));
        assert!(matches!(
            operations[4],
            HostOperation::Process(ProcessOperation::Exec(_))
        ));
    }

    #[test]
    fn every_frozen_process_rpc_is_typed_except_reviewed_adapter_calls() {
        let launch = json!({ "command": "/opt/agentos/bin/true" });
        let cases = vec![
            ("child_process.close_stdin", vec![json!("child-1")]),
            ("child_process.poll", vec![json!("child-1"), json!(0)]),
            ("child_process.spawn", vec![launch.clone()]),
            (
                "child_process.write_stdin",
                vec![json!("child-1"), json!("input")],
            ),
            ("process.exec", vec![launch]),
            (
                "process.exec_fd_image_commit",
                vec![json!({
                    "command": "/proc/self/fd/3",
                    "options": {
                        "executableFd": 3,
                        "localReplacement": true,
                    },
                })],
            ),
            ("process.exec_image_open", vec![json!("/bin/sh")]),
            ("process.exec_image_open_fd", vec![json!(3)]),
            (
                "process.exec_image_read",
                vec![json!("1"), json!("0"), json!(1024)],
            ),
            ("process.exec_image_close", vec![json!("1")]),
            ("process.image", vec![]),
            ("process.getpgid", vec![json!(0)]),
            ("process.getrlimit", vec![json!(7)]),
            ("process.kill", vec![json!(42), json!("SIGTERM")]),
            ("process.setpgid", vec![json!(0), json!(0)]),
            (
                "process.setrlimit",
                vec![json!(7), json!("64"), json!("64")],
            ),
            ("process.system_identity", vec![]),
            ("process.umask", vec![Value::Null]),
            ("process.waitpid", vec![json!(-1), json!(1), json!(10_000)]),
            ("process.waitpid_transition", vec![json!(-1), json!(1)]),
        ];
        let mut covered = BTreeSet::new();
        for (method, args) in cases {
            let operation = decode_host_operation(&request(method, args), true, 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {method}: {error}"))
                .unwrap_or_else(|| panic!("{method} fell through to the legacy dispatcher"));
            assert!(
                matches!(operation, HostOperation::Process(_)),
                "{method} decoded outside the process capability: {operation:?}"
            );
            assert!(covered.insert(method), "duplicate process case {method}");
        }

        let expected = inventory::semantic_rpc_inventory()
            .into_iter()
            .filter(|method| {
                inventory::capability_family(method)
                    == Some(inventory::HostCapabilityFamily::Process)
                    && !inventory::WASM_ADAPTER_ONLY_RPCS.contains(method)
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(covered, expected);
    }

    #[test]
    fn process_preflight_and_post_claim_failures_preserve_the_original_errno() {
        let preflight_target = Arc::new(RecordingReplyTarget::default());
        let preflight_reply = recording_reply(Arc::clone(&preflight_target));
        settle_context_process_reply(
            &preflight_reply,
            Err(SidecarError::host("EINVAL", "invalid launch options")),
        )
        .expect("settle preflight failure");
        let preflight = preflight_target.replies.lock().expect("preflight reply");
        assert!(!preflight[0].0);
        assert!(matches!(&preflight[0].1, Err(error) if error.code == "EINVAL"));

        let claimed_target = Arc::new(RecordingReplyTarget::default());
        let claimed_reply = recording_reply(Arc::clone(&claimed_target));
        assert!(claimed_reply.claim().expect("claim side effect"));
        settle_context_process_reply(
            &claimed_reply,
            Err(SidecarError::host("EPIPE", "child stdin closed")),
        )
        .expect("settle claimed failure");
        let claimed = claimed_target.replies.lock().expect("claimed reply");
        assert!(claimed[0].0);
        assert!(matches!(&claimed[0].1, Err(error) if error.code == "EPIPE"));
    }

    #[test]
    fn compatibility_decoder_preserves_only_unknown_calls_for_legacy_service() {
        assert!(
            decode_host_operation(&request("fs.readSync", vec![]), true, 1024)
                .expect("unknown method")
                .is_none()
        );
        assert!(matches!(
            decode_host_operation(
                &request(
                    "process.fd_socketpair",
                    vec![json!(3), json!(false), json!(false)],
                ),
                true,
                1024
            )
            .expect("seqpacket decode"),
            Some(HostOperation::Network(NetworkOperation::SocketPair {
                kind: SocketKind::SeqPacket,
                nonblocking: false,
                close_on_exec: false,
            }))
        ));
    }

    #[test]
    fn assigned_decoders_reject_unbounded_or_malformed_payloads_before_dispatch() {
        let too_many_groups = vec![json!(1000); MAX_SUPPLEMENTARY_GROUPS + 1];
        assert!(decode_host_operation(
            &request("process.setgroups", vec![Value::Array(too_many_groups)]),
            true,
            1024 * 1024,
        )
        .is_err());

        assert!(decode_host_operation(
            &request(
                "process.getpwnam",
                vec![json!("x".repeat(MAX_ACCOUNT_NAME_BYTES + 1))],
            ),
            true,
            1024 * 1024,
        )
        .is_err());

        let too_many_signals = vec![json!(2); MAX_SIGNAL_SET_ENTRIES + 1];
        assert!(decode_host_operation(
            &request(
                "process.signal_mask_scope_begin",
                vec![Value::Array(too_many_signals)],
            ),
            true,
            1024 * 1024,
        )
        .is_err());

        assert!(decode_host_operation(
            &request(
                "process.signal_state",
                vec![
                    json!(15),
                    json!("user"),
                    json!(" ".repeat(MAX_SIGNAL_STATE_MASK_JSON_BYTES + 1)),
                    json!(0),
                ],
            ),
            true,
            1024 * 1024,
        )
        .is_err());

        assert!(decode_host_operation(
            &request(
                "__kernel_tcsetattr",
                vec![json!(0), json!(0), json!([1, 2, 3, 4, 5, 6, 7, 8])],
            ),
            true,
            1024 * 1024,
        )
        .is_err());

        assert!(decode_host_operation(
            &request(
                "process.random_get",
                vec![json!(MAX_ENTROPY_CHUNK_BYTES + 1)],
            ),
            true,
            1024 * 1024,
        )
        .is_err());
        assert!(decode_host_operation(
            &request("process.random_get", vec![json!(1025)]),
            true,
            1024,
        )
        .is_err());

        assert!(decode_host_operation(
            &request(
                "child_process.write_stdin",
                vec![json!("child-1"), json!("x".repeat(1025))],
            ),
            true,
            1024,
        )
        .is_err());
        assert!(decode_host_operation(
            &request(
                "child_process.spawn",
                vec![json!({
                    "command": "/bin/echo",
                    "args": ["x".repeat(1024)],
                })],
            ),
            true,
            128,
        )
        .is_err());
    }

    #[test]
    fn account_record_decoder_caps_output_to_the_reply_transport_limit() {
        let operation =
            decode_host_operation(&request("process.getpwuid", vec![json!(1000)]), true, 128)
                .expect("decode account lookup")
                .expect("typed account lookup");
        assert!(matches!(
            operation,
            HostOperation::Identity(IdentityOperation::PasswdById {
                max_record_bytes,
                ..
            }) if max_record_bytes.get() == 128
        ));
    }

    #[test]
    fn resource_limit_decoder_preserves_numeric_and_string_wire_values() {
        for value in [json!(64), json!("64")] {
            let operation = decode_host_operation(
                &request("process.setrlimit", vec![json!(7), value.clone(), value]),
                true,
                1024,
            )
            .expect("decode rlimit")
            .expect("typed rlimit route");
            assert!(matches!(
                operation,
                HostOperation::Process(ProcessOperation::SetResourceLimit {
                    kind: ResourceLimitKind::OpenFiles,
                    value: ResourceLimitValue {
                        soft: Some(64),
                        hard: Some(64),
                    },
                })
            ));
        }
    }

    #[test]
    fn side_effecting_typed_operations_claim_the_direct_reply_before_execution() {
        assert!(process::ProcessCapability::requires_claim(
            &ProcessOperation::Kill {
                target: 42,
                signal: 15,
            }
        ));
        assert!(process::ProcessCapability::requires_claim(
            &ProcessOperation::SetProcessGroup {
                pid: Some(42),
                pgid: Some(42),
            }
        ));
        assert!(process::ProcessCapability::requires_claim(
            &ProcessOperation::Wait {
                target: WaitTarget::Any,
                options: 1,
                deadline_ms: None,
                temporary_mask: None,
            }
        ));
        assert!(clock::ClockCapability::requires_claim(
            &ClockOperation::RealIntervalSet {
                initial_us: 1,
                interval_us: 1,
            }
        ));
        assert!(identity::IdentityCapability::requires_claim(
            &IdentityOperation::SetId {
                kind: IdentityIdKind::EffectiveUser,
                value: Some(1),
            }
        ));
        assert!(terminal::TerminalCapability::requires_claim(
            &TerminalOperation::SetRawMode {
                fd: 0,
                enabled: true,
            }
        ));
        assert!(signal::SignalCapability::requires_claim(
            &SignalOperation::SetAction {
                signal: 15,
                action: SignalActionValue {
                    disposition: SignalDispositionValue::User,
                    flags: 0,
                    mask: SignalSetValue::default(),
                },
            }
        ));
        assert!(signal::SignalCapability::requires_claim(
            &SignalOperation::BeginTemporaryMask {
                mask: SignalSetValue::default(),
            }
        ));
        assert!(entropy::EntropyCapability::requires_claim(
            &EntropyOperation {
                length: BoundedUsize::try_new(
                    1,
                    &PayloadLimit::new("maxEntropyChunkBytes", 1).expect("limit"),
                )
                .expect("bounded entropy"),
            }
        ));
        assert!(network::NetworkCapability::requires_claim(
            &NetworkOperation::SocketPair {
                kind: SocketKind::Stream,
                nonblocking: true,
                close_on_exec: true,
            }
        ));

        assert!(!process::ProcessCapability::requires_claim(
            &ProcessOperation::GetPid
        ));
        assert!(!clock::ClockCapability::requires_claim(
            &ClockOperation::RealIntervalGet
        ));
        assert!(!identity::IdentityCapability::requires_claim(
            &IdentityOperation::GetUserIds
        ));
        assert!(!terminal::TerminalCapability::requires_claim(
            &TerminalOperation::GetAttributes { fd: 0 }
        ));
        assert!(!network::NetworkCapability::requires_claim(
            &NetworkOperation::ResolveDns {
                host: BoundedString::try_new(
                    String::from("example.test"),
                    &PayloadLimit::new("maxDnsNameBytes", 253).expect("limit"),
                )
                .expect("host"),
                port: None,
                family: agentos_execution::host::DnsAddressFamily::Any,
                max_results: BoundedUsize::try_new(
                    16,
                    &PayloadLimit::new("maxDnsResults", 16).expect("limit"),
                )
                .expect("results"),
            }
        ));
    }

    #[test]
    fn every_vm_scoped_host_family_is_classified_before_kernel_only_dispatch() {
        let cases = [
            request("__kernel_stdin_read", vec![json!(4096), json!(0)]),
            request("process.fd_read", vec![json!(3), json!(4096), Value::Null]),
            request(
                "dns.lookup",
                vec![json!({"hostname":"localhost","family":4})],
            ),
            request(
                "__kernel_poll",
                vec![json!([{ "fd": 0, "events": 1 }]), json!(0)],
            ),
            request("net.connect", vec![json!({"host":"127.0.0.1","port":80})]),
            request("dgram.poll", vec![json!("udp-1"), json!(0)]),
            request(
                "child_process.spawn",
                vec![json!({"command":"/opt/agentos/bin/true"})],
            ),
            request("process.waitpid", vec![json!(-1), json!(1)]),
            request("process.sleep", vec![json!(1)]),
        ];
        for request in cases {
            let operation = decode_host_operation(&request, true, 1024 * 1024)
                .unwrap_or_else(|error| panic!("decode {}: {error}", request.method))
                .unwrap_or_else(|| panic!("{} fell through to legacy dispatch", request.method));
            assert!(
                requires_context_host_dispatch(&operation),
                "{} must traverse context dispatch before kernel-only fallback",
                request.method
            );
        }

        assert!(!requires_context_host_dispatch(&HostOperation::Process(
            ProcessOperation::GetPid,
        )));
        assert!(!requires_context_host_dispatch(&HostOperation::Clock(
            ClockOperation::Resolution {
                clock: GuestClockId::Monotonic,
            },
        )));
    }
}
