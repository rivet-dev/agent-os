use super::*;
use agentos_kernel::pty::{PartialTermios, PartialTermiosControlChars};

const TTY_IFLAG_ICRNL: u32 = 1 << 0;
const TTY_OFLAG_OPOST: u32 = 1 << 1;
const TTY_OFLAG_ONLCR: u32 = 1 << 2;
const TTY_LFLAG_ICANON: u32 = 1 << 3;
const TTY_LFLAG_ECHO: u32 = 1 << 4;
const TTY_LFLAG_ISIG: u32 = 1 << 5;

pub(super) struct TerminalCapability;

impl SidecarHostCapability<TerminalOperation> for TerminalCapability {
    fn requires_claim(operation: &TerminalOperation) -> bool {
        matches!(
            operation,
            TerminalOperation::SetAttributes { .. }
                | TerminalOperation::SetWindowSize { .. }
                | TerminalOperation::SetForegroundProcessGroup { .. }
                | TerminalOperation::SetRawMode { .. }
                | TerminalOperation::OpenPty
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: TerminalOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let value = match operation {
            TerminalOperation::IsTerminal { fd } => json!(kernel
                .isatty(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_host_error)?),
            TerminalOperation::GetAttributes { fd } => {
                let attributes = kernel
                    .tcgetattr(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                    .map_err(kernel_host_error)?;
                let mut flags = 0_u32;
                flags |= TTY_IFLAG_ICRNL * u32::from(attributes.icrnl);
                flags |= TTY_OFLAG_OPOST * u32::from(attributes.opost);
                flags |= TTY_OFLAG_ONLCR * u32::from(attributes.onlcr);
                flags |= TTY_LFLAG_ICANON * u32::from(attributes.icanon);
                flags |= TTY_LFLAG_ECHO * u32::from(attributes.echo);
                flags |= TTY_LFLAG_ISIG * u32::from(attributes.isig);
                json!({
                    "flags": flags,
                    "cc": [
                        attributes.cc.vintr,
                        attributes.cc.vquit,
                        attributes.cc.vsusp,
                        attributes.cc.veof,
                        attributes.cc.verase,
                        attributes.cc.vkill,
                        attributes.cc.vwerase,
                    ],
                })
            }
            TerminalOperation::SetAttributes { fd, attributes } => {
                let cc = attributes.control_characters;
                kernel
                    .tcsetattr(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        PartialTermios {
                            icrnl: Some(attributes.input_flags & TTY_IFLAG_ICRNL != 0),
                            opost: Some(attributes.output_flags & TTY_OFLAG_OPOST != 0),
                            onlcr: Some(attributes.output_flags & TTY_OFLAG_ONLCR != 0),
                            icanon: Some(attributes.local_flags & TTY_LFLAG_ICANON != 0),
                            echo: Some(attributes.local_flags & TTY_LFLAG_ECHO != 0),
                            isig: Some(attributes.local_flags & TTY_LFLAG_ISIG != 0),
                            cc: Some(PartialTermiosControlChars {
                                vintr: Some(cc[0]),
                                vquit: Some(cc[1]),
                                vsusp: Some(cc[2]),
                                veof: Some(cc[3]),
                                verase: Some(cc[4]),
                                vkill: Some(cc[5]),
                                vwerase: Some(cc[6]),
                            }),
                        },
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            TerminalOperation::GetWindowSize { fd } => {
                let size = kernel
                    .pty_window_size(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                    .map_err(kernel_host_error)?;
                json!({ "cols": size.cols, "rows": size.rows })
            }
            TerminalOperation::SetWindowSize { fd, size } => {
                kernel
                    .pty_resize(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        size.columns,
                        size.rows,
                    )
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            TerminalOperation::GetForegroundProcessGroup { fd } => json!(kernel
                .tcgetpgrp(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_host_error)?),
            TerminalOperation::SetForegroundProcessGroup { fd, pgid } => {
                kernel
                    .pty_set_foreground_pgid(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, pgid)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            TerminalOperation::GetSession { fd } => json!(kernel
                .tcgetsid(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_host_error)?),
            TerminalOperation::SetRawMode { fd, enabled } => {
                process.tty_raw_mode_generation = kernel
                    .pty_set_raw_mode(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, enabled)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            TerminalOperation::OpenPty => {
                let (master_fd, slave_fd, path) = kernel
                    .open_pty(EXECUTION_DRIVER_NAME, process.kernel_pid)
                    .map_err(kernel_host_error)?;
                json!({ "masterFd": master_fd, "slaveFd": slave_fd, "path": path })
            }
            other => return Err(unsupported("terminal", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}
