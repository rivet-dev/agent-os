use super::{ExecutionEvent, HostServiceError, PayloadLimit};
use crate::host::HostProcessContext;
use serde::Serialize;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Runtime-neutral notification used after a common execution event becomes
/// durable. The callback may wake a sidecar broker or an executor event loop;
/// it must not run guest code itself.
pub trait ExecutionEventWakeTarget: Send + Sync {
    fn wake(&self);
}

impl<F> ExecutionEventWakeTarget for F
where
    F: Fn() + Send + Sync,
{
    fn wake(&self) {
        self();
    }
}

struct BoundedExecutionEventQueue {
    state: Mutex<ExecutionEventQueueState>,
    capacity: usize,
    retained_bytes: Arc<RetainedEventByteLedger>,
    closed: AtomicBool,
    wake: Arc<dyn ExecutionEventWakeTarget>,
}

struct ExecutionEventQueueState {
    events: VecDeque<QueuedExecutionEvent>,
}

struct QueuedExecutionEvent {
    event: ExecutionEvent,
    _retention: Option<RetainedEventBytes>,
}

struct RetainedEventByteLedger {
    used: Mutex<usize>,
    limit: PayloadLimit,
}

struct RetainedEventBytes {
    ledger: Arc<RetainedEventByteLedger>,
    bytes: usize,
}

impl Drop for RetainedEventBytes {
    fn drop(&mut self) {
        let mut used = self
            .ledger
            .used
            .lock()
            .unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_EXECUTION_EVENT_ACCOUNTING_POISONED: recovering retained-byte ledger during release"
                );
                poisoned.into_inner()
            });
        *used = used.saturating_sub(self.bytes);
    }
}

/// Pre-admitted retained-byte charge for one owned common event. Construction
/// is possible only through a named [`PayloadLimit`], or through the bound
/// submission handle's configured aggregate byte limit.
#[derive(Debug)]
pub struct ExecutionEventAdmission {
    retained_bytes: usize,
}

impl ExecutionEventAdmission {
    pub fn try_new(retained_bytes: usize, limit: &PayloadLimit) -> Result<Self, HostServiceError> {
        limit.admit(retained_bytes)?;
        Ok(Self { retained_bytes })
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

/// Cloneable, generation-bound producer for common backend events.
///
/// The handle retains no executor object, guest engine, Store, isolate, or
/// sidecar process borrow. A host-call reply is validated against the bound
/// process before admission, and a rejected submission settles that exact
/// reply lane with the typed rejection.
#[derive(Clone)]
pub struct ExecutionEventSubmitHandle {
    process: HostProcessContext,
    queue: Arc<BoundedExecutionEventQueue>,
}

impl fmt::Debug for ExecutionEventSubmitHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionEventSubmitHandle")
            .field("process", &self.process)
            .field("capacity", &self.queue.capacity)
            .field("retained_bytes_limit", &self.queue.retained_bytes.limit)
            .finish_non_exhaustive()
    }
}

impl ExecutionEventSubmitHandle {
    pub fn process(&self) -> HostProcessContext {
        self.process
    }

    pub fn admit(
        &self,
        retained_bytes: usize,
    ) -> Result<ExecutionEventAdmission, HostServiceError> {
        ExecutionEventAdmission::try_new(retained_bytes, &self.queue.retained_bytes.limit)
    }

    /// Measure an already-owned adapter request without allocating an encoded
    /// copy, then admit its additional raw buffers against the queue's byte
    /// bound. The resulting charge must be moved into `submit`.
    pub fn admit_json<T: Serialize + ?Sized>(
        &self,
        value: &T,
        additional_raw_bytes: usize,
    ) -> Result<ExecutionEventAdmission, HostServiceError> {
        let encoded = self.queue.retained_bytes.limit.admit_json(value)?;
        let retained_bytes = encoded.checked_add(additional_raw_bytes).ok_or_else(|| {
            HostServiceError::new("EOVERFLOW", "common event retained-byte charge overflowed")
        })?;
        self.admit(retained_bytes)
    }

