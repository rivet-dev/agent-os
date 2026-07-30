//! One generation-aware registry for native and kernel-backed capabilities.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use crate::accounting::{LimitError, Reservation, ResourceClass, ResourceLedger};

pub type CapabilityId = u64;
/// Capability IDs are never recycled within one VM/session generation, so the
/// per-capability generation remains one. The session generation is part of
/// every validated identity and distinguishes separate VM lifetimes.
pub type CapabilityGeneration = u64;
pub type SessionGeneration = u64;

const NON_RECYCLING_CAPABILITY_GENERATION: CapabilityGeneration = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityKind {
    TcpSocket,
    TcpListener,
    UnixSocket,
    UnixListener,
    UdpSocket,
    TlsTransport,
    Http2Connection,
    Http2Stream,
}

impl CapabilityKind {
    pub const fn is_socket(self) -> bool {
        matches!(
            self,
            Self::TcpSocket
                | Self::TcpListener
                | Self::UnixSocket
                | Self::UnixListener
                | Self::UdpSocket
                | Self::Http2Connection
        )
    }

    pub const fn is_connection(self) -> bool {
        matches!(
            self,
            Self::TcpSocket | Self::UnixSocket | Self::Http2Connection
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityBackend {
    Native { local_id: String },
    Kernel { socket_id: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityLifecycle {
    Allocating,
    Open,
    Closing,
    Failed,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    pub session_generation: SessionGeneration,
    pub id: CapabilityId,
    pub generation: CapabilityGeneration,
    pub kind: CapabilityKind,
    pub backend: CapabilityBackend,
    pub lifecycle: CapabilityLifecycle,
    pub referenced: bool,
}

#[derive(Debug)]
pub enum CapabilityError {
    Limit(LimitError),
    IdExhausted,
    RegistryClosed,
    Stale {
        id: CapabilityId,
        supplied_generation: CapabilityGeneration,
    },
    WrongSession {
        id: CapabilityId,
        expected: SessionGeneration,
        actual: SessionGeneration,
    },
    WrongKind {
        id: CapabilityId,
        expected: CapabilityKind,
        actual: CapabilityKind,
    },
    InvalidTransition {
        id: CapabilityId,
        from: CapabilityLifecycle,
        to: CapabilityLifecycle,
    },
    Poisoned,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(error) => error.fmt(formatter),
            Self::IdExhausted => formatter.write_str(
                "ERR_AGENTOS_CAPABILITY_ID_EXHAUSTED: capability id space exhausted",
            ),
            Self::RegistryClosed => formatter.write_str(
                "ERR_AGENTOS_CAPABILITY_REGISTRY_CLOSED: VM capability admission is closed",
            ),
            Self::Stale {
                id,
                supplied_generation,
            } => write!(
                formatter,
                "ERR_AGENTOS_STALE_CAPABILITY: capability {id} generation {supplied_generation} is not active"
            ),
            Self::WrongSession {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "ERR_AGENTOS_CAPABILITY_SESSION: capability {id} belongs to VM generation {actual}, not {expected}"
            ),
            Self::WrongKind {
                id,
                expected,
                actual,
            } => write!(
                formatter,
                "ERR_AGENTOS_CAPABILITY_KIND: capability {id} has kind {actual:?}, expected {expected:?}"
            ),
            Self::InvalidTransition { id, from, to } => write!(
                formatter,
                "ERR_AGENTOS_CAPABILITY_LIFECYCLE: capability {id} cannot transition from {from:?} to {to:?}"
            ),
            Self::Poisoned => formatter.write_str(
                "ERR_AGENTOS_CAPABILITY_REGISTRY_POISONED: capability registry lock poisoned",
            ),
        }
    }
}

impl std::error::Error for CapabilityError {}

impl From<LimitError> for CapabilityError {
    fn from(value: LimitError) -> Self {
        Self::Limit(value)
    }
}

#[derive(Debug)]
struct CapabilityEntry {
    snapshot: CapabilitySnapshot,
}

#[derive(Debug)]
struct RegistryState {
    next_id: CapabilityId,
    open: bool,
    pending: usize,
    entries: BTreeMap<CapabilityId, CapabilityEntry>,
}

#[derive(Debug)]
struct RegistryInner {
    session_generation: SessionGeneration,
    ledger: Arc<ResourceLedger>,
    state: Mutex<RegistryState>,
    admission_changed: tokio::sync::Notify,
    settled: tokio::sync::Notify,
}

#[derive(Clone, Debug)]
pub struct CapabilityRegistry {
    inner: Arc<RegistryInner>,
}

impl CapabilityRegistry {
    pub fn new(session_generation: SessionGeneration, ledger: Arc<ResourceLedger>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                session_generation,
                ledger,
                state: Mutex::new(RegistryState {
                    next_id: 1,
                    open: true,
                    pending: 0,
                    entries: BTreeMap::new(),
                }),
                admission_changed: tokio::sync::Notify::new(),
                settled: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn session_generation(&self) -> SessionGeneration {
        self.inner.session_generation
    }

    pub fn resources(&self) -> Arc<ResourceLedger> {
        Arc::clone(&self.inner.ledger)
    }

    /// Acquire every count permit before allocating the descriptor/backend.
    pub fn reserve(&self, kind: CapabilityKind) -> Result<PendingCapability, CapabilityError> {
        {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| CapabilityError::Poisoned)?;
            if !state.open {
                return Err(CapabilityError::RegistryClosed);
            }
        }
        let capability = self.inner.ledger.reserve(ResourceClass::Capabilities, 1)?;
        // One possible ready-set entry is reserved with every admitted
        // capability. Readiness for a live handle therefore cannot fail after
        // the backend has already become observable.
        let ready_handle = self.inner.ledger.reserve(ResourceClass::ReadyHandles, 1)?;
        let socket = kind
            .is_socket()
            .then(|| self.inner.ledger.reserve(ResourceClass::Sockets, 1))
            .transpose()?;
        let connection = kind
            .is_connection()
            .then(|| self.inner.ledger.reserve(ResourceClass::Connections, 1))
            .transpose()?;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| CapabilityError::Poisoned)?;
            if !state.open {
                return Err(CapabilityError::RegistryClosed);
            }
            state.pending = state
                .pending
                .checked_add(1)
                .ok_or(CapabilityError::IdExhausted)?;
        }
        Ok(PendingCapability {
            registry: Arc::clone(&self.inner),
            kind,
            reservations: vec![Some(capability), Some(ready_handle), socket, connection]
                .into_iter()
                .flatten()
                .collect(),
            counted: true,
        })
    }

    /// Pause an async source at admission instead of accepting/reading and then
    /// queueing uncharged state. The notification is armed before each retry so
    /// a concurrent release cannot be missed.
    pub async fn reserve_when_available(
        &self,
        kind: CapabilityKind,
    ) -> Result<PendingCapability, CapabilityError> {
        loop {
            let changed = self.inner.ledger.capacity_changed();
            let admission_changed = self.inner.admission_changed.notified();
            match self.reserve(kind) {
                Ok(pending) => return Ok(pending),
                Err(CapabilityError::Limit(error)) if error.requested > error.limit => {
                    return Err(CapabilityError::Limit(error));
                }
                Err(CapabilityError::Limit(_)) => {
                    tokio::select! {
                        _ = changed => {}
                        _ = admission_changed => {}
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn snapshot(&self, id: CapabilityId) -> Option<CapabilitySnapshot> {
        self.inner
            .state
            .lock()
            .ok()?
            .entries
            .get(&id)
            .map(|entry| entry.snapshot.clone())
    }

    pub fn snapshots(&self) -> Vec<CapabilitySnapshot> {
        self.inner
            .state
            .lock()
            .map(|state| {
                state
                    .entries
                    .values()
                    .map(|entry| entry.snapshot.clone())
                    .collect()
            })
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_CAPABILITY_REGISTRY_POISONED: failed to snapshot capability registry"
                );
                Vec::new()
            })
    }

    pub fn close_admission(&self) -> Result<(), CapabilityError> {
        self.inner
            .state
            .lock()
            .map_err(|_| CapabilityError::Poisoned)?
            .open = false;
        self.inner.admission_changed.notify_waiters();
        Ok(())
    }

    pub fn active_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or(0)
    }

    pub fn outstanding_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|state| state.pending.saturating_add(state.entries.len()))
            .unwrap_or(usize::MAX)
    }

