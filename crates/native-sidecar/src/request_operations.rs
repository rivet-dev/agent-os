//! Bounded, connection-scoped request-operation admission.
//!
//! This module deliberately does not contain an ordinary request queue. The
//! protocol router either admits an operation here and starts it independently,
//! or returns a typed rejection while continuing to route progress traffic.

use crate::wire::{OwnershipScope, RequestId};
use agentos_runtime::RuntimeProtocolConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

use crate::extension::ExtensionOrderingPolicy;

pub(crate) const IN_FLIGHT_REQUEST_COUNT_PATH: &str = "runtime.protocol.maxInFlightRequests";
pub(crate) const IN_FLIGHT_REQUEST_BYTES_PATH: &str = "runtime.protocol.maxInFlightRequestBytes";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestOperationLimits {
    pub(crate) max_requests: usize,
    pub(crate) max_request_bytes: usize,
}

impl From<&RuntimeProtocolConfig> for RequestOperationLimits {
    fn from(config: &RuntimeProtocolConfig) -> Self {
        Self {
            max_requests: config.max_in_flight_requests,
            max_request_bytes: config.max_in_flight_request_bytes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RequestOperationKey {
    pub(crate) connection_id: String,
    pub(crate) request_id: RequestId,
}

impl RequestOperationKey {
    pub(crate) fn new(connection_id: impl Into<String>, request_id: RequestId) -> Self {
        Self {
            connection_id: connection_id.into(),
            request_id,
        }
    }
}

/// Narrow conflict domains carried with an independently executing request.
/// This is metadata only: entity coordinators enforce the ordering policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestOrderingKey {
    Connection(String),
    Session {
        connection_id: String,
        session_id: String,
    },
    VmLifecycle {
        connection_id: String,
        session_id: String,
        vm_id: String,
    },
    VmOperation {
        connection_id: String,
        session_id: String,
        vm_id: String,
    },
    Extension {
        namespace: String,
        connection_id: String,
        key: Vec<u8>,
        policy: ExtensionOrderingPolicy,
    },
    Unordered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestOperationMetadata {
    pub(crate) ownership: OwnershipScope,
    pub(crate) operation: String,
    pub(crate) ordering_key: RequestOrderingKey,
}

impl RequestOperationMetadata {
    pub(crate) fn new(
        ownership: OwnershipScope,
        operation: impl Into<String>,
        ordering_key: RequestOrderingKey,
    ) -> Self {
        Self {
            ownership,
            operation: operation.into(),
            ordering_key,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOperationState {
    Admitted,
    Running,
    Cancelling,
    Completing,
    Failed,
    Shutdown,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum OperationCancellationReason {
    Explicit = 1,
    ConnectionClosed = 2,
    Shutdown = 3,
    TransportClosed = 4,
}

impl OperationCancellationReason {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Explicit),
            2 => Some(Self::ConnectionClosed),
            3 => Some(Self::Shutdown),
            4 => Some(Self::TransportClosed),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct CancellationState {
    reason: AtomicU8,
    notified: Notify,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationCancellation {
    inner: Arc<CancellationState>,
}

impl OperationCancellation {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                reason: AtomicU8::new(0),
                notified: Notify::new(),
            }),
        }
    }

    pub(crate) fn signal(&self, reason: OperationCancellationReason) -> bool {
        if self
            .inner
            .reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.inner.notified.notify_waiters();
            true
        } else {
            false
        }
    }

    pub(crate) fn reason(&self) -> Option<OperationCancellationReason> {
        OperationCancellationReason::from_u8(self.inner.reason.load(Ordering::Acquire))
    }

    pub(crate) async fn cancelled(&self) -> OperationCancellationReason {
        loop {
            // Register the waiter before probing state so a signal cannot land
            // between the empty check and the await.
            let notified = self.inner.notified.notified();
            if let Some(reason) = self.reason() {
                return reason;
            }
            notified.await;
        }
    }
}

/// Atomically grants permission to publish one terminal response. Clones share
/// the same state so cancellation and normal completion can safely race.
const PUBLICATION_UNCLAIMED: u8 = 0;
const PUBLICATION_PUBLISHING: u8 = 1;
const PUBLICATION_RETAINED: u8 = 2;
const PUBLICATION_TAKEN_OVER: u8 = 3;

#[derive(Clone, Debug)]
pub(crate) struct TerminalResponseGuard {
    state: Arc<AtomicU8>,
}

impl TerminalResponseGuard {
    fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(PUBLICATION_UNCLAIMED)),
        }
    }

