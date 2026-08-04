//! Cloneable connection/session/VM coordination handles.
//!
//! The coordinator owns only bounded identity indexes, short admission state,
//! and cancellation registrations. It never performs guest work, adapter I/O,
//! filesystem/network I/O, output writes, or an external wait while holding a
//! mutex. A request keeps the returned permit while it runs; every actual
//! access to mutable VM state remains a separate short service command.

use crate::extension::ExtensionOrderingPolicy;
use crate::request_operations::{
    OperationCancellation, OperationCancellationReason, RequestOperationMetadata,
    RequestOrderingKey, IN_FLIGHT_REQUEST_COUNT_PATH,
};
use crate::wire::OwnershipScope;
use agentos_runtime::RuntimeConfig;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::Notify;

const CONNECTION_LIMIT_PATH: &str = "runtime.resources.maxConnections";
const SESSION_LIMIT_PATH: &str = "runtime.protocol.maxInFlightRequests";
const VM_LIMIT_PATH: &str = "runtime.fairness.maxVms";
pub(crate) const INTERNAL_EVENT_OPERATION_LIMIT_PATH: &str = "runtime.protocol.maxProcessEvents";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnershipCoordinatorLimits {
    pub(crate) max_connections: usize,
    pub(crate) max_sessions_per_connection: usize,
    pub(crate) max_vms_per_session: usize,
    pub(crate) max_operations_per_entity: usize,
    /// Internal runtime events have independent admission so ordinary request
    /// saturation cannot prevent the response/progress work needed to complete
    /// those same requests. The process-event broker provides this bound.
    pub(crate) max_internal_event_operations_per_entity: usize,
}

impl OwnershipCoordinatorLimits {
    pub(crate) fn from_runtime_config(config: &RuntimeConfig) -> Self {
        Self {
            max_connections: config.resources.max_connections,
            // There is not a second request queue hidden in this coordinator.
            // Retained live session handles reuse the protocol's bounded
            // request-count ceiling until a dedicated session-membership limit
            // is introduced.
            max_sessions_per_connection: config.protocol.max_in_flight_requests,
            max_vms_per_session: config.fairness.max_vms,
            max_operations_per_entity: config.protocol.max_in_flight_requests,
            max_internal_event_operations_per_entity: config.protocol.max_process_events,
        }
    }

    fn validate(self) {
        assert!(
            self.max_connections > 0,
            "connection limit must be positive"
        );
        assert!(
            self.max_sessions_per_connection > 0,
            "session limit must be positive"
        );
        assert!(
            self.max_vms_per_session > 0,
            "VM membership limit must be positive"
        );
        assert!(
            self.max_operations_per_entity > 0,
            "entity operation limit must be positive"
        );
        assert!(
            self.max_internal_event_operations_per_entity > 0,
            "internal event operation limit must be positive"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoordinatorPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VmLifecyclePhase {
    Idle,
    Pending,
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntityCoordinatorSnapshot {
    pub(crate) phase: CoordinatorPhase,
    pub(crate) active_operations: usize,
    pub(crate) child_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VmCoordinatorSnapshot {
    pub(crate) phase: CoordinatorPhase,
    pub(crate) active_operations: usize,
    pub(crate) lifecycle: VmLifecyclePhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OwnershipCoordinatorError {
    Limit {
        scope: &'static str,
        current: usize,
        limit: usize,
        configuration_path: &'static str,
    },
    Duplicate {
        scope: &'static str,
        id: String,
    },
    NotFound {
        scope: &'static str,
        id: String,
    },
    OwnershipMismatch {
        expected: String,
        actual: String,
    },
    Closing {
        scope: String,
        phase: CoordinatorPhase,
    },
    LifecycleConflict {
        vm: String,
        lifecycle: VmLifecyclePhase,
    },
    OrderingConflict {
        scope: String,
    },
    Cancelled {
        reason: OperationCancellationReason,
    },
    NotDrained {
        scope: String,
        active_operations: usize,
    },
}

impl OwnershipCoordinatorError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Limit { .. } => "ERR_AGENTOS_COORDINATOR_LIMIT",
            Self::Duplicate { .. } => "ERR_AGENTOS_COORDINATOR_DUPLICATE",
            Self::NotFound { .. } => "ERR_AGENTOS_COORDINATOR_NOT_FOUND",
            Self::OwnershipMismatch { .. } => "ERR_AGENTOS_COORDINATOR_OWNERSHIP",
            Self::Closing { .. } => "ERR_AGENTOS_COORDINATOR_CLOSING",
            Self::LifecycleConflict { .. } => "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT",
            Self::OrderingConflict { .. } => "ERR_AGENTOS_ORDERING_CONFLICT",
            Self::Cancelled { .. } => "ERR_AGENTOS_COORDINATOR_CANCELLED",
            Self::NotDrained { .. } => "ERR_AGENTOS_COORDINATOR_NOT_DRAINED",
        }
    }
}

impl fmt::Display for OwnershipCoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                scope,
                current,
                limit,
                configuration_path,
            } => write!(
                formatter,
                "{}: {scope} used {current}, limit {limit}; raise {configuration_path}",
                self.code()
            ),
            Self::Duplicate { scope, id } => {
                write!(formatter, "{}: duplicate {scope} {id}", self.code())
            }
            Self::NotFound { scope, id } => {
                write!(formatter, "{}: unknown {scope} {id}", self.code())
            }
            Self::OwnershipMismatch { expected, actual } => write!(
                formatter,
                "{}: expected ownership {expected}, received {actual}",
                self.code()
            ),
            Self::Closing { scope, phase } => write!(
                formatter,
                "{}: {scope} is {phase:?}; new owned operations are rejected",
                self.code()
            ),
            Self::LifecycleConflict { vm, lifecycle } => write!(
                formatter,
                "{}: VM {vm} lifecycle is {lifecycle:?}",
                self.code()
            ),
            Self::OrderingConflict { scope } => write!(
                formatter,
                "{}: {scope} already has an active operation",
                self.code()
            ),
            Self::Cancelled { reason } => write!(
                formatter,
                "{}: coordinator admission was cancelled ({reason:?})",
                self.code()
            ),
            Self::NotDrained {
                scope,
                active_operations,
            } => write!(
                formatter,
                "{}: {scope} still owns {active_operations} active operations",
                self.code()
            ),
        }
    }
}

impl std::error::Error for OwnershipCoordinatorError {}

#[derive(Clone, Debug)]
pub(crate) struct OwnershipCoordinator {
    inner: Arc<OwnershipCoordinatorInner>,
}

#[derive(Debug)]
struct OwnershipCoordinatorInner {
    limits: OwnershipCoordinatorLimits,
    state: Mutex<OwnershipCoordinatorState>,
}

#[derive(Debug, Default)]
struct OwnershipCoordinatorState {
    connections: BTreeMap<String, ConnectionCoordinator>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConnectionCoordinator {
    root: Weak<OwnershipCoordinatorInner>,
    inner: Arc<ConnectionCoordinatorInner>,
}

#[derive(Debug)]
struct ConnectionCoordinatorInner {
    connection_id: String,
    limits: OwnershipCoordinatorLimits,
    state: Mutex<ConnectionCoordinatorState>,
    drained: Notify,
}

#[derive(Debug)]
struct ConnectionCoordinatorState {
    phase: CoordinatorPhase,
    sessions: BTreeMap<String, SessionCoordinator>,
    operations: BTreeMap<u64, RegisteredOperation>,
    extension_ordering: BTreeMap<(String, Vec<u8>), ()>,
    next_operation_id: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionCoordinator {
    parent: Weak<ConnectionCoordinatorInner>,
    inner: Arc<SessionCoordinatorInner>,
}

#[derive(Debug)]
struct SessionCoordinatorInner {
    connection_id: String,
    session_id: String,
    limits: OwnershipCoordinatorLimits,
    state: Mutex<SessionCoordinatorState>,
    drained: Notify,
}

#[derive(Debug)]
struct SessionCoordinatorState {
    phase: CoordinatorPhase,
    vms: BTreeMap<String, VmCoordinator>,
    operations: BTreeMap<u64, RegisteredOperation>,
    next_operation_id: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct VmCoordinator {
    parent: Weak<SessionCoordinatorInner>,
    inner: Arc<VmCoordinatorInner>,
}

#[derive(Debug)]
struct VmCoordinatorInner {
    connection_id: String,
    session_id: String,
    vm_id: String,
    limits: OwnershipCoordinatorLimits,
    state: Mutex<VmCoordinatorState>,
    changed: Notify,
}

#[derive(Debug)]
struct VmCoordinatorState {
    phase: CoordinatorPhase,
    operations: BTreeMap<u64, RegisteredOperation>,
    next_operation_id: u64,
    next_lifecycle_id: u64,
    lifecycle: VmLifecycleState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationAdmissionClass {
    Ordinary,
    InternalEvent,
    DeferredInternalEvent,
}

#[derive(Debug)]
struct RegisteredOperation {
    cancellation: OperationCancellation,
    class: OperationAdmissionClass,
}

#[derive(Debug)]
pub(crate) enum InternalVmEventAdmission {
    Admitted(CoordinatorOperationPermit),
    Deferred(CoordinatorOperationPermit),
}

#[derive(Debug)]
enum VmLifecycleState {
    Idle,
    Pending {
        id: u64,
        cancellation: OperationCancellation,
    },
    Active {
        id: u64,
        cancellation: OperationCancellation,
    },
}

impl VmLifecycleState {
    fn phase(&self) -> VmLifecyclePhase {
        match self {
            Self::Idle => VmLifecyclePhase::Idle,
            Self::Pending { .. } => VmLifecyclePhase::Pending,
            Self::Active { .. } => VmLifecyclePhase::Active,
        }
    }

