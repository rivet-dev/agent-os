//! Bounded ownership-aware routing for durable process events.
//!
//! Producers publish through [`ProcessEventIngress`]. One broker driver owns
//! the receiver and moves envelopes into target-specific durable queues. A
//! consumer keeps an [`ProcessEventWaiter`] lease while it owns a target; its
//! async wait retains no `NativeSidecar`, VM, or coordinator state.

use crate::request_operations::{OperationCancellation, OperationCancellationReason};
use crate::state::ProcessEventEnvelope;
use crate::wire::OwnershipScope;
use agentos_runtime::RuntimeConfig;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};

pub(crate) const PROCESS_EVENT_LIMIT_PATH: &str = "runtime.protocol.maxProcessEvents";
pub(crate) const PROCESS_EVENT_CONSUMER_LIMIT_PATH: &str = "runtime.protocol.maxInFlightRequests";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessEventBrokerLimits {
    pub(crate) max_events: usize,
    pub(crate) max_consumers: usize,
}

impl ProcessEventBrokerLimits {
    pub(crate) fn from_runtime_config(config: &RuntimeConfig) -> Self {
        Self {
            max_events: config.protocol.max_process_events,
            max_consumers: config.protocol.max_in_flight_requests,
        }
    }

    fn validate(self) -> Result<Self, ProcessEventBrokerError> {
        if self.max_events == 0 {
            return Err(ProcessEventBrokerError::InvalidLimit {
                configuration_path: PROCESS_EVENT_LIMIT_PATH,
            });
        }
        if self.max_consumers == 0 {
            return Err(ProcessEventBrokerError::InvalidLimit {
                configuration_path: PROCESS_EVENT_CONSUMER_LIMIT_PATH,
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProcessEventTarget {
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    pub(crate) process_id: String,
}

impl ProcessEventTarget {
    pub(crate) fn new(
        connection_id: impl Into<String>,
        session_id: impl Into<String>,
        vm_id: impl Into<String>,
        process_id: impl Into<String>,
    ) -> Result<Self, ProcessEventBrokerError> {
        let target = Self {
            connection_id: connection_id.into(),
            session_id: session_id.into(),
            vm_id: vm_id.into(),
            process_id: process_id.into(),
        };
        target.validate()?;
        Ok(target)
    }

    pub(crate) fn for_owned_process(
        ownership: &OwnershipScope,
        process_id: impl Into<String>,
    ) -> Result<Self, ProcessEventBrokerError> {
        let OwnershipScope::VmOwnership(scope) = ownership else {
            return Err(ProcessEventBrokerError::ProcessTargetRequiresVmOwnership);
        };
        Self::new(
            scope.connection_id.clone(),
            scope.session_id.clone(),
            scope.vm_id.clone(),
            process_id,
        )
    }

    fn from_envelope(envelope: &ProcessEventEnvelope) -> Result<Self, ProcessEventBrokerError> {
        Self::new(
            envelope.connection_id.clone(),
            envelope.session_id.clone(),
            envelope.vm_id.clone(),
            envelope.process_id.clone(),
        )
    }

    fn validate(&self) -> Result<(), ProcessEventBrokerError> {
        for (field, value) in [
            ("connection_id", self.connection_id.as_str()),
            ("session_id", self.session_id.as_str()),
            ("vm_id", self.vm_id.as_str()),
            ("process_id", self.process_id.as_str()),
        ] {
            if value.is_empty() {
                return Err(ProcessEventBrokerError::InvalidTarget { field });
            }
        }
        Ok(())
    }

    fn is_owned_by(&self, ownership: &OwnershipScope) -> bool {
        match ownership {
            OwnershipScope::ConnectionOwnership(scope) => scope.connection_id == self.connection_id,
            OwnershipScope::SessionOwnership(scope) => {
                scope.connection_id == self.connection_id && scope.session_id == self.session_id
            }
            OwnershipScope::VmOwnership(scope) => {
                scope.connection_id == self.connection_id
                    && scope.session_id == self.session_id
                    && scope.vm_id == self.vm_id
            }
        }
    }

    fn ownership_description(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.connection_id, self.session_id, self.vm_id, self.process_id
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProcessEventBrokerError {
    InvalidLimit {
        configuration_path: &'static str,
    },
    Limit {
        resource: &'static str,
        current: usize,
        limit: usize,
        configuration_path: &'static str,
    },
    InvalidTarget {
        field: &'static str,
    },
    OwnershipMismatch {
        target: String,
    },
    ProcessTargetRequiresVmOwnership,
    DuplicateConsumer {
        target: String,
    },
    StaleConsumer {
        target: String,
    },
    Cancelled {
        reason: OperationCancellationReason,
    },
    Closed {
        reason: OperationCancellationReason,
    },
    Poisoned,
}

impl ProcessEventBrokerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidLimit { .. } => "ERR_AGENTOS_PROCESS_EVENT_BROKER_CONFIG",
            Self::Limit { .. } => "ERR_AGENTOS_PROCESS_EVENT_BROKER_LIMIT",
            Self::InvalidTarget { .. } => "ERR_AGENTOS_PROCESS_EVENT_TARGET",
            Self::OwnershipMismatch { .. } => "ERR_AGENTOS_PROCESS_EVENT_OWNERSHIP",
            Self::ProcessTargetRequiresVmOwnership => {
                "ERR_AGENTOS_PROCESS_EVENT_VM_OWNERSHIP_REQUIRED"
            }
            Self::DuplicateConsumer { .. } => "ERR_AGENTOS_PROCESS_EVENT_CONSUMER_EXISTS",
            Self::StaleConsumer { .. } => "ERR_AGENTOS_PROCESS_EVENT_CONSUMER_STALE",
            Self::Cancelled { .. } => "ERR_AGENTOS_PROCESS_EVENT_CANCELLED",
            Self::Closed { .. } => "ERR_AGENTOS_PROCESS_EVENT_BROKER_CLOSED",
            Self::Poisoned => "ERR_AGENTOS_PROCESS_EVENT_BROKER_POISONED",
        }
    }
}

impl fmt::Display for ProcessEventBrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit { configuration_path } => write!(
                formatter,
                "{}: {configuration_path} must be greater than zero",
                self.code()
            ),
            Self::Limit {
                resource,
                current,
                limit,
                configuration_path,
            } => write!(
                formatter,
                "{}: {resource} used {current}, limit {limit}; raise {configuration_path}",
                self.code()
            ),
            Self::InvalidTarget { field } => {
                write!(formatter, "{}: target {field} is empty", self.code())
            }
            Self::OwnershipMismatch { target } => write!(
                formatter,
                "{}: ownership does not cover process target {target}",
                self.code()
            ),
            Self::ProcessTargetRequiresVmOwnership => write!(
                formatter,
                "{}: a process-targeted waiter requires VM ownership",
                self.code()
            ),
            Self::DuplicateConsumer { target } => write!(
                formatter,
                "{}: process target {target} already has a consumer",
                self.code()
            ),
            Self::StaleConsumer { target } => write!(
                formatter,
                "{}: process target {target} no longer belongs to this consumer",
                self.code()
            ),
            Self::Cancelled { reason } => {
                write!(formatter, "{}: waiter cancelled ({reason:?})", self.code())
            }
            Self::Closed { reason } => {
                write!(formatter, "{}: broker closed ({reason:?})", self.code())
            }
            Self::Poisoned => write!(formatter, "{}: broker state lock poisoned", self.code()),
        }
    }
}

impl std::error::Error for ProcessEventBrokerError {}

#[derive(Debug)]
pub(crate) struct ProcessEventPublishFailure {
    pub(crate) error: ProcessEventBrokerError,
    pub(crate) envelope: ProcessEventEnvelope,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessEventBrokerSnapshot {
    pub(crate) pending_events: usize,
    pub(crate) active_consumers: usize,
    pub(crate) retained_targets: usize,
    /// Includes ingress frames and durable pending events. Both share one
    /// semaphore, so this can never exceed the configured event limit.
    pub(crate) event_budget_in_use: usize,
    pub(crate) wake_signals: u64,
    pub(crate) delivered_events: u64,
    pub(crate) cancelled_consumers: u64,
    pub(crate) disposed_events: u64,
    pub(crate) closed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessEventBroker {
    inner: Arc<ProcessEventBrokerInner>,
}

#[derive(Debug)]
struct ProcessEventBrokerInner {
    limits: ProcessEventBrokerLimits,
    event_budget: Arc<Semaphore>,
    state: Mutex<ProcessEventBrokerState>,
    closed_notify: Notify,
}

#[derive(Debug, Default)]
struct ProcessEventBrokerState {
    pending: BTreeMap<ProcessEventTarget, VecDeque<RetainedProcessEvent>>,
    consumers: BTreeMap<ProcessEventTarget, ConsumerSlot>,
    /// Process targets reserved for targeted routing. Claims outlive an
    /// individual one-shot poll so the generic public event pump cannot steal
    /// an adapter response between sequential polls.
    claimed_targets: BTreeSet<ProcessEventTarget>,
    next_generation: u64,
    wake_signals: u64,
    delivered_events: u64,
    cancelled_consumers: u64,
    disposed_events: u64,
    closed: Option<OperationCancellationReason>,
}

#[derive(Debug)]
struct ConsumerSlot {
    generation: u64,
    notify: Arc<Notify>,
    cancellation: OperationCancellation,
}

#[derive(Debug)]
struct RetainedProcessEvent {
    envelope: ProcessEventEnvelope,
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessEventIngress {
    sender: mpsc::Sender<RetainedProcessEvent>,
    event_budget: Arc<Semaphore>,
    max_events: usize,
}

#[derive(Debug)]
pub(crate) struct ProcessEventBrokerDriver {
    broker: ProcessEventBroker,
    receiver: mpsc::Receiver<RetainedProcessEvent>,
}

#[derive(Debug)]
pub(crate) struct ProcessEventWaiter {
    broker: Weak<ProcessEventBrokerInner>,
    target: ProcessEventTarget,
    generation: u64,
    notify: Arc<Notify>,
    cancellation: OperationCancellation,
}

/// Checked-out event that remains charged to the broker until handling is
/// committed. Dropping an uncommitted lease requeues the exact envelope at the
/// front of its target queue, which makes cancellation between wake and
/// coordinator admission lossless.
#[derive(Debug)]
pub(crate) struct ProcessEventLease {
    broker: Weak<ProcessEventBrokerInner>,
    target: ProcessEventTarget,
    retained: Option<RetainedProcessEvent>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessEventWaiterCancellation {
    broker: Weak<ProcessEventBrokerInner>,
    target: ProcessEventTarget,
    generation: u64,
    cancellation: OperationCancellation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessEventDisposalReport {
    pub(crate) cancelled_consumers: usize,
    pub(crate) discarded_events: usize,
}

enum ConsumerProbe {
    Event(RetainedProcessEvent),
    Pending,
}

impl ProcessEventBroker {
    pub(crate) fn max_events(&self) -> usize {
        self.inner.limits.max_events
    }

    pub(crate) fn new(
        config: &RuntimeConfig,
    ) -> Result<(Self, ProcessEventIngress, ProcessEventBrokerDriver), ProcessEventBrokerError>
    {
        Self::with_limits(ProcessEventBrokerLimits::from_runtime_config(config))
    }

    pub(crate) fn with_limits(
        limits: ProcessEventBrokerLimits,
    ) -> Result<(Self, ProcessEventIngress, ProcessEventBrokerDriver), ProcessEventBrokerError>
    {
        let limits = limits.validate()?;
        let event_budget = Arc::new(Semaphore::new(limits.max_events));
        let (sender, receiver) = mpsc::channel(limits.max_events);
        let broker = Self {
            inner: Arc::new(ProcessEventBrokerInner {
                limits,
                event_budget: Arc::clone(&event_budget),
                state: Mutex::new(ProcessEventBrokerState::default()),
                closed_notify: Notify::new(),
            }),
        };
        let ingress = ProcessEventIngress {
            sender,
            event_budget,
            max_events: limits.max_events,
        };
        let driver = ProcessEventBrokerDriver {
            broker: broker.clone(),
            receiver,
        };
        Ok((broker, ingress, driver))
    }

    pub(crate) fn register_waiter(
        &self,
        ownership: &OwnershipScope,
        target: ProcessEventTarget,
        cancellation: OperationCancellation,
    ) -> Result<ProcessEventWaiter, ProcessEventBrokerError> {
        target.validate()?;
        if !target.is_owned_by(ownership) {
            return Err(ProcessEventBrokerError::OwnershipMismatch {
                target: target.ownership_description(),
            });
        }
        let mut state = self.lock_state()?;
        if let Some(reason) = state.closed {
            return Err(ProcessEventBrokerError::Closed { reason });
        }
        if state.consumers.contains_key(&target) {
            return Err(ProcessEventBrokerError::DuplicateConsumer {
                target: target.ownership_description(),
            });
        }
        if state.consumers.len() >= self.inner.limits.max_consumers {
            return Err(ProcessEventBrokerError::Limit {
                resource: "process event consumers",
                current: state.consumers.len(),
                limit: self.inner.limits.max_consumers,
                configuration_path: PROCESS_EVENT_CONSUMER_LIMIT_PATH,
            });
        }
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let generation = state.next_generation;
        let notify = Arc::new(Notify::new());
        state.consumers.insert(
            target.clone(),
            ConsumerSlot {
                generation,
                notify: Arc::clone(&notify),
                cancellation: cancellation.clone(),
            },
        );
        Ok(ProcessEventWaiter {
            broker: Arc::downgrade(&self.inner),
            target,
            generation,
            notify,
            cancellation,
        })
    }

    pub(crate) fn claim_target(
        &self,
        ownership: &OwnershipScope,
        target: ProcessEventTarget,
    ) -> Result<bool, ProcessEventBrokerError> {
        target.validate()?;
        if !target.is_owned_by(ownership) {
            return Err(ProcessEventBrokerError::OwnershipMismatch {
                target: target.ownership_description(),
            });
        }
        let mut state = self.lock_state()?;
        if let Some(reason) = state.closed {
            return Err(ProcessEventBrokerError::Closed { reason });
        }
        if state.claimed_targets.contains(&target) {
            return Ok(false);
        }
        if state.claimed_targets.len() >= self.inner.limits.max_events {
            return Err(ProcessEventBrokerError::Limit {
                resource: "claimed process event targets",
                current: state.claimed_targets.len(),
                limit: self.inner.limits.max_events,
                configuration_path: PROCESS_EVENT_LIMIT_PATH,
            });
        }
        state.claimed_targets.insert(target);
        Ok(true)
    }

    pub(crate) fn target_is_claimed(
        &self,
        target: &ProcessEventTarget,
    ) -> Result<bool, ProcessEventBrokerError> {
        Ok(self.lock_state()?.claimed_targets.contains(target))
    }

    pub(crate) fn dispose_connection(
        &self,
        connection_id: &str,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError> {
        self.dispose_matching(|target| target.connection_id == connection_id, reason)
    }

    pub(crate) fn dispose_session(
        &self,
        connection_id: &str,
        session_id: &str,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError> {
        self.dispose_matching(
            |target| target.connection_id == connection_id && target.session_id == session_id,
            reason,
        )
    }

    pub(crate) fn dispose_vm(
        &self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError> {
        self.dispose_matching(
            |target| {
                target.connection_id == connection_id
                    && target.session_id == session_id
                    && target.vm_id == vm_id
            },
            reason,
        )
    }

    pub(crate) fn dispose_process(
        &self,
        target: &ProcessEventTarget,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError> {
        self.dispose_matching(|candidate| candidate == target, reason)
    }

    pub(crate) fn shutdown(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError> {
        let report = {
            let mut state = self.lock_state()?;
            state.closed.get_or_insert(reason);
            // Close admission before releasing retained permits. Otherwise a
            // producer already waiting for capacity can acquire the first
            // disposal-released permit and race one last enqueue past shutdown.
            self.inner.event_budget.close();
            Self::dispose_matching_state(&mut state, |_| true, reason)
        };
        // Wake the receiver owner as well as producers parked on the shared
        // ingress/pending budget.
        self.inner.closed_notify.notify_waiters();
        Ok(report)
    }

    pub(crate) fn snapshot(&self) -> Result<ProcessEventBrokerSnapshot, ProcessEventBrokerError> {
        let state = self.lock_state()?;
        let retained_targets = state
            .pending
            .keys()
            .chain(state.consumers.keys())
            .chain(state.claimed_targets.iter())
            .collect::<BTreeSet<_>>()
            .len();
        Ok(ProcessEventBrokerSnapshot {
            pending_events: state.pending.values().map(VecDeque::len).sum(),
            active_consumers: state.consumers.len(),
            retained_targets,
            event_budget_in_use: self
                .inner
                .limits
                .max_events
                .saturating_sub(self.inner.event_budget.available_permits()),
            wake_signals: state.wake_signals,
            delivered_events: state.delivered_events,
            cancelled_consumers: state.cancelled_consumers,
            disposed_events: state.disposed_events,
            closed: state.closed.is_some(),
        })
    }

    fn route(&self, retained: RetainedProcessEvent) -> Result<(), ProcessEventBrokerError> {
        let target = ProcessEventTarget::from_envelope(&retained.envelope)?;
        let mut state = self.lock_state()?;
        if let Some(reason) = state.closed {
            return Err(ProcessEventBrokerError::Closed { reason });
        }
        let was_empty = state.pending.get(&target).is_none_or(VecDeque::is_empty);
        state
            .pending
            .entry(target.clone())
            .or_default()
            .push_back(retained);
        if was_empty {
            let notify = state
                .consumers
                .get(&target)
                .map(|consumer| Arc::clone(&consumer.notify));
            if let Some(notify) = notify {
                state.wake_signals = state.wake_signals.saturating_add(1);
                notify.notify_one();
            }
        }
        Ok(())
    }

    fn probe_consumer(
        &self,
        target: &ProcessEventTarget,
        generation: u64,
    ) -> Result<ConsumerProbe, ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        if let Some(reason) = state.closed {
            return Err(ProcessEventBrokerError::Closed { reason });
        }
        let current = state.consumers.get(target);
        if current.map(|slot| slot.generation) != Some(generation) {
            return Err(ProcessEventBrokerError::StaleConsumer {
                target: target.ownership_description(),
            });
        }
        if let Some(reason) = current.and_then(|slot| slot.cancellation.reason()) {
            return Err(ProcessEventBrokerError::Cancelled { reason });
        }
        let retained = state.pending.get_mut(target).and_then(VecDeque::pop_front);
        if state.pending.get(target).is_some_and(VecDeque::is_empty) {
            state.pending.remove(target);
        }
        if let Some(retained) = retained {
            Ok(ConsumerProbe::Event(retained))
        } else {
            Ok(ConsumerProbe::Pending)
        }
    }

    fn cancel_consumer(
        &self,
        target: &ProcessEventTarget,
        generation: u64,
        reason: OperationCancellationReason,
    ) -> Result<bool, ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        let matching = state
            .consumers
            .get(target)
            .is_some_and(|slot| slot.generation == generation);
        if !matching {
            return Ok(false);
        }
        let Some(slot) = state.consumers.remove(target) else {
            return Ok(false);
        };
        slot.cancellation.signal(reason);
        slot.notify.notify_waiters();
        state.cancelled_consumers = state.cancelled_consumers.saturating_add(1);
        Ok(true)
    }

    fn unregister_consumer(
        &self,
        target: &ProcessEventTarget,
        generation: u64,
    ) -> Result<bool, ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        if state
            .consumers
            .get(target)
            .is_some_and(|slot| slot.generation == generation)
        {
            state.consumers.remove(target);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn dispose_matching<F>(
        &self,
        matches: F,
        reason: OperationCancellationReason,
    ) -> Result<ProcessEventDisposalReport, ProcessEventBrokerError>
    where
        F: Fn(&ProcessEventTarget) -> bool,
    {
        let mut state = self.lock_state()?;
        Ok(Self::dispose_matching_state(&mut state, matches, reason))
    }

    fn dispose_matching_state<F>(
        state: &mut ProcessEventBrokerState,
        matches: F,
        reason: OperationCancellationReason,
    ) -> ProcessEventDisposalReport
    where
        F: Fn(&ProcessEventTarget) -> bool,
    {
        let consumer_targets = state
            .consumers
            .keys()
            .filter(|target| matches(target))
            .cloned()
            .collect::<Vec<_>>();
        for target in &consumer_targets {
            if let Some(slot) = state.consumers.remove(target) {
                slot.cancellation.signal(reason);
                slot.notify.notify_waiters();
            }
        }
        let event_targets = state
            .pending
            .keys()
            .filter(|target| matches(target))
            .cloned()
            .collect::<Vec<_>>();
        let discarded_events = event_targets
            .iter()
            .filter_map(|target| state.pending.remove(target))
            .map(|events| events.len())
            .sum::<usize>();
        state.claimed_targets.retain(|target| !matches(target));
        state.cancelled_consumers = state
            .cancelled_consumers
            .saturating_add(consumer_targets.len() as u64);
        state.disposed_events = state
            .disposed_events
            .saturating_add(discarded_events as u64);
        ProcessEventDisposalReport {
            cancelled_consumers: consumer_targets.len(),
            discarded_events,
        }
    }

    fn closed_reason(
        &self,
    ) -> Result<Option<OperationCancellationReason>, ProcessEventBrokerError> {
        Ok(self.lock_state()?.closed)
    }

    fn record_discarded_ingress(&self, count: usize) -> Result<(), ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        state.disposed_events = state.disposed_events.saturating_add(count as u64);
        Ok(())
    }

    fn record_delivered(&self) -> Result<(), ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        state.delivered_events = state.delivered_events.saturating_add(1);
        Ok(())
    }

    fn requeue_lease(
        &self,
        target: ProcessEventTarget,
        retained: RetainedProcessEvent,
    ) -> Result<(), ProcessEventBrokerError> {
        let mut state = self.lock_state()?;
        if let Some(reason) = state.closed {
            return Err(ProcessEventBrokerError::Closed { reason });
        }
        let was_empty = state.pending.get(&target).is_none_or(VecDeque::is_empty);
        state
            .pending
            .entry(target.clone())
            .or_default()
            .push_front(retained);
        if was_empty {
            let notify = state
                .consumers
                .get(&target)
                .map(|consumer| Arc::clone(&consumer.notify));
            if let Some(notify) = notify {
                state.wake_signals = state.wake_signals.saturating_add(1);
                notify.notify_one();
            }
        }
        Ok(())
    }

    fn lock_state(
        &self,
    ) -> Result<MutexGuard<'_, ProcessEventBrokerState>, ProcessEventBrokerError> {
        self.inner
            .state
            .lock()
            .map_err(|_| ProcessEventBrokerError::Poisoned)
    }
}

impl ProcessEventIngress {
    pub(crate) async fn publish(
        &self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), ProcessEventPublishFailure> {
        if let Err(error) = ProcessEventTarget::from_envelope(&envelope) {
            return Err(ProcessEventPublishFailure { error, envelope });
        }
        let permit = match Arc::clone(&self.event_budget).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                return Err(ProcessEventPublishFailure {
                    error: ProcessEventBrokerError::Closed {
                        reason: OperationCancellationReason::Shutdown,
                    },
                    envelope,
                });
            }
        };
        match self
            .sender
            .send(RetainedProcessEvent {
                envelope,
                _permit: permit,
            })
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(ProcessEventPublishFailure {
                error: ProcessEventBrokerError::Closed {
                    reason: OperationCancellationReason::Shutdown,
                },
                envelope: error.0.envelope,
            }),
        }
    }

    pub(crate) fn try_publish(
        &self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), ProcessEventPublishFailure> {
        if let Err(error) = ProcessEventTarget::from_envelope(&envelope) {
            return Err(ProcessEventPublishFailure { error, envelope });
        }
        let permit = match Arc::clone(&self.event_budget).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Err(ProcessEventPublishFailure {
                    error: ProcessEventBrokerError::Limit {
                        resource: "process events",
                        current: self.max_events,
                        limit: self.max_events,
                        configuration_path: PROCESS_EVENT_LIMIT_PATH,
                    },
                    envelope,
                });
            }
        };
        match self.sender.try_send(RetainedProcessEvent {
            envelope,
            _permit: permit,
        }) {
            Ok(()) => Ok(()),
            Err(error) => {
                let retained = error.into_inner();
                let broker_error = if self.sender.is_closed() {
                    ProcessEventBrokerError::Closed {
                        reason: OperationCancellationReason::Shutdown,
                    }
                } else {
                    ProcessEventBrokerError::Limit {
                        resource: "process event ingress",
                        current: self.max_events,
                        limit: self.max_events,
                        configuration_path: PROCESS_EVENT_LIMIT_PATH,
                    }
                };
                Err(ProcessEventPublishFailure {
                    error: broker_error,
                    envelope: retained.envelope,
                })
            }
        }
    }
}

impl ProcessEventBrokerDriver {
    pub(crate) async fn run(mut self) -> Result<(), ProcessEventBrokerError> {
        loop {
            // Register the shutdown edge before checking state so closure
            // cannot land between the probe and the select.
            let inner = Arc::clone(&self.broker.inner);
            let closed = inner.closed_notify.notified();
            if self.broker.closed_reason()?.is_some() {
                return self.drain_after_close().await;
            }
            tokio::select! {
                biased;
                _ = closed => return self.drain_after_close().await,
                retained = self.receiver.recv() => {
                    let Some(retained) = retained else {
                        return Ok(());
                    };
                    match self.broker.route(retained) {
                        Ok(()) => {}
                        Err(ProcessEventBrokerError::Closed { .. }) => {
                            self.broker.record_discarded_ingress(1)?;
                            return self.drain_after_close().await;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }

    async fn drain_after_close(&mut self) -> Result<(), ProcessEventBrokerError> {
        self.receiver.close();
        let mut discarded = 0usize;
        while let Some(_retained) = self.receiver.recv().await {
            discarded = discarded.saturating_add(1);
        }
        if discarded > 0 {
            self.broker.record_discarded_ingress(discarded)?;
        }
        Ok(())
    }
}

impl ProcessEventWaiter {
    pub(crate) fn cancellation_handle(&self) -> ProcessEventWaiterCancellation {
        ProcessEventWaiterCancellation {
            broker: self.broker.clone(),
            target: self.target.clone(),
            generation: self.generation,
            cancellation: self.cancellation.clone(),
        }
    }

    pub(crate) async fn next(&self) -> Result<ProcessEventEnvelope, ProcessEventBrokerError> {
        self.next_lease().await?.commit()
    }

    pub(crate) async fn next_lease(&self) -> Result<ProcessEventLease, ProcessEventBrokerError> {
        loop {
            // Register both async edges before probing durable state so neither
            // an event nor cancellation can be lost between probe and await.
            let notified = self.notify.notified();
            let cancelled = self.cancellation.cancelled();
            if let Some(reason) = self.cancellation.reason() {
                return Err(ProcessEventBrokerError::Cancelled { reason });
            }
            let Some(inner) = self.broker.upgrade() else {
                return Err(ProcessEventBrokerError::Closed {
                    reason: OperationCancellationReason::Shutdown,
                });
            };
            let broker = ProcessEventBroker { inner };
            match broker.probe_consumer(&self.target, self.generation) {
                Ok(ConsumerProbe::Event(retained)) => {
                    return Ok(ProcessEventLease {
                        broker: self.broker.clone(),
                        target: self.target.clone(),
                        retained: Some(retained),
                    });
                }
                Ok(ConsumerProbe::Pending) => {}
                Err(ProcessEventBrokerError::StaleConsumer { .. }) => {
                    if let Some(reason) = self.cancellation.reason() {
                        return Err(ProcessEventBrokerError::Cancelled { reason });
                    }
                    return Err(ProcessEventBrokerError::StaleConsumer {
                        target: self.target.ownership_description(),
                    });
                }
                Err(error) => return Err(error),
            }
            tokio::select! {
                _ = notified => {}
                reason = cancelled => {
                    return Err(ProcessEventBrokerError::Cancelled { reason });
                }
            }
        }
    }
}

impl ProcessEventLease {
    pub(crate) fn commit(mut self) -> Result<ProcessEventEnvelope, ProcessEventBrokerError> {
        let Some(inner) = self.broker.upgrade() else {
            return Err(ProcessEventBrokerError::Closed {
                reason: OperationCancellationReason::Shutdown,
            });
        };
        ProcessEventBroker { inner }.record_delivered()?;
        Ok(self
            .retained
            .take()
            .expect("uncommitted process event lease")
            .envelope)
    }
}

impl Drop for ProcessEventLease {
    fn drop(&mut self) {
        let Some(retained) = self.retained.take() else {
            return;
        };
        let Some(inner) = self.broker.upgrade() else {
            return;
        };
        if let Err(error) =
            (ProcessEventBroker { inner }).requeue_lease(self.target.clone(), retained)
        {
            if !matches!(error, ProcessEventBrokerError::Closed { .. }) {
                eprintln!("ERR_AGENTOS_PROCESS_EVENT_LEASE_REQUEUE: {error}");
            }
        }
    }
}

impl Drop for ProcessEventWaiter {
    fn drop(&mut self) {
        let Some(inner) = self.broker.upgrade() else {
            return;
        };
        if let Err(error) =
            (ProcessEventBroker { inner }).unregister_consumer(&self.target, self.generation)
        {
            eprintln!("ERR_AGENTOS_PROCESS_EVENT_WAITER_DROP: {error}");
        }
    }
}

impl ProcessEventWaiterCancellation {
    pub(crate) fn cancel(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<bool, ProcessEventBrokerError> {
        let Some(inner) = self.broker.upgrade() else {
            self.cancellation.signal(reason);
            return Ok(false);
        };
        (ProcessEventBroker { inner }).cancel_consumer(&self.target, self.generation, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActiveExecutionEvent;
    use std::future::Future as _;
    use std::task::{Context, Poll};
    use std::time::Duration;

    fn limits(max_events: usize, max_consumers: usize) -> ProcessEventBrokerLimits {
        ProcessEventBrokerLimits {
            max_events,
            max_consumers,
        }
    }

    fn target(vm: &str, process: &str) -> ProcessEventTarget {
        ProcessEventTarget::new("connection-a", "session-a", vm, process).expect("target")
    }

    fn event(target: &ProcessEventTarget, value: u8) -> ProcessEventEnvelope {
        ProcessEventEnvelope {
            connection_id: target.connection_id.clone(),
            session_id: target.session_id.clone(),
            vm_id: target.vm_id.clone(),
            process_id: target.process_id.clone(),
            child_path: Vec::new(),
            event: ActiveExecutionEvent::Stdout(vec![value]),
        }
    }

    fn stdout(envelope: ProcessEventEnvelope) -> Vec<u8> {
        match envelope.event {
            ActiveExecutionEvent::Stdout(bytes) => bytes,
            other => panic!("expected stdout event, received {other:?}"),
        }
    }

    async fn wait_for_pending(broker: &ProcessEventBroker, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if broker.snapshot().expect("snapshot").pending_events == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("broker routed event");
    }

    #[test]
    fn runtime_defaults_define_broker_bounds() {
        let config = RuntimeConfig::default();
        let (broker, _ingress, _driver) = ProcessEventBroker::new(&config).expect("default broker");
        assert_eq!(
            broker.inner.limits.max_events,
            config.protocol.max_process_events
        );
        assert_eq!(
            broker.inner.limits.max_consumers,
            config.protocol.max_in_flight_requests
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn long_waiter_on_vm_a_does_not_block_vm_b() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(4, 4)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let target_a = target("vm-a", "process-a");
        let target_b = target("vm-b", "process-b");
        let waiter_a = broker
            .register_waiter(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                target_a,
                OperationCancellation::new(),
            )
            .expect("VM A waiter");
        let waiter_b = broker
            .register_waiter(
                &OwnershipScope::vm("connection-a", "session-a", "vm-b"),
                target_b.clone(),
                OperationCancellation::new(),
            )
            .expect("VM B waiter");
        let mut blocked_a = Box::pin(waiter_a.next());
        let mut context = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            blocked_a.as_mut().poll(&mut context),
            Poll::Pending
        ));

        ingress
            .publish(event(&target_b, 7))
            .await
            .expect("publish VM B event");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiter_b.next())
                .await
                .expect("VM B waiter was independent")
                .map(stdout)
                .expect("VM B event"),
            vec![7]
        );
        assert!(matches!(
            blocked_a.as_mut().poll(&mut context),
            Poll::Pending
        ));
        drop(blocked_a);
        drop(waiter_b);
        drop(waiter_a);
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiter_wakes_only_for_matching_ownership_and_process() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(4, 4)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let matching = target("vm-a", "process-a");
        let other_process = target("vm-a", "process-b");
        let other_vm = target("vm-b", "process-a");
        let waiter = broker
            .register_waiter(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                matching.clone(),
                OperationCancellation::new(),
            )
            .expect("matching waiter");
        let ownership_error = broker
            .register_waiter(
                &OwnershipScope::vm("connection-a", "session-a", "vm-b"),
                matching.clone(),
                OperationCancellation::new(),
            )
            .expect_err("VM B cannot consume VM A process");
        assert_eq!(
            ownership_error.code(),
            "ERR_AGENTOS_PROCESS_EVENT_OWNERSHIP"
        );

        ingress
            .publish(event(&other_process, 1))
            .await
            .expect("publish other process");
        ingress
            .publish(event(&other_vm, 2))
            .await
            .expect("publish other VM");
        wait_for_pending(&broker, 2).await;
        let mut waiting = Box::pin(waiter.next());
        let mut context = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));

        ingress
            .publish(event(&matching, 3))
            .await
            .expect("publish match");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .expect("matching event wakes waiter")
                .map(stdout)
                .expect("matching event"),
            vec![3]
        );
        assert_eq!(broker.snapshot().expect("snapshot").pending_events, 2);
        drop(waiter);
        broker
            .shutdown(OperationCancellationReason::Shutdown)
            .expect("shutdown");
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_queue_coalesces_repeated_target_wakes() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(3, 1)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let target = target("vm-a", "process-a");
        let waiter = broker
            .register_waiter(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                target.clone(),
                OperationCancellation::new(),
            )
            .expect("waiter");
        ingress
            .publish(event(&target, 1))
            .await
            .expect("first event");
        ingress
            .publish(event(&target, 2))
            .await
            .expect("second event");
        wait_for_pending(&broker, 2).await;
        assert_eq!(broker.snapshot().expect("snapshot").wake_signals, 1);
        assert_eq!(
            stdout(waiter.next().await.expect("first routed event")),
            vec![1]
        );
        assert_eq!(
            stdout(waiter.next().await.expect("second routed event")),
            vec![2]
        );
        drop(waiter);
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_checked_out_lease_requeues_before_sequential_response() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(2, 1)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let target = target("vm-a", "process-a");
        let ownership = OwnershipScope::vm("connection-a", "session-a", "vm-a");
        assert!(broker
            .claim_target(&ownership, target.clone())
            .expect("claim target"));
        let waiter = broker
            .register_waiter(&ownership, target.clone(), OperationCancellation::new())
            .expect("waiter");
        ingress
            .publish(event(&target, 1))
            .await
            .expect("first response");
        let lease = waiter.next_lease().await.expect("checked-out response");
        assert_eq!(broker.snapshot().expect("snapshot").event_budget_in_use, 1);
        drop(lease);
        assert_eq!(
            stdout(waiter.next().await.expect("requeued first response")),
            vec![1]
        );
        drop(waiter);

        assert!(!broker
            .claim_target(&ownership, target.clone())
            .expect("claim persists between polls"));
        assert!(broker.target_is_claimed(&target).expect("claim snapshot"));
        let sequential = broker
            .register_waiter(&ownership, target.clone(), OperationCancellation::new())
            .expect("sequential waiter");
        ingress
            .publish(event(&target, 2))
            .await
            .expect("sequential response");
        assert_eq!(
            stdout(sequential.next().await.expect("sequential response routed")),
            vec![2]
        );
        broker
            .dispose_process(&target, OperationCancellationReason::Explicit)
            .expect("dispose claimed process");
        assert!(!broker.target_is_claimed(&target).expect("claim released"));
        drop(sequential);
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }

    #[test]
    fn duplicate_consumer_is_rejected_and_drop_releases_slot() {
        let (broker, _ingress, _driver) =
            ProcessEventBroker::with_limits(limits(2, 1)).expect("broker");
        let target = target("vm-a", "process-a");
        let ownership = OwnershipScope::vm("connection-a", "session-a", "vm-a");
        let first = broker
            .register_waiter(&ownership, target.clone(), OperationCancellation::new())
            .expect("first waiter");
        let duplicate = broker
            .register_waiter(&ownership, target.clone(), OperationCancellation::new())
            .expect_err("duplicate waiter rejected");
        assert_eq!(
            duplicate.code(),
            "ERR_AGENTOS_PROCESS_EVENT_CONSUMER_EXISTS"
        );
        assert_eq!(broker.snapshot().expect("snapshot").active_consumers, 1);
        drop(first);
        assert_eq!(broker.snapshot().expect("snapshot").active_consumers, 0);
        broker
            .register_waiter(&ownership, target, OperationCancellation::new())
            .expect("slot reused after drop");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_and_session_disposal_release_waiter_and_event_budget() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(2, 2)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let process_a = target("vm-a", "process-a");
        let process_b = target("vm-a", "process-b");
        let ownership = OwnershipScope::vm("connection-a", "session-a", "vm-a");
        let waiter_a = broker
            .register_waiter(&ownership, process_a.clone(), OperationCancellation::new())
            .expect("waiter A");
        let cancel_a = waiter_a.cancellation_handle();
        let waiting_a = tokio::spawn(async move { waiter_a.next().await });
        assert!(cancel_a
            .cancel(OperationCancellationReason::Explicit)
            .expect("cancel waiter"));
        let cancelled = waiting_a
            .await
            .expect("join waiter")
            .expect_err("cancelled");
        assert_eq!(
            cancelled,
            ProcessEventBrokerError::Cancelled {
                reason: OperationCancellationReason::Explicit
            }
        );
        assert_eq!(broker.snapshot().expect("snapshot").active_consumers, 0);

        let waiter_b = broker
            .register_waiter(&ownership, process_b.clone(), OperationCancellation::new())
            .expect("waiter B");
        ingress
            .publish(event(&process_b, 9))
            .await
            .expect("publish retained event");
        wait_for_pending(&broker, 1).await;
        let report = broker
            .dispose_session(
                "connection-a",
                "session-a",
                OperationCancellationReason::ConnectionClosed,
            )
            .expect("dispose session");
        assert_eq!(
            report,
            ProcessEventDisposalReport {
                cancelled_consumers: 1,
                discarded_events: 1
            }
        );
        let disposed = waiter_b.next().await.expect_err("disposed");
        assert_eq!(
            disposed,
            ProcessEventBrokerError::Cancelled {
                reason: OperationCancellationReason::ConnectionClosed
            }
        );
        let snapshot = broker.snapshot().expect("snapshot");
        assert_eq!(snapshot.active_consumers, 0);
        assert_eq!(snapshot.pending_events, 0);
        assert_eq!(snapshot.event_budget_in_use, 0);
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_budget_bounds_ingress_and_pending_and_shutdown_wakes_producer() {
        let (broker, ingress, driver) =
            ProcessEventBroker::with_limits(limits(1, 1)).expect("broker");
        let driver_task = tokio::spawn(driver.run());
        let target = target("vm-a", "process-a");
        ingress
            .publish(event(&target, 1))
            .await
            .expect("fill event budget");
        wait_for_pending(&broker, 1).await;
        let rejected = ingress
            .try_publish(event(&target, 2))
            .expect_err("combined ingress and pending budget is full");
        assert_eq!(
            rejected.error.code(),
            "ERR_AGENTOS_PROCESS_EVENT_BROKER_LIMIT"
        );
        assert_eq!(stdout(rejected.envelope), vec![2]);

        let blocked_ingress = ingress.clone();
        let blocked_target = target.clone();
        let blocked =
            tokio::spawn(async move { blocked_ingress.publish(event(&blocked_target, 3)).await });
        tokio::task::yield_now().await;
        broker
            .shutdown(OperationCancellationReason::Shutdown)
            .expect("shutdown broker");
        let rejected = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("shutdown wakes producer")
            .expect("join producer")
            .expect_err("producer receives typed closure");
        assert_eq!(
            rejected.error,
            ProcessEventBrokerError::Closed {
                reason: OperationCancellationReason::Shutdown
            }
        );
        assert_eq!(stdout(rejected.envelope), vec![3]);
        assert_eq!(broker.snapshot().expect("snapshot").event_budget_in_use, 0);
        drop(ingress);
        driver_task.await.expect("join driver").expect("driver");
    }
}