    pub(crate) fn try_claim(&self) -> bool {
        self.state
            .compare_exchange(
                PUBLICATION_UNCLAIMED,
                PUBLICATION_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_retained(&self) -> bool {
        self.state
            .compare_exchange(
                PUBLICATION_PUBLISHING,
                PUBLICATION_RETAINED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn try_take_over_unretained(&self) -> bool {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if matches!(current, PUBLICATION_RETAINED | PUBLICATION_TAKEN_OVER) {
                return false;
            }
            if self
                .state
                .compare_exchange(
                    current,
                    PUBLICATION_TAKEN_OVER,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    pub(crate) fn is_claimed(&self) -> bool {
        self.state.load(Ordering::Acquire) != PUBLICATION_UNCLAIMED
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestOperationSnapshot {
    pub(crate) key: RequestOperationKey,
    pub(crate) metadata: RequestOperationMetadata,
    pub(crate) request_bytes: usize,
    pub(crate) state: RequestOperationState,
    pub(crate) cancellation_reason: Option<OperationCancellationReason>,
    pub(crate) terminal_claimed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestOperationRegistrySnapshot {
    pub(crate) in_flight_requests: usize,
    pub(crate) in_flight_request_bytes: usize,
    pub(crate) closed: Option<OperationCancellationReason>,
    pub(crate) closed_connections: BTreeSet<String>,
    pub(crate) operations: Vec<RequestOperationSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForcedRequestOutcome {
    pub(crate) key: RequestOperationKey,
    pub(crate) ownership: OwnershipScope,
}

#[derive(Debug)]
struct OperationRecord {
    generation: u64,
    metadata: RequestOperationMetadata,
    request_bytes: usize,
    state: RequestOperationState,
    cancellation: OperationCancellation,
    terminal: TerminalResponseGuard,
}

#[derive(Debug)]
struct RegistryState {
    operations: BTreeMap<RequestOperationKey, OperationRecord>,
    in_flight_request_bytes: usize,
    next_generation: u64,
    closed: Option<OperationCancellationReason>,
    closed_connections: BTreeMap<String, OperationCancellationReason>,
}

#[derive(Debug)]
struct RequestOperationRegistryInner {
    limits: RequestOperationLimits,
    state: Mutex<RegistryState>,
}

#[derive(Clone, Debug)]
pub(crate) struct RequestOperationRegistry {
    inner: Arc<RequestOperationRegistryInner>,
}

impl RequestOperationRegistry {
    pub(crate) fn new(limits: RequestOperationLimits) -> Self {
        assert!(
            limits.max_requests > 0,
            "request count limit must be positive"
        );
        assert!(
            limits.max_request_bytes > 0,
            "request byte limit must be positive"
        );
        Self {
            inner: Arc::new(RequestOperationRegistryInner {
                limits,
                state: Mutex::new(RegistryState {
                    operations: BTreeMap::new(),
                    in_flight_request_bytes: 0,
                    next_generation: 1,
                    closed: None,
                    closed_connections: BTreeMap::new(),
                }),
            }),
        }
    }

    pub(crate) fn from_protocol_config(config: &RuntimeProtocolConfig) -> Self {
        Self::new(RequestOperationLimits::from(config))
    }

    pub(crate) fn admit(
        &self,
        key: RequestOperationKey,
        metadata: RequestOperationMetadata,
        request_bytes: usize,
    ) -> Result<RequestOperation, RequestAdmissionError> {
        let mut state = self.lock_state();
        self.check_admission_locked(&state, &key, &metadata, request_bytes)?;

        let requested_total = state
            .in_flight_request_bytes
            .checked_add(request_bytes)
            .expect("admission preflight checked the request byte total");
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let cancellation = OperationCancellation::new();
        let terminal = TerminalResponseGuard::new();
        state.in_flight_request_bytes = requested_total;
        state.operations.insert(
            key.clone(),
            OperationRecord {
                generation,
                metadata: metadata.clone(),
                request_bytes,
                state: RequestOperationState::Admitted,
                cancellation: cancellation.clone(),
                terminal: terminal.clone(),
            },
        );
        drop(state);

        Ok(RequestOperation {
            registry: self.clone(),
            key,
            generation,
            metadata,
            request_bytes,
            cancellation,
            terminal,
            released: false,
        })
    }

    /// Check admission before acquiring a terminal-output reservation. The
    /// router is the sole admission producer, but [`Self::admit`] repeats this
    /// check so this preflight can never weaken the registry's bounds.
    pub(crate) fn check_admission(
        &self,
        key: &RequestOperationKey,
        metadata: &RequestOperationMetadata,
        request_bytes: usize,
    ) -> Result<(), RequestAdmissionError> {
        let state = self.lock_state();
        self.check_admission_locked(&state, key, metadata, request_bytes)
    }

    fn check_admission_locked(
        &self,
        state: &RegistryState,
        key: &RequestOperationKey,
        metadata: &RequestOperationMetadata,
        request_bytes: usize,
    ) -> Result<(), RequestAdmissionError> {
        let ownership_connection_id = ownership_connection_id(&metadata.ownership);
        if ownership_connection_id != key.connection_id {
            return Err(RequestAdmissionError::OwnershipMismatch {
                key_connection_id: key.connection_id.clone(),
                ownership_connection_id: ownership_connection_id.to_owned(),
            });
        }
        if let Some(reason) = state.closed {
            return Err(RequestAdmissionError::RegistryClosed { reason });
        }
        if let Some(reason) = state.closed_connections.get(&key.connection_id).copied() {
            return Err(RequestAdmissionError::ConnectionClosed {
                connection_id: key.connection_id.clone(),
                reason,
            });
        }
        if state.operations.contains_key(&key) {
            return Err(RequestAdmissionError::DuplicateRequest { key: key.clone() });
        }
        if state.operations.len() >= self.inner.limits.max_requests {
            return Err(RequestAdmissionError::CountLimit {
                current: state.operations.len(),
                requested: 1,
                limit: self.inner.limits.max_requests,
            });
        }
        let requested_total = state
            .in_flight_request_bytes
            .checked_add(request_bytes)
            .ok_or(RequestAdmissionError::ByteLimit {
                current: state.in_flight_request_bytes,
                requested: request_bytes,
                limit: self.inner.limits.max_request_bytes,
            })?;
        if requested_total > self.inner.limits.max_request_bytes {
            return Err(RequestAdmissionError::ByteLimit {
                current: state.in_flight_request_bytes,
                requested: request_bytes,
                limit: self.inner.limits.max_request_bytes,
            });
        }
        Ok(())
    }

    pub(crate) fn cancel(
        &self,
        key: &RequestOperationKey,
        reason: OperationCancellationReason,
    ) -> CancelOperationResult {
        let mut state = self.lock_state();
        let Some(record) = state.operations.get_mut(key) else {
            return CancelOperationResult::NotFound;
        };
        if record.state == RequestOperationState::Terminal {
            return CancelOperationResult::AlreadyTerminal;
        }
        let signalled = record.cancellation.signal(reason);
        if signalled {
            advance_cancellation_state(record, reason);
            CancelOperationResult::Signalled
        } else {
            CancelOperationResult::AlreadySignalled(
                record
                    .cancellation
                    .reason()
                    .expect("signalled cancellation has a reason"),
            )
        }
    }

    pub(crate) fn close_connection(
        &self,
        connection_id: &str,
        reason: OperationCancellationReason,
    ) -> usize {
        let mut state = self.lock_state();
        state
            .closed_connections
            .entry(connection_id.to_owned())
            .or_insert(reason);
        let mut signalled = 0;
        for (key, record) in &mut state.operations {
            if key.connection_id == connection_id
                && record.state != RequestOperationState::Terminal
                && record.cancellation.signal(reason)
            {
                advance_cancellation_state(record, reason);
                signalled += 1;
            }
        }
        signalled
    }

    pub(crate) fn close(&self, reason: OperationCancellationReason) -> usize {
        let mut state = self.lock_state();
        state.closed.get_or_insert(reason);
        let mut signalled = 0;
        for record in state.operations.values_mut() {
            if record.state != RequestOperationState::Terminal && record.cancellation.signal(reason)
            {
                advance_cancellation_state(record, reason);
                signalled += 1;
            }
        }
        signalled
    }

    /// Claim terminal ownership for every unfinished request and remove all
    /// registry accounting in one critical section. The supervisor calls this
    /// only after the cooperative drain deadline; task-local operation handles
    /// share the claimed guards and therefore cannot publish a second terminal
    /// response when they are subsequently aborted.
    pub(crate) fn force_terminalize(
        &self,
        reason: OperationCancellationReason,
    ) -> Vec<ForcedRequestOutcome> {
        let mut state = self.lock_state();
        state.closed.get_or_insert(reason);
        let operations = std::mem::take(&mut state.operations);
        state.in_flight_request_bytes = 0;
        operations
            .into_iter()
            .filter_map(|(key, mut record)| {
                record.cancellation.signal(reason);
                advance_cancellation_state(&mut record, reason);
                // A claimed-but-still-registered record may be draining its
                // finite ordinary event batch. Its terminal response already
                // exists, so removing accounting must not synthesize a second
                // terminal during shutdown.
                if !record.terminal.try_take_over_unretained() {
                    return None;
                }
                Some(ForcedRequestOutcome {
                    key,
                    ownership: record.metadata.ownership,
                })
            })
            .collect()
    }

    pub(crate) fn snapshot(&self) -> RequestOperationRegistrySnapshot {
        let state = self.lock_state();
        RequestOperationRegistrySnapshot {
            in_flight_requests: state.operations.len(),
            in_flight_request_bytes: state.in_flight_request_bytes,
            closed: state.closed,
            closed_connections: state.closed_connections.keys().cloned().collect(),
            operations: state
                .operations
                .iter()
                .map(|(key, record)| RequestOperationSnapshot {
                    key: key.clone(),
                    metadata: record.metadata.clone(),
                    request_bytes: record.request_bytes,
                    state: record.state,
                    cancellation_reason: record.cancellation.reason(),
                    terminal_claimed: record.terminal.is_claimed(),
                })
                .collect(),
        }
    }

    fn transition(
        &self,
        key: &RequestOperationKey,
        generation: u64,
        next: RequestOperationState,
    ) -> Result<(), OperationTransitionError> {
        let mut state = self.lock_state();
        let Some(record) = state.operations.get_mut(key) else {
            return Err(OperationTransitionError::NotFound(key.clone()));
        };
        if record.generation != generation {
            return Err(OperationTransitionError::StaleGeneration(key.clone()));
        }
        if !valid_transition(record.state, next) {
            return Err(OperationTransitionError::Invalid {
                key: key.clone(),
                current: record.state,
                next,
            });
        }
        record.state = next;
        Ok(())
    }

    fn try_mark_terminal(
        &self,
        key: &RequestOperationKey,
        generation: u64,
    ) -> Result<bool, OperationTransitionError> {
        let mut state = self.lock_state();
        let Some(record) = state.operations.get_mut(key) else {
            return Err(OperationTransitionError::NotFound(key.clone()));
        };
        if record.generation != generation {
            return Err(OperationTransitionError::StaleGeneration(key.clone()));
        }
        if record.state == RequestOperationState::Terminal {
            debug_assert!(record.terminal.is_claimed());
            return Ok(false);
        }
        if !valid_transition(record.state, RequestOperationState::Terminal) {
            return Err(OperationTransitionError::Invalid {
                key: key.clone(),
                current: record.state,
                next: RequestOperationState::Terminal,
            });
        }
        if !record.terminal.try_claim() {
            // All claims go through this method, so a claimed guard and a
            // terminal state are updated in the same registry critical section.
            tracing::error!(
                request_id = key.request_id,
                connection_id = %key.connection_id,
                "ERR_AGENTOS_REQUEST_TERMINAL_STATE: terminal response was claimed without a terminal registry state"
            );
            return Ok(false);
        }
        record.state = RequestOperationState::Terminal;
        Ok(true)
    }

    fn release(&self, key: &RequestOperationKey, generation: u64, request_bytes: usize) {
        let mut state = self.lock_state();
        let should_remove = state
            .operations
            .get(key)
            .is_some_and(|record| record.generation == generation);
        if should_remove {
            state.operations.remove(key);
            state.in_flight_request_bytes = state
                .in_flight_request_bytes
                .checked_sub(request_bytes)
                .unwrap_or_else(|| {
                    tracing::error!(
                        request_id = key.request_id,
                        connection_id = %key.connection_id,
                        request_bytes,
                        retained_bytes = state.in_flight_request_bytes,
                        "ERR_AGENTOS_REQUEST_ADMISSION_ACCOUNTING: request byte reservation underflow"
                    );
                    0
                });
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        self.inner.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!(
                "ERR_AGENTOS_REQUEST_REGISTRY_POISONED: recovering request-operation registry state"
            );
            poisoned.into_inner()
        })
    }
}

fn ownership_connection_id(ownership: &OwnershipScope) -> &str {
    match ownership {
        OwnershipScope::ConnectionOwnership(scope) => &scope.connection_id,
        OwnershipScope::SessionOwnership(scope) => &scope.connection_id,
        OwnershipScope::VmOwnership(scope) => &scope.connection_id,
    }
}

fn cancellation_state(reason: OperationCancellationReason) -> RequestOperationState {
    match reason {
        OperationCancellationReason::Explicit => RequestOperationState::Cancelling,
        OperationCancellationReason::ConnectionClosed
        | OperationCancellationReason::Shutdown
        | OperationCancellationReason::TransportClosed => RequestOperationState::Shutdown,
    }
}

fn advance_cancellation_state(record: &mut OperationRecord, reason: OperationCancellationReason) {
    let next = cancellation_state(reason);
    if valid_transition(record.state, next) {
        record.state = next;
    }
}

fn valid_transition(current: RequestOperationState, next: RequestOperationState) -> bool {
    use RequestOperationState as State;
    matches!(
        (current, next),
        (
            State::Admitted,
            State::Running | State::Cancelling | State::Failed | State::Shutdown
        ) | (
            State::Running,
            State::Cancelling | State::Completing | State::Failed | State::Shutdown
        ) | (
            State::Cancelling,
            State::Completing | State::Failed | State::Shutdown
        ) | (State::Completing, State::Failed | State::Terminal)
            | (State::Failed, State::Terminal)
            | (
                State::Shutdown,
                State::Completing | State::Failed | State::Terminal
            )
    )
}

#[derive(Debug)]
pub(crate) struct RequestOperation {
    registry: RequestOperationRegistry,
    key: RequestOperationKey,
    generation: u64,
    metadata: RequestOperationMetadata,
    request_bytes: usize,
    cancellation: OperationCancellation,
    terminal: TerminalResponseGuard,
    released: bool,
}

impl RequestOperation {
    pub(crate) fn key(&self) -> &RequestOperationKey {
        &self.key
    }

    pub(crate) fn metadata(&self) -> &RequestOperationMetadata {
        &self.metadata
    }

    pub(crate) fn request_bytes(&self) -> usize {
        self.request_bytes
    }

    pub(crate) fn cancellation(&self) -> OperationCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn transition(
        &self,
        next: RequestOperationState,
    ) -> Result<(), OperationTransitionError> {
        self.registry.transition(&self.key, self.generation, next)
    }

    pub(crate) fn try_mark_terminal(&self) -> Result<bool, OperationTransitionError> {
        self.registry.try_mark_terminal(&self.key, self.generation)
    }

    pub(crate) fn mark_terminal_retained(&self) -> bool {
        self.terminal.mark_retained()
    }

    pub(crate) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        self.registry
            .release(&self.key, self.generation, self.request_bytes);
        self.released = true;
    }
}

impl Drop for RequestOperation {
    fn drop(&mut self) {
        if !self.terminal.is_claimed() {
            tracing::error!(
                request_id = self.key.request_id,
                connection_id = %self.key.connection_id,
                operation = %self.metadata.operation,
                "ERR_AGENTOS_REQUEST_OPERATION_DROPPED: admitted operation released before claiming its terminal response"
            );
        }
        self.release_inner();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CancelOperationResult {
    Signalled,
    AlreadySignalled(OperationCancellationReason),
    AlreadyTerminal,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RequestAdmissionError {
    CountLimit {
        current: usize,
        requested: usize,
        limit: usize,
    },
    ByteLimit {
        current: usize,
        requested: usize,
        limit: usize,
    },
    DuplicateRequest {
        key: RequestOperationKey,
    },
    RegistryClosed {
        reason: OperationCancellationReason,
    },
    ConnectionClosed {
        connection_id: String,
        reason: OperationCancellationReason,
    },
    OwnershipMismatch {
        key_connection_id: String,
        ownership_connection_id: String,
    },
}

impl RequestAdmissionError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::CountLimit { .. } => "ERR_AGENTOS_IN_FLIGHT_REQUEST_LIMIT",
            Self::ByteLimit { .. } => "ERR_AGENTOS_IN_FLIGHT_REQUEST_BYTE_LIMIT",
            Self::DuplicateRequest { .. } => "ERR_AGENTOS_DUPLICATE_REQUEST_ID",
            Self::RegistryClosed { .. } | Self::ConnectionClosed { .. } => {
                "ERR_AGENTOS_REQUEST_ADMISSION_CLOSED"
            }
            Self::OwnershipMismatch { .. } => "ERR_AGENTOS_REQUEST_OWNERSHIP_MISMATCH",
        }
    }

    pub(crate) fn configuration_path(&self) -> Option<&'static str> {
        match self {
            Self::CountLimit { .. } => Some(IN_FLIGHT_REQUEST_COUNT_PATH),
            Self::ByteLimit { .. } => Some(IN_FLIGHT_REQUEST_BYTES_PATH),
            _ => None,
        }
    }
}

impl fmt::Display for RequestAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountLimit {
                current,
                requested,
                limit,
            } => write!(
                formatter,
                "{}: in-flight requests used {current}, requested {requested}, limit {limit}; raise {IN_FLIGHT_REQUEST_COUNT_PATH}",
                self.code()
            ),
            Self::ByteLimit {
                current,
                requested,
                limit,
            } => write!(
                formatter,
                "{}: in-flight request bytes used {current}, requested {requested}, limit {limit}; raise {IN_FLIGHT_REQUEST_BYTES_PATH}",
                self.code()
            ),
            Self::DuplicateRequest { key } => write!(
                formatter,
                "{}: connection {} already has live request id {}",
                self.code(), key.connection_id, key.request_id
            ),
            Self::RegistryClosed { reason } => write!(
                formatter,
                "{}: request admission is closed ({reason:?})",
                self.code()
            ),
            Self::ConnectionClosed {
                connection_id,
                reason,
            } => write!(
                formatter,
                "{}: connection {connection_id} is closed ({reason:?})",
                self.code()
            ),
            Self::OwnershipMismatch {
                key_connection_id,
                ownership_connection_id,
            } => write!(
                formatter,
                "{}: registry key connection {key_connection_id} does not match ownership connection {ownership_connection_id}",
                self.code()
            ),
        }
    }
}

impl std::error::Error for RequestAdmissionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OperationTransitionError {
    NotFound(RequestOperationKey),
    StaleGeneration(RequestOperationKey),
    Invalid {
        key: RequestOperationKey,
        current: RequestOperationState,
        next: RequestOperationState,
    },
}

impl fmt::Display for OperationTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(key) => write!(
                formatter,
                "request operation {}:{} is not registered",
                key.connection_id, key.request_id
            ),
            Self::StaleGeneration(key) => write!(
                formatter,
                "request operation {}:{} has a stale generation",
                key.connection_id, key.request_id
            ),
            Self::Invalid { key, current, next } => write!(
                formatter,
                "invalid request operation transition for {}:{}: {current:?} -> {next:?}",
                key.connection_id, key.request_id
            ),
        }
    }
}

impl std::error::Error for OperationTransitionError {}

pub(crate) const PROGRESS_REQUEST_COUNT_PATH: &str = "runtime.protocol.maxProgressFrames";
pub(crate) const PROGRESS_REQUEST_BYTES_PATH: &str = "runtime.protocol.maxProgressBytes";

/// Direct-progress admission is intentionally separate from ordinary request
/// admission. Its bounds reuse the already-reserved progress output contract:
/// every live progress request owns one future acknowledgement frame and its
/// retained request bytes must fit the progress byte ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgressRequestLimits {
    pub(crate) max_requests: usize,
    pub(crate) max_request_bytes: usize,
}

