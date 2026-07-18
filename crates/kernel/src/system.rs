use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelClockId {
    Realtime,
    Monotonic,
    ProcessCpu,
    ThreadCpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemIdentity {
    pub hostname: String,
    pub os_type: String,
    pub os_release: String,
    pub os_version: String,
    pub machine: String,
    pub domain_name: String,
}

impl Default for SystemIdentity {
    fn default() -> Self {
        Self {
            hostname: String::from("secure-exec"),
            os_type: String::from("Linux"),
            os_release: String::from("6.8.0-secure-exec"),
            os_version: String::from("#1 SMP PREEMPT_DYNAMIC secure-exec"),
            machine: String::from("x86_64"),
            domain_name: String::from("localdomain"),
        }
    }
}

pub(crate) fn realtime_now_ns() -> Option<u64> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    u64::try_from(nanos).ok()
}
