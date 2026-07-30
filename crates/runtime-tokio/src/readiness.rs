//! Durable, revisioned per-VM readiness with a capacity-one wake lane.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign, Sub};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::accounting::{ResourceClass, ResourceLedger};
use crate::metrics::{ChannelMetricClass, RuntimeMetrics, WakeMetric};

pub type CapabilityId = u64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadyFlags(u16);

impl ReadyFlags {
    pub const READABLE: Self = Self(1 << 0);
    pub const WRITABLE: Self = Self(1 << 1);
    pub const ACCEPT: Self = Self(1 << 2);
    pub const DATAGRAM: Self = Self(1 << 3);
    pub const END: Self = Self(1 << 4);
    pub const ERROR: Self = Self(1 << 5);
    pub const CLOSE: Self = Self(1 << 6);

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl BitOr for ReadyFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ReadyFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for ReadyFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl Sub for ReadyFlags {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 & !rhs.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyWake {
    pub generation: u64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyObservation {
    pub capability_id: CapabilityId,
    pub capability_generation: u64,
    pub flags: ReadyFlags,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalObservation {
    pub signal: i32,
    pub delivery_token: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyBatch {
    pub generation: u64,
    pub epoch: u64,
    pub entries: Vec<ReadyObservation>,
    pub signals_ready: bool,
    pub timers_ready: bool,
    pub more: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyAcknowledgement {
    pub capability_id: CapabilityId,
    pub capability_generation: u64,
    pub observed_revision: u64,
    pub clear: ReadyFlags,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadyError {
    WrongGeneration {
        supplied: u64,
        expected: u64,
    },
    StaleWake {
        supplied: u64,
        outstanding: Option<u64>,
    },
    HandleLimit {
        limit: usize,
    },
    ControlLimit {
        control: &'static str,
        limit: usize,
        config_path: String,
    },
    InvalidSignal {
        signal: i32,
    },
    StaleCapabilityGeneration {
        capability_id: CapabilityId,
        supplied: u64,
        expected: u64,
    },
    MissingResourceLimit {
        resource: ResourceClass,
    },
    RevisionExhausted {
        capability_id: CapabilityId,
    },
    EpochExhausted,
    WakeInvariant,
    WakeDisconnected,
    Poisoned,
}

impl fmt::Display for ReadyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongGeneration { supplied, expected } => write!(
                formatter,
                "ERR_AGENTOS_READY_STALE_GENERATION: supplied generation {supplied}, expected {expected}"
            ),
            Self::StaleWake {
                supplied,
                outstanding,
            } => write!(
                formatter,
                "ERR_AGENTOS_READY_STALE_WAKE: supplied epoch {supplied}, outstanding epoch {outstanding:?}"
            ),
            Self::HandleLimit { limit } => write!(
                formatter,
                "ERR_AGENTOS_READY_HANDLE_LIMIT: ready handle count exceeded {limit}; raise limits.reactor.maxReadyHandles (VM) or runtime.resources.maxReadyHandles (process)"
            ),
            Self::ControlLimit {
                control,
                limit,
                config_path,
            } => write!(
                formatter,
                "ERR_AGENTOS_READY_CONTROL_LIMIT: pending {control} count exceeded {limit}; raise {config_path}"
            ),
            Self::InvalidSignal { signal } => write!(
                formatter,
                "ERR_AGENTOS_READY_SIGNAL_INVALID: signal {signal} is outside the supported 1..=64 range"
            ),
            Self::StaleCapabilityGeneration {
                capability_id,
                supplied,
                expected,
            } => write!(
                formatter,
                "ERR_AGENTOS_READY_STALE_CAPABILITY: capability {capability_id} generation {supplied} does not match live generation {expected}"
            ),
            Self::MissingResourceLimit { resource } => write!(
                formatter,
                "ERR_AGENTOS_READY_RESOURCE_UNBOUNDED: resource={} has no configured VM limit",
                resource.name()
            ),
            Self::RevisionExhausted { capability_id } => write!(
                formatter,
                "ERR_AGENTOS_READY_REVISION_EXHAUSTED: capability {capability_id} exhausted readiness revisions"
            ),
            Self::EpochExhausted => formatter.write_str(
                "ERR_AGENTOS_READY_EPOCH_EXHAUSTED: VM generation exhausted wake epochs",
            ),
            Self::WakeInvariant => formatter.write_str(
                "ERR_AGENTOS_READY_WAKE_INVARIANT: capacity-one wake lane was full while broker state was idle",
            ),
            Self::WakeDisconnected => formatter.write_str(
                "ERR_AGENTOS_READY_WAKE_DISCONNECTED: VM wake consumer disconnected",
            ),
            Self::Poisoned => formatter.write_str(
                "ERR_AGENTOS_READY_STATE_POISONED: VM readiness state lock poisoned",
            ),
        }
    }
}

impl std::error::Error for ReadyError {}

#[derive(Debug)]
struct ReadyEntry {
    capability_generation: u64,
    flags: ReadyFlags,
    revision: u64,
    application_read_interest: bool,
    ready_since: Option<Instant>,
}

#[derive(Debug)]
enum WakeState {
    Idle,
    Outstanding { epoch: u64 },
    Failed(WakeFailure),
}

#[derive(Clone, Copy, Debug)]
enum WakeFailure {
    Invariant,
    Disconnected,
}

impl WakeFailure {
    fn error(self) -> ReadyError {
        match self {
            Self::Invariant => ReadyError::WakeInvariant,
            Self::Disconnected => ReadyError::WakeDisconnected,
        }
    }
}

#[derive(Debug)]
struct ReadyState {
    handles: BTreeMap<CapabilityId, ReadyEntry>,
    last_delivered_capability: Option<CapabilityId>,
    signals: BTreeMap<i32, u64>,
    timers: BTreeSet<u64>,
    wake: WakeState,
    next_epoch: u64,
}

#[derive(Clone, Debug)]
pub struct SessionReadyBroker {
    generation: u64,
    max_handles: usize,
    max_batch_handles: usize,
    max_pending_timers: usize,
    timer_limit_config_path: String,
    state: Arc<Mutex<ReadyState>>,
    wake_tx: tokio::sync::mpsc::Sender<ReadyWake>,
    metrics: Option<RuntimeMetrics>,
}

impl SessionReadyBroker {
    pub fn new(
        generation: u64,
        max_handles: usize,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<ReadyWake>), ReadyError> {
        Self::new_inner(
            generation,
            max_handles,
            max_handles,
            max_handles,
            String::from("limits.jsRuntime.maxTimers"),
            None,
        )
    }

    pub fn new_with_metrics(
        generation: u64,
        max_handles: usize,
        metrics: RuntimeMetrics,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<ReadyWake>), ReadyError> {
        Self::new_inner(
            generation,
            max_handles,
            max_handles,
            max_handles,
            String::from("limits.jsRuntime.maxTimers"),
            Some(metrics),
        )
    }

    pub fn new_with_resources(
        generation: u64,
        resources: Arc<ResourceLedger>,
        metrics: RuntimeMetrics,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<ReadyWake>), ReadyError> {
        let max_handles = configured_limit(&resources, ResourceClass::ReadyHandles)?;
        let timer_limit = configured_resource_limit(&resources, ResourceClass::Timers)?;
        Self::new_inner(
            generation,
            max_handles,
            max_handles,
            timer_limit.maximum,
            timer_limit.config_path,
            Some(metrics),
        )
    }

    fn new_inner(
        generation: u64,
        max_handles: usize,
        max_batch_handles: usize,
        max_pending_timers: usize,
        timer_limit_config_path: String,
        metrics: Option<RuntimeMetrics>,
    ) -> Result<(Self, tokio::sync::mpsc::Receiver<ReadyWake>), ReadyError> {
        if max_handles == 0 {
            return Err(ReadyError::HandleLimit { limit: 0 });
        }
        let (wake_tx, wake_rx) = tokio::sync::mpsc::channel(1);
        Ok((
            Self {
                generation,
                max_handles,
                max_batch_handles,
                max_pending_timers,
                timer_limit_config_path,
                state: Arc::new(Mutex::new(ReadyState {
                    handles: BTreeMap::new(),
                    last_delivered_capability: None,
                    signals: BTreeMap::new(),
                    timers: BTreeSet::new(),
                    wake: WakeState::Idle,
                    next_epoch: 1,
                })),
                wake_tx,
                metrics,
            },
            wake_rx,
        ))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn max_batch_handles(&self) -> usize {
        self.max_batch_handles
    }

    pub fn mark_signal_ready(
        &self,
        generation: u64,
        signal: i32,
        delivery_token: u64,
    ) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        if !(1..=64).contains(&signal) {
            return Err(ReadyError::InvalidSignal { signal });
        }
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        state.signals.insert(signal, delivery_token);
        self.schedule_wake_locked(&mut state, false)
    }

    pub fn mark_timer_ready(&self, generation: u64, timer_id: u64) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        if !state.timers.contains(&timer_id) && state.timers.len() >= self.max_pending_timers {
            return Err(ReadyError::ControlLimit {
                control: "timers",
                limit: self.max_pending_timers,
                config_path: self.timer_limit_config_path.clone(),
            });
        }
        state.timers.insert(timer_id);
        self.schedule_wake_locked(&mut state, false)
    }

    pub fn drain_signals(
        &self,
        generation: u64,
        epoch: u64,
        max: usize,
    ) -> Result<Vec<SignalObservation>, ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        self.validate_epoch(&state, epoch)?;
        let values = state
            .signals
            .iter()
            .map(|(signal, delivery_token)| SignalObservation {
                signal: *signal,
                delivery_token: *delivery_token,
            })
            .take(max.max(1))
            .collect::<Vec<_>>();
        for value in &values {
            state.signals.remove(&value.signal);
        }
        Ok(values)
    }

    pub fn drain_timers(
        &self,
        generation: u64,
        epoch: u64,
        max: usize,
    ) -> Result<Vec<u64>, ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        self.validate_epoch(&state, epoch)?;
        let values = state
            .timers
            .iter()
            .copied()
            .take(max.max(1))
            .collect::<Vec<_>>();
        for value in &values {
            state.timers.remove(value);
        }
        Ok(values)
    }

    pub fn mark_ready(
        &self,
        generation: u64,
        capability_id: CapabilityId,
        capability_generation: u64,
        mut flags: ReadyFlags,
    ) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        let existing = state.handles.get(&capability_id);
        if let Some(entry) = existing {
            validate_capability_generation(
                capability_id,
                capability_generation,
                entry.capability_generation,
            )?;
        } else if state.handles.len() >= self.max_handles {
            return Err(ReadyError::HandleLimit {
                limit: self.max_handles,
            });
        }
        if existing.is_some_and(|entry| !entry.application_read_interest) {
            flags = flags - ReadyFlags::READABLE;
        }
        if flags.is_empty() {
            if existing.is_none() {
                state.handles.insert(
                    capability_id,
                    ReadyEntry {
                        capability_generation,
                        flags: ReadyFlags::default(),
                        revision: 0,
                        application_read_interest: true,
                        ready_since: None,
                    },
                );
            }
            return Ok(());
        }
        let entry = state.handles.entry(capability_id).or_insert(ReadyEntry {
            capability_generation,
            flags: ReadyFlags::default(),
            revision: 0,
            application_read_interest: true,
            ready_since: None,
        });
        if entry.flags.is_empty() {
            entry.ready_since = Some(Instant::now());
        }
        entry.revision = entry
            .revision
            .checked_add(1)
            .ok_or(ReadyError::RevisionExhausted { capability_id })?;
        entry.flags |= flags;
        if let Some(metrics) = &self.metrics {
            metrics.record_wake(WakeMetric::Attempted);
        }
        let result = self.schedule_wake_locked(&mut state, false);
        self.observe_ready_state(&state);
        result
    }