    fn signal(&self, reason: OperationCancellationReason) {
        match self {
            Self::Pending { cancellation, .. } | Self::Active { cancellation, .. } => {
                cancellation.signal(reason);
            }
            Self::Idle => {}
        }
    }
}

impl OwnershipCoordinator {
    pub(crate) fn new(limits: OwnershipCoordinatorLimits) -> Self {
        limits.validate();
        Self {
            inner: Arc::new(OwnershipCoordinatorInner {
                limits,
                state: Mutex::new(OwnershipCoordinatorState::default()),
            }),
        }
    }

    pub(crate) fn from_runtime_config(config: &RuntimeConfig) -> Self {
        Self::new(OwnershipCoordinatorLimits::from_runtime_config(config))
    }

    pub(crate) fn register_connection(
        &self,
        connection_id: impl Into<String>,
    ) -> Result<ConnectionCoordinator, OwnershipCoordinatorError> {
        let connection_id = connection_id.into();
        let mut state = lock(&self.inner.state, "connection registry");
        if state.connections.contains_key(&connection_id) {
            return Err(OwnershipCoordinatorError::Duplicate {
                scope: "connection",
                id: connection_id,
            });
        }
        check_limit(
            "connection coordinators",
            state.connections.len(),
            self.inner.limits.max_connections,
            CONNECTION_LIMIT_PATH,
        )?;
        let connection = ConnectionCoordinator {
            root: Arc::downgrade(&self.inner),
            inner: Arc::new(ConnectionCoordinatorInner {
                connection_id: connection_id.clone(),
                limits: self.inner.limits,
                state: Mutex::new(ConnectionCoordinatorState {
                    phase: CoordinatorPhase::Open,
                    sessions: BTreeMap::new(),
                    operations: BTreeMap::new(),
                    extension_ordering: BTreeMap::new(),
                    next_operation_id: 1,
                }),
                drained: Notify::new(),
            }),
        };
        state.connections.insert(connection_id, connection.clone());
        Ok(connection)
    }

    pub(crate) fn connection(
        &self,
        connection_id: &str,
    ) -> Result<ConnectionCoordinator, OwnershipCoordinatorError> {
        lock(&self.inner.state, "connection registry")
            .connections
            .get(connection_id)
            .cloned()
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "connection",
                id: connection_id.to_owned(),
            })
    }

    pub(crate) async fn admit(
        &self,
        metadata: &RequestOperationMetadata,
        cancellation: OperationCancellation,
    ) -> Result<CoordinatorOperationPermit, OwnershipCoordinatorError> {
        validate_ordering_ownership(&metadata.ownership, &metadata.ordering_key)?;
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let resolved = self.resolve(&metadata.ownership)?;
        let connection = resolved
            .connection
            .register_operation(cancellation.clone())?;
        let session = match resolved.session.as_ref() {
            Some(session) => Some(session.register_operation(cancellation.clone())?),
            None => None,
        };

        let mut permit = CoordinatorOperationPermit {
            _connection: connection,
            _session: session,
            vm_operation: None,
            vm_lifecycle: None,
            _extension_ordering: None,
        };
        match &metadata.ordering_key {
            RequestOrderingKey::VmOperation { .. } => {
                permit.vm_operation = Some(
                    resolved
                        .vm
                        .as_ref()
                        .expect("VM ordering key requires VM ownership")
                        .register_operation(cancellation.clone())?,
                );
            }
            RequestOrderingKey::VmLifecycle { .. } => {
                let admission = resolved
                    .vm
                    .as_ref()
                    .expect("VM lifecycle key requires VM ownership")
                    .begin_lifecycle(cancellation.clone())?;
                permit.vm_lifecycle = Some(admission.wait().await?);
            }
            RequestOrderingKey::Extension {
                namespace,
                key,
                policy,
                ..
            } => {
                if *policy == ExtensionOrderingPolicy::CoreExclusive {
                    permit._extension_ordering = Some(
                        resolved
                            .connection
                            .register_extension_ordering(namespace.clone(), key.clone())?,
                    );
                }
            }
            RequestOrderingKey::Connection(_)
            | RequestOrderingKey::Session { .. }
            | RequestOrderingKey::Unordered => {}
        }
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        Ok(permit)
    }

    /// Admit progress-critical runtime event work independently from ordinary
    /// request capacity. These registrations remain visible to connection,
    /// session, and VM disposal so shutdown still cancels and drains them.
    ///
    /// This path is deliberately synchronous. A process event has already left
    /// its durable producer queue when it reaches admission, so it may not wait
    /// in an untracked future or lose ownership between coordinator turns.
    pub(crate) fn admit_internal_vm_event(
        &self,
        metadata: &RequestOperationMetadata,
        cancellation: OperationCancellation,
    ) -> Result<InternalVmEventAdmission, OwnershipCoordinatorError> {
        validate_ordering_ownership(&metadata.ownership, &metadata.ordering_key)?;
        if !matches!(&metadata.ownership, OwnershipScope::VmOwnership(_))
            || !matches!(
                &metadata.ordering_key,
                RequestOrderingKey::VmOperation { .. }
            )
        {
            return Err(OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("VM ownership with VM-operation ordering"),
                actual: format!(
                    "{}/{}",
                    ownership_label(&metadata.ownership),
                    ordering_label(&metadata.ordering_key)
                ),
            });
        }
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let resolved = self.resolve(&metadata.ownership)?;
        let vm = resolved
            .vm
            .as_ref()
            .expect("internal VM event requires VM ownership");
        let session = resolved
            .session
            .as_ref()
            .expect("internal VM event requires session ownership");

        // Lock narrowest-to-broadest and register all three scopes as one
        // non-suspending ownership transition. If active service capacity is
        // unavailable, the claimed event still receives a bounded deferred
        // registration, so disposal can cancel and drain it before it ever
        // obtains an execution slot.
        let mut vm_state = lock(&vm.inner.state, "VM coordinator");
        ensure_open(vm_state.phase, vm.label())?;
        let mut session_state = lock(&session.inner.state, "session coordinator");
        ensure_open(session_state.phase, session.label())?;
        let mut connection_state = lock(&resolved.connection.inner.state, "connection coordinator");
        ensure_open(
            connection_state.phase,
            format!("connection {}", resolved.connection.inner.connection_id),
        )?;
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }

        let active_capacity = [&vm_state.operations, &session_state.operations]
            .into_iter()
            .all(|operations| {
                operation_count(operations, OperationAdmissionClass::InternalEvent)
                    < self.inner.limits.max_internal_event_operations_per_entity
            })
            && operation_count(
                &connection_state.operations,
                OperationAdmissionClass::InternalEvent,
            ) < self.inner.limits.max_internal_event_operations_per_entity;
        let class = if active_capacity {
            OperationAdmissionClass::InternalEvent
        } else {
            OperationAdmissionClass::DeferredInternalEvent
        };
        check_operation_limit(
            &vm_state.operations,
            class,
            self.inner.limits,
            "VM-owned operations",
            "VM internal-event operations",
        )?;
        check_operation_limit(
            &session_state.operations,
            class,
            self.inner.limits,
            "session-owned operations",
            "session internal-event operations",
        )?;
        check_operation_limit(
            &connection_state.operations,
            class,
            self.inner.limits,
            "connection-owned operations",
            "connection internal-event operations",
        )?;

