use super::{HostServiceError, PayloadLimit};
use serde_json::Value;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

const REPLY_OPEN: u8 = 0;
const REPLY_CLAIMED: u8 = 1;
const REPLY_SETTLED: u8 = 2;
const REPLY_TRANSITIONING: u8 = 3;
const REPLY_DELIVERY_FAILED: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostCallIdentity {
    pub generation: u64,
    pub pid: u32,
    pub call_id: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostCallReply {
    Empty,
    Json(Value),
    Raw(Vec<u8>),
}

type DirectHostReplyResult = Result<HostCallReply, HostServiceError>;

struct DirectHostReplyWaiterTarget {
    identity: HostCallIdentity,
    sender: Mutex<Option<tokio::sync::oneshot::Sender<DirectHostReplyResult>>>,
}

impl DirectHostReplyTarget for DirectHostReplyWaiterTarget {
    fn claim(&self, call_id: u64) -> Result<bool, HostServiceError> {
        self.validate_call_id(call_id)?;
        let sender = self.sender.lock().map_err(|_| {
            HostServiceError::new("EIO", "direct host-reply waiter lock is poisoned")
        })?;
        Ok(sender.as_ref().is_some_and(|sender| !sender.is_closed()))
    }

    fn respond(
        &self,
        call_id: u64,
        _claimed: bool,
        result: DirectHostReplyResult,
    ) -> Result<(), HostServiceError> {
        self.validate_call_id(call_id)?;
        let sender = self
            .sender
            .lock()
            .map_err(|_| HostServiceError::new("EIO", "direct host-reply waiter lock is poisoned"))?
            .take()
            .ok_or_else(|| {
                HostServiceError::new("EALREADY", "direct host-reply waiter is already settled")
            })?;
        sender
            .send(result)
            .map_err(|_| HostServiceError::new("EPIPE", "direct host-reply receiver was canceled"))
    }

    fn dismiss_claimed(&self, call_id: u64) -> Result<(), HostServiceError> {
        self.validate_call_id(call_id)?;
        let sender = self
            .sender
            .lock()
            .map_err(|_| HostServiceError::new("EIO", "direct host-reply waiter lock is poisoned"))?
            .take()
            .ok_or_else(|| {
                HostServiceError::new("EALREADY", "direct host-reply waiter is already settled")
            })?;
        sender
            .send(Err(HostServiceError::new(
                "ERR_AGENTOS_EXEC_REPLACED",
                "the kernel committed a replacement process image",
            )))
            .map_err(|_| HostServiceError::new("EPIPE", "direct host-reply receiver was canceled"))
    }
}

impl DirectHostReplyWaiterTarget {
    fn validate_call_id(&self, call_id: u64) -> Result<(), HostServiceError> {
        if call_id == self.identity.call_id {
            return Ok(());
        }
        Err(HostServiceError::new(
            "ESTALE",
            "direct host-reply call identity does not match its waiter",
        )
        .with_details(serde_json::json!({
            "generation": self.identity.generation,
            "pid": self.identity.pid,
            "expectedCallId": self.identity.call_id,
            "actualCallId": call_id,
        })))
    }
}

/// Capacity-one, call-specific completion Future for a native execution
/// adapter. It receives only its registered reply and never scans an event
/// stream.
pub struct DirectHostReplyReceiver {
    identity: HostCallIdentity,
    receiver: tokio::sync::oneshot::Receiver<DirectHostReplyResult>,
}

impl fmt::Debug for DirectHostReplyReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectHostReplyReceiver")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl Future for DirectHostReplyReceiver {
    type Output = DirectHostReplyResult;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(context).map(|result| {
            result.unwrap_or_else(|_| {
                Err(HostServiceError::new(
                    "ECANCELED",
                    "direct host-reply sender was dropped without settlement",
                )
                .with_details(serde_json::json!({
                    "generation": self.identity.generation,
                    "pid": self.identity.pid,
                    "callId": self.identity.call_id,
                })))
            })
        })
    }
}

pub fn direct_host_reply_channel(
    identity: HostCallIdentity,
    max_payload_bytes: usize,
) -> Result<(DirectHostReplyHandle, DirectHostReplyReceiver), HostServiceError> {
    direct_host_reply_channel_with_limit(
        identity,
        PayloadLimit::with_stderr_warning(
            "limits.reactor.maxBridgeResponseBytes",
            max_payload_bytes,
        )?,
    )
}