    pub fn set_application_read_interest(
        &self,
        generation: u64,
        capability_id: CapabilityId,
        capability_generation: u64,
        enabled: bool,
    ) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        if !state.handles.contains_key(&capability_id) && state.handles.len() >= self.max_handles {
            return Err(ReadyError::HandleLimit {
                limit: self.max_handles,
            });
        }
        let entry = state.handles.entry(capability_id).or_insert(ReadyEntry {
            capability_generation,
            flags: ReadyFlags::default(),
            revision: 0,
            application_read_interest: enabled,
            ready_since: None,
        });
        validate_capability_generation(
            capability_id,
            capability_generation,
            entry.capability_generation,
        )?;
        entry.application_read_interest = enabled;
        if !enabled && entry.flags.intersects(ReadyFlags::READABLE) {
            entry.revision = entry
                .revision
                .checked_add(1)
                .ok_or(ReadyError::RevisionExhausted { capability_id })?;
            entry.flags = entry.flags - ReadyFlags::READABLE;
            if entry.flags.is_empty() {
                entry.ready_since = None;
            }
        }
        self.observe_ready_state(&state);
        Ok(())
    }

    pub fn remove_capability(
        &self,
        generation: u64,
        capability_id: CapabilityId,
        capability_generation: u64,
    ) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        if let Some(entry) = state.handles.get(&capability_id) {
            validate_capability_generation(
                capability_id,
                capability_generation,
                entry.capability_generation,
            )?;
        }
        state.handles.remove(&capability_id);
        self.observe_ready_state(&state);
        Ok(())
    }

    pub fn ready_batch(
        &self,
        generation: u64,
        epoch: u64,
        max_handles: usize,
    ) -> Result<ReadyBatch, ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        self.validate_epoch(&state, epoch)?;
        if let Some(metrics) = &self.metrics {
            metrics.record_wake(WakeMetric::Delivered);
        }
        let limit = max_handles.max(1).min(self.max_batch_handles);
        let ready_count = state
            .handles
            .values()
            .filter(|entry| !entry.flags.is_empty())
            .count();
        let mut entries = Vec::with_capacity(limit.min(ready_count));

        // Continue after the last capability delivered by the previous batch,
        // then wrap once. A continuously republished low capability therefore
        // cannot occupy every bounded batch and starve higher capability IDs.
        if let Some(cursor) = state.last_delivered_capability {
            for (&capability_id, entry) in state.handles.range((
                std::ops::Bound::Excluded(cursor),
                std::ops::Bound::Unbounded,
            )) {
                append_ready_observation(&mut entries, limit, capability_id, entry);
                if entries.len() == limit {
                    break;
                }
            }
            if entries.len() < limit {
                for (&capability_id, entry) in state.handles.range(..=cursor) {
                    append_ready_observation(&mut entries, limit, capability_id, entry);
                    if entries.len() == limit {
                        break;
                    }
                }
            }
        } else {
            for (&capability_id, entry) in &state.handles {
                append_ready_observation(&mut entries, limit, capability_id, entry);
                if entries.len() == limit {
                    break;
                }
            }
        }
        if let Some(last) = entries.last() {
            state.last_delivered_capability = Some(last.capability_id);
        }
        self.observe_ready_state(&state);
        Ok(ReadyBatch {
            generation,
            epoch,
            signals_ready: !state.signals.is_empty(),
            timers_ready: !state.timers.is_empty(),
            more: ready_count > entries.len(),
            entries,
        })
    }

    pub fn complete_wake(
        &self,
        generation: u64,
        epoch: u64,
        acknowledgements: &[ReadyAcknowledgement],
    ) -> Result<(), ReadyError> {
        self.validate_generation(generation)?;
        let mut state = self.state.lock().map_err(|_| ReadyError::Poisoned)?;
        self.validate_epoch(&state, epoch)?;
        for acknowledgement in acknowledgements {
            let Some(entry) = state.handles.get_mut(&acknowledgement.capability_id) else {
                continue;
            };
            if entry.capability_generation != acknowledgement.capability_generation {
                continue;
            }
            if entry.revision == acknowledgement.observed_revision {
                let mut clear = acknowledgement.clear;
                if !entry.application_read_interest {
                    clear |= ReadyFlags::READABLE;
                }
                entry.flags = entry.flags - clear;
                if entry.flags.is_empty() {
                    entry.ready_since = None;
                }
            }
        }
        state.wake = WakeState::Idle;
        let result = self.schedule_wake_locked(&mut state, true);
        self.observe_ready_state(&state);
        result
    }

    pub fn pending_handle_count(&self) -> Result<usize, ReadyError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ReadyError::Poisoned)?
            .handles
            .values()
            .filter(|entry| !entry.flags.is_empty())
            .count())
    }

    fn validate_generation(&self, generation: u64) -> Result<(), ReadyError> {
        if generation != self.generation {
            return Err(ReadyError::WrongGeneration {
                supplied: generation,
                expected: self.generation,
            });
        }
        Ok(())
    }

    fn validate_epoch(&self, state: &ReadyState, epoch: u64) -> Result<(), ReadyError> {
        let outstanding = match state.wake {
            WakeState::Outstanding { epoch } => Some(epoch),
            WakeState::Idle | WakeState::Failed(_) => None,
        };
        if outstanding != Some(epoch) {
            return Err(ReadyError::StaleWake {
                supplied: epoch,
                outstanding,
            });
        }
        Ok(())
    }

    fn schedule_wake_locked(
        &self,
        state: &mut ReadyState,
        rearmed: bool,
    ) -> Result<(), ReadyError> {
        if !state.handles.values().any(|entry| !entry.flags.is_empty())
            && state.signals.is_empty()
            && state.timers.is_empty()
        {
            return Ok(());
        }
        match state.wake {
            WakeState::Idle => {}
            WakeState::Outstanding { .. } => {
                if let Some(metrics) = &self.metrics {
                    metrics.record_wake(WakeMetric::Coalesced);
                }
                return Ok(());
            }
            WakeState::Failed(failure) => return Err(failure.error()),
        }
        let epoch = state.next_epoch;
        state.next_epoch = state
            .next_epoch
            .checked_add(1)
            .ok_or(ReadyError::EpochExhausted)?;
        state.wake = WakeState::Outstanding { epoch };
        match self.wake_tx.try_send(ReadyWake {
            generation: self.generation,
            epoch,
        }) {
            Ok(()) => {
                if let Some(metrics) = &self.metrics {
                    if rearmed {
                        metrics.record_wake(WakeMetric::Rearmed);
                    }
                    metrics.observe_channel(
                        ChannelMetricClass::ReadyWake,
                        1,
                        std::mem::size_of::<ReadyWake>(),
                    );
                }
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                state.wake = WakeState::Failed(WakeFailure::Invariant);
                Err(ReadyError::WakeInvariant)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                state.wake = WakeState::Failed(WakeFailure::Disconnected);
                Err(ReadyError::WakeDisconnected)
            }
        }
    }

    fn observe_ready_state(&self, state: &ReadyState) {
        let Some(metrics) = &self.metrics else {
            return;
        };
        let now = Instant::now();
        let mut size = 0usize;
        let mut oldest = Duration::ZERO;
        for entry in state.handles.values() {
            if entry.flags.is_empty() {
                continue;
            }
            size += 1;
            if let Some(ready_since) = entry.ready_since {
                oldest = oldest.max(now.saturating_duration_since(ready_since));
            }
        }
        metrics.observe_readiness(size, oldest);
    }
}

