use super::*;
use agentos_kernel::system::KernelClockId;

pub(super) struct ClockCapability;

impl SidecarHostCapability<ClockOperation> for ClockCapability {
    fn requires_claim(operation: &ClockOperation) -> bool {
        matches!(
            operation,
            ClockOperation::Sleep { .. } | ClockOperation::RealIntervalSet { .. }
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: ClockOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let value = match operation {
            ClockOperation::Time {
                clock,
                precision_ns: _,
                deterministic_realtime_ns,
            } => kernel
                .clock_time_ns(kernel_clock(clock), deterministic_realtime_ns)
                .map(|nanoseconds| json!(nanoseconds.to_string()))
                .map_err(kernel_host_error)?,
            ClockOperation::Resolution { clock } => kernel
                .clock_resolution_ns(kernel_clock(clock))
                .map(|nanoseconds| json!(nanoseconds.to_string()))
                .map_err(kernel_host_error)?,
            ClockOperation::Sleep { .. } => {
                return Err(HostServiceError::new(
                    "EINVAL",
                    "sleep requires sidecar timer context",
                ));
            }
            ClockOperation::RealIntervalGet => {
                let values = process.real_interval_timer.get();
                json!({ "remainingUs": values.0, "intervalUs": values.1 })
            }
            ClockOperation::RealIntervalSet {
                initial_us,
                interval_us,
            } => {
                let values = process.real_interval_timer.set(initial_us, interval_us);
                if values.2 {
                    process.kernel_handle.kill(libc::SIGALRM);
                }
                json!({ "remainingUs": values.0, "intervalUs": values.1 })
            }
            other => return Err(unsupported("clock", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}

fn kernel_clock(clock: GuestClockId) -> KernelClockId {
    match clock {
        GuestClockId::Realtime => KernelClockId::Realtime,
        GuestClockId::Monotonic => KernelClockId::Monotonic,
        GuestClockId::ProcessCpu => KernelClockId::ProcessCpu,
        GuestClockId::ThreadCpu => KernelClockId::ThreadCpu,
    }
}