pub fn direct_host_reply_channel_with_limit(
    identity: HostCallIdentity,
    payload_limit: PayloadLimit,
) -> Result<(DirectHostReplyHandle, DirectHostReplyReceiver), HostServiceError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let target = Arc::new(DirectHostReplyWaiterTarget {
        identity,
        sender: Mutex::new(Some(sender)),
    });
    let reply = DirectHostReplyHandle::new_with_limit(identity, target, payload_limit)?;
    Ok((reply, DirectHostReplyReceiver { identity, receiver }))
}

/// Adapter-owned one-request response lane.
///
/// Implementations retain only the adapter's response channel and pending-call
/// token. They must not retain an isolate, Store, process-table borrow, or the
/// sidecar's owned execution enum.
pub trait DirectHostReplyTarget: Send + Sync {
    fn claim(&self, call_id: u64) -> Result<bool, HostServiceError>;

    fn respond(
        &self,
        call_id: u64,
        claimed: bool,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError>;

    /// Complete a claimed request without resuming the old guest image.
    /// This is only valid for a successful exec-style image replacement: the
    /// adapter must already have removed the pending waiter during `claim`.
    fn dismiss_claimed(&self, _call_id: u64) -> Result<(), HostServiceError> {
        Err(HostServiceError::new(
            "ENOTSUP",
            "adapter does not support dismissing a claimed host reply",
        ))
    }
}

struct DirectHostReplyState {
    identity: HostCallIdentity,
    target: Arc<dyn DirectHostReplyTarget>,
    state: AtomicU8,
    transition_lock: Mutex<()>,
    payload_limit: PayloadLimit,
    request_retention: Mutex<Option<Box<dyn Send + Sync>>>,
}

/// Cloneable, generation-bound direct reply capability for one host call.
/// Exactly one clone may claim or settle it.
#[derive(Clone)]
pub struct DirectHostReplyHandle {
    inner: Arc<DirectHostReplyState>,
}

impl fmt::Debug for DirectHostReplyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirectHostReplyHandle")
            .field("identity", &self.inner.identity)
            .field("payload_limit", &self.inner.payload_limit)
            .finish_non_exhaustive()
    }
}

impl DirectHostReplyHandle {
    pub fn new(
        identity: HostCallIdentity,
        target: Arc<dyn DirectHostReplyTarget>,
        max_payload_bytes: usize,
    ) -> Result<Self, HostServiceError> {
        let payload_limit = PayloadLimit::with_stderr_warning(
            "limits.reactor.maxBridgeResponseBytes",
            max_payload_bytes,
        )?;
        Self::new_with_limit(identity, target, payload_limit)
    }

    pub fn new_with_limit(
        identity: HostCallIdentity,
        target: Arc<dyn DirectHostReplyTarget>,
        payload_limit: PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        Ok(Self {
            inner: Arc::new(DirectHostReplyState {
                identity,
                target,
                state: AtomicU8::new(REPLY_OPEN),
                transition_lock: Mutex::new(()),
                payload_limit,
                request_retention: Mutex::new(None),
            }),
        })
    }

    pub fn identity(&self) -> HostCallIdentity {
        self.inner.identity
    }

    /// Retain opaque request accounting/ownership until this exact call is
    /// terminal. The retention is released on success, typed failure,
    /// dismissal, delivery failure, or final-handle drop.
    pub fn retain_request<T>(&self, retention: T) -> Result<(), HostServiceError>
    where
        T: Send + Sync + 'static,
    {
        let _transition =
            self.inner.transition_lock.lock().map_err(|_| {
                HostServiceError::new("EIO", "host reply transition lock is poisoned")
            })?;
        let state = self.inner.state.load(Ordering::Acquire);
        if state != REPLY_OPEN {
            return Err(already_settled(self.inner.identity));
        }
        let mut slot =
            self.inner.request_retention.lock().map_err(|_| {
                HostServiceError::new("EIO", "host reply retention lock is poisoned")
            })?;
        if slot.is_some() {
            return Err(HostServiceError::new(
                "EALREADY",
                format!(
                    "host call {} already owns request retention",
                    self.inner.identity.call_id
                ),
            ));
        }
        if self.inner.state.load(Ordering::Acquire) != REPLY_OPEN {
            return Err(already_settled(self.inner.identity));
        }
        *slot = Some(Box::new(retention));
        Ok(())
    }