        let vm_operation_id = take_id(&mut vm_state.next_operation_id);
        vm_state.operations.insert(
            vm_operation_id,
            RegisteredOperation {
                cancellation: cancellation.clone(),
                class,
            },
        );
        let session_operation_id = take_id(&mut session_state.next_operation_id);
        session_state.operations.insert(
            session_operation_id,
            RegisteredOperation {
                cancellation: cancellation.clone(),
                class,
            },
        );
        let connection_operation_id = take_id(&mut connection_state.next_operation_id);
        connection_state.operations.insert(
            connection_operation_id,
            RegisteredOperation {
                cancellation: cancellation.clone(),
                class,
            },
        );
        drop(connection_state);
        drop(session_state);
        drop(vm_state);

        let vm_operation = VmOperationRegistration {
            inner: Arc::clone(&vm.inner),
            id: vm_operation_id,
        };
        let session = SessionOperationRegistration {
            inner: Arc::clone(&session.inner),
            id: session_operation_id,
        };
        let connection = ConnectionOperationRegistration {
            inner: Arc::clone(&resolved.connection.inner),
            id: connection_operation_id,
        };
        let permit = CoordinatorOperationPermit {
            _connection: connection,
            _session: Some(session),
            vm_operation: Some(vm_operation),
            vm_lifecycle: None,
            _extension_ordering: None,
        };
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        Ok(match class {
            OperationAdmissionClass::InternalEvent => InternalVmEventAdmission::Admitted(permit),
            OperationAdmissionClass::DeferredInternalEvent => {
                InternalVmEventAdmission::Deferred(permit)
            }
            OperationAdmissionClass::Ordinary => {
                unreachable!("internal event registered as ordinary work")
            }
        })
    }

    pub(crate) fn begin_session_disposal(
        &self,
        ownership: &OwnershipScope,
        reason: OperationCancellationReason,
    ) -> Result<SessionDisposal, OwnershipCoordinatorError> {
        let resolved = self.resolve(ownership)?;
        let session =
            resolved
                .session
                .ok_or_else(|| OwnershipCoordinatorError::OwnershipMismatch {
                    expected: String::from("session or VM ownership"),
                    actual: ownership_label(ownership),
                })?;
        session.begin_disposal(reason)
    }

    pub(crate) fn begin_vm_disposal(
        &self,
        ownership: &OwnershipScope,
        reason: OperationCancellationReason,
    ) -> Result<VmDisposal, OwnershipCoordinatorError> {
        let resolved = self.resolve(ownership)?;
        let vm = resolved
            .vm
            .ok_or_else(|| OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("VM ownership"),
                actual: ownership_label(ownership),
            })?;
        vm.begin_disposal(reason)
    }

    pub(crate) fn begin_connection_disposal(
        &self,
        connection_id: &str,
        reason: OperationCancellationReason,
    ) -> Result<ConnectionDisposal, OwnershipCoordinatorError> {
        self.connection(connection_id)?.begin_disposal(reason)
    }

    fn resolve(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<ResolvedCoordinators, OwnershipCoordinatorError> {
        let (connection_id, session_id, vm_id) = ownership_ids(ownership);
        let connection = self.connection(connection_id)?;
        let session = match session_id {
            Some(session_id) => Some(connection.session(session_id)?),
            None => None,
        };
        let vm = match (session.as_ref(), vm_id) {
            (Some(session), Some(vm_id)) => Some(session.vm(vm_id)?),
            _ => None,
        };
        Ok(ResolvedCoordinators {
            connection,
            session,
            vm,
        })
    }
}

struct ResolvedCoordinators {
    connection: ConnectionCoordinator,
    session: Option<SessionCoordinator>,
    vm: Option<VmCoordinator>,
}

impl ConnectionCoordinator {
    pub(crate) fn connection_id(&self) -> &str {
        &self.inner.connection_id
    }

    pub(crate) fn open_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<SessionCoordinator, OwnershipCoordinatorError> {
        let session_id = session_id.into();
        let mut state = lock(&self.inner.state, "connection coordinator");
        ensure_open(
            state.phase,
            format!("connection {}", self.inner.connection_id),
        )?;
        if state.sessions.contains_key(&session_id) {
            return Err(OwnershipCoordinatorError::Duplicate {
                scope: "session",
                id: format!("{}:{session_id}", self.inner.connection_id),
            });
        }
        check_limit(
            "session coordinators per connection",
            state.sessions.len(),
            self.inner.limits.max_sessions_per_connection,
            SESSION_LIMIT_PATH,
        )?;
        let session = SessionCoordinator {
            parent: Arc::downgrade(&self.inner),
            inner: Arc::new(SessionCoordinatorInner {
                connection_id: self.inner.connection_id.clone(),
                session_id: session_id.clone(),
                limits: self.inner.limits,
                state: Mutex::new(SessionCoordinatorState {
                    phase: CoordinatorPhase::Open,
                    vms: BTreeMap::new(),
                    operations: BTreeMap::new(),
                    next_operation_id: 1,
                }),
                drained: Notify::new(),
            }),
        };
        state.sessions.insert(session_id, session.clone());
        Ok(session)
    }

    pub(crate) fn session(
        &self,
        session_id: &str,
    ) -> Result<SessionCoordinator, OwnershipCoordinatorError> {
        lock(&self.inner.state, "connection coordinator")
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "session",
                id: format!("{}:{session_id}", self.inner.connection_id),
            })
    }

    pub(crate) fn snapshot(&self) -> EntityCoordinatorSnapshot {
        let state = lock(&self.inner.state, "connection coordinator");
        EntityCoordinatorSnapshot {
            phase: state.phase,
            active_operations: state.operations.len(),
            child_count: state.sessions.len(),
        }
    }

    pub(crate) fn begin_disposal(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<ConnectionDisposal, OwnershipCoordinatorError> {
        let sessions = {
            let mut state = lock(&self.inner.state, "connection coordinator");
            ensure_open(
                state.phase,
                format!("connection {}", self.inner.connection_id),
            )?;
            state.phase = CoordinatorPhase::Closing;
            signal_all(&state.operations, reason);
            state.sessions.values().cloned().collect::<Vec<_>>()
        };
        for session in &sessions {
            session.force_closing(reason);
        }
        Ok(ConnectionDisposal {
            connection: self.clone(),
            sessions,
            completed: false,
        })
    }

    fn register_operation(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<ConnectionOperationRegistration, OwnershipCoordinatorError> {
        self.register_operation_with_class(cancellation, OperationAdmissionClass::Ordinary)
    }

    fn register_operation_with_class(
        &self,
        cancellation: OperationCancellation,
        class: OperationAdmissionClass,
    ) -> Result<ConnectionOperationRegistration, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut state = lock(&self.inner.state, "connection coordinator");
        ensure_open(
            state.phase,
            format!("connection {}", self.inner.connection_id),
        )?;
        check_operation_limit(
            &state.operations,
            class,
            self.inner.limits,
            "connection-owned operations",
            "connection internal-event operations",
        )?;
        let id = take_id(&mut state.next_operation_id);
        state.operations.insert(
            id,
            RegisteredOperation {
                cancellation,
                class,
            },
        );
        Ok(ConnectionOperationRegistration {
            inner: Arc::clone(&self.inner),
            id,
        })
    }

    fn register_extension_ordering(
        &self,
        namespace: String,
        key: Vec<u8>,
    ) -> Result<ExtensionOrderingRegistration, OwnershipCoordinatorError> {
        let mut state = lock(&self.inner.state, "connection coordinator");
        ensure_open(
            state.phase,
            format!("connection {}", self.inner.connection_id),
        )?;
        let ordering_key = (namespace, key);
        if state.extension_ordering.contains_key(&ordering_key) {
            return Err(OwnershipCoordinatorError::OrderingConflict {
                scope: format!(
                    "connection {}/extension/{}/{}-bytes",
                    self.inner.connection_id,
                    ordering_key.0,
                    ordering_key.1.len()
                ),
            });
        }
        state.extension_ordering.insert(ordering_key.clone(), ());
        Ok(ExtensionOrderingRegistration {
            inner: Arc::clone(&self.inner),
            key: ordering_key,
        })
    }
}