impl From<&RuntimeProtocolConfig> for ProgressRequestLimits {
    fn from(config: &RuntimeProtocolConfig) -> Self {
        Self {
            max_requests: config.max_progress_frames,
            max_request_bytes: config.max_progress_bytes,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProgressAcknowledgementGuard {
    state: Arc<AtomicU8>,
}

impl ProgressAcknowledgementGuard {
    fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(PUBLICATION_UNCLAIMED)),
        }
    }

    fn try_claim(&self) -> bool {
        self.state
            .compare_exchange(
                PUBLICATION_UNCLAIMED,
                PUBLICATION_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_retained(&self) -> bool {
        self.state
            .compare_exchange(
                PUBLICATION_PUBLISHING,
                PUBLICATION_RETAINED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn try_take_over_unretained(&self) -> bool {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if matches!(current, PUBLICATION_RETAINED | PUBLICATION_TAKEN_OVER) {
                return false;
            }
            if self
                .state
                .compare_exchange(
                    current,
                    PUBLICATION_TAKEN_OVER,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    fn is_claimed(&self) -> bool {
        self.state.load(Ordering::Acquire) != PUBLICATION_UNCLAIMED
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgressRequestSnapshot {
    pub(crate) key: RequestOperationKey,
    pub(crate) ownership: OwnershipScope,
    pub(crate) request_bytes: usize,
    pub(crate) cancellation_reason: Option<OperationCancellationReason>,
    pub(crate) acknowledgement_claimed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgressRequestRegistrySnapshot {
    pub(crate) in_flight_requests: usize,
    pub(crate) in_flight_request_bytes: usize,
    pub(crate) closed: Option<OperationCancellationReason>,
    pub(crate) closed_connections: BTreeSet<String>,
    pub(crate) requests: Vec<ProgressRequestSnapshot>,
}

#[derive(Debug)]
struct ProgressRequestRecord {
    generation: u64,
    ownership: OwnershipScope,
    request_bytes: usize,
    cancellation: OperationCancellation,
    acknowledgement: ProgressAcknowledgementGuard,
}

#[derive(Debug)]
struct ProgressRegistryState {
    requests: BTreeMap<RequestOperationKey, ProgressRequestRecord>,
    in_flight_request_bytes: usize,
    next_generation: u64,
    closed: Option<OperationCancellationReason>,
    closed_connections: BTreeMap<String, OperationCancellationReason>,
}

#[derive(Debug)]
struct ProgressRequestRegistryInner {
    limits: ProgressRequestLimits,
    state: Mutex<ProgressRegistryState>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProgressRequestRegistry {
    inner: Arc<ProgressRequestRegistryInner>,
}

impl ProgressRequestRegistry {
    pub(crate) fn new(limits: ProgressRequestLimits) -> Self {
        assert!(
            limits.max_requests > 0,
            "progress request count limit must be positive"
        );
        assert!(
            limits.max_request_bytes > 0,
            "progress request byte limit must be positive"
        );
        Self {
            inner: Arc::new(ProgressRequestRegistryInner {
                limits,
                state: Mutex::new(ProgressRegistryState {
                    requests: BTreeMap::new(),
                    in_flight_request_bytes: 0,
                    next_generation: 1,
                    closed: None,
                    closed_connections: BTreeMap::new(),
                }),
            }),
        }
    }

    pub(crate) fn from_protocol_config(config: &RuntimeProtocolConfig) -> Self {
        Self::new(ProgressRequestLimits::from(config))
    }

    /// Preflight before acquiring progress-output capacity. [`Self::admit`]
    /// repeats the check and remains authoritative.
    pub(crate) fn check_admission(
        &self,
        key: &RequestOperationKey,
        request_bytes: usize,
    ) -> Result<(), ProgressRequestAdmissionError> {
        let state = self.lock_state();
        self.check_admission_locked(&state, key, request_bytes)
    }

    pub(crate) fn admit(
        &self,
        key: RequestOperationKey,
        request_bytes: usize,
    ) -> Result<ProgressRequest, ProgressRequestAdmissionError> {
        let ownership = OwnershipScope::ConnectionOwnership(crate::wire::ConnectionOwnership {
            connection_id: key.connection_id.clone(),
        });
        self.admit_owned(key, ownership, request_bytes)
    }

    pub(crate) fn admit_owned(
        &self,
        key: RequestOperationKey,
        ownership: OwnershipScope,
        request_bytes: usize,
    ) -> Result<ProgressRequest, ProgressRequestAdmissionError> {
        debug_assert_eq!(
            ownership_connection_id(&ownership),
            key.connection_id.as_str()
        );
        let mut state = self.lock_state();
        self.check_admission_locked(&state, &key, request_bytes)?;
        let requested_total = state
            .in_flight_request_bytes
            .checked_add(request_bytes)
            .expect("progress admission preflight checked the request byte total");
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let cancellation = OperationCancellation::new();
        let acknowledgement = ProgressAcknowledgementGuard::new();
        state.in_flight_request_bytes = requested_total;
        state.requests.insert(
            key.clone(),
            ProgressRequestRecord {
                generation,
                ownership,
                request_bytes,
                cancellation: cancellation.clone(),
                acknowledgement: acknowledgement.clone(),
            },
        );
        drop(state);

        Ok(ProgressRequest {
            registry: self.clone(),
            key,
            generation,
            request_bytes,
            cancellation,
            acknowledgement,
            released: false,
        })
    }

    pub(crate) fn cancel(
        &self,
        key: &RequestOperationKey,
        reason: OperationCancellationReason,
    ) -> ProgressCancelResult {
        let state = self.lock_state();
        let Some(record) = state.requests.get(key) else {
            return ProgressCancelResult::NotFound;
        };
        if record.cancellation.signal(reason) {
            ProgressCancelResult::Signalled
        } else {
            ProgressCancelResult::AlreadySignalled(
                record
                    .cancellation
                    .reason()
                    .expect("signalled progress cancellation has a reason"),
            )
        }
    }

    pub(crate) fn close_connection(
        &self,
        connection_id: &str,
        reason: OperationCancellationReason,
    ) -> usize {
        let mut state = self.lock_state();
        state
            .closed_connections
            .entry(connection_id.to_owned())
            .or_insert(reason);
        state
            .requests
            .iter()
            .filter(|(key, _)| key.connection_id == connection_id)
            .filter(|(_, record)| record.cancellation.signal(reason))
            .count()
    }

    pub(crate) fn close(&self, reason: OperationCancellationReason) -> usize {
        let mut state = self.lock_state();
        state.closed.get_or_insert(reason);
        state
            .requests
            .values()
            .filter(|record| record.cancellation.signal(reason))
            .count()
    }

    /// Signal progress work that was already active when ordinary shutdown
    /// began without closing the reserved progress admission lane. New direct
    /// progress messages remain routable during cooperative drain.
    pub(crate) fn signal_all(&self, reason: OperationCancellationReason) -> usize {
        let state = self.lock_state();
        state
            .requests
            .values()
            .filter(|record| record.cancellation.signal(reason))
            .count()
    }

    /// Exactly-once counterpart to [`RequestOperationRegistry::force_terminalize`]
    /// for direct progress requests. Returned descriptors must receive a
    /// synthetic acknowledgement after task-local reservations are released.
    pub(crate) fn force_acknowledge(
        &self,
        reason: OperationCancellationReason,
    ) -> Vec<ForcedRequestOutcome> {
        let mut state = self.lock_state();
        state.closed.get_or_insert(reason);
        let requests = std::mem::take(&mut state.requests);
        state.in_flight_request_bytes = 0;
        requests
            .into_iter()
            .filter_map(|(key, record)| {
                record.cancellation.signal(reason);
                // A claimed acknowledgement may remain registered while its
                // finite ordinary event batch drains. Remove accounting but
                // never synthesize a duplicate acknowledgement.
                if !record.acknowledgement.try_take_over_unretained() {
                    return None;
                }
                Some(ForcedRequestOutcome {
                    key,
                    ownership: record.ownership,
                })
            })
            .collect()
    }

    pub(crate) fn snapshot(&self) -> ProgressRequestRegistrySnapshot {
        let state = self.lock_state();
        ProgressRequestRegistrySnapshot {
            in_flight_requests: state.requests.len(),
            in_flight_request_bytes: state.in_flight_request_bytes,
            closed: state.closed,
            closed_connections: state.closed_connections.keys().cloned().collect(),
            requests: state
                .requests
                .iter()
                .map(|(key, record)| ProgressRequestSnapshot {
                    key: key.clone(),
                    ownership: record.ownership.clone(),
                    request_bytes: record.request_bytes,
                    cancellation_reason: record.cancellation.reason(),
                    acknowledgement_claimed: record.acknowledgement.is_claimed(),
                })
                .collect(),
        }
    }

    fn check_admission_locked(
        &self,
        state: &ProgressRegistryState,
        key: &RequestOperationKey,
        request_bytes: usize,
    ) -> Result<(), ProgressRequestAdmissionError> {
        if let Some(reason) = state.closed {
            return Err(ProgressRequestAdmissionError::RegistryClosed { reason });
        }
        if let Some(reason) = state.closed_connections.get(&key.connection_id).copied() {
            return Err(ProgressRequestAdmissionError::ConnectionClosed {
                connection_id: key.connection_id.clone(),
                reason,
            });
        }
        if state.requests.contains_key(key) {
            return Err(ProgressRequestAdmissionError::DuplicateRequest { key: key.clone() });
        }
        if state.requests.len() >= self.inner.limits.max_requests {
            return Err(ProgressRequestAdmissionError::CountLimit {
                current: state.requests.len(),
                requested: 1,
                limit: self.inner.limits.max_requests,
            });
        }
        let requested_total = state
            .in_flight_request_bytes
            .checked_add(request_bytes)
            .ok_or(ProgressRequestAdmissionError::ByteLimit {
                current: state.in_flight_request_bytes,
                requested: request_bytes,
                limit: self.inner.limits.max_request_bytes,
            })?;
        if requested_total > self.inner.limits.max_request_bytes {
            return Err(ProgressRequestAdmissionError::ByteLimit {
                current: state.in_flight_request_bytes,
                requested: request_bytes,
                limit: self.inner.limits.max_request_bytes,
            });
        }
        Ok(())
    }

    fn release(&self, key: &RequestOperationKey, generation: u64, request_bytes: usize) {
        let mut state = self.lock_state();
        let should_remove = state
            .requests
            .get(key)
            .is_some_and(|record| record.generation == generation);
        if should_remove {
            state.requests.remove(key);
            state.in_flight_request_bytes = state
                .in_flight_request_bytes
                .checked_sub(request_bytes)
                .unwrap_or_else(|| {
                    tracing::error!(
                        request_id = key.request_id,
                        connection_id = %key.connection_id,
                        request_bytes,
                        retained_bytes = state.in_flight_request_bytes,
                        "ERR_AGENTOS_PROGRESS_ADMISSION_ACCOUNTING: progress request byte reservation underflow"
                    );
                    0
                });
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, ProgressRegistryState> {
        self.inner.state.lock().unwrap_or_else(|poisoned| {
            tracing::error!(
                "ERR_AGENTOS_PROGRESS_REGISTRY_POISONED: recovering progress-request registry state"
            );
            poisoned.into_inner()
        })
    }
}

#[derive(Debug)]
pub(crate) struct ProgressRequest {
    registry: ProgressRequestRegistry,
    key: RequestOperationKey,
    generation: u64,
    request_bytes: usize,
    cancellation: OperationCancellation,
    acknowledgement: ProgressAcknowledgementGuard,
    released: bool,
}

impl ProgressRequest {
    pub(crate) fn key(&self) -> &RequestOperationKey {
        &self.key
    }

    pub(crate) fn request_bytes(&self) -> usize {
        self.request_bytes
    }

    pub(crate) fn cancellation(&self) -> OperationCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn try_acknowledge(&self) -> bool {
        self.acknowledgement.try_claim()
    }

    pub(crate) fn mark_acknowledgement_retained(&self) -> bool {
        self.acknowledgement.mark_retained()
    }

    pub(crate) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        self.registry
            .release(&self.key, self.generation, self.request_bytes);
        self.released = true;
    }
}

impl Drop for ProgressRequest {
    fn drop(&mut self) {
        if !self.acknowledgement.is_claimed() {
            tracing::error!(
                request_id = self.key.request_id,
                connection_id = %self.key.connection_id,
                "ERR_AGENTOS_PROGRESS_REQUEST_DROPPED: admitted progress request released before claiming its acknowledgement"
            );
        }
        self.release_inner();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgressCancelResult {
    Signalled,
    AlreadySignalled(OperationCancellationReason),
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgressRequestAdmissionError {
    CountLimit {
        current: usize,
        requested: usize,
        limit: usize,
    },
    ByteLimit {
        current: usize,
        requested: usize,
        limit: usize,
    },
    DuplicateRequest {
        key: RequestOperationKey,
    },
    RegistryClosed {
        reason: OperationCancellationReason,
    },
    ConnectionClosed {
        connection_id: String,
        reason: OperationCancellationReason,
    },
}

impl ProgressRequestAdmissionError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::CountLimit { .. } => "ERR_AGENTOS_PROGRESS_REQUEST_LIMIT",
            Self::ByteLimit { .. } => "ERR_AGENTOS_PROGRESS_REQUEST_BYTE_LIMIT",
            Self::DuplicateRequest { .. } => "ERR_AGENTOS_DUPLICATE_PROGRESS_REQUEST_ID",
            Self::RegistryClosed { .. } | Self::ConnectionClosed { .. } => {
                "ERR_AGENTOS_PROGRESS_ADMISSION_CLOSED"
            }
        }
    }

    pub(crate) fn configuration_path(&self) -> Option<&'static str> {
        match self {
            Self::CountLimit { .. } => Some(PROGRESS_REQUEST_COUNT_PATH),
            Self::ByteLimit { .. } => Some(PROGRESS_REQUEST_BYTES_PATH),
            _ => None,
        }
    }
}

impl fmt::Display for ProgressRequestAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountLimit {
                current,
                requested,
                limit,
            } => write!(
                formatter,
                "{}: in-flight progress requests used {current}, requested {requested}, limit {limit}; raise {PROGRESS_REQUEST_COUNT_PATH}",
                self.code()
            ),
            Self::ByteLimit {
                current,
                requested,
                limit,
            } => write!(
                formatter,
                "{}: in-flight progress request bytes used {current}, requested {requested}, limit {limit}; raise {PROGRESS_REQUEST_BYTES_PATH}",
                self.code()
            ),
            Self::DuplicateRequest { key } => write!(
                formatter,
                "{}: connection {} already has live progress request id {}",
                self.code(), key.connection_id, key.request_id
            ),
            Self::RegistryClosed { reason } => write!(
                formatter,
                "{}: progress request admission is closed ({reason:?})",
                self.code()
            ),
            Self::ConnectionClosed {
                connection_id,
                reason,
            } => write!(
                formatter,
                "{}: progress request connection {connection_id} is closed ({reason:?})",
                self.code()
            ),
        }
    }
}

impl std::error::Error for ProgressRequestAdmissionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ConnectionOwnership, OwnershipScope};
    use std::time::Duration;

    fn ownership(connection_id: &str) -> OwnershipScope {
        OwnershipScope::ConnectionOwnership(ConnectionOwnership {
            connection_id: connection_id.to_owned(),
        })
    }

    fn metadata(connection_id: &str) -> RequestOperationMetadata {
        RequestOperationMetadata::new(
            ownership(connection_id),
            "test",
            RequestOrderingKey::Unordered,
        )
    }

    fn registry(max_requests: usize, max_request_bytes: usize) -> RequestOperationRegistry {
        RequestOperationRegistry::new(RequestOperationLimits {
            max_requests,
            max_request_bytes,
        })
    }

    fn finish(operation: RequestOperation) {
        operation
            .transition(RequestOperationState::Running)
            .expect("mark running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("mark completing");
        assert!(operation.try_mark_terminal().expect("mark terminal"));
        operation.release();
    }

    #[test]
    fn count_admission_is_bounded_and_released() {
        let registry = registry(1, 100);
        let first = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("first admission");
        let error = registry
            .admit(RequestOperationKey::new("a", 2), metadata("a"), 10)
            .expect_err("second admission exceeds count");
        assert_eq!(error.code(), "ERR_AGENTOS_IN_FLIGHT_REQUEST_LIMIT");
        assert_eq!(
            error.configuration_path(),
            Some(IN_FLIGHT_REQUEST_COUNT_PATH)
        );
        assert!(error
            .to_string()
            .contains("raise runtime.protocol.maxInFlightRequests"));
        finish(first);
        let second = registry
            .admit(RequestOperationKey::new("a", 2), metadata("a"), 10)
            .expect("released count is reusable");
        finish(second);
        assert_eq!(registry.snapshot().in_flight_requests, 0);
    }

    #[test]
    fn byte_admission_is_bounded_and_released() {
        let registry = registry(4, 10);
        let first = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 7)
            .expect("first admission");
        let error = registry
            .admit(RequestOperationKey::new("a", 2), metadata("a"), 4)
            .expect_err("aggregate bytes exceed limit");
        assert_eq!(error.code(), "ERR_AGENTOS_IN_FLIGHT_REQUEST_BYTE_LIMIT");
        assert_eq!(
            error.configuration_path(),
            Some(IN_FLIGHT_REQUEST_BYTES_PATH)
        );
        assert!(error
            .to_string()
            .contains("raise runtime.protocol.maxInFlightRequestBytes"));
        finish(first);
        let second = registry
            .admit(RequestOperationKey::new("a", 2), metadata("a"), 10)
            .expect("released bytes are reusable");
        finish(second);
        assert_eq!(registry.snapshot().in_flight_request_bytes, 0);
    }

    #[test]
    fn duplicate_ids_conflict_only_within_one_connection() {
        let registry = registry(4, 100);
        let a = registry
            .admit(RequestOperationKey::new("a", 7), metadata("a"), 10)
            .expect("connection a request");
        let error = registry
            .admit(RequestOperationKey::new("a", 7), metadata("a"), 10)
            .expect_err("duplicate live id on same connection");
        assert_eq!(error.code(), "ERR_AGENTOS_DUPLICATE_REQUEST_ID");

        let b = registry
            .admit(RequestOperationKey::new("b", 7), metadata("b"), 10)
            .expect("same numeric id on another connection is valid");
        finish(a);
        finish(b);
    }

    #[tokio::test]
    async fn explicit_cancel_notifies_the_matching_operation_once() {
        let registry = registry(2, 100);
        let operation = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("admit request");
        let cancellation = operation.cancellation();
        assert_eq!(
            registry.cancel(operation.key(), OperationCancellationReason::Explicit),
            CancelOperationResult::Signalled
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
                .await
                .expect("cancel notification"),
            OperationCancellationReason::Explicit
        );
        assert_eq!(
            registry.cancel(operation.key(), OperationCancellationReason::Shutdown),
            CancelOperationResult::AlreadySignalled(OperationCancellationReason::Explicit)
        );
        operation
            .transition(RequestOperationState::Completing)
            .expect("cancel completion");
        assert!(operation.try_mark_terminal().expect("cancel terminal"));
        operation.release();
    }

    #[tokio::test]
    async fn connection_and_registry_close_signal_owned_work_and_stop_admission() {
        let registry = registry(4, 100);
        let a = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("connection a");
        let b = registry
            .admit(RequestOperationKey::new("b", 1), metadata("b"), 10)
            .expect("connection b");
        let a_cancel = a.cancellation();
        let b_cancel = b.cancellation();

        assert_eq!(
            registry.close_connection("a", OperationCancellationReason::ConnectionClosed),
            1
        );
        assert_eq!(
            a_cancel.cancelled().await,
            OperationCancellationReason::ConnectionClosed
        );
        let error = registry
            .admit(RequestOperationKey::new("a", 2), metadata("a"), 1)
            .expect_err("closed connection rejects admission");
        assert!(matches!(
            error,
            RequestAdmissionError::ConnectionClosed { .. }
        ));

        assert_eq!(registry.close(OperationCancellationReason::Shutdown), 1);
        assert_eq!(
            b_cancel.cancelled().await,
            OperationCancellationReason::Shutdown
        );
        let error = registry
            .admit(RequestOperationKey::new("b", 2), metadata("b"), 1)
            .expect_err("closed registry rejects admission");
        assert!(matches!(
            error,
            RequestAdmissionError::RegistryClosed { .. }
        ));

        assert!(a.try_mark_terminal().expect("connection close terminal"));
        assert!(b.try_mark_terminal().expect("shutdown terminal"));
        a.release();
        b.release();
    }

    #[tokio::test]
    async fn forced_terminalization_takes_over_claimed_unretained_outcome() {
        let registry = registry(2, 100);
        let operation = registry
            .admit(
                RequestOperationKey::new("a", 9),
                RequestOperationMetadata::new(
                    OwnershipScope::SessionOwnership(crate::wire::SessionOwnership {
                        connection_id: String::from("a"),
                        session_id: String::from("session-a"),
                    }),
                    "gated",
                    RequestOrderingKey::Unordered,
                ),
                17,
            )
            .expect("admit gated request");
        operation
            .transition(RequestOperationState::Running)
            .expect("mark running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("begin completion before output publication");
        assert!(operation
            .try_mark_terminal()
            .expect("claim normal terminal publication"));
        let cancellation = operation.cancellation();

        let forced = registry.force_terminalize(OperationCancellationReason::Shutdown);
        assert_eq!(forced.len(), 1);
        assert_eq!(forced[0].key, RequestOperationKey::new("a", 9));
        assert!(matches!(
            &forced[0].ownership,
            OwnershipScope::SessionOwnership(scope) if scope.session_id == "session-a"
        ));
        assert_eq!(
            cancellation.cancelled().await,
            OperationCancellationReason::Shutdown
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.in_flight_requests, 0);
        assert_eq!(snapshot.in_flight_request_bytes, 0);
        assert!(registry
            .force_terminalize(OperationCancellationReason::Shutdown)
            .is_empty());

        drop(operation);
    }

    #[test]
    fn forced_terminalization_skips_a_terminal_already_retained_by_output() {
        let registry = registry(1, 100);
        let operation = registry
            .admit(RequestOperationKey::new("a", 10), metadata("a"), 11)
            .expect("admit retained request");
        operation
            .transition(RequestOperationState::Running)
            .expect("mark running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("begin completion");
        assert!(operation.try_mark_terminal().expect("claim terminal"));
        assert!(operation.mark_terminal_retained());

        assert!(registry
            .force_terminalize(OperationCancellationReason::Shutdown)
            .is_empty());
        assert_eq!(registry.snapshot().in_flight_requests, 0);
        drop(operation);
    }

    #[test]
    fn terminal_response_guard_is_exactly_once_across_racing_clones() {
        let guard = TerminalResponseGuard::new();
        let clones = (0..16).map(|_| guard.clone()).collect::<Vec<_>>();
        let winners = clones
            .into_iter()
            .map(|candidate| std::thread::spawn(move || candidate.try_claim()))
            .map(|thread| thread.join().expect("terminal race thread"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(guard.is_claimed());
    }

    #[test]
    fn operation_preserves_metadata_and_state_transitions() {
        let registry = registry(1, 100);
        let operation = registry
            .admit(
                RequestOperationKey::new("a", 1),
                RequestOperationMetadata::new(
                    ownership("a"),
                    "configure_vm",
                    RequestOrderingKey::VmLifecycle {
                        connection_id: String::from("a"),
                        session_id: String::from("session"),
                        vm_id: String::from("vm"),
                    },
                ),
                17,
            )
            .expect("admit operation");
        assert_eq!(operation.request_bytes(), 17);
        assert_eq!(operation.metadata().operation, "configure_vm");
        operation
            .transition(RequestOperationState::Running)
            .expect("running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("completing");
        assert!(operation.try_mark_terminal().expect("terminal"));
        assert!(!operation.try_mark_terminal().expect("duplicate terminal"));
        assert_eq!(
            registry.snapshot().operations[0].state,
            RequestOperationState::Terminal
        );
        operation.release();
    }

    #[test]
    fn invalid_terminal_transition_does_not_consume_the_terminal_claim() {
        let registry = registry(1, 100);
        let operation = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("admit operation");

        let error = operation
            .try_mark_terminal()
            .expect_err("admitted work must not skip completion");
        assert!(matches!(error, OperationTransitionError::Invalid { .. }));
        assert!(!registry.snapshot().operations[0].terminal_claimed);

        operation
            .transition(RequestOperationState::Running)
            .expect("running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("completing");
        assert!(operation.try_mark_terminal().expect("terminal"));
        operation.release();
    }

    #[test]
    fn late_cancellation_does_not_regress_a_terminal_operation() {
        let registry = registry(1, 100);
        let operation = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("admit operation");
        operation
            .transition(RequestOperationState::Running)
            .expect("running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("completing");
        assert!(operation.try_mark_terminal().expect("terminal"));

        assert_eq!(
            registry.cancel(operation.key(), OperationCancellationReason::Shutdown),
            CancelOperationResult::AlreadyTerminal
        );
        assert_eq!(registry.close(OperationCancellationReason::Shutdown), 0);
        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot.operations[0].state,
            RequestOperationState::Terminal
        );
        assert_eq!(snapshot.operations[0].cancellation_reason, None);
        operation.release();
    }

    #[test]
    fn cancellation_during_completion_signals_without_regressing_state() {
        let registry = registry(1, 100);
        let operation = registry
            .admit(RequestOperationKey::new("a", 1), metadata("a"), 10)
            .expect("admit operation");
        operation
            .transition(RequestOperationState::Running)
            .expect("running");
        operation
            .transition(RequestOperationState::Completing)
            .expect("completing");

        assert_eq!(
            registry.cancel(operation.key(), OperationCancellationReason::Explicit),
            CancelOperationResult::Signalled
        );
        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot.operations[0].state,
            RequestOperationState::Completing
        );
        assert_eq!(
            snapshot.operations[0].cancellation_reason,
            Some(OperationCancellationReason::Explicit)
        );
        assert!(operation.try_mark_terminal().expect("terminal"));
        operation.release();
    }

    fn progress_registry(max_requests: usize, max_request_bytes: usize) -> ProgressRequestRegistry {
        ProgressRequestRegistry::new(ProgressRequestLimits {
            max_requests,
            max_request_bytes,
        })
    }

    #[test]
    fn progress_admission_uses_reserved_lane_limits_not_ordinary_limits() {
        let mut protocol = RuntimeProtocolConfig::default();
        protocol.max_in_flight_requests = 1;
        protocol.max_in_flight_request_bytes = 1;
        protocol.max_progress_frames = 2;
        protocol.max_progress_bytes = 10;
        let ordinary_registry = RequestOperationRegistry::from_protocol_config(&protocol);
        let registry = ProgressRequestRegistry::from_protocol_config(&protocol);
        let first_key = RequestOperationKey::new("a", 1);
        registry
            .check_admission(&first_key, 5)
            .expect("progress preflight ignores ordinary request limits");
        assert_eq!(registry.snapshot().in_flight_requests, 0);
        let first = registry
            .admit(first_key, 5)
            .expect("first progress request");
        let second = registry
            .admit(RequestOperationKey::new("a", 2), 5)
            .expect("second progress request");
        let error = registry
            .admit(RequestOperationKey::new("a", 3), 0)
            .expect_err("progress frame contract bounds live requests");
        assert_eq!(error.code(), "ERR_AGENTOS_PROGRESS_REQUEST_LIMIT");
        assert_eq!(
            error.configuration_path(),
            Some(PROGRESS_REQUEST_COUNT_PATH)
        );
        assert!(error
            .to_string()
            .contains("raise runtime.protocol.maxProgressFrames"));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.in_flight_requests, 2);
        assert_eq!(snapshot.in_flight_request_bytes, 10);
        assert_eq!(ordinary_registry.snapshot().in_flight_requests, 0);
        assert_eq!(ordinary_registry.snapshot().in_flight_request_bytes, 0);
        assert!(first.try_acknowledge());
        assert!(second.try_acknowledge());
        first.release();
        second.release();
    }

    #[test]
    fn progress_byte_admission_releases_via_raii() {
        let registry = progress_registry(4, 5);
        let first = registry
            .admit(RequestOperationKey::new("a", 1), 4)
            .expect("first progress request");
        let error = registry
            .admit(RequestOperationKey::new("a", 2), 2)
            .expect_err("progress byte contract is bounded");
        assert_eq!(error.code(), "ERR_AGENTOS_PROGRESS_REQUEST_BYTE_LIMIT");
        assert_eq!(
            error.configuration_path(),
            Some(PROGRESS_REQUEST_BYTES_PATH)
        );
        assert!(error
            .to_string()
            .contains("raise runtime.protocol.maxProgressBytes"));
        assert!(first.try_acknowledge());
        drop(first);
        assert_eq!(registry.snapshot().in_flight_request_bytes, 0);

        let replacement = registry
            .admit(RequestOperationKey::new("a", 2), 5)
            .expect("RAII release restores byte capacity");
        assert_eq!(replacement.request_bytes(), 5);
        assert!(replacement.try_acknowledge());
        replacement.release();
    }

    #[test]
    fn progress_duplicates_are_connection_scoped_and_reusable_after_release() {
        let registry = progress_registry(4, 100);
        let key = RequestOperationKey::new("a", 7);
        let first = registry
            .admit(key.clone(), 10)
            .expect("connection a progress request");
        let error = registry
            .admit(key.clone(), 10)
            .expect_err("duplicate live progress id");
        assert_eq!(error.code(), "ERR_AGENTOS_DUPLICATE_PROGRESS_REQUEST_ID");
        let other_connection = registry
            .admit(RequestOperationKey::new("b", 7), 10)
            .expect("same progress id on another connection");
        assert!(first.try_acknowledge());
        assert!(other_connection.try_acknowledge());
        first.release();
        other_connection.release();

        let reused = registry
            .admit(key, 10)
            .expect("completed progress id is reusable");
        assert!(reused.try_acknowledge());
        reused.release();
    }

    #[tokio::test]
    async fn progress_cancel_and_close_signal_owned_requests_and_stop_admission() {
        let registry = progress_registry(4, 100);
        let explicit = registry
            .admit(RequestOperationKey::new("a", 1), 10)
            .expect("explicit cancellation target");
        let connection = registry
            .admit(RequestOperationKey::new("b", 1), 10)
            .expect("connection close target");
        let shutdown = registry
            .admit(RequestOperationKey::new("c", 1), 10)
            .expect("registry close target");
        let explicit_cancel = explicit.cancellation();
        let connection_cancel = connection.cancellation();
        let shutdown_cancel = shutdown.cancellation();

        assert_eq!(
            registry.cancel(explicit.key(), OperationCancellationReason::Explicit),
            ProgressCancelResult::Signalled
        );
        assert_eq!(
            explicit_cancel.cancelled().await,
            OperationCancellationReason::Explicit
        );
        assert_eq!(
            registry.close_connection("b", OperationCancellationReason::ConnectionClosed),
            1
        );
        assert_eq!(
            connection_cancel.cancelled().await,
            OperationCancellationReason::ConnectionClosed
        );
        assert!(matches!(
            registry
                .admit(RequestOperationKey::new("b", 2), 1)
                .expect_err("closed progress connection rejects admission"),
            ProgressRequestAdmissionError::ConnectionClosed { .. }
        ));
        assert_eq!(registry.close(OperationCancellationReason::Shutdown), 1);
        assert_eq!(
            shutdown_cancel.cancelled().await,
            OperationCancellationReason::Shutdown
        );
        assert!(matches!(
            registry
                .admit(RequestOperationKey::new("d", 1), 1)
                .expect_err("closed progress registry rejects admission"),
            ProgressRequestAdmissionError::RegistryClosed { .. }
        ));

        for request in [explicit, connection, shutdown] {
            assert!(request.try_acknowledge());
            request.release();
        }
        assert_eq!(registry.snapshot().in_flight_requests, 0);
    }

    #[tokio::test]
    async fn forced_progress_acknowledgement_takes_over_claimed_unretained_outcome() {
        let registry = progress_registry(2, 100);
        let ownership = OwnershipScope::VmOwnership(crate::wire::VmOwnership {
            connection_id: String::from("a"),
            session_id: String::from("session-a"),
            vm_id: String::from("vm-a"),
        });
        let request = registry
            .admit_owned(RequestOperationKey::new("a", 11), ownership.clone(), 19)
            .expect("admit progress request");
        assert!(request.try_acknowledge());
        let cancellation = request.cancellation();

        let forced = registry.force_acknowledge(OperationCancellationReason::TransportClosed);
        assert_eq!(forced.len(), 1);
        assert_eq!(forced[0].ownership, ownership);
        assert_eq!(
            cancellation.cancelled().await,
            OperationCancellationReason::TransportClosed
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.in_flight_requests, 0);
        assert_eq!(snapshot.in_flight_request_bytes, 0);
        assert!(registry
            .force_acknowledge(OperationCancellationReason::TransportClosed)
            .is_empty());

        drop(request);
    }

    #[test]
    fn forced_progress_acknowledgement_skips_an_ack_already_retained_by_output() {
        let registry = progress_registry(1, 100);
        let request = registry
            .admit(RequestOperationKey::new("a", 12), 7)
            .expect("admit retained progress request");
        assert!(request.try_acknowledge());
        assert!(request.mark_acknowledgement_retained());

        assert!(registry
            .force_acknowledge(OperationCancellationReason::Shutdown)
            .is_empty());
        assert_eq!(registry.snapshot().in_flight_requests, 0);
        drop(request);
    }

    #[test]
    fn progress_acknowledgement_is_exactly_once_across_racing_callers() {
        let registry = progress_registry(1, 100);
        let request = Arc::new(
            registry
                .admit(RequestOperationKey::new("a", 1), 10)
                .expect("progress request"),
        );
        let winners = (0..16)
            .map(|_| Arc::clone(&request))
            .map(|candidate| std::thread::spawn(move || candidate.try_acknowledge()))
            .map(|thread| thread.join().expect("progress acknowledgement race"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(registry.snapshot().requests[0].acknowledgement_claimed);
        Arc::try_unwrap(request)
            .expect("all acknowledgement racers completed")
            .release();
        assert_eq!(registry.snapshot().in_flight_requests, 0);
    }
}