    /// Claims the pending adapter request before a destructive host operation.
    /// A false result means the guest timed out or replaced the request, so no
    /// side effect may be performed.
    pub fn claim(&self) -> Result<bool, HostServiceError> {
        let transition =
            self.inner.transition_lock.lock().map_err(|_| {
                HostServiceError::new("EIO", "host reply transition lock is poisoned")
            })?;
        self.transition(REPLY_OPEN)?;
        drop(transition);
        match self.inner.target.claim(self.inner.identity.call_id) {
            Ok(true) => {
                self.inner.state.store(REPLY_CLAIMED, Ordering::Release);
                Ok(true)
            }
            Ok(false) => {
                self.inner.state.store(REPLY_SETTLED, Ordering::Release);
                self.release_request_retention();
                Ok(false)
            }
            Err(error) => {
                self.inner.state.store(REPLY_OPEN, Ordering::Release);
                Err(error)
            }
        }
    }

    pub fn succeed(&self, reply: HostCallReply) -> Result<(), HostServiceError> {
        self.settle(Ok(reply))
    }

    /// Settle a reply synchronously while retaining source-side accounting or
    /// storage ownership until the adapter has encoded/transferred it.
    ///
    /// `T` remains deliberately opaque to the common execution layer: queue
    /// reservations, reactor buffers, and engine-specific backing stores do
    /// not become part of [`HostCallReply`] or its public ABI.
    pub fn succeed_retained<T>(
        &self,
        reply: HostCallReply,
        retention: T,
    ) -> Result<(), HostServiceError> {
        let result = self.succeed(reply);
        drop(retention);
        result
    }

    /// Admits bytes against this reply lane before constructing its reply
    /// envelope. Common host-service implementations should prefer this over
    /// constructing `HostCallReply::Raw` directly.
    pub fn succeed_raw(&self, bytes: Vec<u8>) -> Result<(), HostServiceError> {
        match self.inner.payload_limit.admit(bytes.len()) {
            Ok(()) => self.settle_admitted(Ok(HostCallReply::Raw(bytes))),
            Err(error) => self.settle_admitted(Err(error)),
        }
    }

    /// Measures JSON with a bounded counting writer before constructing its
    /// reply envelope. No encoded temporary is allocated for admission.
    pub fn succeed_json(&self, value: Value) -> Result<(), HostServiceError> {
        match self.inner.payload_limit.admit_json(&value) {
            Ok(_) => self.settle_admitted(Ok(HostCallReply::Json(value))),
            Err(error) => self.settle_admitted(Err(error)),
        }
    }

    pub fn fail(&self, error: HostServiceError) -> Result<(), HostServiceError> {
        self.settle(Err(error))
    }

    /// Mark a successfully claimed exec request complete without sending a
    /// response into the replaced image. Ordinary operations must settle with
    /// `succeed` or `fail`; using this on an open or settled lane is an error.
    pub fn dismiss_claimed(&self) -> Result<(), HostServiceError> {
        let transition =
            self.inner.transition_lock.lock().map_err(|_| {
                HostServiceError::new("EIO", "host reply transition lock is poisoned")
            })?;
        if self.inner.state.load(Ordering::Acquire) != REPLY_CLAIMED {
            return Err(already_settled(self.inner.identity));
        }
        self.transition(REPLY_CLAIMED)?;
        drop(transition);
        let result = self
            .inner
            .target
            .dismiss_claimed(self.inner.identity.call_id);
        self.inner.state.store(
            if result.is_ok() {
                REPLY_SETTLED
            } else {
                REPLY_DELIVERY_FAILED
            },
            Ordering::Release,
        );
        self.release_request_retention();
        result
    }