impl SessionCoordinator {
    pub(crate) fn connection_id(&self) -> &str {
        &self.inner.connection_id
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub(crate) fn open_vm(
        &self,
        vm_id: impl Into<String>,
    ) -> Result<VmCoordinator, OwnershipCoordinatorError> {
        let vm_id = vm_id.into();
        let mut state = lock(&self.inner.state, "session coordinator");
        ensure_open(state.phase, self.label())?;
        if state.vms.contains_key(&vm_id) {
            return Err(OwnershipCoordinatorError::Duplicate {
                scope: "VM",
                id: format!("{}:{vm_id}", self.label()),
            });
        }
        check_limit(
            "VM coordinators per session",
            state.vms.len(),
            self.inner.limits.max_vms_per_session,
            VM_LIMIT_PATH,
        )?;
        let vm = VmCoordinator {
            parent: Arc::downgrade(&self.inner),
            inner: Arc::new(VmCoordinatorInner {
                connection_id: self.inner.connection_id.clone(),
                session_id: self.inner.session_id.clone(),
                vm_id: vm_id.clone(),
                limits: self.inner.limits,
                state: Mutex::new(VmCoordinatorState {
                    phase: CoordinatorPhase::Open,
                    operations: BTreeMap::new(),
                    next_operation_id: 1,
                    next_lifecycle_id: 1,
                    lifecycle: VmLifecycleState::Idle,
                }),
                changed: Notify::new(),
            }),
        };
        state.vms.insert(vm_id, vm.clone());
        Ok(vm)
    }

    pub(crate) fn vm(&self, vm_id: &str) -> Result<VmCoordinator, OwnershipCoordinatorError> {
        lock(&self.inner.state, "session coordinator")
            .vms
            .get(vm_id)
            .cloned()
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "VM",
                id: format!("{}:{vm_id}", self.label()),
            })
    }

    pub(crate) fn snapshot(&self) -> EntityCoordinatorSnapshot {
        let state = lock(&self.inner.state, "session coordinator");
        EntityCoordinatorSnapshot {
            phase: state.phase,
            active_operations: state.operations.len(),
            child_count: state.vms.len(),
        }
    }

    pub(crate) fn begin_disposal(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<SessionDisposal, OwnershipCoordinatorError> {
        let vms = {
            let mut state = lock(&self.inner.state, "session coordinator");
            ensure_open(state.phase, self.label())?;
            state.phase = CoordinatorPhase::Closing;
            signal_all(&state.operations, reason);
            state.vms.values().cloned().collect::<Vec<_>>()
        };
        for vm in &vms {
            vm.begin_closing(reason);
        }
        Ok(SessionDisposal {
            session: self.clone(),
            vms,
            completed: false,
        })
    }

    fn force_closing(&self, reason: OperationCancellationReason) {
        let vms = {
            let mut state = lock(&self.inner.state, "session coordinator");
            if state.phase == CoordinatorPhase::Open {
                state.phase = CoordinatorPhase::Closing;
            }
            signal_all(&state.operations, reason);
            state.vms.values().cloned().collect::<Vec<_>>()
        };
        for vm in &vms {
            vm.begin_closing(reason);
        }
    }

    fn register_operation(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<SessionOperationRegistration, OwnershipCoordinatorError> {
        self.register_operation_with_class(cancellation, OperationAdmissionClass::Ordinary)
    }

    fn register_operation_with_class(
        &self,
        cancellation: OperationCancellation,
        class: OperationAdmissionClass,
    ) -> Result<SessionOperationRegistration, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut state = lock(&self.inner.state, "session coordinator");
        ensure_open(state.phase, self.label())?;
        check_operation_limit(
            &state.operations,
            class,
            self.inner.limits,
            "session-owned operations",
            "session internal-event operations",
        )?;
        let id = take_id(&mut state.next_operation_id);
        state.operations.insert(
            id,
            RegisteredOperation {
                cancellation,
                class,
            },
        );
        Ok(SessionOperationRegistration {
            inner: Arc::clone(&self.inner),
            id,
        })
    }

    fn label(&self) -> String {
        format!(
            "session {}:{}",
            self.inner.connection_id, self.inner.session_id
        )
    }
}

impl VmCoordinator {
    pub(crate) fn connection_id(&self) -> &str {
        &self.inner.connection_id
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub(crate) fn vm_id(&self) -> &str {
        &self.inner.vm_id
    }

    pub(crate) fn snapshot(&self) -> VmCoordinatorSnapshot {
        let state = lock(&self.inner.state, "VM coordinator");
        VmCoordinatorSnapshot {
            phase: state.phase,
            active_operations: state.operations.len(),
            lifecycle: state.lifecycle.phase(),
        }
    }

    fn register_operation(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<VmOperationRegistration, OwnershipCoordinatorError> {
        self.register_operation_with_class(cancellation, OperationAdmissionClass::Ordinary)
    }

    fn register_operation_with_class(
        &self,
        cancellation: OperationCancellation,
        class: OperationAdmissionClass,
    ) -> Result<VmOperationRegistration, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut state = lock(&self.inner.state, "VM coordinator");
        ensure_open(state.phase, self.label())?;
        if class == OperationAdmissionClass::Ordinary
            && !matches!(state.lifecycle, VmLifecycleState::Idle)
        {
            return Err(OwnershipCoordinatorError::LifecycleConflict {
                vm: self.label(),
                lifecycle: state.lifecycle.phase(),
            });
        }
        check_operation_limit(
            &state.operations,
            class,
            self.inner.limits,
            "VM-owned operations",
            "VM internal-event operations",
        )?;
        let id = take_id(&mut state.next_operation_id);
        state.operations.insert(
            id,
            RegisteredOperation {
                cancellation,
                class,
            },
        );
        Ok(VmOperationRegistration {
            inner: Arc::clone(&self.inner),
            id,
        })
    }

    fn begin_lifecycle(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<VmLifecycleAdmission, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let id = {
            let mut state = lock(&self.inner.state, "VM coordinator");
            ensure_open(state.phase, self.label())?;
            if !matches!(state.lifecycle, VmLifecycleState::Idle) {
                return Err(OwnershipCoordinatorError::LifecycleConflict {
                    vm: self.label(),
                    lifecycle: state.lifecycle.phase(),
                });
            }
            let id = take_id(&mut state.next_lifecycle_id);
            state.lifecycle = VmLifecycleState::Pending {
                id,
                cancellation: cancellation.clone(),
            };
            id
        };
        Ok(VmLifecycleAdmission {
            inner: Arc::clone(&self.inner),
            id,
            cancellation,
            armed: true,
        })
    }

    fn begin_closing(&self, reason: OperationCancellationReason) {
        let mut state = lock(&self.inner.state, "VM coordinator");
        if state.phase == CoordinatorPhase::Open {
            state.phase = CoordinatorPhase::Closing;
        }
        signal_all(&state.operations, reason);
        state.lifecycle.signal(reason);
        self.inner.changed.notify_one();
    }

    pub(crate) fn begin_disposal(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<VmDisposal, OwnershipCoordinatorError> {
        {
            let state = lock(&self.inner.state, "VM coordinator");
            ensure_open(state.phase, self.label())?;
        }
        self.begin_closing(reason);
        Ok(VmDisposal {
            vm: self.clone(),
            completed: false,
        })
    }

    fn label(&self) -> String {
        format!(
            "VM {}:{}:{}",
            self.inner.connection_id, self.inner.session_id, self.inner.vm_id
        )
    }
}

#[derive(Debug)]
pub(crate) struct CoordinatorOperationPermit {
    vm_lifecycle: Option<VmLifecycleGuard>,
    vm_operation: Option<VmOperationRegistration>,
    _extension_ordering: Option<ExtensionOrderingRegistration>,
    _session: Option<SessionOperationRegistration>,
    _connection: ConnectionOperationRegistration,
}

impl CoordinatorOperationPermit {
    pub(crate) fn is_vm_lifecycle(&self) -> bool {
        self.vm_lifecycle.is_some()
    }

    pub(crate) fn is_vm_operation(&self) -> bool {
        self.vm_operation.is_some()
    }

    /// Promote one already-tracked internal event into active service
    /// capacity. A false result keeps the same ownership registrations live;
    /// disposal can therefore cancel and drain the event while it waits.
    pub(crate) fn try_activate_deferred_internal_event(
        &mut self,
    ) -> Result<bool, OwnershipCoordinatorError> {
        let vm = self.vm_operation.as_ref().ok_or_else(|| {
            OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("VM-bound internal-event permit"),
                actual: String::from("permit without VM operation registration"),
            }
        })?;
        let session =
            self._session
                .as_ref()
                .ok_or_else(|| OwnershipCoordinatorError::OwnershipMismatch {
                    expected: String::from("session-bound internal-event permit"),
                    actual: String::from("permit without session registration"),
                })?;

        let mut vm_state = lock(&vm.inner.state, "VM coordinator");
        let mut session_state = lock(&session.inner.state, "session coordinator");
        let mut connection_state = lock(&self._connection.inner.state, "connection coordinator");
        let cancellation = vm_state
            .operations
            .get(&vm.id)
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "VM operation",
                id: vm.id.to_string(),
            })?
            .cancellation
            .clone();
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        ensure_open(vm_state.phase, format!("VM {}", vm.inner.vm_id))?;
        ensure_open(
            session_state.phase,
            format!(
                "session {}:{}",
                session.inner.connection_id, session.inner.session_id
            ),
        )?;
        ensure_open(
            connection_state.phase,
            format!("connection {}", self._connection.inner.connection_id),
        )?;

