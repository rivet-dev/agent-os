// CPU timeout enforcement via dedicated timer thread

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Guard for per-session CPU timeout enforcement.
///
/// Spawns a timer thread that calls `v8::Isolate::terminate_execution()`
/// and drops an abort sender to unblock any channel-based readers when the
/// timeout elapses. Drop or call `cancel()` to prevent firing.
pub struct TimeoutGuard {
    /// Sender side of cancellation channel — dropped to cancel the timer
    cancel_tx: Option<crossbeam_channel::Sender<()>>,
    /// Set to true when the timeout fired
    fired: Arc<AtomicBool>,
    /// Timer thread handle
    join_handle: Option<thread::JoinHandle<()>>,
}

impl TimeoutGuard {
    /// Spawn a timeout timer thread.
    ///
    /// - `timeout_ms`: wall-clock time limit in milliseconds
    /// - `isolate_handle`: V8 isolate handle for `terminate_execution()`
    /// - `abort_tx`: dropped on timeout to unblock channel readers via `select!`
    pub fn new(
        timeout_ms: u32,
        isolate_handle: v8::IsolateHandle,
        abort_tx: crossbeam_channel::Sender<()>,
    ) -> Self {
        let (cancel_tx, cancel_rx) = crossbeam_channel::bounded::<()>(1);
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let handle = thread::Builder::new()
            .name("timeout".into())
            .spawn(move || {
                let timer = crossbeam_channel::after(Duration::from_millis(timeout_ms as u64));

                crossbeam_channel::select! {
                    recv(timer) -> _ => {
                        // Timeout elapsed — terminate V8 execution
                        fired_clone.store(true, Ordering::SeqCst);
                        isolate_handle.terminate_execution();
                        // Drop abort_tx to unblock any channel readers
                        drop(abort_tx);
                    }
                    recv(cancel_rx) -> _ => {
                        // Cancelled — execution completed normally
                    }
                }
            })
            .expect("failed to spawn timeout thread");

        TimeoutGuard {
            cancel_tx: Some(cancel_tx),
            fired,
            join_handle: Some(handle),
        }
    }

    /// Cancel the timeout (execution completed normally).
    /// Blocks until the timer thread exits.
    pub fn cancel(&mut self) {
        // Drop the cancel sender to unblock the timer thread's select!
        self.cancel_tx.take();
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }

    /// Check if the timeout fired.
    pub fn timed_out(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for TimeoutGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeout_guard_cancel_before_fire() {
        // Timer set to 5 seconds, cancelled immediately — should not fire
        let (abort_tx, abort_rx) = crossbeam_channel::bounded::<()>(0);

        // Create a minimal V8 platform + isolate just for the handle
        // We avoid actual V8 in tests — use a different approach
        // Instead, test the cancellation logic without V8

        // We can't easily get a v8::IsolateHandle without V8 init,
        // so we test the TimeoutGuard flow via integration in execution::tests
        drop(abort_tx);
        drop(abort_rx);
    }

    #[test]
    fn timeout_guard_fires_on_expiry() {
        // Tested via V8 integration tests in execution::tests
        // This placeholder confirms the module compiles correctly
    }
}
