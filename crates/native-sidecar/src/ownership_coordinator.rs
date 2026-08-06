//! Cloneable connection/session/VM coordination handles.
//!
//! The coordinator owns only bounded identity indexes, short admission state,
//! and cancellation registrations. It never performs guest work, adapter I/O,
//! filesystem/network I/O, output writes, or an external wait while holding a
//! mutex. A request keeps the returned permit while it runs; every actual
//! access to mutable VM state remains a separate short service command.

use crate::request_operations::{
    OperationCancellation, OperationCancellationReason, OperationTable, RequestOperationKey,
    RequestOperationMetadata, ScopeOperationDrain, VmConcurrencyClass,
    IN_FLIGHT_REQUEST_COUNT_PATH,
};
use crate::wire::OwnershipScope;
use agentos_runtime::RuntimeConfig;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::Notify;

const CONNECTION_LIMIT_PATH: &str = "runtime.resources.maxConnections";
const SESSION_LIMIT_PATH: &str = "runtime.protocol.maxSessionsPerConnection";
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
            max_sessions_per_connection: config.protocol.max_sessions_per_connection,
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
    #[cfg(test)]
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VmLifecyclePhase {
    Idle,
    Pending,
    Active,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntityCoordinatorSnapshot {
    pub(crate) phase: CoordinatorPhase,
    pub(crate) active_operations: usize,
    pub(crate) child_count: usize,
}

#[cfg(test)]
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
            Self::Cancelled { .. } => "ERR_AGENTOS_COORDINATOR_CANCELLED",
            Self::NotDrained { .. } => "ERR_AGENTOS_COORDINATOR_NOT_DRAINED",
        }
    }

    pub(crate) fn configuration_path(&self) -> Option<&'static str> {
        match self {
            Self::Limit {
                configuration_path, ..
            } => Some(configuration_path),
            _ => None,
        }
    }

    pub(crate) fn retryable(&self) -> bool {
        matches!(self, Self::Limit { .. } | Self::LifecycleConflict { .. })
    }

    pub(crate) fn errno(&self) -> &'static str {
        match self {
            Self::Limit { .. } | Self::LifecycleConflict { .. } => "EAGAIN",
            Self::Duplicate { .. } => "EEXIST",
            Self::NotFound { .. } => "ENOENT",
            Self::OwnershipMismatch { .. } => "EACCES",
            Self::Closing { .. } => "ESHUTDOWN",
            Self::Cancelled { .. } => "ECANCELED",
            Self::NotDrained { .. } => "EBUSY",
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

#[derive(Debug)]
struct OwnershipCoordinatorState {
    next_generation: u64,
    connections: BTreeMap<String, ConnectionRecord>,
}

impl Default for OwnershipCoordinatorState {
    fn default() -> Self {
        Self {
            next_generation: 1,
            connections: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct ConnectionRecord {
    generation: u64,
    phase: CoordinatorPhase,
    sessions: BTreeMap<String, SessionRecord>,
}

#[derive(Debug)]
struct SessionRecord {
    generation: u64,
    phase: CoordinatorPhase,
    vms: BTreeMap<String, VmRecord>,
}

#[derive(Debug)]
struct VmRecord {
    generation: u64,
    phase: CoordinatorPhase,
    gate: Arc<VmLifecycleGate>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConnectionCoordinator {
    root: Weak<OwnershipCoordinatorInner>,
    connection_id: String,
    generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionCoordinator {
    root: Weak<OwnershipCoordinatorInner>,
    connection_id: String,
    connection_generation: u64,
    session_id: String,
    generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct VmCoordinator {
    root: Weak<OwnershipCoordinatorInner>,
    connection_id: String,
    connection_generation: u64,
    session_id: String,
    session_generation: u64,
    vm_id: String,
    generation: u64,
    gate: Arc<VmLifecycleGate>,
}

/// Explicit per-VM lifecycle admission gate.
///
/// The state machine is `Idle -> Pending -> Active -> Idle`. The transition to
/// `Pending` is the linearization point that closes ordinary VM admission;
/// operations registered before it drain, while later ordinary requests get
/// `ERR_AGENTOS_VM_LIFECYCLE_CONFLICT`. `Pending -> Active` occurs only after
/// every ordinary and internal permit has drained. Progress-critical internal
/// settlement events have separate bounded admission during `Pending`; during
/// `Active` they remain durably claimed but cannot mutate the VM.
///
/// This is not a standard-library or Tokio `RwLock`. A standard lock cannot
/// cross `.await` without blocking a runtime worker. A Tokio `RwLock` would put
/// new ordinary work into an implicit waiter queue after lifecycle admission,
/// while this protocol must reject that work immediately. Neither lock models
/// the distinct bounded internal-settlement class, cancellation generations,
/// disposal closure, or the active counts needed for a safe drain. The mutex
/// here protects only non-suspending state transitions and is released before
/// waiting. A short global membership mutex is acceptable for the same reason:
/// the forbidden design is a lock held across request execution, not a lock
/// around bounded map and counter transitions.
#[derive(Debug)]
struct VmLifecycleGate {
    connection_id: String,
    session_id: String,
    vm_id: String,
    limits: OwnershipCoordinatorLimits,
    state: Mutex<VmGateState>,
    changed: Notify,
}

#[derive(Debug)]
struct VmGateState {
    closing: bool,
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
        let mut state = lock(&self.inner.state, "ownership membership");
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
        let generation = take_id(&mut state.next_generation);
        let connection = ConnectionCoordinator {
            root: Arc::downgrade(&self.inner),
            connection_id: connection_id.clone(),
            generation,
        };
        state.connections.insert(
            connection_id,
            ConnectionRecord {
                generation,
                phase: CoordinatorPhase::Open,
                sessions: BTreeMap::new(),
            },
        );
        Ok(connection)
    }

    pub(crate) fn connection(
        &self,
        connection_id: &str,
    ) -> Result<ConnectionCoordinator, OwnershipCoordinatorError> {
        let state = lock(&self.inner.state, "ownership membership");
        let record = state.connections.get(connection_id).ok_or_else(|| {
            OwnershipCoordinatorError::NotFound {
                scope: "connection",
                id: connection_id.to_owned(),
            }
        })?;
        Ok(ConnectionCoordinator {
            root: Arc::downgrade(&self.inner),
            connection_id: connection_id.to_owned(),
            generation: record.generation,
        })
    }

    pub(crate) async fn admit(
        &self,
        metadata: &RequestOperationMetadata,
        cancellation: OperationCancellation,
    ) -> Result<CoordinatorOperationPermit, OwnershipCoordinatorError> {
        validate_vm_concurrency_ownership(&metadata.ownership, &metadata.vm_concurrency)?;
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut permit = CoordinatorOperationPermit {
            vm_gate: None,
            vm_lifecycle: None,
        };
        let lifecycle_admission = {
            // Membership validation and gate admission share one lock order:
            // membership, then the selected VM gate. Disposal uses the same
            // order, so an operation is either admitted against one coherent
            // entity generation or observes Closing; it cannot slip between a
            // path check and gate registration.
            let state = lock(&self.inner.state, "ownership membership");
            let gate = validate_ownership_locked(&state, &metadata.ownership)?;
            match &metadata.vm_concurrency {
                VmConcurrencyClass::SharedVm => {
                    permit.vm_gate = Some(
                        gate.expect("shared VM concurrency requires VM ownership")
                            .register_operation(cancellation.clone())?,
                    );
                    None
                }
                VmConcurrencyClass::ExclusiveVmLifecycle => Some(
                    gate.expect("exclusive VM lifecycle concurrency requires VM ownership")
                        .begin_lifecycle(cancellation.clone())?,
                ),
                VmConcurrencyClass::OwnershipOnly => None,
            }
        };
        if let Some(admission) = lifecycle_admission {
            permit.vm_lifecycle = Some(admission.wait().await?);
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
        validate_vm_concurrency_ownership(&metadata.ownership, &metadata.vm_concurrency)?;
        if !matches!(&metadata.ownership, OwnershipScope::VmOwnership(_))
            || !matches!(&metadata.vm_concurrency, VmConcurrencyClass::SharedVm)
        {
            return Err(OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("VM ownership with shared-VM concurrency"),
                actual: format!(
                    "{}/{}",
                    ownership_label(&metadata.ownership),
                    vm_concurrency_label(&metadata.vm_concurrency)
                ),
            });
        }
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let (gate_permit, class) = {
            let state = lock(&self.inner.state, "ownership membership");
            let gate = validate_ownership_locked(&state, &metadata.ownership)?
                .expect("internal VM event requires VM ownership");
            gate.register_internal(cancellation.clone())?
        };
        let permit = CoordinatorOperationPermit {
            vm_gate: Some(gate_permit),
            vm_lifecycle: None,
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

    pub(crate) fn begin_vm_disposal(
        &self,
        ownership: &OwnershipScope,
        reason: OperationCancellationReason,
    ) -> Result<VmDisposal, OwnershipCoordinatorError> {
        let (connection_id, session_id, vm_id) = ownership_ids(ownership);
        let (Some(session_id), Some(vm_id)) = (session_id, vm_id) else {
            return Err(OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("VM ownership"),
                actual: ownership_label(ownership),
            });
        };
        self.connection(connection_id)?
            .session(session_id)?
            .vm(vm_id)?
            .begin_disposal(reason, None)
    }

    /// Begin VM disposal while closing the same ownership subtree in the
    /// authoritative operation table. The dispose request is excluded from its
    /// own drain; every other ordinary or progress operation is cancelled and
    /// must release before teardown proceeds.
    pub(crate) fn begin_vm_disposal_with_operations(
        &self,
        ownership: &OwnershipScope,
        reason: OperationCancellationReason,
        operations: &OperationTable,
        excluded: &RequestOperationKey,
    ) -> Result<VmDisposal, OwnershipCoordinatorError> {
        // Close membership/gate admission first. A request that reaches the
        // operation table in the tiny interval before scope closure still
        // fails the coherent membership check and cannot touch the VM.
        let mut disposal = self.begin_vm_disposal(ownership, reason)?;
        disposal.operation_drain =
            Some(operations.close_scope(ownership.clone(), reason, Some(excluded)));
        Ok(disposal)
    }

    pub(crate) fn begin_connection_disposal(
        &self,
        connection_id: &str,
        reason: OperationCancellationReason,
    ) -> Result<ConnectionDisposal, OwnershipCoordinatorError> {
        self.connection(connection_id)?.begin_disposal(reason)
    }
}

impl ConnectionCoordinator {
    pub(crate) fn open_session(
        &self,
        session_id: impl Into<String>,
    ) -> Result<SessionCoordinator, OwnershipCoordinatorError> {
        let session_id = session_id.into();
        let root = upgrade_root(&self.root, "connection", &self.connection_id)?;
        let mut state = lock(&root.state, "ownership membership");
        let connection = matching_connection_mut(&mut state, self)?;
        ensure_open(
            connection.phase,
            format!("connection {}", self.connection_id),
        )?;
        if connection.sessions.contains_key(&session_id) {
            return Err(OwnershipCoordinatorError::Duplicate {
                scope: "session",
                id: format!("{}:{session_id}", self.connection_id),
            });
        }
        check_limit(
            "session coordinators per connection",
            connection.sessions.len(),
            root.limits.max_sessions_per_connection,
            SESSION_LIMIT_PATH,
        )?;
        let generation = take_id(&mut state.next_generation);
        matching_connection_mut(&mut state, self)?.sessions.insert(
            session_id.clone(),
            SessionRecord {
                generation,
                phase: CoordinatorPhase::Open,
                vms: BTreeMap::new(),
            },
        );
        let session = SessionCoordinator {
            root: Arc::downgrade(&root),
            connection_id: self.connection_id.clone(),
            connection_generation: self.generation,
            session_id,
            generation,
        };
        Ok(session)
    }

    pub(crate) fn session(
        &self,
        session_id: &str,
    ) -> Result<SessionCoordinator, OwnershipCoordinatorError> {
        let root = upgrade_root(&self.root, "connection", &self.connection_id)?;
        let state = lock(&root.state, "ownership membership");
        let connection = matching_connection(&state, self)?;
        let session = connection.sessions.get(session_id).ok_or_else(|| {
            OwnershipCoordinatorError::NotFound {
                scope: "session",
                id: format!("{}:{session_id}", self.connection_id),
            }
        })?;
        Ok(SessionCoordinator {
            root: Arc::downgrade(&root),
            connection_id: self.connection_id.clone(),
            connection_generation: self.generation,
            session_id: session_id.to_owned(),
            generation: session.generation,
        })
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> EntityCoordinatorSnapshot {
        let Some(root) = self.root.upgrade() else {
            return closed_entity_snapshot();
        };
        let state = lock(&root.state, "ownership membership");
        let Ok(connection) = matching_connection(&state, self) else {
            return closed_entity_snapshot();
        };
        EntityCoordinatorSnapshot {
            phase: connection.phase,
            active_operations: connection
                .sessions
                .values()
                .flat_map(|session| session.vms.values())
                .map(|vm| vm.gate.active_operations())
                .sum(),
            child_count: connection.sessions.len(),
        }
    }

    pub(crate) fn begin_disposal(
        &self,
        reason: OperationCancellationReason,
    ) -> Result<ConnectionDisposal, OwnershipCoordinatorError> {
        let root = upgrade_root(&self.root, "connection", &self.connection_id)?;
        let gates = {
            let mut state = lock(&root.state, "ownership membership");
            let connection = matching_connection_mut(&mut state, self)?;
            ensure_open(
                connection.phase,
                format!("connection {}", self.connection_id),
            )?;
            connection.phase = CoordinatorPhase::Closing;
            let mut gates = Vec::new();
            for session in connection.sessions.values_mut() {
                session.phase = CoordinatorPhase::Closing;
                for vm in session.vms.values_mut() {
                    vm.phase = CoordinatorPhase::Closing;
                    vm.gate.begin_closing(reason);
                    gates.push(Arc::clone(&vm.gate));
                }
            }
            gates
        };
        Ok(ConnectionDisposal {
            root,
            connection_id: self.connection_id.clone(),
            generation: self.generation,
            gates,
            completed: false,
        })
    }
}

impl SessionCoordinator {
    pub(crate) fn open_vm(
        &self,
        vm_id: impl Into<String>,
    ) -> Result<VmCoordinator, OwnershipCoordinatorError> {
        let vm_id = vm_id.into();
        let root = upgrade_root(&self.root, "session", &self.label())?;
        let mut state = lock(&root.state, "ownership membership");
        {
            let session = matching_session(&state, self)?;
            ensure_open(session.phase, self.label())?;
            if session.vms.contains_key(&vm_id) {
                return Err(OwnershipCoordinatorError::Duplicate {
                    scope: "VM",
                    id: format!("{}:{vm_id}", self.label()),
                });
            }
            check_limit(
                "VM coordinators per session",
                session.vms.len(),
                root.limits.max_vms_per_session,
                VM_LIMIT_PATH,
            )?;
        }
        let generation = take_id(&mut state.next_generation);
        let gate = Arc::new(VmLifecycleGate {
            connection_id: self.connection_id.clone(),
            session_id: self.session_id.clone(),
            vm_id: vm_id.clone(),
            limits: root.limits,
            state: Mutex::new(VmGateState {
                closing: false,
                operations: BTreeMap::new(),
                next_operation_id: 1,
                next_lifecycle_id: 1,
                lifecycle: VmLifecycleState::Idle,
            }),
            changed: Notify::new(),
        });
        matching_session_mut(&mut state, self)?.vms.insert(
            vm_id.clone(),
            VmRecord {
                generation,
                phase: CoordinatorPhase::Open,
                gate: Arc::clone(&gate),
            },
        );
        Ok(VmCoordinator {
            root: Arc::downgrade(&root),
            connection_id: self.connection_id.clone(),
            connection_generation: self.connection_generation,
            session_id: self.session_id.clone(),
            session_generation: self.generation,
            vm_id,
            generation,
            gate,
        })
    }

    pub(crate) fn vm(&self, vm_id: &str) -> Result<VmCoordinator, OwnershipCoordinatorError> {
        let root = upgrade_root(&self.root, "session", &self.label())?;
        let state = lock(&root.state, "ownership membership");
        let session = matching_session(&state, self)?;
        let vm = session
            .vms
            .get(vm_id)
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "VM",
                id: format!("{}:{vm_id}", self.label()),
            })?;
        Ok(VmCoordinator {
            root: Arc::downgrade(&root),
            connection_id: self.connection_id.clone(),
            connection_generation: self.connection_generation,
            session_id: self.session_id.clone(),
            session_generation: self.generation,
            vm_id: vm_id.to_owned(),
            generation: vm.generation,
            gate: Arc::clone(&vm.gate),
        })
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> EntityCoordinatorSnapshot {
        let Some(root) = self.root.upgrade() else {
            return closed_entity_snapshot();
        };
        let state = lock(&root.state, "ownership membership");
        let Ok(session) = matching_session(&state, self) else {
            return closed_entity_snapshot();
        };
        EntityCoordinatorSnapshot {
            phase: session.phase,
            active_operations: session
                .vms
                .values()
                .map(|vm| vm.gate.active_operations())
                .sum(),
            child_count: session.vms.len(),
        }
    }

    fn label(&self) -> String {
        format!("session {}:{}", self.connection_id, self.session_id)
    }
}

impl VmCoordinator {
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> VmCoordinatorSnapshot {
        let phase = self
            .root
            .upgrade()
            .and_then(|root| {
                let state = lock(&root.state, "ownership membership");
                matching_vm(&state, self).ok().map(|vm| vm.phase)
            })
            .unwrap_or(CoordinatorPhase::Closed);
        let state = lock(&self.gate.state, "VM lifecycle gate");
        VmCoordinatorSnapshot {
            phase,
            active_operations: state.operations.len(),
            lifecycle: state.lifecycle.phase(),
        }
    }

    #[cfg(test)]
    fn begin_lifecycle(
        &self,
        cancellation: OperationCancellation,
    ) -> Result<VmLifecycleAdmission, OwnershipCoordinatorError> {
        let root = upgrade_root(&self.root, "VM", &self.label())?;
        let state = lock(&root.state, "ownership membership");
        let vm = matching_vm(&state, self)?;
        ensure_open(vm.phase, self.label())?;
        self.gate.begin_lifecycle(cancellation)
    }

    pub(crate) fn begin_disposal(
        &self,
        reason: OperationCancellationReason,
        operation_drain: Option<ScopeOperationDrain>,
    ) -> Result<VmDisposal, OwnershipCoordinatorError> {
        let root = upgrade_root(&self.root, "VM", &self.label())?;
        {
            let mut state = lock(&root.state, "ownership membership");
            let vm = matching_vm_mut(&mut state, self)?;
            ensure_open(vm.phase, self.label())?;
            vm.phase = CoordinatorPhase::Closing;
            vm.gate.begin_closing(reason);
        }
        Ok(VmDisposal {
            root,
            connection_id: self.connection_id.clone(),
            connection_generation: self.connection_generation,
            session_id: self.session_id.clone(),
            session_generation: self.session_generation,
            vm_id: self.vm_id.clone(),
            generation: self.generation,
            gate: Arc::clone(&self.gate),
            operation_drain,
            completed: false,
        })
    }

    fn label(&self) -> String {
        format!(
            "VM {}:{}:{}",
            self.connection_id, self.session_id, self.vm_id
        )
    }
}

impl VmLifecycleGate {
    fn label(&self) -> String {
        format!(
            "VM {}:{}:{}",
            self.connection_id, self.session_id, self.vm_id
        )
    }

    fn active_operations(&self) -> usize {
        let state = lock(&self.state, "VM lifecycle gate");
        state.operations.len() + usize::from(!matches!(state.lifecycle, VmLifecycleState::Idle))
    }

    async fn wait_drained(&self) {
        loop {
            let changed = self.changed.notified();
            if self.active_operations() == 0 {
                return;
            }
            changed.await;
        }
    }

    fn register_operation(
        self: &Arc<Self>,
        cancellation: OperationCancellation,
    ) -> Result<VmGatePermit, OwnershipCoordinatorError> {
        self.register_operation_with_class(cancellation, OperationAdmissionClass::Ordinary)
    }

    fn register_internal(
        self: &Arc<Self>,
        cancellation: OperationCancellation,
    ) -> Result<(VmGatePermit, OperationAdmissionClass), OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut state = lock(&self.state, "VM lifecycle gate");
        if state.closing {
            return Err(OwnershipCoordinatorError::Closing {
                scope: self.label(),
                phase: CoordinatorPhase::Closing,
            });
        }
        let class = if matches!(state.lifecycle, VmLifecycleState::Active { .. })
            || operation_count(&state.operations, OperationAdmissionClass::InternalEvent)
                >= self.limits.max_internal_event_operations_per_entity
        {
            OperationAdmissionClass::DeferredInternalEvent
        } else {
            OperationAdmissionClass::InternalEvent
        };
        check_operation_limit(
            &state.operations,
            class,
            self.limits,
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
        Ok((
            VmGatePermit {
                gate: Arc::clone(self),
                id,
            },
            class,
        ))
    }

    fn register_operation_with_class(
        self: &Arc<Self>,
        cancellation: OperationCancellation,
        class: OperationAdmissionClass,
    ) -> Result<VmGatePermit, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let mut state = lock(&self.state, "VM lifecycle gate");
        if state.closing {
            return Err(OwnershipCoordinatorError::Closing {
                scope: self.label(),
                phase: CoordinatorPhase::Closing,
            });
        }
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
            self.limits,
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
        Ok(VmGatePermit {
            gate: Arc::clone(self),
            id,
        })
    }

    fn begin_lifecycle(
        self: &Arc<Self>,
        cancellation: OperationCancellation,
    ) -> Result<VmLifecycleAdmission, OwnershipCoordinatorError> {
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        let id = {
            let mut state = lock(&self.state, "VM lifecycle gate");
            if state.closing {
                return Err(OwnershipCoordinatorError::Closing {
                    scope: self.label(),
                    phase: CoordinatorPhase::Closing,
                });
            }
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
            gate: Arc::clone(self),
            id,
            cancellation,
            armed: true,
        })
    }

    fn begin_closing(&self, reason: OperationCancellationReason) {
        let mut state = lock(&self.state, "VM lifecycle gate");
        state.closing = true;
        signal_all(&state.operations, reason);
        state.lifecycle.signal(reason);
        self.changed.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) struct CoordinatorOperationPermit {
    vm_lifecycle: Option<VmLifecycleGuard>,
    vm_gate: Option<VmGatePermit>,
}

impl CoordinatorOperationPermit {
    #[cfg(test)]
    pub(crate) fn is_vm_lifecycle(&self) -> bool {
        self.vm_lifecycle.is_some()
    }

    /// Promote one already-tracked internal event into active service
    /// capacity. A false result keeps the same ownership registrations live;
    /// disposal can therefore cancel and drain the event while it waits.
    pub(crate) fn try_activate_deferred_internal_event(
        &mut self,
    ) -> Result<bool, OwnershipCoordinatorError> {
        let permit =
            self.vm_gate
                .as_ref()
                .ok_or_else(|| OwnershipCoordinatorError::OwnershipMismatch {
                    expected: String::from("VM-bound internal-event permit"),
                    actual: String::from("permit without VM gate admission"),
                })?;
        let mut vm_state = lock(&permit.gate.state, "VM lifecycle gate");
        let cancellation = vm_state
            .operations
            .get(&permit.id)
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "VM gate permit",
                id: permit.id.to_string(),
            })?
            .cancellation
            .clone();
        if let Some(reason) = cancellation.reason() {
            return Err(OwnershipCoordinatorError::Cancelled { reason });
        }
        if vm_state.closing {
            return Err(OwnershipCoordinatorError::Closing {
                scope: permit.gate.label(),
                phase: CoordinatorPhase::Closing,
            });
        }
        if matches!(vm_state.lifecycle, VmLifecycleState::Active { .. }) {
            return Ok(false);
        }
        let class = vm_state
            .operations
            .get(&permit.id)
            .map(|operation| operation.class);
        if class == Some(OperationAdmissionClass::InternalEvent) {
            return Ok(true);
        }
        if class != Some(OperationAdmissionClass::DeferredInternalEvent) {
            return Err(OwnershipCoordinatorError::OwnershipMismatch {
                expected: String::from("deferred internal-event registration"),
                actual: format!("registration class {class:?}"),
            });
        }

        let limit = permit.gate.limits.max_internal_event_operations_per_entity;
        let has_capacity =
            operation_count(&vm_state.operations, OperationAdmissionClass::InternalEvent) < limit;
        if !has_capacity {
            return Ok(false);
        }
        vm_state
            .operations
            .get_mut(&permit.id)
            .expect("deferred VM registration checked above")
            .class = OperationAdmissionClass::InternalEvent;
        Ok(true)
    }
}

#[derive(Debug)]
struct VmGatePermit {
    gate: Arc<VmLifecycleGate>,
    id: u64,
}

impl Drop for VmGatePermit {
    fn drop(&mut self) {
        let mut state = lock(&self.gate.state, "VM lifecycle gate");
        if state.operations.remove(&self.id).is_some() {
            self.gate.changed.notify_waiters();
        }
    }
}

#[derive(Debug)]
struct VmLifecycleAdmission {
    gate: Arc<VmLifecycleGate>,
    id: u64,
    cancellation: OperationCancellation,
    armed: bool,
}

impl VmLifecycleAdmission {
    async fn wait(mut self) -> Result<VmLifecycleGuard, OwnershipCoordinatorError> {
        loop {
            // Create the notification future before checking state. A permit
            // released after this point either changes the state we observe or
            // wakes this registered listener, avoiding a check-then-sleep lost
            // wakeup. `Notify` is only a hint: the state and lifecycle id remain
            // authoritative, so spurious or coalesced wakes are harmless.
            let changed = self.gate.changed.notified();
            {
                let mut state = lock(&self.gate.state, "VM lifecycle gate");
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
                                gate: Arc::clone(&self.gate),
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
            release_lifecycle(&self.gate, self.id);
        }
    }
}

#[derive(Debug)]
struct VmLifecycleGuard {
    gate: Arc<VmLifecycleGate>,
    id: u64,
}

impl Drop for VmLifecycleGuard {
    fn drop(&mut self) {
        release_lifecycle(&self.gate, self.id);
    }
}

fn release_lifecycle(gate: &Arc<VmLifecycleGate>, id: u64) {
    let mut state = lock(&gate.state, "VM lifecycle gate");
    // The id prevents a stale dropped admission/guard from reopening a newer
    // lifecycle generation. Both cancellation before activation and normal
    // completion converge here through RAII.
    let matches_id = match &state.lifecycle {
        VmLifecycleState::Pending { id: active, .. }
        | VmLifecycleState::Active { id: active, .. } => *active == id,
        VmLifecycleState::Idle => false,
    };
    if matches_id {
        state.lifecycle = VmLifecycleState::Idle;
        gate.changed.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) struct VmDisposal {
    root: Arc<OwnershipCoordinatorInner>,
    connection_id: String,
    connection_generation: u64,
    session_id: String,
    session_generation: u64,
    vm_id: String,
    generation: u64,
    gate: Arc<VmLifecycleGate>,
    operation_drain: Option<ScopeOperationDrain>,
    completed: bool,
}

impl VmDisposal {
    pub(crate) async fn wait_drained(&self) {
        if let Some(drain) = &self.operation_drain {
            drain.wait_drained().await;
        }
        self.gate.wait_drained().await;
    }

    pub(crate) fn complete(mut self) -> Result<(), OwnershipCoordinatorError> {
        if let Some(drain) = &self.operation_drain {
            let active_operations = drain.active_operations();
            if active_operations != 0 {
                return Err(OwnershipCoordinatorError::NotDrained {
                    scope: self.gate.label(),
                    active_operations,
                });
            }
        }
        let active_operations = self.gate.active_operations();
        if active_operations != 0 {
            return Err(OwnershipCoordinatorError::NotDrained {
                scope: self.gate.label(),
                active_operations,
            });
        }
        let mut state = lock(&self.root.state, "ownership membership");
        let connection = state
            .connections
            .get_mut(&self.connection_id)
            .filter(|connection| connection.generation == self.connection_generation)
            .ok_or_else(|| stale_generation("connection", &self.connection_id))?;
        let session = connection
            .sessions
            .get_mut(&self.session_id)
            .filter(|session| session.generation == self.session_generation)
            .ok_or_else(|| stale_generation("session", &self.session_id))?;
        let vm = session
            .vms
            .get(&self.vm_id)
            .filter(|vm| vm.generation == self.generation && Arc::ptr_eq(&vm.gate, &self.gate))
            .ok_or_else(|| stale_generation("VM", &self.vm_id))?;
        ensure_closing(vm.phase, self.gate.label())?;
        session.vms.remove(&self.vm_id);
        self.completed = true;
        Ok(())
    }
}

impl Drop for VmDisposal {
    fn drop(&mut self) {
        if !self.completed {
            tracing::warn!(
                connection_id = %self.connection_id,
                session_id = %self.session_id,
                vm_id = %self.vm_id,
                "ERR_AGENTOS_VM_DISPOSAL_INCOMPLETE: VM remains Closing for a bounded retry"
            );
        }
    }
}

#[derive(Debug)]
pub(crate) struct ConnectionDisposal {
    root: Arc<OwnershipCoordinatorInner>,
    connection_id: String,
    generation: u64,
    gates: Vec<Arc<VmLifecycleGate>>,
    completed: bool,
}

impl ConnectionDisposal {
    pub(crate) async fn wait_drained(&self) {
        for gate in &self.gates {
            gate.wait_drained().await;
        }
    }

    pub(crate) fn complete(mut self) -> Result<(), OwnershipCoordinatorError> {
        let active_operations = self.gates.iter().map(|gate| gate.active_operations()).sum();
        if active_operations != 0 {
            return Err(OwnershipCoordinatorError::NotDrained {
                scope: format!("connection {}", self.connection_id),
                active_operations,
            });
        }
        let mut state = lock(&self.root.state, "ownership membership");
        let connection = state
            .connections
            .get(&self.connection_id)
            .filter(|connection| connection.generation == self.generation)
            .ok_or_else(|| stale_generation("connection", &self.connection_id))?;
        ensure_closing(
            connection.phase,
            format!("connection {}", self.connection_id),
        )?;
        state.connections.remove(&self.connection_id);
        self.completed = true;
        Ok(())
    }
}

impl Drop for ConnectionDisposal {
    fn drop(&mut self) {
        if !self.completed {
            tracing::warn!(
                connection_id = %self.connection_id,
                "ERR_AGENTOS_CONNECTION_DISPOSAL_INCOMPLETE: connection remains Closing for a bounded retry"
            );
        }
    }
}

fn upgrade_root(
    root: &Weak<OwnershipCoordinatorInner>,
    scope: &'static str,
    id: &str,
) -> Result<Arc<OwnershipCoordinatorInner>, OwnershipCoordinatorError> {
    root.upgrade()
        .ok_or_else(|| OwnershipCoordinatorError::NotFound {
            scope,
            id: id.to_owned(),
        })
}

fn stale_generation(scope: &'static str, id: &str) -> OwnershipCoordinatorError {
    OwnershipCoordinatorError::NotFound {
        scope,
        id: format!("{id} (stale generation)"),
    }
}

fn matching_connection<'a>(
    state: &'a OwnershipCoordinatorState,
    handle: &ConnectionCoordinator,
) -> Result<&'a ConnectionRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get(&handle.connection_id)
        .filter(|connection| connection.generation == handle.generation)
        .ok_or_else(|| stale_generation("connection", &handle.connection_id))
}

fn matching_connection_mut<'a>(
    state: &'a mut OwnershipCoordinatorState,
    handle: &ConnectionCoordinator,
) -> Result<&'a mut ConnectionRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get_mut(&handle.connection_id)
        .filter(|connection| connection.generation == handle.generation)
        .ok_or_else(|| stale_generation("connection", &handle.connection_id))
}

fn matching_session<'a>(
    state: &'a OwnershipCoordinatorState,
    handle: &SessionCoordinator,
) -> Result<&'a SessionRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get(&handle.connection_id)
        .filter(|connection| connection.generation == handle.connection_generation)
        .and_then(|connection| connection.sessions.get(&handle.session_id))
        .filter(|session| session.generation == handle.generation)
        .ok_or_else(|| stale_generation("session", &handle.session_id))
}