        let classes = [
            vm_state
                .operations
                .get(&vm.id)
                .map(|operation| operation.class),
            session_state
                .operations
                .get(&session.id)
                .map(|operation| operation.class),
            connection_state
                .operations
                .get(&self._connection.id)
                .map(|operation| operation.class),
        ];
        if classes
            .iter()
            .all(|class| *class == Some(OperationAdmissionClass::InternalEvent))
        {
            return Ok(true);
        }
        if !classes
            .iter()
            .all(|class| *class == Some(OperationAdmissionClass::DeferredInternalEvent))
        {
            return Err(OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("matching deferred internal-event registrations"),
                actual: format!("registration classes {classes:?}"),
            });
        }

        let limit = vm.inner.limits.max_internal_event_operations_per_entity;
        let has_capacity =
            operation_count(&vm_state.operations, OperationAdmissionClass::InternalEvent) < limit
                && operation_count(
                    &session_state.operations,
                    OperationAdmissionClass::InternalEvent,
                ) < limit
                && operation_count(
                    &connection_state.operations,
                    OperationAdmissionClass::InternalEvent,
                ) < limit;
        if !has_capacity {
            return Ok(false);
        }
        vm_state
            .operations
            .get_mut(&vm.id)
            .expect("deferred VM registration checked above")
            .class = OperationAdmissionClass::InternalEvent;
        session_state
            .operations
            .get_mut(&session.id)
            .expect("deferred session registration checked above")
            .class = OperationAdmissionClass::InternalEvent;
        connection_state
            .operations
            .get_mut(&self._connection.id)
            .expect("deferred connection registration checked above")
            .class = OperationAdmissionClass::InternalEvent;
        Ok(true)
    }
}

#[derive(Debug)]
struct ConnectionOperationRegistration {
    inner: Arc<ConnectionCoordinatorInner>,
    id: u64,
}

impl Drop for ConnectionOperationRegistration {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state, "connection coordinator");
        if state.operations.remove(&self.id).is_some() {
            self.inner.drained.notify_one();
        }
    }
}

#[derive(Debug)]
struct ExtensionOrderingRegistration {
    inner: Arc<ConnectionCoordinatorInner>,
    key: (String, Vec<u8>),
}

impl Drop for ExtensionOrderingRegistration {
    fn drop(&mut self) {
        lock(&self.inner.state, "connection coordinator")
            .extension_ordering
            .remove(&self.key);
    }
}

#[derive(Debug)]
struct SessionOperationRegistration {
    inner: Arc<SessionCoordinatorInner>,
    id: u64,
}

impl Drop for SessionOperationRegistration {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state, "session coordinator");
        if state.operations.remove(&self.id).is_some() {
            self.inner.drained.notify_one();
        }
    }
}

#[derive(Debug)]
struct VmOperationRegistration {
    inner: Arc<VmCoordinatorInner>,
    id: u64,
}

impl Drop for VmOperationRegistration {
    fn drop(&mut self) {
        let mut state = lock(&self.inner.state, "VM coordinator");
        if state.operations.remove(&self.id).is_some() {
            self.inner.changed.notify_one();
        }
    }
}

#[derive(Debug)]
struct VmLifecycleAdmission {
    inner: Arc<VmCoordinatorInner>,
    id: u64,
    cancellation: OperationCancellation,
    armed: bool,
}

impl VmLifecycleAdmission {
    async fn wait(mut self) -> Result<VmLifecycleGuard, OwnershipCoordinatorError> {
        loop {
            let changed = self.inner.changed.notified();
            {
                let mut state = lock(&self.inner.state, "VM coordinator");
                match &state.lifecycle {
                    VmLifecycleState::Pending { id, .. } if *id == self.id => {
                        if state.operations.is_empty() {
                            let cancellation = self.cancellation.clone();
                            state.lifecycle = VmLifecycleState::Active {
                                id: self.id,
                                cancellation,
                            };
                            self.armed = false;
                            return Ok(VmLifecycleGuard {
                                inner: Arc::clone(&self.inner),
                                id: self.id,
                            });
                        }
                    }
                    _ => {
                        let reason = self
                            .cancellation
                            .reason()
                            .unwrap_or(OperationCancellationReason::ConnectionClosed);
                        return Err(OwnershipCoordinatorError::Cancelled { reason });
                    }
                }
            }
            tokio::select! {
                _ = changed => {}
                reason = self.cancellation.cancelled() => {
                    return Err(OwnershipCoordinatorError::Cancelled { reason });
                }
            }
        }
    }
}

impl Drop for VmLifecycleAdmission {
    fn drop(&mut self) {
        if self.armed {
            release_lifecycle(&self.inner, self.id);
        }
    }
}

#[derive(Debug)]
struct VmLifecycleGuard {
    inner: Arc<VmCoordinatorInner>,
    id: u64,
}

impl Drop for VmLifecycleGuard {
    fn drop(&mut self) {
        release_lifecycle(&self.inner, self.id);
    }
}

fn release_lifecycle(inner: &Arc<VmCoordinatorInner>, id: u64) {
    let mut state = lock(&inner.state, "VM coordinator");
    let matches_id = match &state.lifecycle {
        VmLifecycleState::Pending { id: active, .. }
        | VmLifecycleState::Active { id: active, .. } => *active == id,
        VmLifecycleState::Idle => false,
    };
    if matches_id {
        state.lifecycle = VmLifecycleState::Idle;
        inner.changed.notify_one();
    }
}

#[derive(Debug)]
pub(crate) struct SessionDisposal {
    session: SessionCoordinator,
    vms: Vec<VmCoordinator>,
    completed: bool,
}

impl SessionDisposal {
    pub(crate) async fn wait_drained(&self) {
        loop {
            let drained = self.session.inner.drained.notified();
            if lock(&self.session.inner.state, "session coordinator")
                .operations
                .is_empty()
            {
                return;
            }
            drained.await;
        }
    }

    pub(crate) fn complete(mut self) -> Result<(), OwnershipCoordinatorError> {
        let active_operations = lock(&self.session.inner.state, "session coordinator")
            .operations
            .len();
        if active_operations != 0 {
            return Err(OwnershipCoordinatorError::NotDrained {
                scope: self.session.label(),
                active_operations,
            });
        }
        for vm in &self.vms {
            let mut state = lock(&vm.inner.state, "VM coordinator");
            let vm_active = state.operations.len()
                + usize::from(!matches!(state.lifecycle, VmLifecycleState::Idle));
            if vm_active != 0 {
                return Err(OwnershipCoordinatorError::NotDrained {
                    scope: vm.label(),
                    active_operations: vm_active,
                });
            }
            state.phase = CoordinatorPhase::Closed;
        }
        {
            let mut state = lock(&self.session.inner.state, "session coordinator");
            state.phase = CoordinatorPhase::Closed;
            state.vms.clear();
        }
        if let Some(parent) = self.session.parent.upgrade() {
            let mut state = lock(&parent.state, "connection coordinator");
            if state
                .sessions
                .get(&self.session.inner.session_id)
                .is_some_and(|current| Arc::ptr_eq(&current.inner, &self.session.inner))
            {
                state.sessions.remove(&self.session.inner.session_id);
            }
        }
        self.completed = true;
        Ok(())
    }
}

impl Drop for SessionDisposal {
    fn drop(&mut self) {
        if !self.completed {
            tracing::warn!(
                connection_id = %self.session.inner.connection_id,
                session_id = %self.session.inner.session_id,
                "ERR_AGENTOS_SESSION_DISPOSAL_INCOMPLETE: session remains Closing for a bounded retry"
            );
        }
    }
}