    fn settle(
        &self,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        // A response that exceeds the configured lane bound is itself settled
        // as a typed limit error. Returning the validation error without
        // settling would leave the guest waiting until Drop converted it into
        // an unrelated ECANCELED response.
        let result = match self.validate_payload(&result) {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        self.settle_admitted(result)
    }

    fn settle_admitted(
        &self,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        let transition =
            self.inner.transition_lock.lock().map_err(|_| {
                HostServiceError::new("EIO", "host reply transition lock is poisoned")
            })?;
        let current = self.inner.state.load(Ordering::Acquire);
        if current != REPLY_OPEN && current != REPLY_CLAIMED {
            return Err(already_settled(self.inner.identity));
        }
        self.transition(current)?;
        drop(transition);
        let response = self.inner.target.respond(
            self.inner.identity.call_id,
            current == REPLY_CLAIMED,
            result,
        );
        self.inner.state.store(
            if response.is_ok() {
                REPLY_SETTLED
            } else {
                REPLY_DELIVERY_FAILED
            },
            Ordering::Release,
        );
        self.release_request_retention();
        response
    }

    /// Whether the adapter's one response lane failed after settlement was
    /// claimed. This is terminal: callers must fail or tear down the adapter
    /// waiter instead of replaying a potentially destructive host operation.
    pub fn delivery_failed(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == REPLY_DELIVERY_FAILED
    }

    fn release_request_retention(&self) {
        let retention = self
            .inner
            .request_retention
            .lock()
            .unwrap_or_else(|poisoned| {
                eprintln!(
                    "ERR_AGENTOS_DIRECT_HOST_REPLY_RETENTION_POISONED: recovering request retention for call {}",
                    self.inner.identity.call_id
                );
                poisoned.into_inner()
            })
            .take();
        drop(retention);
    }

    fn transition(&self, expected: u8) -> Result<(), HostServiceError> {
        self.inner
            .state
            .compare_exchange(
                expected,
                REPLY_TRANSITIONING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| already_settled(self.inner.identity))
    }

    fn validate_payload(
        &self,
        result: &Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        match result {
            Ok(HostCallReply::Empty) => self.inner.payload_limit.admit(0),
            Ok(HostCallReply::Raw(bytes)) => self.inner.payload_limit.admit(bytes.len()),
            Ok(HostCallReply::Json(value)) => {
                self.inner.payload_limit.admit_json(value).map(|_| ())
            }
            Err(error) => self.inner.payload_limit.admit_json(error).map(|_| ()),
        }
    }
}

impl Drop for DirectHostReplyState {
    fn drop(&mut self) {
        let state = self.state.swap(REPLY_SETTLED, Ordering::AcqRel);
        if state != REPLY_OPEN && state != REPLY_CLAIMED {
            return;
        }
        let error = HostServiceError::new(
            "ECANCELED",
            "host dropped a direct reply handle without settling it",
        )
        .with_details(serde_json::json!({
            "generation": self.identity.generation,
            "pid": self.identity.pid,
            "callId": self.identity.call_id,
        }));
        if let Err(reply_error) =
            self.target
                .respond(self.identity.call_id, state == REPLY_CLAIMED, Err(error))
        {
            eprintln!("ERR_AGENTOS_DIRECT_HOST_REPLY_DROP: {reply_error}");
        }
    }
}

fn already_settled(identity: HostCallIdentity) -> HostServiceError {
    HostServiceError::new(
        "EALREADY",
        format!("host call {} already claimed or settled", identity.call_id),
    )
    .with_details(serde_json::json!({
        "generation": identity.generation,
        "pid": identity.pid,
        "callId": identity.call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Mutex;

    type RecordedReply = (u64, bool, Result<HostCallReply, HostServiceError>);

    #[derive(Default)]
    struct RecordingTarget {
        replies: Mutex<Vec<RecordedReply>>,
        dismissed: Mutex<Vec<u64>>,
        fail_delivery: bool,
    }

    impl DirectHostReplyTarget for RecordingTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            call_id: u64,
            claimed: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            if self.fail_delivery {
                return Err(HostServiceError::new(
                    "EPIPE",
                    "adapter reply lane is closed",
                ));
            }
            self.replies
                .lock()
                .expect("reply lock")
                .push((call_id, claimed, result));
            Ok(())
        }

        fn dismiss_claimed(&self, call_id: u64) -> Result<(), HostServiceError> {
            self.dismissed.lock().expect("dismiss lock").push(call_id);
            Ok(())
        }
    }

    fn handle(target: Arc<RecordingTarget>) -> DirectHostReplyHandle {
        DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 7,
                pid: 42,
                call_id: 9,
            },
            target,
            1024,
        )
        .expect("reply handle")
    }

    #[test]
    fn only_one_clone_can_settle() {
        let target = Arc::new(RecordingTarget::default());
        let first = handle(target.clone());
        let second = first.clone();
        first.succeed(HostCallReply::Empty).expect("first reply");
        assert_eq!(
            second.succeed(HostCallReply::Empty).unwrap_err().code,
            "EALREADY"
        );
        assert_eq!(target.replies.lock().expect("reply lock").len(), 1);
    }