    pub async fn wait_empty(&self) {
        loop {
            let settled = self.inner.settled.notified();
            if self.outstanding_len() == 0 {
                return;
            }
            settled.await;
        }
    }
}

#[derive(Debug)]
pub struct PendingCapability {
    registry: Arc<RegistryInner>,
    kind: CapabilityKind,
    reservations: Vec<Reservation>,
    counted: bool,
}

impl PendingCapability {
    /// Commit only after the backend has been allocated. Dropping before commit
    /// rolls admission back; dropping the returned lease closes the registry row.
    pub fn commit(
        mut self,
        backend: CapabilityBackend,
    ) -> Result<CapabilityLease, CapabilityError> {
        let (id, generation) = {
            let mut state = self
                .registry
                .state
                .lock()
                .map_err(|_| CapabilityError::Poisoned)?;
            if !state.open {
                return Err(CapabilityError::RegistryClosed);
            }
            if state.pending == 0 {
                eprintln!(
                    "ERR_AGENTOS_CAPABILITY_ACCOUNTING_UNDERFLOW: pending commit without admission"
                );
                return Err(CapabilityError::Poisoned);
            }
            let id = state.next_id;
            let next_id = id.checked_add(1).ok_or(CapabilityError::IdExhausted)?;
            state.pending -= 1;
            self.counted = false;
            state.next_id = next_id;
            let generation = NON_RECYCLING_CAPABILITY_GENERATION;
            let replaced = state.entries.insert(
                id,
                CapabilityEntry {
                    snapshot: CapabilitySnapshot {
                        session_generation: self.registry.session_generation,
                        id,
                        generation,
                        kind: self.kind,
                        backend,
                        lifecycle: CapabilityLifecycle::Open,
                        referenced: true,
                    },
                },
            );
            if let Some(previous) = replaced {
                eprintln!(
                    "ERR_AGENTOS_CAPABILITY_ID_REUSED: monotonic capability id {id} was already active"
                );
                state.entries.insert(id, previous);
                return Err(CapabilityError::Poisoned);
            }
            (id, generation)
        };
        Ok(CapabilityLease {
            registry: Arc::downgrade(&self.registry),
            id,
            generation,
            reservations: std::mem::take(&mut self.reservations),
        })
    }
}