#[derive(Debug)]
pub(crate) struct VmDisposal {
    vm: VmCoordinator,
    completed: bool,
}

impl VmDisposal {
    pub(crate) async fn wait_drained(&self) {
        loop {
            let changed = self.vm.inner.changed.notified();
            let drained = {
                let state = lock(&self.vm.inner.state, "VM coordinator");
                state.operations.is_empty() && matches!(state.lifecycle, VmLifecycleState::Idle)
            };
            if drained {
                return;
            }
            changed.await;
        }
    }

    pub(crate) fn complete(mut self) -> Result<(), OwnershipCoordinatorError> {
        {
            let mut state = lock(&self.vm.inner.state, "VM coordinator");
            let active_operations = state.operations.len()
                + usize::from(!matches!(state.lifecycle, VmLifecycleState::Idle));
            if active_operations != 0 {
                return Err(OwnershipCoordinatorError::NotDrained {
                    scope: self.vm.label(),
                    active_operations,
                });
            }
            state.phase = CoordinatorPhase::Closed;
        }
        if let Some(parent) = self.vm.parent.upgrade() {
            let mut state = lock(&parent.state, "session coordinator");
            if state
                .vms
                .get(&self.vm.inner.vm_id)
                .is_some_and(|current| Arc::ptr_eq(&current.inner, &self.vm.inner))
            {
                state.vms.remove(&self.vm.inner.vm_id);
            }
        }
        self.completed = true;
        Ok(())
    }
}

impl Drop for VmDisposal {
    fn drop(&mut self) {
        if !self.completed {
            tracing::warn!(
                connection_id = %self.vm.inner.connection_id,
                session_id = %self.vm.inner.session_id,
                vm_id = %self.vm.inner.vm_id,
                "ERR_AGENTOS_VM_DISPOSAL_INCOMPLETE: VM remains Closing for a bounded retry"
            );
        }
    }
}

#[derive(Debug)]
pub(crate) struct ConnectionDisposal {
    connection: ConnectionCoordinator,
    sessions: Vec<SessionCoordinator>,
    completed: bool,
}

impl ConnectionDisposal {
    pub(crate) async fn wait_drained(&self) {
        loop {
            let drained = self.connection.inner.drained.notified();
            if lock(&self.connection.inner.state, "connection coordinator")
                .operations
                .is_empty()
            {
                return;
            }
            drained.await;
        }
    }

    pub(crate) fn complete(mut self) -> Result<(), OwnershipCoordinatorError> {
        let active_operations = lock(&self.connection.inner.state, "connection coordinator")
            .operations
            .len();
        if active_operations != 0 {
            return Err(OwnershipCoordinatorError::NotDrained {
                scope: format!("connection {}", self.connection.inner.connection_id),
                active_operations,
            });
        }
        for session in &self.sessions {
            let mut session_state = lock(&session.inner.state, "session coordinator");
            if !session_state.operations.is_empty() {
                return Err(OwnershipCoordinatorError::NotDrained {
                    scope: session.label(),
                    active_operations: session_state.operations.len(),
                });
            }
            for vm in session_state.vms.values() {
                let mut vm_state = lock(&vm.inner.state, "VM coordinator");
                let vm_active = vm_state.operations.len()
                    + usize::from(!matches!(vm_state.lifecycle, VmLifecycleState::Idle));
                if vm_active != 0 {
                    return Err(OwnershipCoordinatorError::NotDrained {
                        scope: vm.label(),
                        active_operations: vm_active,
                    });
                }
                vm_state.phase = CoordinatorPhase::Closed;
            }
            session_state.vms.clear();
            session_state.phase = CoordinatorPhase::Closed;
        }
        {
            let mut state = lock(&self.connection.inner.state, "connection coordinator");
            state.sessions.clear();
            state.phase = CoordinatorPhase::Closed;
        }
        if let Some(root) = self.connection.root.upgrade() {
            let mut state = lock(&root.state, "connection registry");
            if state
                .connections
                .get(&self.connection.inner.connection_id)
                .is_some_and(|current| Arc::ptr_eq(&current.inner, &self.connection.inner))
            {
                state
                    .connections
                    .remove(&self.connection.inner.connection_id);
            }
        }
        self.completed = true;
        Ok(())
    }
}

impl Drop for ConnectionDisposal {
    fn drop(&mut self) {
        if !self.completed {
            tracing::warn!(
                connection_id = %self.connection.inner.connection_id,
                "ERR_AGENTOS_CONNECTION_DISPOSAL_INCOMPLETE: connection remains Closing for a bounded retry"
            );
        }
    }
}

fn ownership_ids(ownership: &OwnershipScope) -> (&str, Option<&str>, Option<&str>) {
    match ownership {
        OwnershipScope::ConnectionOwnership(scope) => (&scope.connection_id, None, None),
        OwnershipScope::SessionOwnership(scope) => {
            (&scope.connection_id, Some(&scope.session_id), None)
        }
        OwnershipScope::VmOwnership(scope) => (
            &scope.connection_id,
            Some(&scope.session_id),
            Some(&scope.vm_id),
        ),
    }
}

fn validate_ordering_ownership(
    ownership: &OwnershipScope,
    ordering: &RequestOrderingKey,
) -> Result<(), OwnershipCoordinatorError> {
    let (connection_id, session_id, vm_id) = ownership_ids(ownership);
    let valid = match ordering {
        RequestOrderingKey::Connection(key_connection) => key_connection == connection_id,
        RequestOrderingKey::Session {
            connection_id: key_connection,
            session_id: key_session,
        } => key_connection == connection_id && session_id.is_some_and(|id| id == key_session),
        RequestOrderingKey::VmLifecycle {
            connection_id: key_connection,
            session_id: key_session,
            vm_id: key_vm,
        }
        | RequestOrderingKey::VmOperation {
            connection_id: key_connection,
            session_id: key_session,
            vm_id: key_vm,
        } => {
            key_connection == connection_id
                && session_id.is_some_and(|id| id == key_session)
                && vm_id.is_some_and(|id| id == key_vm)
        }
        RequestOrderingKey::Extension {
            connection_id: key_connection,
            ..
        } => key_connection == connection_id,
        RequestOrderingKey::Unordered => true,
    };
    if valid {
        Ok(())
    } else {
        Err(OwnershipCoordinatorError::OwnershipMismatch {
            expected: ownership_label(ownership),
            actual: ordering_label(ordering),
        })
    }
}

fn ownership_label(ownership: &OwnershipScope) -> String {
    let (connection_id, session_id, vm_id) = ownership_ids(ownership);
    match (session_id, vm_id) {
        (Some(session), Some(vm)) => format!("{connection_id}/{session}/{vm}"),
        (Some(session), None) => format!("{connection_id}/{session}"),
        _ => connection_id.to_owned(),
    }
}

fn ordering_label(ordering: &RequestOrderingKey) -> String {
    match ordering {
        RequestOrderingKey::Connection(connection) => connection.clone(),
        RequestOrderingKey::Session {
            connection_id,
            session_id,
        } => format!("{connection_id}/{session_id}"),
        RequestOrderingKey::VmLifecycle {
            connection_id,
            session_id,
            vm_id,
        }
        | RequestOrderingKey::VmOperation {
            connection_id,
            session_id,
            vm_id,
        } => format!("{connection_id}/{session_id}/{vm_id}"),
        RequestOrderingKey::Extension {
            namespace,
            connection_id,
            key,
            ..
        } => format!("{connection_id}/extension/{namespace}/{}-bytes", key.len()),
        RequestOrderingKey::Unordered => String::from("unordered"),
    }
}

fn ensure_open(phase: CoordinatorPhase, scope: String) -> Result<(), OwnershipCoordinatorError> {
    if phase == CoordinatorPhase::Open {
        Ok(())
    } else {
        Err(OwnershipCoordinatorError::Closing { scope, phase })
    }
}

fn check_limit(
    scope: &'static str,
    current: usize,
    limit: usize,
    configuration_path: &'static str,
) -> Result<(), OwnershipCoordinatorError> {
    if current >= limit {
        return Err(OwnershipCoordinatorError::Limit {
            scope,
            current,
            limit,
            configuration_path,
        });
    }
    let after = current + 1;
    if after.saturating_mul(10) >= limit.saturating_mul(8) {
        tracing::warn!(
            scope,
            current = after,
            limit,
            configuration_path,
            "agentOS ownership coordinator is near its configured bound"
        );
    }
    Ok(())
}