    pub fn submit(
        &self,
        event: ExecutionEvent,
        admission: ExecutionEventAdmission,
    ) -> Result<(), HostServiceError> {
        let reply = match &event {
            ExecutionEvent::HostCall { reply, .. } => {
                let identity = reply.identity();
                if identity.generation != self.process.generation
                    || identity.pid != self.process.pid
                {
                    let error = HostServiceError::new(
                        "ESTALE",
                        "host-call reply identity does not match the bound execution generation",
                    )
                    .with_details(serde_json::json!({
                        "expectedGeneration": self.process.generation,
                        "expectedPid": self.process.pid,
                        "actualGeneration": identity.generation,
                        "actualPid": identity.pid,
                        "callId": identity.call_id,
                    }));
                    reply.fail(error.clone())?;
                    return Err(error);
                }
                Some(reply.clone())
            }
            _ => None,
        };

        let result = self.submit_admitted_identity(event, admission);
        if let Err(error) = &result {
            if let Some(reply) = reply {
                reply.fail(error.clone())?;
            }
        }
        result
    }

    pub fn retained_bytes(&self) -> Result<usize, HostServiceError> {
        self.queue
            .retained_bytes
            .used
            .lock()
            .map(|used| *used)
            .map_err(|_| {
                HostServiceError::new(
                    "EIO",
                    "common execution-event retained-byte ledger lock is poisoned",
                )
            })
    }

    fn submit_admitted_identity(
        &self,
        event: ExecutionEvent,
        admission: ExecutionEventAdmission,
    ) -> Result<(), HostServiceError> {
        if self.queue.closed.load(Ordering::Acquire) {
            return Err(HostServiceError::new(
                "EPIPE",
                "common execution-event receiver is closed",
            ));
        }
        let mut state = self.queue.state.lock().map_err(|_| {
            HostServiceError::new("EIO", "common execution-event queue lock is poisoned")
        })?;
        if self.queue.closed.load(Ordering::Acquire) {
            return Err(HostServiceError::new(
                "EPIPE",
                "common execution-event receiver is closed",
            ));
        }
        if state.events.len() >= self.queue.capacity {
            return Err(HostServiceError::limit(
                "EAGAIN",
                "limits.process.pendingEventCount/runtime.protocol.maxProcessEvents",
                u64::try_from(self.queue.capacity).unwrap_or(u64::MAX),
                u64::try_from(state.events.len().saturating_add(1)).unwrap_or(u64::MAX),
            ));
        }
        let retention = {
            let mut used = self.queue.retained_bytes.used.lock().map_err(|_| {
                HostServiceError::new(
                    "EIO",
                    "common execution-event retained-byte ledger lock is poisoned",
                )
            })?;
            let observed_bytes = used.checked_add(admission.retained_bytes).ok_or_else(|| {
                HostServiceError::new(
                    "EOVERFLOW",
                    "common execution-event retained-byte total overflowed",
                )
            })?;
            self.queue.retained_bytes.limit.admit(observed_bytes)?;
            *used = observed_bytes;
            RetainedEventBytes {
                ledger: Arc::clone(&self.queue.retained_bytes),
                bytes: admission.retained_bytes,
            }
        };
        let queued_retention = match &event {
            ExecutionEvent::HostCall { reply, .. } => {
                reply.retain_request(retention)?;
                None
            }
            _ => Some(retention),
        };
        state.events.push_back(QueuedExecutionEvent {
            event,
            _retention: queued_retention,
        });
        drop(state);
        self.queue.wake.wake();
        Ok(())
    }
}

/// Single-consumer side of a bounded common execution-event queue.
pub struct ExecutionEventReceiver {
    queue: Arc<BoundedExecutionEventQueue>,
}