impl Drop for PendingCapability {
    fn drop(&mut self) {
        if self.counted {
            match self.registry.state.lock() {
                Ok(mut state) => {
                    if state.pending == 0 {
                        eprintln!(
                            "ERR_AGENTOS_CAPABILITY_ACCOUNTING_UNDERFLOW: pending reservation released at zero"
                        );
                    } else {
                        state.pending -= 1;
                    }
                }
                Err(_) => eprintln!(
                    "ERR_AGENTOS_CAPABILITY_REGISTRY_POISONED: pending reservation release failed"
                ),
            }
            self.counted = false;
            self.registry.settled.notify_waiters();
        }
    }
}

#[derive(Debug)]
pub struct CapabilityLease {
    registry: Weak<RegistryInner>,
    id: CapabilityId,
    generation: CapabilityGeneration,
    reservations: Vec<Reservation>,
}

impl CapabilityLease {
    pub fn id(&self) -> CapabilityId {
        self.id
    }

    pub fn generation(&self) -> CapabilityGeneration {
        self.generation
    }

    /// Validate a guest-visible alias against the live registry row before an
    /// operation reaches its backend.
    pub fn validate(
        &self,
        session_generation: SessionGeneration,
        kind: CapabilityKind,
    ) -> Result<(), CapabilityError> {
        let registry = self
            .registry
            .upgrade()
            .ok_or(CapabilityError::RegistryClosed)?;
        let state = registry
            .state
            .lock()
            .map_err(|_| CapabilityError::Poisoned)?;
        let entry = state.entries.get(&self.id).ok_or(CapabilityError::Stale {
            id: self.id,
            supplied_generation: self.generation,
        })?;
        if entry.snapshot.generation != self.generation {
            return Err(CapabilityError::Stale {
                id: self.id,
                supplied_generation: self.generation,
            });
        }
        if entry.snapshot.session_generation != session_generation {
            return Err(CapabilityError::WrongSession {
                id: self.id,
                expected: session_generation,
                actual: entry.snapshot.session_generation,
            });
        }
        if entry.snapshot.kind != kind {
            return Err(CapabilityError::WrongKind {
                id: self.id,
                expected: kind,
                actual: entry.snapshot.kind,
            });
        }
        Ok(())
    }