    #[test]
    fn claim_is_explicit_and_preserved_on_response() {
        let target = Arc::new(RecordingTarget::default());
        let reply = handle(target.clone());
        assert!(reply.claim().expect("claim"));
        reply.succeed(HostCallReply::Empty).expect("claimed reply");
        assert!(target.replies.lock().expect("reply lock")[0].1);
    }

    #[test]
    fn claimed_exec_lane_can_complete_without_resuming_the_old_image() {
        let target = Arc::new(RecordingTarget::default());
        let reply = handle(target.clone());
        assert!(reply.claim().expect("claim exec request"));
        reply.dismiss_claimed().expect("dismiss exec request");
        assert_eq!(*target.dismissed.lock().expect("dismiss lock"), vec![9]);
        assert!(target.replies.lock().expect("reply lock").is_empty());
    }

    #[test]
    fn request_retention_is_released_on_dismissal_and_final_drop() {
        struct Retention(Arc<AtomicBool>);
        impl Drop for Retention {
            fn drop(&mut self) {
                self.0.store(true, AtomicOrdering::Release);
            }
        }

        let dismissed = Arc::new(AtomicBool::new(false));
        let reply = handle(Arc::new(RecordingTarget::default()));
        reply
            .retain_request(Retention(Arc::clone(&dismissed)))
            .expect("retain dismissed request");
        assert!(reply.claim().expect("claim dismissed request"));
        reply.dismiss_claimed().expect("dismiss request");
        assert!(dismissed.load(AtomicOrdering::Acquire));

        let dropped = Arc::new(AtomicBool::new(false));
        let reply = handle(Arc::new(RecordingTarget::default()));
        reply
            .retain_request(Retention(Arc::clone(&dropped)))
            .expect("retain dropped request");
        drop(reply);
        assert!(dropped.load(AtomicOrdering::Acquire));
    }