impl fmt::Debug for ExecutionEventReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionEventReceiver")
            .field("capacity", &self.queue.capacity)
            .finish_non_exhaustive()
    }
}

impl ExecutionEventReceiver {
    pub fn try_recv(&self) -> Result<Option<ExecutionEvent>, HostServiceError> {
        self.queue
            .state
            .lock()
            .map_err(|_| {
                HostServiceError::new("EIO", "common execution-event queue lock is poisoned")
            })
            .map(|mut state| {
                let queued = state.events.pop_front()?;
                Some(queued.event)
            })
    }
}

impl Drop for ExecutionEventReceiver {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Release);
        match self.queue.state.lock() {
            Ok(mut state) => {
                state.events.clear();
            }
            Err(poisoned) => {
                eprintln!(
                    "ERR_AGENTOS_EXECUTION_EVENT_QUEUE_POISONED: recovering queue during receiver teardown"
                );
                let mut state = poisoned.into_inner();
                state.events.clear();
            }
        }
    }
}

pub fn bounded_execution_event_channel(
    process: HostProcessContext,
    capacity: usize,
    retained_bytes_limit: PayloadLimit,
    wake: Arc<dyn ExecutionEventWakeTarget>,
) -> Result<(ExecutionEventSubmitHandle, ExecutionEventReceiver), HostServiceError> {
    if capacity == 0 {
        return Err(HostServiceError::new(
            "EINVAL",
            "common execution-event queue capacity must be greater than zero",
        ));
    }
    let queue = Arc::new(BoundedExecutionEventQueue {
        state: Mutex::new(ExecutionEventQueueState {
            events: VecDeque::with_capacity(capacity),
        }),
        capacity,
        retained_bytes: Arc::new(RetainedEventByteLedger {
            used: Mutex::new(0),
            limit: retained_bytes_limit,
        }),
        closed: AtomicBool::new(false),
        wake,
    });
    Ok((
        ExecutionEventSubmitHandle {
            process,
            queue: Arc::clone(&queue),
        },
        ExecutionEventReceiver { queue },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        DirectHostReplyHandle, DirectHostReplyTarget, HostCallIdentity, HostCallReply,
    };
    use crate::host::{HostOperation, ProcessOperation};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Default)]
    struct RecordingReplyTarget {
        replies: Mutex<Vec<Result<HostCallReply, HostServiceError>>>,
    }

    struct RejectingReplyTarget;

    impl DirectHostReplyTarget for RejectingReplyTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _: u64,
            _: bool,
            _: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            Err(HostServiceError::new(
                "EIO",
                "reply target rejected settlement",
            ))
        }
    }

    impl DirectHostReplyTarget for RecordingReplyTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _: u64,
            _: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            self.replies.lock().expect("reply lock").push(result);
            Ok(())
        }
    }

    fn reply(
        process: HostProcessContext,
        call_id: u64,
        target: Arc<RecordingReplyTarget>,
    ) -> DirectHostReplyHandle {
        DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: process.generation,
                pid: process.pid,
                call_id,
            },
            target,
            1024,
        )
        .expect("reply")
    }

    #[test]
    fn bounded_queue_wakes_and_preserves_the_direct_reply_lane() {
        let process = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = Arc::clone(&wakes);
        let (submit, receiver) = bounded_execution_event_channel(
            process,
            1,
            PayloadLimit::new("limits.process.pendingEventBytes", 1024).expect("byte limit"),
            Arc::new(move || {
                wake_count.fetch_add(1, AtomicOrdering::Relaxed);
            }),
        )
        .expect("queue");
        let target = Arc::new(RecordingReplyTarget::default());
        submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 1, Arc::clone(&target)),
                },
                submit.admit(128).expect("request admission"),
            )
            .expect("submit");
        assert_eq!(wakes.load(AtomicOrdering::Relaxed), 1);

        let ExecutionEvent::HostCall { reply, .. } =
            receiver.try_recv().expect("receive").expect("event")
        else {
            panic!("expected host call")
        };
        reply
            .succeed(HostCallReply::Empty)
            .expect("settle direct lane");
        assert!(target.replies.lock().expect("replies")[0].is_ok());
    }

    #[test]
    fn full_and_stale_submissions_settle_the_exact_waiter() {
        let process = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let (submit, _receiver) = bounded_execution_event_channel(
            process,
            1,
            PayloadLimit::new("limits.process.pendingEventBytes", 128).expect("byte limit"),
            Arc::new(|| {}),
        )
        .expect("queue");
        let target = Arc::new(RecordingReplyTarget::default());
        submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 1, Arc::clone(&target)),
                },
                submit.admit(128).expect("request admission"),
            )
            .expect("first submit");
        let error = submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 2, Arc::clone(&target)),
                },
                submit.admit(1).expect("request admission"),
            )
            .expect_err("full queue");
        assert_eq!(error.code, "EAGAIN");

        let stale = HostProcessContext {
            generation: 8,
            pid: process.pid,
        };
        let error = submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(stale, 3, Arc::clone(&target)),
                },
                submit.admit(1).expect("request admission"),
            )
            .expect_err("stale reply");
        assert_eq!(error.code, "ESTALE");

        let replies = target.replies.lock().expect("replies");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].as_ref().unwrap_err().code, "EAGAIN");
        assert_eq!(replies[1].as_ref().unwrap_err().code, "ESTALE");
    }

    #[test]
    fn stale_submission_propagates_reply_settlement_failure() {
        let process = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let (submit, _receiver) = bounded_execution_event_channel(
            process,
            1,
            PayloadLimit::new("limits.process.pendingEventBytes", 128).expect("byte limit"),
            Arc::new(|| {}),
        )
        .expect("queue");
        let stale = HostProcessContext {
            generation: 8,
            pid: process.pid,
        };
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: stale.generation,
                pid: stale.pid,
                call_id: 1,
            },
            Arc::new(RejectingReplyTarget),
            1024,
        )
        .expect("reply");
        let error = submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply,
                },
                submit.admit(1).expect("request admission"),
            )
            .expect_err("reply settlement failure must propagate");
        assert_eq!(error.code, "EIO");
    }

    #[test]
    fn aggregate_retained_bytes_survive_dequeue_until_settle() {
        let process = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let (submit, receiver) = bounded_execution_event_channel(
            process,
            2,
            PayloadLimit::new("limits.process.pendingEventBytes", 8).expect("byte limit"),
            Arc::new(|| {}),
        )
        .expect("queue");
        let target = Arc::new(RecordingReplyTarget::default());
        submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 1, Arc::clone(&target)),
                },
                submit.admit(8).expect("exact admission"),
            )
            .expect("first submit");
        let error = submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 2, Arc::clone(&target)),
                },
                submit.admit(1).expect("individual admission"),
            )
            .expect_err("aggregate byte bound");
        assert_eq!(
            error.details.expect("limit details")["limitName"],
            "limits.process.pendingEventBytes"
        );
        let ExecutionEvent::HostCall {
            reply: pending_reply,
            ..
        } = receiver
            .try_recv()
            .expect("receive")
            .expect("queued request")
        else {
            panic!("expected host call")
        };
        assert_eq!(
            submit.retained_bytes().expect("retained bytes"),
            8,
            "dequeue must not release a pending host request"
        );
        pending_reply
            .succeed(HostCallReply::Empty)
            .expect("settle pending request");
        assert_eq!(submit.retained_bytes().expect("released bytes"), 0);
        submit
            .submit(
                ExecutionEvent::HostCall {
                    operation: HostOperation::Process(ProcessOperation::GetPid),
                    reply: reply(process, 3, target),
                },
                submit.admit(1).expect("re-admit released bytes"),
            )
            .expect("submit after release");
    }
}
