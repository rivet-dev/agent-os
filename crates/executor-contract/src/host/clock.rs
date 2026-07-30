#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestClockId {
    Realtime,
    Monotonic,
    ProcessCpu,
    ThreadCpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockOperation {
    Time {
        clock: GuestClockId,
        precision_ns: u64,
        /// Optional runtime-configured realtime value. This is owned adapter
        /// input rather than a host clock lookup so deterministic VMs observe
        /// the same value under every execution engine.
        deterministic_realtime_ns: Option<u64>,
    },
    Resolution {
        clock: GuestClockId,
    },
    /// Interruptible guest sleep. The sidecar owns its timer and settles the
    /// adapter's direct reply lane when the deadline or a signal wins.
    Sleep {
        duration_ms: u64,
    },
    RealIntervalGet,
    RealIntervalSet {
        initial_us: u64,
        interval_us: u64,
    },
}
