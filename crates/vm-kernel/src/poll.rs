use crate::socket_table::SocketId;
use event_listener::Event;
use std::ops::{BitOr, BitOrAssign};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use web_time::Instant;

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
pub const POLLRDNORM: PollEvents = PollEvents(0x0040);
pub const POLLWRNORM: PollEvents = PollEvents(0x0100);

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

/// Cloneable, Send handle onto the kernel's poll notifier so a caller can wait
/// for "some poll-visible state changed" OFF the thread that owns the kernel
/// (deferred readiness servicing), then re-check readiness with a zero-timeout
/// poll on the owning thread. Take `snapshot()` BEFORE the readiness check it
/// guards so a change landing between the check and the wait wakes immediately
/// instead of being lost.
#[derive(Debug, Clone)]
pub struct PollWaitHandle {
    notifier: PollNotifier,
}

impl PollWaitHandle {
    pub(crate) fn new(notifier: PollNotifier) -> Self {
        Self { notifier }
    }

    /// Snapshot the current change generation.
    pub fn snapshot(&self) -> u64 {
        self.notifier.snapshot()
    }

    /// Block until the generation moves past `observed` or `timeout` elapses
    /// (`None` = wait forever). Returns true when a change was observed.
    pub fn wait_for_change(&self, observed: u64, timeout: Option<Duration>) -> bool {
        self.notifier.wait_for_change(observed, timeout)
    }

    /// Yield until the generation moves past `observed` without occupying an
    /// OS thread. The returned future is runtime-neutral; the sidecar may await
    /// it on its one process-owned executor while browser-free kernel users may
    /// use any compatible executor.
    pub async fn wait_for_change_async(&self, observed: u64) -> bool {
        self.notifier.wait_for_change_async(observed).await
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PollNotifier {
    inner: Arc<PollNotifierInner>,
}

#[derive(Debug, Default)]
struct PollNotifierInner {
    generation: Mutex<u64>,
    waiters: Condvar,
    async_waiters: Event,
}

impl PollNotifier {
    pub(crate) fn notify(&self) {
        {
            let mut generation = lock_or_recover(&self.inner.generation);
            *generation = generation.wrapping_add(1);
        }
        self.inner.waiters.notify_all();
        self.inner.async_waiters.notify(usize::MAX);
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

    pub(crate) async fn wait_for_change_async(&self, observed: u64) -> bool {
        loop {
            // Register before checking the generation so a notification that
            // races this check is retained by the listener rather than lost.
            let listener = self.inner.async_waiters.listen();
            if self.snapshot() != observed {
                return true;
            }
            listener.await;
            if self.snapshot() != observed {
                return true;
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

#[cfg(test)]
mod tests {
    use super::PollNotifier;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn infinite_wait_returns_after_notification_without_waiter_storage() {
        let notifier = PollNotifier::default();
        let observed = notifier.snapshot();
        let waiter = notifier.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            started_tx.send(()).expect("signal waiter start");
            let changed = waiter.wait_for_change(observed, None);
            done_tx.send(changed).expect("signal waiter result");
        });

        started_rx.recv().expect("waiter should start");
        assert!(
            done_rx.recv_timeout(Duration::from_millis(25)).is_err(),
            "waiter should stay blocked before notification"
        );

        notifier.notify();
        assert!(done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter should wake after notification"));
        handle.join().expect("waiter thread should finish");
    }

    #[test]
    fn saturated_generation_still_notifies_waiters() {
        let notifier = PollNotifier::default();
        {
            let mut generation = super::lock_or_recover(&notifier.inner.generation);
            *generation = u64::MAX;
        }

        let observed = notifier.snapshot();
        let waiter = notifier.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            started_tx.send(()).expect("signal waiter start");
            let changed = waiter.wait_for_change(observed, Some(Duration::from_secs(1)));
            done_tx.send(changed).expect("signal waiter result");
        });

        started_rx.recv().expect("waiter should start");
        notifier.notify();

        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("waiter should return after saturated notify"),
            "saturated notify should still wake the waiter"
        );
        handle.join().expect("waiter thread should finish");
    }
}