    pub fn set_referenced(&self, referenced: bool) -> Result<(), CapabilityError> {
        self.update(|snapshot| snapshot.referenced = referenced)
    }

    pub fn transition(&self, to: CapabilityLifecycle) -> Result<(), CapabilityError> {
        let registry = self
            .registry
            .upgrade()
            .ok_or(CapabilityError::RegistryClosed)?;
        let mut state = registry
            .state
            .lock()
            .map_err(|_| CapabilityError::Poisoned)?;
        let entry = state
            .entries
            .get_mut(&self.id)
            .ok_or(CapabilityError::Stale {
                id: self.id,
                supplied_generation: self.generation,
            })?;
        if entry.snapshot.generation != self.generation {
            return Err(CapabilityError::Stale {
                id: self.id,
                supplied_generation: self.generation,
            });
        }
        let from = entry.snapshot.lifecycle;
        if !valid_transition(from, to) {
            return Err(CapabilityError::InvalidTransition {
                id: self.id,
                from,
                to,
            });
        }
        entry.snapshot.lifecycle = to;
        Ok(())
    }

    fn update(&self, update: impl FnOnce(&mut CapabilitySnapshot)) -> Result<(), CapabilityError> {
        let registry = self
            .registry
            .upgrade()
            .ok_or(CapabilityError::RegistryClosed)?;
        let mut state = registry
            .state
            .lock()
            .map_err(|_| CapabilityError::Poisoned)?;
        let entry = state
            .entries
            .get_mut(&self.id)
            .ok_or(CapabilityError::Stale {
                id: self.id,
                supplied_generation: self.generation,
            })?;
        if entry.snapshot.generation != self.generation {
            return Err(CapabilityError::Stale {
                id: self.id,
                supplied_generation: self.generation,
            });
        }
        update(&mut entry.snapshot);
        Ok(())
    }
}

fn valid_transition(from: CapabilityLifecycle, to: CapabilityLifecycle) -> bool {
    from == to
        || matches!(
            (from, to),
            (CapabilityLifecycle::Open, CapabilityLifecycle::Closing)
                | (CapabilityLifecycle::Open, CapabilityLifecycle::Failed)
                | (CapabilityLifecycle::Open, CapabilityLifecycle::Closed)
                | (CapabilityLifecycle::Closing, CapabilityLifecycle::Closed)
                | (CapabilityLifecycle::Failed, CapabilityLifecycle::Closed)
        )
}