    #[test]
    fn last_unsettled_clone_sends_typed_cancellation() {
        let target = Arc::new(RecordingTarget::default());
        drop(handle(target.clone()));
        let replies = target.replies.lock().expect("reply lock");
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].2.as_ref().unwrap_err().code, "ECANCELED");
    }

    #[test]
    fn oversized_reply_is_settled_as_a_typed_limit_error() {
        let target = Arc::new(RecordingTarget::default());
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 7,
                pid: 42,
                call_id: 9,
            },
            target.clone(),
            4,
        )
        .expect("reply handle");

        reply
            .succeed(HostCallReply::Raw(vec![0; 5]))
            .expect("limit reply");

        let replies = target.replies.lock().expect("reply lock");
        assert_eq!(replies.len(), 1);
        let error = replies[0].2.as_ref().unwrap_err();
        assert_eq!(error.code, "E2BIG");
        assert_eq!(
            error.details.as_ref().expect("limit details")["limitName"],
            "limits.reactor.maxBridgeResponseBytes"
        );
    }

    #[test]
    fn adapter_delivery_failure_is_an_explicit_terminal_state() {
        let target = Arc::new(RecordingTarget {
            fail_delivery: true,
            ..RecordingTarget::default()
        });
        let reply = handle(target);

        let error = reply
            .succeed(HostCallReply::Empty)
            .expect_err("closed adapter lane");
        assert_eq!(error.code, "EPIPE");
        assert!(reply.delivery_failed());
        assert_eq!(
            reply.succeed(HostCallReply::Empty).unwrap_err().code,
            "EALREADY"
        );
    }

    #[test]
    fn retained_source_lives_through_synchronous_adapter_response() {
        struct Retention(Arc<AtomicBool>);
        impl Drop for Retention {
            fn drop(&mut self) {
                self.0.store(true, AtomicOrdering::Release);
            }
        }
        struct OrderingTarget(Arc<AtomicBool>);
        impl DirectHostReplyTarget for OrderingTarget {
            fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
                Ok(true)
            }
            fn respond(
                &self,
                _: u64,
                _: bool,
                _: Result<HostCallReply, HostServiceError>,
            ) -> Result<(), HostServiceError> {
                assert!(
                    !self.0.load(AtomicOrdering::Acquire),
                    "retention dropped before adapter transfer"
                );
                Ok(())
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 1,
                pid: 2,
                call_id: 3,
            },
            Arc::new(OrderingTarget(dropped.clone())),
            1024,
        )
        .expect("reply");
        reply
            .succeed_retained(HostCallReply::Empty, Retention(dropped.clone()))
            .expect("settle");
        assert!(dropped.load(AtomicOrdering::Acquire));
    }

    fn poll_direct_receiver(
        receiver: DirectHostReplyReceiver,
    ) -> Result<HostCallReply, HostServiceError> {
        let mut receiver = Box::pin(receiver);
        let mut context = Context::from_waker(std::task::Waker::noop());
        match receiver.as_mut().poll(&mut context) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("direct reply should already be settled"),
        }
    }

    #[test]
    fn native_direct_waiter_receives_only_its_typed_result() {
        let identity = HostCallIdentity {
            generation: 9,
            pid: 17,
            call_id: 23,
        };
        let (reply, receiver) = direct_host_reply_channel(identity, 1024).expect("direct channel");
        reply
            .fail(HostServiceError::new("EACCES", "denied"))
            .expect("settle error");
        let error = poll_direct_receiver(receiver).expect_err("typed error");
        assert_eq!(error.code, "EACCES");
    }

    #[test]
    fn dismissed_native_exec_waiter_receives_replacement_outcome() {
        let identity = HostCallIdentity {
            generation: 9,
            pid: 17,
            call_id: 26,
        };
        let (reply, receiver) = direct_host_reply_channel(identity, 1024).expect("direct channel");
        assert!(reply.claim().expect("claim exec"));
        reply.dismiss_claimed().expect("dismiss exec");
        let error = poll_direct_receiver(receiver).expect_err("exec replacement");
        assert_eq!(error.code, "ERR_AGENTOS_EXEC_REPLACED");
    }

    #[test]
    fn canceled_native_waiter_prevents_a_claimed_side_effect() {
        let identity = HostCallIdentity {
            generation: 9,
            pid: 17,
            call_id: 24,
        };
        let (reply, receiver) = direct_host_reply_channel(identity, 1024).expect("direct channel");
        drop(receiver);
        assert!(!reply.claim().expect("canceled claim"));
        assert_eq!(
            reply.succeed(HostCallReply::Empty).unwrap_err().code,
            "EALREADY"
        );
    }

    #[test]
    fn dropping_native_reply_settles_waiter_as_canceled() {
        let identity = HostCallIdentity {
            generation: 9,
            pid: 17,
            call_id: 25,
        };
        let (reply, receiver) = direct_host_reply_channel(identity, 1024).expect("direct channel");
        drop(reply);
        let error = poll_direct_receiver(receiver).expect_err("drop cancellation");
        assert_eq!(error.code, "ECANCELED");
    }

    #[test]
    fn retention_install_racing_settlement_never_leaks() {
        use std::sync::Barrier;

        struct Retention(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Retention {
            fn drop(&mut self) {
                self.0.fetch_sub(1, AtomicOrdering::AcqRel);
            }
        }

        for call_id in 1..=128 {
            let target = Arc::new(RecordingTarget::default());
            let reply = DirectHostReplyHandle::new(
                HostCallIdentity {
                    generation: 3,
                    pid: 41,
                    call_id,
                },
                target,
                1024,
            )
            .expect("reply");
            let retained = Arc::new(std::sync::atomic::AtomicUsize::new(1));
            let barrier = Arc::new(Barrier::new(3));
            let retain_reply = reply.clone();
            let retain_barrier = Arc::clone(&barrier);
            let retain_count = Arc::clone(&retained);
            let retain = std::thread::spawn(move || {
                retain_barrier.wait();
                retain_reply.retain_request(Retention(retain_count))
            });
            let settle_reply = reply.clone();
            let settle_barrier = Arc::clone(&barrier);
            let settle = std::thread::spawn(move || {
                settle_barrier.wait();
                settle_reply.succeed(HostCallReply::Empty)
            });
            barrier.wait();
            let retain_result = retain.join().expect("retention thread");
            let settle_result = settle.join().expect("settlement thread");
            assert!(
                settle_result.is_ok(),
                "call {call_id} did not reach its terminal settlement"
            );
            assert!(
                retain_result.is_ok()
                    || retain_result
                        .as_ref()
                        .is_err_and(|error| error.code == "EALREADY"),
                "call {call_id} returned an unexpected retention result: {retain_result:?}"
            );
            assert_eq!(
                retained.load(AtomicOrdering::Acquire),
                0,
                "call {call_id} retained request bytes after terminal settlement"
            );
            drop(reply);
            assert_eq!(
                retained.load(AtomicOrdering::Acquire),
                0,
                "call {call_id} leaked request retention"
            );
        }
    }
}