fn matching_session_mut<'a>(
    state: &'a mut OwnershipCoordinatorState,
    handle: &SessionCoordinator,
) -> Result<&'a mut SessionRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get_mut(&handle.connection_id)
        .filter(|connection| connection.generation == handle.connection_generation)
        .and_then(|connection| connection.sessions.get_mut(&handle.session_id))
        .filter(|session| session.generation == handle.generation)
        .ok_or_else(|| stale_generation("session", &handle.session_id))
}

#[cfg(test)]
fn matching_vm<'a>(
    state: &'a OwnershipCoordinatorState,
    handle: &VmCoordinator,
) -> Result<&'a VmRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get(&handle.connection_id)
        .filter(|connection| connection.generation == handle.connection_generation)
        .and_then(|connection| connection.sessions.get(&handle.session_id))
        .filter(|session| session.generation == handle.session_generation)
        .and_then(|session| session.vms.get(&handle.vm_id))
        .filter(|vm| vm.generation == handle.generation && Arc::ptr_eq(&vm.gate, &handle.gate))
        .ok_or_else(|| stale_generation("VM", &handle.vm_id))
}

fn matching_vm_mut<'a>(
    state: &'a mut OwnershipCoordinatorState,
    handle: &VmCoordinator,
) -> Result<&'a mut VmRecord, OwnershipCoordinatorError> {
    state
        .connections
        .get_mut(&handle.connection_id)
        .filter(|connection| connection.generation == handle.connection_generation)
        .and_then(|connection| connection.sessions.get_mut(&handle.session_id))
        .filter(|session| session.generation == handle.session_generation)
        .and_then(|session| session.vms.get_mut(&handle.vm_id))
        .filter(|vm| vm.generation == handle.generation && Arc::ptr_eq(&vm.gate, &handle.gate))
        .ok_or_else(|| stale_generation("VM", &handle.vm_id))
}