fn append_ready_observation(
    entries: &mut Vec<ReadyObservation>,
    limit: usize,
    capability_id: CapabilityId,
    entry: &ReadyEntry,
) {
    if entries.len() < limit && !entry.flags.is_empty() {
        entries.push(ReadyObservation {
            capability_id,
            capability_generation: entry.capability_generation,
            flags: entry.flags,
            revision: entry.revision,
        });
    }
}

fn configured_limit(
    resources: &ResourceLedger,
    resource: ResourceClass,
) -> Result<usize, ReadyError> {
    resources
        .usage(resource)
        .limit
        .filter(|limit| *limit > 0)
        .ok_or(ReadyError::MissingResourceLimit { resource })
}

fn configured_resource_limit(
    resources: &ResourceLedger,
    resource: ResourceClass,
) -> Result<crate::accounting::ResourceLimit, ReadyError> {
    resources
        .configured_limit(resource)
        .filter(|limit| limit.maximum > 0)
        .ok_or(ReadyError::MissingResourceLimit { resource })
}

fn validate_capability_generation(
    capability_id: CapabilityId,
    supplied: u64,
    expected: u64,
) -> Result<(), ReadyError> {
    if supplied == expected {
        Ok(())
    } else {
        Err(ReadyError::StaleCapabilityGeneration {
            capability_id,
            supplied,
            expected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::ResourceLimit;
    use crate::capability::{CapabilityBackend, CapabilityKind, CapabilityRegistry};

    #[tokio::test]
    async fn repeated_marks_coalesce_to_one_wake() {
        let (broker, mut wakes) = SessionReadyBroker::new(7, 8).expect("broker");
        for _ in 0..1_000_000 {
            broker
                .mark_ready(7, 41, 1, ReadyFlags::READABLE)
                .expect("mark ready");
        }
        let wake = wakes.recv().await.expect("one wake");
        assert_eq!(
            wake,
            ReadyWake {
                generation: 7,
                epoch: 1
            }
        );
        assert!(matches!(
            wakes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        let batch = broker.ready_batch(7, wake.epoch, 8).expect("ready batch");
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].revision, 1_000_000);
    }

    #[tokio::test]
    async fn signals_and_timers_share_one_wake_but_keep_durable_control_state() {
        let (broker, mut wakes) = SessionReadyBroker::new(8, 8).expect("broker");
        for _ in 0..10_000 {
            broker
                .mark_signal_ready(8, 15, 41)
                .expect("coalesce SIGTERM");
            broker.mark_timer_ready(8, 91).expect("coalesce timer");
        }
        let wake = wakes.recv().await.expect("one control wake");
        assert!(matches!(
            wakes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        let batch = broker.ready_batch(8, wake.epoch, 8).expect("control batch");
        assert!(batch.signals_ready);
        assert!(batch.timers_ready);
        assert_eq!(
            broker.drain_signals(8, wake.epoch, 8).unwrap(),
            vec![SignalObservation {
                signal: 15,
                delivery_token: 41,
            }]
        );
        assert_eq!(broker.drain_timers(8, wake.epoch, 8).unwrap(), vec![91]);
        broker
            .complete_wake(8, wake.epoch, &[])
            .expect("complete control wake");
        assert!(matches!(
            wakes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn timer_limit_error_names_the_owning_configuration_path() {
        let resources = Arc::new(ResourceLedger::root(
            "vm=timer-limit generation=10",
            [
                (
                    ResourceClass::ReadyHandles,
                    ResourceLimit::new(2, "limits.reactor.maxReadyHandles"),
                ),
                (
                    ResourceClass::Timers,
                    ResourceLimit::new(1, "limits.jsRuntime.maxTimers"),
                ),
            ],
        ));
        let (broker, _wakes) =
            SessionReadyBroker::new_with_resources(10, resources, RuntimeMetrics::new())
                .expect("broker");
        broker.mark_timer_ready(10, 1).expect("first timer");

        let error = broker
            .mark_timer_ready(10, 2)
            .expect_err("second unique timer exceeds configured limit");

        assert_eq!(
            error,
            ReadyError::ControlLimit {
                control: "timers",
                limit: 1,
                config_path: String::from("limits.jsRuntime.maxTimers"),
            }
        );
        assert!(error
            .to_string()
            .ends_with("raise limits.jsRuntime.maxTimers"));
    }

    #[tokio::test]
    async fn concurrent_republication_is_not_cleared_by_old_observation() {
        let (broker, mut wakes) = SessionReadyBroker::new(9, 8).expect("broker");
        broker
            .mark_ready(9, 3, 1, ReadyFlags::READABLE)
            .expect("first mark");
        let wake = wakes.recv().await.expect("first wake");
        let batch = broker.ready_batch(9, wake.epoch, 8).expect("batch");
        broker
            .mark_ready(9, 3, 1, ReadyFlags::READABLE)
            .expect("concurrent mark");
        broker
            .complete_wake(
                9,
                wake.epoch,
                &[ReadyAcknowledgement {
                    capability_id: 3,
                    capability_generation: 1,
                    observed_revision: batch.entries[0].revision,
                    clear: ReadyFlags::READABLE,
                }],
            )
            .expect("complete old wake");

        let replacement = wakes.recv().await.expect("replacement wake");
        let replacement_batch = broker
            .ready_batch(9, replacement.epoch, 8)
            .expect("replacement batch");
        assert_eq!(replacement_batch.entries[0].revision, 2);
    }

    #[tokio::test]
    async fn bounded_batches_rotate_past_continuously_hot_low_capability_ids() {
        let (broker, mut wakes) = SessionReadyBroker::new(10, 8).expect("broker");
        for capability_id in 1..=8 {
            broker
                .mark_ready(10, capability_id, 1, ReadyFlags::READABLE)
                .expect("mark initial readiness");
        }

        let first_wake = wakes.recv().await.expect("first wake");
        let first = broker
            .ready_batch(10, first_wake.epoch, 2)
            .expect("first bounded batch");
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.capability_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        // Republish the first two handles before acknowledging their old
        // observations. Their revisions remain pending, but the next batch
        // must advance to capabilities that have not received a turn yet.
        for capability_id in 1..=2 {
            broker
                .mark_ready(10, capability_id, 1, ReadyFlags::READABLE)
                .expect("keep low capability hot");
        }
        let first_acknowledgements = first
            .entries
            .iter()
            .map(|entry| ReadyAcknowledgement {
                capability_id: entry.capability_id,
                capability_generation: entry.capability_generation,
                observed_revision: entry.revision,
                clear: ReadyFlags::READABLE,
            })
            .collect::<Vec<_>>();
        broker
            .complete_wake(10, first_wake.epoch, &first_acknowledgements)
            .expect("complete first wake");

        let second_wake = wakes.recv().await.expect("second wake");
        let second = broker
            .ready_batch(10, second_wake.epoch, 2)
            .expect("second bounded batch");
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.capability_id)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(second.more);
    }

    #[tokio::test]
    async fn read_interest_suppresses_readable_publication() {
        let (broker, mut wakes) = SessionReadyBroker::new(11, 8).expect("broker");
        broker
            .mark_ready(11, 5, 1, ReadyFlags::WRITABLE)
            .expect("create entry");
        let first = wakes.recv().await.expect("first wake");
        let batch = broker.ready_batch(11, first.epoch, 8).expect("batch");
        broker
            .complete_wake(
                11,
                first.epoch,
                &[ReadyAcknowledgement {
                    capability_id: 5,
                    capability_generation: 1,
                    observed_revision: batch.entries[0].revision,
                    clear: ReadyFlags::WRITABLE,
                }],
            )
            .expect("complete wake");
        broker
            .set_application_read_interest(11, 5, 1, false)
            .expect("pause reads");
        broker
            .mark_ready(11, 5, 1, ReadyFlags::READABLE)
            .expect("suppressed readable mark");
        assert!(matches!(
            wakes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn read_interest_set_before_first_publication_is_durable() {
        let (broker, mut wakes) = SessionReadyBroker::new(12, 8).expect("broker");
        broker
            .set_application_read_interest(12, 6, 1, false)
            .expect("pause before first readiness");
        broker
            .mark_ready(12, 6, 1, ReadyFlags::READABLE)
            .expect("suppressed first readable mark");
        assert!(matches!(
            wakes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        broker
            .mark_ready(12, 6, 1, ReadyFlags::END)
            .expect("terminal readiness remains observable while reads are paused");
        let wake = wakes.recv().await.expect("terminal wake");
        let batch = broker.ready_batch(12, wake.epoch, 8).expect("batch");
        assert_eq!(batch.entries.len(), 1);
        assert_eq!(batch.entries[0].flags, ReadyFlags::END);
    }

    #[tokio::test]
    async fn stale_completion_cannot_mutate_current_epoch() {
        let (broker, mut wakes) = SessionReadyBroker::new(13, 8).expect("broker");
        broker
            .mark_ready(13, 1, 1, ReadyFlags::CLOSE)
            .expect("mark close");
        let wake = wakes.recv().await.expect("wake");
        let error = broker
            .complete_wake(13, wake.epoch + 1, &[])
            .expect_err("stale epoch must fail");
        assert!(matches!(error, ReadyError::StaleWake { .. }));
        assert_eq!(broker.pending_handle_count().expect("pending count"), 1);
    }

    #[test]
    fn disconnected_wake_lane_remains_a_hard_error() {
        let (broker, wakes) = SessionReadyBroker::new(14, 8).expect("broker");
        drop(wakes);

        assert!(matches!(
            broker.mark_ready(14, 1, 1, ReadyFlags::READABLE),
            Err(ReadyError::WakeDisconnected)
        ));
        assert!(matches!(
            broker.mark_ready(14, 1, 1, ReadyFlags::END),
            Err(ReadyError::WakeDisconnected)
        ));
    }

    #[tokio::test]
    async fn production_metrics_follow_broker_state_without_id_labels() {
        let metrics = RuntimeMetrics::new();
        let (broker, mut wakes) =
            SessionReadyBroker::new_with_metrics(17, 8, metrics.clone()).expect("broker");
        broker
            .mark_ready(17, 4, 1, ReadyFlags::READABLE)
            .expect("first publication");
        broker
            .mark_ready(17, 4, 1, ReadyFlags::READABLE)
            .expect("coalesced publication");
        let wake = wakes.recv().await.expect("wake");
        let batch = broker.ready_batch(17, wake.epoch, 8).expect("batch");
        broker
            .complete_wake(
                17,
                wake.epoch,
                &[ReadyAcknowledgement {
                    capability_id: 4,
                    capability_generation: 1,
                    observed_revision: batch.entries[0].revision,
                    clear: ReadyFlags::READABLE,
                }],
            )
            .expect("complete");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.wakes[WakeMetric::Attempted.index()], 2);
        assert_eq!(snapshot.wakes[WakeMetric::Coalesced.index()], 1);
        assert_eq!(snapshot.wakes[WakeMetric::Delivered.index()], 1);
        assert_eq!(snapshot.readiness.current_size, 0);
        assert_eq!(
            snapshot.channels[ChannelMetricClass::ReadyWake.index()].count_high_water,
            1
        );
    }

    #[tokio::test]
    async fn capability_admission_owns_ready_permits_without_broker_double_charge() {
        let resources = Arc::new(ResourceLedger::root(
            "vm=readiness-integration generation=23",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(4, "limits.reactor.maxCapabilities"),
                ),
                (
                    ResourceClass::ReadyHandles,
                    ResourceLimit::new(4, "limits.reactor.maxReadyHandles"),
                ),
                (
                    ResourceClass::Timers,
                    ResourceLimit::new(4, "runtime.resources.maxTimers"),
                ),
            ],
        ));
        let capabilities = CapabilityRegistry::new(23, Arc::clone(&resources));
        let (broker, mut wakes) = SessionReadyBroker::new_with_resources(
            23,
            Arc::clone(&resources),
            RuntimeMetrics::new(),
        )
        .expect("bounded broker");

        let mut leases = Vec::new();
        for index in 0..4_u64 {
            let lease = capabilities
                .reserve(CapabilityKind::Http2Stream)
                .expect("admit capability up to configured maximum")
                .commit(CapabilityBackend::Native {
                    local_id: format!("stream-{index}"),
                })
                .expect("commit capability");
            broker
                .mark_ready(23, lease.id(), lease.generation(), ReadyFlags::READABLE)
                .expect("readiness publication must consume no second permit");
            leases.push(lease);
        }
        assert_eq!(resources.usage(ResourceClass::ReadyHandles).used, 4);

        let wake = wakes.recv().await.expect("one coalesced wake");
        let batch = broker.ready_batch(23, wake.epoch, 4).expect("ready batch");
        assert_eq!(batch.entries.len(), 4);
        let acknowledgements = batch
            .entries
            .iter()
            .map(|entry| ReadyAcknowledgement {
                capability_id: entry.capability_id,
                capability_generation: entry.capability_generation,
                observed_revision: entry.revision,
                clear: entry.flags,
            })
            .collect::<Vec<_>>();
        broker
            .complete_wake(23, wake.epoch, &acknowledgements)
            .expect("complete wake");
        drop(broker);
        drop(leases);
        drop(capabilities);
        assert!(resources.is_zero(), "all capability permits must reconcile");
    }
}