impl Drop for CapabilityLease {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            match registry.state.lock() {
                Ok(mut state) => {
                    let remove = state
                        .entries
                        .get(&self.id)
                        .is_some_and(|entry| entry.snapshot.generation == self.generation);
                    if remove {
                        state.entries.remove(&self.id);
                    } else {
                        eprintln!(
                            "ERR_AGENTOS_CAPABILITY_RELEASE_STALE: capability={} generation={}",
                            self.id, self.generation
                        );
                    }
                }
                Err(_) => eprintln!(
                    "ERR_AGENTOS_CAPABILITY_REGISTRY_POISONED: capability={} release failed",
                    self.id
                ),
            }
            registry.settled.notify_waiters();
        }
        self.reservations.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::ResourceLimit;

    fn registry(maximum: usize) -> (Arc<ResourceLedger>, CapabilityRegistry) {
        let ledger = Arc::new(ResourceLedger::root(
            "vm-1",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(maximum, "runtime.capabilities.maxPerVm"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(maximum, "runtime.resources.maxSockets"),
                ),
                (
                    ResourceClass::Connections,
                    ResourceLimit::new(maximum, "runtime.resources.maxConnections"),
                ),
            ],
        ));
        let registry = CapabilityRegistry::new(7, Arc::clone(&ledger));
        (ledger, registry)
    }

    #[test]
    fn admission_precedes_commit_and_drop_reconciles() {
        let (ledger, registry) = registry(1);
        let pending = registry.reserve(CapabilityKind::TcpSocket).unwrap();
        assert_eq!(ledger.usage(ResourceClass::Sockets).used, 1);
        assert!(registry.reserve(CapabilityKind::TcpSocket).is_err());
        let lease = pending
            .commit(CapabilityBackend::Native {
                local_id: String::from("socket-1"),
            })
            .unwrap();
        assert_eq!(registry.active_len(), 1);
        assert_eq!(registry.snapshot(lease.id()).unwrap().session_generation, 7);
        drop(lease);
        assert_eq!(registry.active_len(), 0);
        assert!(ledger.is_zero());
    }

    #[test]
    fn failed_backend_allocation_rolls_back_pending_reservation() {
        let (ledger, registry) = registry(1);
        drop(registry.reserve(CapabilityKind::UdpSocket).unwrap());
        assert!(ledger.is_zero());
        assert_eq!(registry.active_len(), 0);
    }

    #[test]
    fn alias_validation_rejects_wrong_vm_generation_and_kind() {
        let (_ledger, registry) = registry(1);
        let lease = registry
            .reserve(CapabilityKind::TcpSocket)
            .expect("reserve TCP capability")
            .commit(CapabilityBackend::Native {
                local_id: String::from("socket-1"),
            })
            .expect("commit TCP capability");

        lease
            .validate(7, CapabilityKind::TcpSocket)
            .expect("matching generation and kind");
        assert!(matches!(
            lease.validate(8, CapabilityKind::TcpSocket),
            Err(CapabilityError::WrongSession {
                expected: 8,
                actual: 7,
                ..
            })
        ));
        assert!(matches!(
            lease.validate(7, CapabilityKind::UdpSocket),
            Err(CapabilityError::WrongKind {
                expected: CapabilityKind::UdpSocket,
                actual: CapabilityKind::TcpSocket,
                ..
            })
        ));
    }

    #[test]
    fn close_admission_rejects_pending_commit_without_leaking() {
        let (ledger, registry) = registry(1);
        let pending = registry.reserve(CapabilityKind::TcpListener).unwrap();
        registry.close_admission().unwrap();
        assert!(pending
            .commit(CapabilityBackend::Kernel { socket_id: 4 })
            .is_err());
        assert!(ledger.is_zero());
    }

    #[tokio::test]
    async fn close_admission_wakes_capacity_waiters_with_typed_error() {
        let (ledger, registry) = registry(1);
        let held = registry
            .reserve(CapabilityKind::TcpSocket)
            .expect("fill registry");
        let waiting_registry = registry.clone();
        let waiter = tokio::spawn(async move {
            waiting_registry
                .reserve_when_available(CapabilityKind::TcpSocket)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        registry.close_admission().expect("close admission");
        let error = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("close must wake waiter")
            .expect("waiter task")
            .expect_err("closed registry must reject admission");
        assert!(matches!(error, CapabilityError::RegistryClosed));
        drop(held);
        assert!(ledger.is_zero());
    }

    #[tokio::test]
    async fn non_retryable_zero_limit_returns_without_waiting_or_leaking() {
        let ledger = Arc::new(ResourceLedger::root(
            "vm-zero-sockets",
            [
                (
                    ResourceClass::Capabilities,
                    ResourceLimit::new(1, "runtime.capabilities.maxPerVm"),
                ),
                (
                    ResourceClass::Sockets,
                    ResourceLimit::new(0, "runtime.resources.maxSockets"),
                ),
                (
                    ResourceClass::Connections,
                    ResourceLimit::new(1, "runtime.resources.maxConnections"),
                ),
            ],
        ));
        let registry = CapabilityRegistry::new(7, Arc::clone(&ledger));

        let error = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            registry.reserve_when_available(CapabilityKind::TcpSocket),
        )
        .await
        .expect("an impossible admission must not wait for capacity")
        .expect_err("zero socket limit must reject admission");
        let CapabilityError::Limit(error) = error else {
            panic!("expected typed resource limit error");
        };
        assert_eq!(error.resource, ResourceClass::Sockets);
        assert_eq!(error.requested, 1);
        assert_eq!(error.limit, 0);
        assert_eq!(error.config_path, "runtime.resources.maxSockets");
        assert!(ledger.is_zero());
        assert_eq!(registry.outstanding_len(), 0);
    }

    #[test]
    fn capability_ids_are_monotonic_and_never_recycled() {
        let (_ledger, registry) = registry(1);
        let first = registry
            .reserve(CapabilityKind::UdpSocket)
            .expect("first reservation")
            .commit(CapabilityBackend::Native {
                local_id: String::from("udp-1"),
            })
            .expect("first capability");
        assert_eq!(first.id(), 1);
        assert_eq!(first.generation(), NON_RECYCLING_CAPABILITY_GENERATION);
        drop(first);

        let second = registry
            .reserve(CapabilityKind::UdpSocket)
            .expect("second reservation")
            .commit(CapabilityBackend::Native {
                local_id: String::from("udp-2"),
            })
            .expect("second capability");
        assert_eq!(second.id(), 2);
        assert_eq!(second.generation(), NON_RECYCLING_CAPABILITY_GENERATION);
        assert!(registry.snapshot(1).is_none());
    }

    #[tokio::test]
    async fn wait_empty_includes_pending_and_committed_ownership() {
        let (ledger, registry) = registry(2);
        let pending = registry
            .reserve(CapabilityKind::UdpSocket)
            .expect("pending capability");
        let lease = registry
            .reserve(CapabilityKind::TcpListener)
            .expect("committed capability")
            .commit(CapabilityBackend::Kernel { socket_id: 8 })
            .expect("commit");
        assert_eq!(registry.outstanding_len(), 2);
        let waiter = tokio::spawn({
            let registry = registry.clone();
            async move { registry.wait_empty().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(pending);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(lease);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("empty notification")
            .expect("wait task");
        assert!(ledger.is_zero());
    }

    #[tokio::test]
    async fn id_exhaustion_reconciles_pending_waiters() {
        let (_, registry) = registry(1);
        let pending = registry
            .reserve(CapabilityKind::TcpSocket)
            .expect("pending capability");
        registry.inner.state.lock().expect("registry state").next_id = u64::MAX;
        let waiter = tokio::spawn({
            let registry = registry.clone();
            async move { registry.wait_empty().await }
        });
        tokio::task::yield_now().await;

        let error = pending
            .commit(CapabilityBackend::Native {
                local_id: String::from("exhausted"),
            })
            .expect_err("exhausted id must reject commit");
        assert!(matches!(error, CapabilityError::IdExhausted));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("pending release must wake waiter")
            .expect("wait task");
        assert_eq!(registry.outstanding_len(), 0);
    }
}