fn check_operation_limit(
    operations: &BTreeMap<u64, RegisteredOperation>,
    class: OperationAdmissionClass,
    limits: OwnershipCoordinatorLimits,
    ordinary_scope: &'static str,
    internal_event_scope: &'static str,
) -> Result<(), OwnershipCoordinatorError> {
    let current = operation_count(operations, class);
    match class {
        OperationAdmissionClass::Ordinary => check_limit(
            ordinary_scope,
            current,
            limits.max_operations_per_entity,
            IN_FLIGHT_REQUEST_COUNT_PATH,
        ),
        OperationAdmissionClass::InternalEvent => check_limit(
            internal_event_scope,
            current,
            limits.max_internal_event_operations_per_entity,
            INTERNAL_EVENT_OPERATION_LIMIT_PATH,
        ),
        OperationAdmissionClass::DeferredInternalEvent => check_limit(
            "deferred claimed internal-event operations",
            current,
            limits.max_internal_event_operations_per_entity,
            INTERNAL_EVENT_OPERATION_LIMIT_PATH,
        ),
    }
}

fn operation_count(
    operations: &BTreeMap<u64, RegisteredOperation>,
    class: OperationAdmissionClass,
) -> usize {
    operations
        .values()
        .filter(|operation| operation.class == class)
        .count()
}

fn signal_all(
    operations: &BTreeMap<u64, RegisteredOperation>,
    reason: OperationCancellationReason,
) {
    for operation in operations.values() {
        operation.cancellation.signal(reason);
    }
}

fn take_id(next: &mut u64) -> u64 {
    let id = *next;
    *next = next.wrapping_add(1).max(1);
    id
}