fn validate_ownership_locked(
    state: &OwnershipCoordinatorState,
    ownership: &OwnershipScope,
) -> Result<Option<Arc<VmLifecycleGate>>, OwnershipCoordinatorError> {
    let (connection_id, session_id, vm_id) = ownership_ids(ownership);
    let connection = state.connections.get(connection_id).ok_or_else(|| {
        OwnershipCoordinatorError::NotFound {
            scope: "connection",
            id: connection_id.to_owned(),
        }
    })?;
    ensure_open(connection.phase, format!("connection {connection_id}"))?;
    let Some(session_id) = session_id else {
        return Ok(None);
    };
    let session =
        connection
            .sessions
            .get(session_id)
            .ok_or_else(|| OwnershipCoordinatorError::NotFound {
                scope: "session",
                id: format!("{connection_id}:{session_id}"),
            })?;
    ensure_open(
        session.phase,
        format!("session {connection_id}:{session_id}"),
    )?;
    let Some(vm_id) = vm_id else {
        return Ok(None);
    };
    let vm = session
        .vms
        .get(vm_id)
        .ok_or_else(|| OwnershipCoordinatorError::NotFound {
            scope: "VM",
            id: format!("{connection_id}:{session_id}:{vm_id}"),
        })?;
    ensure_open(vm.phase, format!("VM {connection_id}:{session_id}:{vm_id}"))?;
    Ok(Some(Arc::clone(&vm.gate)))
}

