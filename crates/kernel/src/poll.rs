use crate::socket_table::SocketId;
use std::ops::{BitOr, BitOrAssign};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PollEvents(u16);

impl PollEvents {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for PollEvents {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for PollEvents {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

pub const POLLIN: PollEvents = PollEvents(0x0001);
pub const POLLOUT: PollEvents = PollEvents(0x0004);
pub const POLLERR: PollEvents = PollEvents(0x0008);
pub const POLLHUP: PollEvents = PollEvents(0x0010);
pub const POLLNVAL: PollEvents = PollEvents(0x0020);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollFd {
    pub fd: u32,
    pub events: PollEvents,
    pub revents: PollEvents,
}

impl PollFd {
    pub const fn new(fd: u32, events: PollEvents) -> Self {
        Self {
            fd,
            events,
            revents: PollEvents::empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    pub ready_count: usize,
    pub fds: Vec<PollFd>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTarget {
    Fd(u32),
    Socket(SocketId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollTargetEntry {
    pub target: PollTarget,
    pub events: PollEvents,
    pub revents: PollEvents,
}

impl PollTargetEntry {
    pub const fn new(target: PollTarget, events: PollEvents) -> Self {
        Self {
            target,
            events,
            revents: PollEvents::empty(),
        }
    }

    pub const fn fd(fd: u32, events: PollEvents) -> Self {
        Self::new(PollTarget::Fd(fd), events)
    }

    pub const fn socket(socket_id: SocketId, events: PollEvents) -> Self {
        Self::new(PollTarget::Socket(socket_id), events)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollTargetResult {
    pub ready_count: usize,
    pub targets: Vec<PollTargetEntry>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PollNotifier {
    inner: Arc<PollNotifierInner>,
}

#[derive(Debug, Default)]
struct PollNotifierInner {
    generation: Mutex<u64>,
    waiters: Condvar,
}

impl PollNotifier {
    pub(crate) fn notify(&self) {
        let mut generation = lock_or_recover(&self.inner.generation);
        *generation = generation.saturating_add(1);
        self.inner.waiters.notify_all();
    }

    pub(crate) fn snapshot(&self) -> u64 {
        *lock_or_recover(&self.inner.generation)
    }

    pub(crate) fn wait_for_change(&self, observed: u64, timeout: Option<Duration>) -> bool {
        let mut generation = lock_or_recover(&self.inner.generation);
        if *generation != observed {
            return true;
        }

        let Some(timeout) = timeout else {
            while *generation == observed {
                generation = wait_or_recover(&self.inner.waiters, generation);
            }
            return true;
        };

        if timeout.is_zero() {
            return *generation != observed;
        }

        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return *generation != observed;
            }

            let remaining = deadline.saturating_duration_since(now);
            let (next_generation, wait_result) =
                wait_timeout_or_recover(&self.inner.waiters, generation, remaining);
            generation = next_generation;
            if *generation != observed {
                return true;
            }
            if wait_result.timed_out() {
                return false;
            }
        }
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_or_recover<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condvar.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_timeout_or_recover<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult) {
    match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => poisoned.into_inner(),
    }
}