fn lock<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            coordinator = label,
            "ERR_AGENTOS_OWNERSHIP_COORDINATOR_POISONED: recovering coordinator state"
        );
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::OwnershipScope;
    use std::future::Future as _;
    use std::task::{Context, Poll};

    fn coordinator() -> OwnershipCoordinator {
        OwnershipCoordinator::new(OwnershipCoordinatorLimits {
            max_connections: 4,
            max_sessions_per_connection: 4,
            max_vms_per_session: 4,
            max_operations_per_entity: 8,
            max_internal_event_operations_per_entity: 8,
        })
    }

    fn expect_internal_admitted(admission: InternalVmEventAdmission) -> CoordinatorOperationPermit {
        match admission {
            InternalVmEventAdmission::Admitted(permit) => permit,
            InternalVmEventAdmission::Deferred(_) => {
                panic!("expected active internal-event admission")
            }
        }
    }

    fn expect_internal_deferred(admission: InternalVmEventAdmission) -> CoordinatorOperationPermit {
        match admission {
            InternalVmEventAdmission::Deferred(permit) => permit,
            InternalVmEventAdmission::Admitted(_) => {
                panic!("expected deferred internal-event registration")
            }
        }
    }

    fn configured() -> (
        OwnershipCoordinator,
        ConnectionCoordinator,
        SessionCoordinator,
        VmCoordinator,
        VmCoordinator,
    ) {
        let coordinator = coordinator();
        let connection = coordinator
            .register_connection("connection-a")
            .expect("register connection");
        let session = connection.open_session("session-a").expect("open session");
        let vm_a = session.open_vm("vm-a").expect("open VM A");
        let vm_b = session.open_vm("vm-b").expect("open VM B");
        (coordinator, connection, session, vm_a, vm_b)
    }

    fn vm_metadata(connection: &str, session: &str, vm: &str) -> RequestOperationMetadata {
        RequestOperationMetadata::new(
            OwnershipScope::vm(connection, session, vm),
            "VM operation",
            RequestOrderingKey::VmOperation {
                connection_id: connection.to_owned(),
                session_id: session.to_owned(),
                vm_id: vm.to_owned(),
            },
        )
    }

    fn lifecycle_metadata(connection: &str, session: &str, vm: &str) -> RequestOperationMetadata {
        RequestOperationMetadata::new(
            OwnershipScope::vm(connection, session, vm),
            "VM lifecycle",
            RequestOrderingKey::VmLifecycle {
                connection_id: connection.to_owned(),
                session_id: session.to_owned(),
                vm_id: vm.to_owned(),
            },
        )
    }

    fn extension_metadata(connection: &str, key: &[u8]) -> RequestOperationMetadata {
        RequestOperationMetadata::new(
            OwnershipScope::connection(connection),
            "extension operation",
            RequestOrderingKey::Extension {
                namespace: String::from("test.extension"),
                connection_id: connection.to_owned(),
                key: key.to_vec(),
                policy: ExtensionOrderingPolicy::CoreExclusive,
            },
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gated_vm_a_does_not_delay_vm_b() {
        let (coordinator, _, _, vm_a, vm_b) = configured();
        let gate_a = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect("VM A operation starts");
        assert_eq!(vm_a.snapshot().active_operations, 1);

        let operation_b = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-b"),
                OperationCancellation::new(),
            )
            .await
            .expect("VM B operation starts independently");
        assert_eq!(vm_a.snapshot().active_operations, 1);
        assert_eq!(vm_b.snapshot().active_operations, 1);
        drop(operation_b);
        drop(gate_a);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_waits_for_same_vm_operation_and_excludes_new_work() {
        let (coordinator, _, _, vm_a, _) = configured();
        let operation = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect("VM operation starts");
        let lifecycle_cancel = OperationCancellation::new();
        let lifecycle_request = lifecycle_metadata("connection-a", "session-a", "vm-a");
        let mut lifecycle = Box::pin(coordinator.admit(&lifecycle_request, lifecycle_cancel));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(lifecycle.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(vm_a.snapshot().lifecycle, VmLifecyclePhase::Pending);

        let conflict = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect_err("pending lifecycle excludes new VM operations");
        assert_eq!(conflict.code(), "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT");

        drop(operation);
        let lifecycle = lifecycle
            .await
            .expect("lifecycle starts after prior operation drains");
        assert!(lifecycle.is_vm_lifecycle());
        assert_eq!(vm_a.snapshot().lifecycle, VmLifecyclePhase::Active);
        drop(lifecycle);

        let after = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect("VM reopens after lifecycle critical section");
        drop(after);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn internal_vm_events_have_independent_bounds_and_remain_disposal_tracked() {
        let coordinator = OwnershipCoordinator::new(OwnershipCoordinatorLimits {
            max_connections: 1,
            max_sessions_per_connection: 1,
            max_vms_per_session: 1,
            max_operations_per_entity: 1,
            max_internal_event_operations_per_entity: 1,
        });
        let connection = coordinator
            .register_connection("connection-a")
            .expect("register connection");
        let session = connection.open_session("session-a").expect("open session");
        let vm = session.open_vm("vm-a").expect("open VM");
        let metadata = vm_metadata("connection-a", "session-a", "vm-a");

        let ordinary = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect("ordinary VM operation fills its independent bound");
        let internal = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .map(expect_internal_admitted)
            .expect("internal event bypasses saturated ordinary admission");
        assert_eq!(vm.snapshot().active_operations, 2);

        let deferred = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .map(expect_internal_deferred)
            .expect("saturated internal event remains disposal-tracked");
        assert_eq!(vm.snapshot().active_operations, 3);
        let internal_limit = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .expect_err("deferred internal-event tracking remains bounded");
        assert_eq!(internal_limit.code(), "ERR_AGENTOS_COORDINATOR_LIMIT");
        assert!(
            internal_limit
                .to_string()
                .contains(INTERNAL_EVENT_OPERATION_LIMIT_PATH),
            "internal event rejection must name its independent configuration path"
        );

        drop(deferred);
        drop(internal);
        let lifecycle_cancel = OperationCancellation::new();
        let mut lifecycle = Box::pin(
            vm.begin_lifecycle(lifecycle_cancel)
                .expect("begin VM lifecycle")
                .wait(),
        );
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(lifecycle.as_mut().poll(&mut cx), Poll::Pending));
        assert_eq!(vm.snapshot().lifecycle, VmLifecyclePhase::Pending);

        let internal_during_lifecycle = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .map(expect_internal_admitted)
            .expect("internal event bypasses pending VM lifecycle exclusion");
        drop(ordinary);
        assert!(matches!(lifecycle.as_mut().poll(&mut cx), Poll::Pending));
        drop(internal_during_lifecycle);
        let lifecycle = lifecycle
            .await
            .expect("lifecycle starts after internal event registration drains");
        drop(lifecycle);

        let disposal_cancellation = OperationCancellation::new();
        let disposal_tracked = coordinator
            .admit_internal_vm_event(&metadata, disposal_cancellation.clone())
            .map(expect_internal_admitted)
            .expect("internal event registers for disposal");
        let disposal = coordinator
            .begin_vm_disposal(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                OperationCancellationReason::ConnectionClosed,
            )
            .expect("VM enters Closing");
        assert_eq!(
            disposal_cancellation.reason(),
            Some(OperationCancellationReason::ConnectionClosed),
            "VM disposal must cancel internal event work"
        );
        let rejected = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .expect_err("closing VM rejects new internal events");
        assert_eq!(rejected.code(), "ERR_AGENTOS_COORDINATOR_CLOSING");

        let mut drained = Box::pin(disposal.wait_drained());
        assert!(matches!(drained.as_mut().poll(&mut cx), Poll::Pending));
        drop(disposal_tracked);
        drained.as_mut().await;
        drop(drained);
        disposal.complete().expect("complete drained VM disposal");
        assert!(session.vm("vm-a").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deferred_internal_vm_event_is_cancelled_and_drained_by_disposal() {
        let coordinator = OwnershipCoordinator::new(OwnershipCoordinatorLimits {
            max_connections: 1,
            max_sessions_per_connection: 1,
            max_vms_per_session: 1,
            max_operations_per_entity: 1,
            max_internal_event_operations_per_entity: 1,
        });
        let connection = coordinator
            .register_connection("connection-a")
            .expect("register connection");
        let session = connection.open_session("session-a").expect("open session");
        let _vm = session.open_vm("vm-a").expect("open VM");
        let metadata = vm_metadata("connection-a", "session-a", "vm-a");
        let active_cancellation = OperationCancellation::new();
        let active = coordinator
            .admit_internal_vm_event(&metadata, active_cancellation.clone())
            .map(expect_internal_admitted)
            .expect("fill active internal-event capacity");
        let deferred_cancellation = OperationCancellation::new();
        let deferred = coordinator
            .admit_internal_vm_event(&metadata, deferred_cancellation.clone())
            .map(expect_internal_deferred)
            .expect("track exact deferred internal event");

        let disposal = coordinator
            .begin_vm_disposal(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                OperationCancellationReason::Explicit,
            )
            .expect("begin VM disposal");
        assert_eq!(
            active_cancellation.reason(),
            Some(OperationCancellationReason::Explicit)
        );
        assert_eq!(
            deferred_cancellation.reason(),
            Some(OperationCancellationReason::Explicit),
            "the deferred claimed event must be cancelled exactly like active work"
        );

        let mut drained = Box::pin(disposal.wait_drained());
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(drained.as_mut().poll(&mut cx), Poll::Pending));
        drop(active);
        assert!(
            matches!(drained.as_mut().poll(&mut cx), Poll::Pending),
            "disposal must not pass the exact deferred target"
        );
        drop(deferred);
        drained.as_mut().await;
        drop(drained);
        disposal
            .complete()
            .expect("deferred target release completes disposal");
        assert!(session.vm("vm-a").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_disposal_closes_admission_cancels_owned_work_and_drains() {
        let (coordinator, connection, session, vm_a, _) = configured();
        let cancellation = OperationCancellation::new();
        let operation = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                cancellation.clone(),
            )
            .await
            .expect("owned operation starts");
        let disposal = coordinator
            .begin_session_disposal(
                &OwnershipScope::session("connection-a", "session-a"),
                OperationCancellationReason::ConnectionClosed,
            )
            .expect("session enters Closing");
        assert_eq!(session.snapshot().phase, CoordinatorPhase::Closing);
        assert_eq!(vm_a.snapshot().phase, CoordinatorPhase::Closing);
        assert_eq!(
            cancellation.reason(),
            Some(OperationCancellationReason::ConnectionClosed)
        );

        let rejected = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect_err("closing session rejects new owned operations");
        assert_eq!(rejected.code(), "ERR_AGENTOS_COORDINATOR_CLOSING");

        let mut drained = Box::pin(disposal.wait_drained());
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(drained.as_mut().poll(&mut cx), Poll::Pending));
        drop(operation);
        drained.as_mut().await;
        drop(drained);
        disposal.complete().expect("complete drained disposal");
        assert!(connection.session("session-a").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_connection_ordering_key_cannot_address_owned_vm() {
        let (coordinator, _, _, _, _) = configured();
        let connection_b = coordinator
            .register_connection("connection-b")
            .expect("register connection B");
        let session_b = connection_b.open_session("session-a").expect("session B");
        session_b.open_vm("vm-a").expect("VM B");

        let metadata = RequestOperationMetadata::new(
            OwnershipScope::vm("connection-b", "session-a", "vm-a"),
            "forged operation",
            RequestOrderingKey::VmOperation {
                connection_id: String::from("connection-a"),
                session_id: String::from("session-a"),
                vm_id: String::from("vm-a"),
            },
        );
        let error = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect_err("ordering key cannot cross connection ownership");
        assert_eq!(error.code(), "ERR_AGENTOS_COORDINATOR_OWNERSHIP");
        assert_eq!(connection_b.snapshot().active_operations, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opaque_extension_ordering_keys_reject_only_same_connection_conflicts() {
        let (coordinator, _, _, _, _) = configured();
        coordinator
            .register_connection("connection-b")
            .expect("register connection B");

        let first = coordinator
            .admit(
                &extension_metadata("connection-a", b"same-key"),
                OperationCancellation::new(),
            )
            .await
            .expect("first keyed operation starts");
        let same_key = coordinator
            .admit(
                &extension_metadata("connection-a", b"same-key"),
                OperationCancellation::new(),
            )
            .await
            .expect_err("same connection and key conflict");
        assert_eq!(same_key.code(), "ERR_AGENTOS_ORDERING_CONFLICT");

        let different_key = coordinator
            .admit(
                &extension_metadata("connection-a", b"different-key"),
                OperationCancellation::new(),
            )
            .await
            .expect("different key progresses concurrently");
        let different_connection = coordinator
            .admit(
                &extension_metadata("connection-b", b"same-key"),
                OperationCancellation::new(),
            )
            .await
            .expect("same key is isolated by connection");

        drop(different_connection);
        drop(different_key);
        drop(first);
        coordinator
            .admit(
                &extension_metadata("connection-a", b"same-key"),
                OperationCancellation::new(),
            )
            .await
            .expect("key is released with terminal ownership permit");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opaque_extension_key_cannot_cross_connection_ownership() {
        let (coordinator, _, _, _, _) = configured();
        let metadata = RequestOperationMetadata::new(
            OwnershipScope::connection("connection-a"),
            "forged extension operation",
            RequestOrderingKey::Extension {
                namespace: String::from("test.extension"),
                connection_id: String::from("connection-b"),
                key: b"key".to_vec(),
                policy: ExtensionOrderingPolicy::CoreExclusive,
            },
        );
        let error = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect_err("extension ordering key cannot cross connection ownership");
        assert_eq!(error.code(), "ERR_AGENTOS_COORDINATOR_OWNERSHIP");
    }

    #[test]
    fn coordinator_membership_and_operation_state_are_bounded() {
        let coordinator = OwnershipCoordinator::new(OwnershipCoordinatorLimits {
            max_connections: 1,
            max_sessions_per_connection: 1,
            max_vms_per_session: 1,
            max_operations_per_entity: 1,
            max_internal_event_operations_per_entity: 1,
        });
        let connection = coordinator
            .register_connection("connection-a")
            .expect("first connection");
        let error = coordinator
            .register_connection("connection-b")
            .expect_err("connection bound");
        assert_eq!(error.code(), "ERR_AGENTOS_COORDINATOR_LIMIT");
        assert!(error.to_string().contains(CONNECTION_LIMIT_PATH));

        let active = connection
            .register_operation(OperationCancellation::new())
            .expect("first connection operation");
        let error = connection
            .register_operation(OperationCancellation::new())
            .expect_err("per-entity operation bound");
        assert!(error.to_string().contains(IN_FLIGHT_REQUEST_COUNT_PATH));
        drop(active);

        let session = connection.open_session("session-a").expect("first session");
        let error = connection
            .open_session("session-b")
            .expect_err("session bound");
        assert!(error.to_string().contains(SESSION_LIMIT_PATH));
        session.open_vm("vm-a").expect("first VM");
        let error = session.open_vm("vm-b").expect_err("VM bound");
        assert!(error.to_string().contains(VM_LIMIT_PATH));
    }
}