#[cfg(test)]
fn closed_entity_snapshot() -> EntityCoordinatorSnapshot {
    EntityCoordinatorSnapshot {
        phase: CoordinatorPhase::Closed,
        active_operations: 0,
        child_count: 0,
    }
}

fn ensure_closing(phase: CoordinatorPhase, scope: String) -> Result<(), OwnershipCoordinatorError> {
    if phase == CoordinatorPhase::Closing {
        Ok(())
    } else {
        Err(OwnershipCoordinatorError::Closing { scope, phase })
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

fn validate_vm_concurrency_ownership(
    ownership: &OwnershipScope,
    vm_concurrency: &VmConcurrencyClass,
) -> Result<(), OwnershipCoordinatorError> {
    let valid = match vm_concurrency {
        VmConcurrencyClass::OwnershipOnly => true,
        VmConcurrencyClass::SharedVm | VmConcurrencyClass::ExclusiveVmLifecycle => {
            matches!(ownership, OwnershipScope::VmOwnership(_))
        }
    };
    if valid {
        Ok(())
    } else {
        Err(OwnershipCoordinatorError::OwnershipMismatch {
            expected: String::from("VM ownership"),
            actual: format!(
                "{} with {} VM concurrency",
                ownership_label(ownership),
                vm_concurrency_label(vm_concurrency)
            ),
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

fn vm_concurrency_label(vm_concurrency: &VmConcurrencyClass) -> String {
    match vm_concurrency {
        VmConcurrencyClass::OwnershipOnly => String::from("ownership-only"),
        VmConcurrencyClass::SharedVm => String::from("shared-vm"),
        VmConcurrencyClass::ExclusiveVmLifecycle => String::from("exclusive-vm-lifecycle"),
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
            VmConcurrencyClass::SharedVm,
        )
    }

    fn lifecycle_metadata(connection: &str, session: &str, vm: &str) -> RequestOperationMetadata {
        RequestOperationMetadata::new(
            OwnershipScope::vm(connection, session, vm),
            "VM lifecycle",
            VmConcurrencyClass::ExclusiveVmLifecycle,
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
    async fn ordinary_operations_share_one_vm_without_serializing() {
        let (coordinator, _, _, vm_a, _) = configured();
        let metadata = vm_metadata("connection-a", "session-a", "vm-a");
        let first = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect("first ordinary VM operation starts");
        let second = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect("second ordinary VM operation starts concurrently");
        assert_eq!(vm_a.snapshot().active_operations, 2);
        drop(second);
        drop(first);
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
        let mut internal_during_active = coordinator
            .admit_internal_vm_event(&metadata, OperationCancellation::new())
            .map(expect_internal_deferred)
            .expect("internal progress remains durably deferred while lifecycle is active");
        assert!(!internal_during_active
            .try_activate_deferred_internal_event()
            .expect("active lifecycle remains deferred"));
        drop(lifecycle);
        assert!(internal_during_active
            .try_activate_deferred_internal_event()
            .expect("deferred progress activates after lifecycle"));
        assert_eq!(vm.snapshot().active_operations, 1);
        drop(internal_during_active);

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
    async fn vm_concurrency_requires_vm_ownership() {
        let (coordinator, _, _, _, _) = configured();
        let metadata = RequestOperationMetadata::new(
            OwnershipScope::connection("connection-a"),
            "invalid shared VM operation",
            VmConcurrencyClass::SharedVm,
        );
        let error = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect_err("VM concurrency requires VM ownership");
        assert_eq!(error.code(), "ERR_AGENTOS_COORDINATOR_OWNERSHIP");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ownership_only_policy_tracks_scope_without_serializing_requests() {
        let (coordinator, connection, session, _, _) = configured();
        let metadata = RequestOperationMetadata::new(
            OwnershipScope::session("connection-a", "session-a"),
            "session-owned work",
            VmConcurrencyClass::OwnershipOnly,
        );
        let first = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect("first ownership-only request starts");
        let second = coordinator
            .admit(&metadata, OperationCancellation::new())
            .await
            .expect("second ownership-only request starts concurrently");
        assert_eq!(connection.snapshot().active_operations, 0);
        assert_eq!(session.snapshot().active_operations, 0);
        drop(second);
        drop(first);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_conflicts_and_closing_never_reopen_admission() {
        let (coordinator, _, _, vm, _) = configured();
        let ordinary = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect("ordinary operation starts");
        let pending = vm
            .begin_lifecycle(OperationCancellation::new())
            .expect("first lifecycle becomes pending");
        let second = vm
            .begin_lifecycle(OperationCancellation::new())
            .expect_err("second pending lifecycle is rejected");
        assert_eq!(second.code(), "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT");
        drop(pending);
        assert_eq!(vm.snapshot().lifecycle, VmLifecyclePhase::Idle);

        let pending = vm
            .begin_lifecycle(OperationCancellation::new())
            .expect("lifecycle can retry after cancellation");
        drop(ordinary);
        let active = pending.wait().await.expect("lifecycle activates");
        let second = vm
            .begin_lifecycle(OperationCancellation::new())
            .expect_err("second active lifecycle is rejected");
        assert_eq!(second.code(), "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT");

        let disposal = coordinator
            .begin_vm_disposal(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                OperationCancellationReason::Explicit,
            )
            .expect("closing begins while lifecycle is active");
        drop(active);
        let rejected = coordinator
            .admit(
                &vm_metadata("connection-a", "session-a", "vm-a"),
                OperationCancellation::new(),
            )
            .await
            .expect_err("active lifecycle drop cannot reopen a closing VM");
        assert_eq!(rejected.code(), "ERR_AGENTOS_COORDINATOR_CLOSING");
        disposal.wait_drained().await;
        disposal.complete().expect("closed gate drains");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_vm_generation_cannot_mutate_recreated_membership() {
        let (coordinator, _, session, stale_vm, _) = configured();
        let disposal = coordinator
            .begin_vm_disposal(
                &OwnershipScope::vm("connection-a", "session-a", "vm-a"),
                OperationCancellationReason::Explicit,
            )
            .expect("dispose first VM generation");
        disposal.wait_drained().await;
        disposal.complete().expect("remove first VM generation");

        let current_vm = session.open_vm("vm-a").expect("recreate textual VM id");
        assert_ne!(stale_vm.generation, current_vm.generation);
        let stale_error = stale_vm
            .begin_lifecycle(OperationCancellation::new())
            .expect_err("stale handle cannot mutate the replacement VM");
        assert_eq!(stale_error.code(), "ERR_AGENTOS_COORDINATOR_NOT_FOUND");
        assert_eq!(current_vm.snapshot().lifecycle, VmLifecyclePhase::Idle);

        // Even a stale gate generation token cannot release a later lifecycle.
        let current = current_vm
            .begin_lifecycle(OperationCancellation::new())
            .expect("current lifecycle begins");
        let current_id = match lock(&current_vm.gate.state, "test VM gate").lifecycle {
            VmLifecycleState::Pending { id, .. } => id,
            _ => panic!("current lifecycle must be pending"),
        };
        release_lifecycle(&current_vm.gate, current_id.wrapping_sub(1));
        assert_eq!(current_vm.snapshot().lifecycle, VmLifecyclePhase::Pending);
        drop(current);
        assert_eq!(current_vm.snapshot().lifecycle, VmLifecyclePhase::Idle);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deterministic_gate_model_matches_randomized_admit_drop_and_cancel_sequence() {
        let (coordinator, _, _, vm, _) = configured();
        let metadata = vm_metadata("connection-a", "session-a", "vm-a");
        let mut ordinary = Vec::<CoordinatorOperationPermit>::new();
        let mut internal = Vec::<CoordinatorOperationPermit>::new();
        let mut pending = None::<VmLifecycleAdmission>;
        let mut active = None::<VmLifecycleGuard>;
        let mut seed = 0x4d59_5df4_d0f3_3173_u64;

        for _ in 0..512 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            match (seed >> 32) % 8 {
                0 if pending.is_none() && active.is_none() => {
                    match vm.begin_lifecycle(OperationCancellation::new()) {
                        Ok(admission) => pending = Some(admission),
                        Err(error) => assert_eq!(error.code(), "ERR_AGENTOS_COORDINATOR_CLOSING"),
                    }
                }
                1 if pending.is_some() => drop(pending.take()),
                2 if active.is_some() => drop(active.take()),
                3 if !ordinary.is_empty() => {
                    let index = seed as usize % ordinary.len();
                    ordinary.swap_remove(index);
                }
                4 if !internal.is_empty() => {
                    let index = seed as usize % internal.len();
                    internal.swap_remove(index);
                }
                5 => {
                    if let Ok(permit) = coordinator
                        .admit(&metadata, OperationCancellation::new())
                        .await
                    {
                        ordinary.push(permit);
                    }
                }
                _ => {
                    if let Ok(admission) =
                        coordinator.admit_internal_vm_event(&metadata, OperationCancellation::new())
                    {
                        match admission {
                            InternalVmEventAdmission::Admitted(permit)
                            | InternalVmEventAdmission::Deferred(permit) => internal.push(permit),
                        }
                    }
                }
            }

            if pending.is_some() && ordinary.is_empty() && internal.is_empty() {
                active = Some(
                    pending
                        .take()
                        .expect("pending lifecycle")
                        .wait()
                        .await
                        .expect("drained lifecycle activates"),
                );
            }
            let snapshot = vm.snapshot();
            assert_eq!(snapshot.active_operations, ordinary.len() + internal.len());
            assert_eq!(
                snapshot.lifecycle,
                if active.is_some() {
                    VmLifecyclePhase::Active
                } else if pending.is_some() {
                    VmLifecyclePhase::Pending
                } else {
                    VmLifecyclePhase::Idle
                }
            );
        }

        drop(pending);
        drop(active);
        drop(ordinary);
        drop(internal);
        assert_eq!(vm.snapshot().active_operations, 0);
        assert_eq!(vm.snapshot().lifecycle, VmLifecyclePhase::Idle);
    }

    #[test]
    fn coordinator_membership_state_is_bounded() {
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
