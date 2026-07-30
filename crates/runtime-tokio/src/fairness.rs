//! Deterministic hierarchical deficit round robin for VM-owned capabilities.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::metrics::{FairnessLevel, RuntimeMetrics};

/// Independent count and byte dimensions for one scheduling turn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FairBudget {
    pub operations: usize,
    pub bytes: usize,
}

impl FairBudget {
    pub const fn new(operations: usize, bytes: usize) -> Self {
        Self { operations, bytes }
    }

    fn add_capped(&mut self, quantum: Self, maximum: Self) {
        self.operations = self
            .operations
            .saturating_add(quantum.operations)
            .min(maximum.operations);
        self.bytes = self.bytes.saturating_add(quantum.bytes).min(maximum.bytes);
    }

    fn turn_allowance(self, other: Self, turn_cap: Self) -> Self {
        Self {
            operations: self
                .operations
                .min(other.operations)
                .min(turn_cap.operations),
            bytes: self.bytes.min(other.bytes).min(turn_cap.bytes),
        }
    }

    fn consume(&mut self, used: Self) {
        self.operations -= used.operations;
        self.bytes -= used.bytes;
    }

    fn min(self, other: Self) -> Self {
        Self {
            operations: self.operations.min(other.operations),
            bytes: self.bytes.min(other.bytes),
        }
    }
}

/// Bounded policy for the process -> VM -> capability scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FairnessConfig {
    /// Credit added whenever a ready VM reaches the front of the process ring.
    pub vm_quantum: FairBudget,
    /// Credit added whenever a ready capability reaches the front of its VM ring.
    pub capability_quantum: FairBudget,
    /// Maximum credit a VM may retain between selections, including while idle.
    pub max_vm_deficit: FairBudget,
    /// Maximum credit a capability may retain between selections, including while idle.
    pub max_capability_deficit: FairBudget,
    /// Maximum number of VM states retained by the scheduler.
    pub max_vms: usize,
    /// Maximum number of capability states retained for one VM.
    pub max_capabilities_per_vm: usize,
}

impl FairnessConfig {
    fn validate(self) -> Result<Self, FairnessError> {
        for (field, value) in [
            ("fairness.vmQuantum.operations", self.vm_quantum.operations),
            ("fairness.vmQuantum.bytes", self.vm_quantum.bytes),
            (
                "fairness.capabilityQuantum.operations",
                self.capability_quantum.operations,
            ),
            (
                "fairness.capabilityQuantum.bytes",
                self.capability_quantum.bytes,
            ),
            (
                "fairness.maxVmDeficit.operations",
                self.max_vm_deficit.operations,
            ),
            ("fairness.maxVmDeficit.bytes", self.max_vm_deficit.bytes),
            (
                "fairness.maxCapabilityDeficit.operations",
                self.max_capability_deficit.operations,
            ),
            (
                "fairness.maxCapabilityDeficit.bytes",
                self.max_capability_deficit.bytes,
            ),
            ("fairness.maxVms", self.max_vms),
            (
                "fairness.maxCapabilitiesPerVm",
                self.max_capabilities_per_vm,
            ),
        ] {
            if value == 0 {
                return Err(FairnessError::InvalidConfig { field });
            }
        }
        for (field, maximum, quantum) in [
            (
                "fairness.maxVmDeficit.operations",
                self.max_vm_deficit.operations,
                self.vm_quantum.operations,
            ),
            (
                "fairness.maxVmDeficit.bytes",
                self.max_vm_deficit.bytes,
                self.vm_quantum.bytes,
            ),
            (
                "fairness.maxCapabilityDeficit.operations",
                self.max_capability_deficit.operations,
                self.capability_quantum.operations,
            ),
            (
                "fairness.maxCapabilityDeficit.bytes",
                self.max_capability_deficit.bytes,
                self.capability_quantum.bytes,
            ),
        ] {
            if maximum < quantum {
                return Err(FairnessError::DeficitBelowQuantum { field });
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipUpdate {
    Enqueued,
    Coalesced,
}

/// One deterministic grant. Exactly one grant may be outstanding at a time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FairSelection<VmId, CapabilityId> {
    pub sequence: u64,
    pub vm_id: VmId,
    pub capability_id: CapabilityId,
    /// Hard per-turn ceiling in both dimensions.
    pub allowance: FairBudget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FairnessSnapshot {
    pub vm_deficit: FairBudget,
    pub capability_deficit: FairBudget,
    pub vm_queued: bool,
    pub capability_queued: bool,
    pub capability_in_flight: bool,
    pub capability_rearmed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FairnessError {
    InvalidConfig {
        field: &'static str,
    },
    DeficitBelowQuantum {
        field: &'static str,
    },
    VmLimit {
        limit: usize,
    },
    CapabilityLimit {
        limit: usize,
    },
    SelectionInFlight {
        sequence: u64,
    },
    StaleSelection {
        supplied: u64,
        outstanding: Option<u64>,
    },
    OperationBudgetExceeded {
        used: usize,
        allowance: usize,
    },
    ByteBudgetExceeded {
        used: usize,
        allowance: usize,
    },
    CapabilityInFlight,
    CapabilityRetired {
        vm_generation: u64,
        capability_id: u64,
    },
    SequenceExhausted,
    Invariant,
}

impl fmt::Display for FairnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_CONFIG: {field} must be greater than zero"
            ),
            Self::DeficitBelowQuantum { field } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_CONFIG: {field} must be at least its quantum"
            ),
            Self::VmLimit { limit } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_VM_LIMIT: scheduler VM state exceeded {limit}; raise runtime.fairness.maxVms"
            ),
            Self::CapabilityLimit { limit } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_CAPABILITY_LIMIT: scheduler capability state exceeded {limit}; raise runtime.fairness.maxCapabilitiesPerVm"
            ),
            Self::SelectionInFlight { sequence } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_SELECTION_IN_FLIGHT: selection {sequence} must complete before selecting again"
            ),
            Self::StaleSelection {
                supplied,
                outstanding,
            } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_STALE_SELECTION: supplied selection {supplied}, outstanding {outstanding:?}"
            ),
            Self::OperationBudgetExceeded { used, allowance } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_OPERATION_BUDGET: used {used} operations, allowance {allowance}"
            ),
            Self::ByteBudgetExceeded { used, allowance } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_BYTE_BUDGET: used {used} bytes, allowance {allowance}"
            ),
            Self::CapabilityInFlight => formatter.write_str(
                "ERR_AGENTOS_FAIRNESS_CAPABILITY_IN_FLIGHT: cannot remove an in-flight capability",
            ),
            Self::CapabilityRetired {
                vm_generation,
                capability_id,
            } => write!(
                formatter,
                "ERR_AGENTOS_FAIRNESS_CAPABILITY_RETIRED: capability {capability_id} in VM generation {vm_generation} is closed",
            ),
            Self::SequenceExhausted => formatter.write_str(
                "ERR_AGENTOS_FAIRNESS_SEQUENCE_EXHAUSTED: selection sequence exhausted",
            ),
            Self::Invariant => formatter.write_str(
                "ERR_AGENTOS_FAIRNESS_INVARIANT: scheduler membership state is inconsistent",
            ),
        }
    }
}

impl std::error::Error for FairnessError {}

#[derive(Debug)]
struct CapabilityState {
    deficit: FairBudget,
    queued: bool,
    in_flight: bool,
    rearmed: bool,
}

impl CapabilityState {
    fn new() -> Self {
        Self {
            deficit: FairBudget::default(),
            queued: false,
            in_flight: false,
            rearmed: false,
        }
    }
}

#[derive(Debug)]
struct VmState<CapabilityId> {
    deficit: FairBudget,
    queued: bool,
    ready_capabilities: VecDeque<CapabilityId>,
    capabilities: BTreeMap<CapabilityId, CapabilityState>,
}

impl<CapabilityId> VmState<CapabilityId> {
    fn new() -> Self {
        Self {
            deficit: FairBudget::default(),
            queued: false,
            ready_capabilities: VecDeque::new(),
            capabilities: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct InFlight<VmId, CapabilityId> {
    selection: FairSelection<VmId, CapabilityId>,
}

/// Process-level VM rotation containing a second capability rotation per VM.
///
/// The scheduler is intentionally serial: callers select one bounded turn,
/// perform no more than its allowance, then report actual use. This makes the
/// selection order reproducible in tests and keeps requeueing atomic.
#[derive(Debug)]
pub struct HierarchicalDeficitRoundRobin<VmId, CapabilityId> {
    config: FairnessConfig,
    ready_vms: VecDeque<VmId>,
    vms: BTreeMap<VmId, VmState<CapabilityId>>,
    in_flight: Option<InFlight<VmId, CapabilityId>>,
    next_sequence: u64,
    metrics: Option<RuntimeMetrics>,
}

/// Process-owned async admission facade over the deterministic HDRR state.
///
/// There is exactly one broker per [`crate::SidecarRuntime`]. A ready handle
/// first joins the coalesced VM/capability rings, then receives one bounded
/// turn. At most one grant is outstanding process-wide, which makes completion
/// and requeueing atomic and prevents independent handle tasks from bypassing
/// tenant rotation by racing on Tokio workers.
#[derive(Clone, Debug)]
pub struct FairWorkBroker {
    inner: Arc<FairWorkInner>,
}

#[derive(Debug)]
struct FairWorkInner {
    state: Mutex<FairWorkState>,
    changed: tokio::sync::Notify,
}

#[derive(Debug)]
struct FairWorkState {
    scheduler: HierarchicalDeficitRoundRobin<u64, u64>,
    granted: BTreeMap<(u64, u64), FairSelection<u64, u64>>,
    /// VM generations are monotonic process-wide. Retain compact ranges so a
    /// stale task can never recreate scheduler membership after VM teardown.
    retired_vm_generations: RetiredIdRanges,
    /// Capability IDs are monotonic within one VM generation. Merged retired
    /// ranges prevent a late transport task from recreating scheduler state
    /// without retaining one tombstone per closed capability.
    retired_capabilities: BTreeMap<u64, RetiredIdRanges>,
}

#[derive(Debug, Default)]
struct RetiredIdRanges {
    ranges: BTreeMap<u64, u64>,
}

impl RetiredIdRanges {
    fn contains(&self, id: u64) -> bool {
        self.ranges
            .range(..=id)
            .next_back()
            .is_some_and(|(_, end)| id <= *end)
    }

    fn insert(&mut self, id: u64) -> bool {
        if self.contains(id) {
            return false;
        }

        let mut start = id;
        let mut end = id;
        if let Some((previous_start, previous_end)) = self
            .ranges
            .range(..id)
            .next_back()
            .map(|(start, end)| (*start, *end))
        {
            if previous_end.checked_add(1) == Some(id) {
                start = previous_start;
                self.ranges.remove(&previous_start);
            }
        }
        while let Some((next_start, next_end)) = self
            .ranges
            .range(start..)
            .next()
            .map(|(start, end)| (*start, *end))
        {
            if next_start > end.saturating_add(1) {
                break;
            }
            end = end.max(next_end);
            self.ranges.remove(&next_start);
        }
        self.ranges.insert(start, end);
        true
    }
}

/// One process-fair work turn. Dropping an unfinished turn reconciles the
/// scheduler with zero work and without requeueing, so task cancellation can
/// never strand the global scheduler in an in-flight state.
#[derive(Debug)]
pub struct FairWorkTurn {
    broker: FairWorkBroker,
    selection: Option<FairSelection<u64, u64>>,
    allowance: FairBudget,
}

struct FairAcquireGuard {
    broker: FairWorkBroker,
    key: (u64, u64),
    armed: bool,
}

impl FairWorkBroker {
    pub fn new(config: FairnessConfig, metrics: RuntimeMetrics) -> Result<Self, FairnessError> {
        Ok(Self {
            inner: Arc::new(FairWorkInner {
                state: Mutex::new(FairWorkState {
                    scheduler: HierarchicalDeficitRoundRobin::new_with_metrics(config, metrics)?,
                    granted: BTreeMap::new(),
                    retired_vm_generations: RetiredIdRanges::default(),
                    retired_capabilities: BTreeMap::new(),
                }),
                changed: tokio::sync::Notify::new(),
            }),
        })
    }

    /// Wait for this ready capability's next process-fair turn. `requested`
    /// carries the VM's configured per-turn ceilings; the returned allowance
    /// is the lower of those ceilings and the process scheduler's grant.
    pub async fn acquire(
        &self,
        vm_generation: u64,
        capability_id: u64,
        requested: FairBudget,
    ) -> Result<FairWorkTurn, FairnessError> {
        if requested.operations == 0 {
            return Err(FairnessError::InvalidConfig {
                field: "limits.reactor.perHandleOperationQuantum",
            });
        }
        if requested.bytes == 0 {
            return Err(FairnessError::InvalidConfig {
                field: "limits.reactor.byteQuantum",
            });
        }

        let key = (vm_generation, capability_id);
        let mut cancellation = FairAcquireGuard {
            broker: self.clone(),
            key,
            armed: true,
        };
        loop {
            // Arm before observing state so a completion between the state
            // probe and await cannot be lost.
            // `Notify::notified()` does not register its waiter until the
            // future is first polled. Enable it before observing scheduler
            // state, otherwise another completion can notify between the
            // state probe and `.await`, permanently stranding this acquire.
            let mut changed = Box::pin(self.inner.changed.notified());
            changed.as_mut().enable();
            let published = {
                let mut state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| FairnessError::Invariant)?;
                if state.retired_vm_generations.contains(vm_generation) {
                    return Err(FairnessError::CapabilityRetired {
                        vm_generation,
                        capability_id,
                    });
                }
                if state
                    .retired_capabilities
                    .get(&vm_generation)
                    .is_some_and(|retired| retired.contains(capability_id))
                {
                    return Err(FairnessError::CapabilityRetired {
                        vm_generation,
                        capability_id,
                    });
                }
                if let Some(selection) = state.granted.remove(&key) {
                    let allowance = selection.allowance.min(requested);
                    cancellation.armed = false;
                    return Ok(FairWorkTurn {
                        broker: self.clone(),
                        selection: Some(selection),
                        allowance,
                    });
                }
                // Multiple protocol tasks for one full-duplex description can
                // wait on the same capability concurrently (for example, one
                // reader and one writer). A grant is consumed by only one of
                // them, so every still-waiting acquire must reassert the
                // coalesced ready edge after it wakes. `mark_ready` is
                // idempotent and rearms an in-flight capability.
                state.scheduler.mark_ready(vm_generation, capability_id)?;
                let published = Self::publish_next_locked(&mut state)?;
                if let Some(selection) = state.granted.remove(&key) {
                    let allowance = selection.allowance.min(requested);
                    cancellation.armed = false;
                    return Ok(FairWorkTurn {
                        broker: self.clone(),
                        selection: Some(selection),
                        allowance,
                    });
                }
                published
            };
            if published {
                self.inner.changed.notify_waiters();
            }
            changed.await;
        }
    }

    /// Permanently retire one monotonic capability identity for this VM
    /// generation. Queued or granted work is revoked immediately. If native
    /// work already owns the bounded turn, its completion removes membership;
    /// later acquire attempts fail instead of recreating stale scheduler state.
    pub fn retire_capability(
        &self,
        vm_generation: u64,
        capability_id: u64,
    ) -> Result<bool, FairnessError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| FairnessError::Invariant)?;
        if state.retired_vm_generations.contains(vm_generation) {
            return Ok(false);
        }
        let key = (vm_generation, capability_id);
        let newly_retired = state
            .retired_capabilities
            .entry(vm_generation)
            .or_default()
            .insert(capability_id);
        if let Some(selection) = state.granted.remove(&key) {
            state
                .scheduler
                .complete(&selection, FairBudget::default(), false)?;
        }
        state.scheduler.clear_ready(&vm_generation, &capability_id);
        let removed = match state
            .scheduler
            .remove_capability(&vm_generation, &capability_id)
        {
            Ok(removed) => removed,
            Err(FairnessError::CapabilityInFlight) => false,
            Err(error) => return Err(error),
        };
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(newly_retired || removed)
    }

    pub fn retire_vm(&self, vm_generation: u64) -> Result<bool, FairnessError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| FairnessError::Invariant)?;
        let newly_retired = state.retired_vm_generations.insert(vm_generation);
        let granted_key = state
            .granted
            .keys()
            .find(|(generation, _)| *generation == vm_generation)
            .copied();
        if let Some(key) = granted_key {
            let selection = state.granted.remove(&key).ok_or(FairnessError::Invariant)?;
            state
                .scheduler
                .complete(&selection, FairBudget::default(), false)?;
        }
        state.scheduler.clear_vm_ready(&vm_generation);
        let removed = match state.scheduler.remove_vm(&vm_generation) {
            Ok(removed) => removed,
            Err(FairnessError::CapabilityInFlight) => false,
            Err(error) => return Err(error),
        };
        state.retired_capabilities.remove(&vm_generation);
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(newly_retired || removed)
    }

    fn publish_next_locked(state: &mut FairWorkState) -> Result<bool, FairnessError> {
        if !state.granted.is_empty() {
            return Ok(false);
        }
        let selection = match state.scheduler.select_next() {
            Ok(Some(selection)) => selection,
            Ok(None) | Err(FairnessError::SelectionInFlight { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let key = (selection.vm_id, selection.capability_id);
        if state.granted.insert(key, selection).is_some() {
            return Err(FairnessError::Invariant);
        }
        Ok(true)
    }

    fn finish(
        &self,
        selection: FairSelection<u64, u64>,
        used: FairBudget,
        still_ready: bool,
    ) -> Result<(), FairnessError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| FairnessError::Invariant)?;
        let vm_retired = state.retired_vm_generations.contains(selection.vm_id);
        let capability_retired = state
            .retired_capabilities
            .get(&selection.vm_id)
            .is_some_and(|retired| retired.contains(selection.capability_id));
        state.scheduler.complete(
            &selection,
            used,
            still_ready && !vm_retired && !capability_retired,
        )?;
        if vm_retired {
            state.scheduler.remove_vm(&selection.vm_id)?;
        } else if capability_retired {
            state
                .scheduler
                .remove_capability(&selection.vm_id, &selection.capability_id)?;
        }
        let _ = Self::publish_next_locked(&mut state)?;
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(())
    }

    fn cancel_waiter(&self, key: (u64, u64)) -> Result<(), FairnessError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| FairnessError::Invariant)?;
        if let Some(selection) = state.granted.remove(&key) {
            state
                .scheduler
                .complete(&selection, FairBudget::default(), false)?;
        } else {
            state.scheduler.clear_ready(&key.0, &key.1);
        }
        if state.retired_vm_generations.contains(key.0) {
            match state.scheduler.remove_vm(&key.0) {
                Ok(_) | Err(FairnessError::CapabilityInFlight) => {}
                Err(error) => return Err(error),
            }
        } else if state
            .retired_capabilities
            .get(&key.0)
            .is_some_and(|retired| retired.contains(key.1))
        {
            state.scheduler.remove_capability(&key.0, &key.1)?;
        }
        let _ = Self::publish_next_locked(&mut state)?;
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(())
    }
}

impl FairWorkTurn {
    pub fn allowance(&self) -> FairBudget {
        self.allowance
    }

    pub fn complete(mut self, used: FairBudget, still_ready: bool) -> Result<(), FairnessError> {
        if used.operations > self.allowance.operations {
            return Err(FairnessError::OperationBudgetExceeded {
                used: used.operations,
                allowance: self.allowance.operations,
            });
        }
        if used.bytes > self.allowance.bytes {
            return Err(FairnessError::ByteBudgetExceeded {
                used: used.bytes,
                allowance: self.allowance.bytes,
            });
        }
        let selection = self.selection.take().ok_or(FairnessError::Invariant)?;
        self.broker.finish(selection, used, still_ready)
    }
}

impl Drop for FairWorkTurn {
    fn drop(&mut self) {
        let Some(selection) = self.selection.take() else {
            return;
        };
        if let Err(error) = self.broker.finish(selection, FairBudget::default(), false) {
            eprintln!("ERR_AGENTOS_FAIRNESS_TURN_DROP: {error}");
        }
    }
}

impl Drop for FairAcquireGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.broker.cancel_waiter(self.key) {
                eprintln!("ERR_AGENTOS_FAIRNESS_ACQUIRE_CANCEL: {error}");
            }
        }
    }
}

impl<VmId, CapabilityId> HierarchicalDeficitRoundRobin<VmId, CapabilityId>
where
    VmId: Clone + Ord,
    CapabilityId: Clone + Ord,
{
    pub fn new(config: FairnessConfig) -> Result<Self, FairnessError> {
        Self::new_inner(config, None)
    }

    pub fn new_with_metrics(
        config: FairnessConfig,
        metrics: RuntimeMetrics,
    ) -> Result<Self, FairnessError> {
        Self::new_inner(config, Some(metrics))
    }

    fn new_inner(
        config: FairnessConfig,
        metrics: Option<RuntimeMetrics>,
    ) -> Result<Self, FairnessError> {
        Ok(Self {
            config: config.validate()?,
            ready_vms: VecDeque::new(),
            vms: BTreeMap::new(),
            in_flight: None,
            next_sequence: 1,
            metrics,
        })
    }

    pub fn config(&self) -> FairnessConfig {
        self.config
    }

    /// Insert one ready membership edge. Repeated marks never add queue entries.
    pub fn mark_ready(
        &mut self,
        vm_id: VmId,
        capability_id: CapabilityId,
    ) -> Result<MembershipUpdate, FairnessError> {
        if !self.vms.contains_key(&vm_id) {
            if self.vms.len() >= self.config.max_vms {
                return Err(FairnessError::VmLimit {
                    limit: self.config.max_vms,
                });
            }
            self.vms.insert(vm_id.clone(), VmState::new());
        }

        let vm = self.vms.get_mut(&vm_id).ok_or(FairnessError::Invariant)?;
        if !vm.capabilities.contains_key(&capability_id) {
            if vm.capabilities.len() >= self.config.max_capabilities_per_vm {
                return Err(FairnessError::CapabilityLimit {
                    limit: self.config.max_capabilities_per_vm,
                });
            }
            vm.capabilities
                .insert(capability_id.clone(), CapabilityState::new());
        }

        let capability = vm
            .capabilities
            .get_mut(&capability_id)
            .ok_or(FairnessError::Invariant)?;
        if capability.in_flight {
            capability.rearmed = true;
            if let Some(metrics) = &self.metrics {
                metrics.record_fairness_yield(FairnessLevel::Capability);
            }
            return Ok(MembershipUpdate::Coalesced);
        }
        if capability.queued {
            if let Some(metrics) = &self.metrics {
                metrics.record_fairness_yield(FairnessLevel::Capability);
            }
            return Ok(MembershipUpdate::Coalesced);
        }
        capability.queued = true;
        vm.ready_capabilities.push_back(capability_id);
        if !vm.queued {
            vm.queued = true;
            self.ready_vms.push_back(vm_id);
        }
        Ok(MembershipUpdate::Enqueued)
    }

    /// Remove queued readiness without discarding retained, capped deficit.
    pub fn clear_ready(&mut self, vm_id: &VmId, capability_id: &CapabilityId) -> bool {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return false;
        };
        let Some(capability) = vm.capabilities.get_mut(capability_id) else {
            return false;
        };
        capability.rearmed = false;
        if !capability.queued {
            return capability.in_flight;
        }
        capability.queued = false;
        vm.ready_capabilities
            .retain(|queued| queued != capability_id);
        if vm.ready_capabilities.is_empty() && vm.queued {
            vm.queued = false;
            self.ready_vms.retain(|queued| queued != vm_id);
        }
        true
    }

    /// Choose the next VM and capability in activation order.
    pub fn select_next(
        &mut self,
    ) -> Result<Option<FairSelection<VmId, CapabilityId>>, FairnessError> {
        if let Some(in_flight) = &self.in_flight {
            return Err(FairnessError::SelectionInFlight {
                sequence: in_flight.selection.sequence,
            });
        }

        while let Some(vm_id) = self.ready_vms.pop_front() {
            let Some(vm) = self.vms.get_mut(&vm_id) else {
                continue;
            };
            vm.queued = false;
            while let Some(capability_id) = vm.ready_capabilities.pop_front() {
                let Some(capability) = vm.capabilities.get_mut(&capability_id) else {
                    continue;
                };
                if !capability.queued || capability.in_flight {
                    continue;
                }
                let sequence = self.next_sequence;
                let Some(next_sequence) = self.next_sequence.checked_add(1) else {
                    vm.ready_capabilities.push_front(capability_id);
                    vm.queued = true;
                    self.ready_vms.push_front(vm_id);
                    return Err(FairnessError::SequenceExhausted);
                };
                capability.queued = false;
                capability.in_flight = true;
                vm.deficit
                    .add_capped(self.config.vm_quantum, self.config.max_vm_deficit);
                capability.deficit.add_capped(
                    self.config.capability_quantum,
                    self.config.max_capability_deficit,
                );
                let allowance = vm
                    .deficit
                    .turn_allowance(capability.deficit, self.config.capability_quantum);
                self.next_sequence = next_sequence;
                let selection = FairSelection {
                    sequence,
                    vm_id,
                    capability_id,
                    allowance,
                };
                self.in_flight = Some(InFlight {
                    selection: selection.clone(),
                });
                return Ok(Some(selection));
            }
        }
        Ok(None)
    }

    /// Charge actual work and atomically requeue a source that remains ready.
    pub fn complete(
        &mut self,
        selection: &FairSelection<VmId, CapabilityId>,
        used: FairBudget,
        still_ready: bool,
    ) -> Result<(), FairnessError> {
        let Some(outstanding) = self
            .in_flight
            .as_ref()
            .map(|in_flight| &in_flight.selection)
        else {
            return Err(FairnessError::StaleSelection {
                supplied: selection.sequence,
                outstanding: None,
            });
        };
        if outstanding.sequence != selection.sequence
            || outstanding.vm_id != selection.vm_id
            || outstanding.capability_id != selection.capability_id
        {
            return Err(FairnessError::StaleSelection {
                supplied: selection.sequence,
                outstanding: Some(outstanding.sequence),
            });
        }
        let allowance = outstanding.allowance;
        if used.operations > allowance.operations {
            return Err(FairnessError::OperationBudgetExceeded {
                used: used.operations,
                allowance: allowance.operations,
            });
        }
        if used.bytes > allowance.bytes {
            return Err(FairnessError::ByteBudgetExceeded {
                used: used.bytes,
                allowance: allowance.bytes,
            });
        }

        self.in_flight = None;
        let vm = self
            .vms
            .get_mut(&selection.vm_id)
            .ok_or(FairnessError::Invariant)?;
        let capability = vm
            .capabilities
            .get_mut(&selection.capability_id)
            .ok_or(FairnessError::Invariant)?;
        if !capability.in_flight {
            return Err(FairnessError::Invariant);
        }
        vm.deficit.consume(used);
        capability.deficit.consume(used);
        capability.in_flight = false;
        let requeue = still_ready || capability.rearmed;
        capability.rearmed = false;
        if requeue && !capability.queued {
            capability.queued = true;
            vm.ready_capabilities
                .push_back(selection.capability_id.clone());
        }
        if !vm.ready_capabilities.is_empty() && !vm.queued {
            vm.queued = true;
            self.ready_vms.push_back(selection.vm_id.clone());
        }
        if requeue
            || used.operations == selection.allowance.operations
            || used.bytes == selection.allowance.bytes
        {
            if let Some(metrics) = &self.metrics {
                metrics.record_fairness_yield(FairnessLevel::Capability);
            }
        }
        Ok(())
    }

    pub fn remove_capability(
        &mut self,
        vm_id: &VmId,
        capability_id: &CapabilityId,
    ) -> Result<bool, FairnessError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(false);
        };
        if vm
            .capabilities
            .get(capability_id)
            .is_some_and(|capability| capability.in_flight)
        {
            return Err(FairnessError::CapabilityInFlight);
        }
        let removed = vm.capabilities.remove(capability_id).is_some();
        vm.ready_capabilities
            .retain(|queued| queued != capability_id);
        if vm.ready_capabilities.is_empty() && vm.queued {
            vm.queued = false;
            self.ready_vms.retain(|queued| queued != vm_id);
        }
        Ok(removed)
    }

    /// Revoke every queued/rearmed turn for a VM without disturbing a turn
    /// already issued to native work. VM retirement uses this before attempting
    /// removal so no queued capability can run while issued work settles.
    pub fn clear_vm_ready(&mut self, vm_id: &VmId) -> bool {
        self.ready_vms.retain(|queued| queued != vm_id);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return false;
        };
        vm.queued = false;
        vm.ready_capabilities.clear();
        for capability in vm.capabilities.values_mut() {
            capability.queued = false;
            capability.rearmed = false;
        }
        true
    }

    pub fn remove_vm(&mut self, vm_id: &VmId) -> Result<bool, FairnessError> {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|in_flight| &in_flight.selection.vm_id == vm_id)
        {
            return Err(FairnessError::CapabilityInFlight);
        }
        self.ready_vms.retain(|queued| queued != vm_id);
        Ok(self.vms.remove(vm_id).is_some())
    }

    pub fn snapshot(&self, vm_id: &VmId, capability_id: &CapabilityId) -> Option<FairnessSnapshot> {
        let vm = self.vms.get(vm_id)?;
        let capability = vm.capabilities.get(capability_id)?;
        Some(FairnessSnapshot {
            vm_deficit: vm.deficit,
            capability_deficit: capability.deficit,
            vm_queued: vm.queued,
            capability_queued: capability.queued,
            capability_in_flight: capability.in_flight,
            capability_rearmed: capability.rearmed,
        })
    }

    pub fn ready_vm_count(&self) -> usize {
        self.ready_vms.len()
    }

    pub fn ready_capability_count(&self, vm_id: &VmId) -> usize {
        self.vms
            .get(vm_id)
            .map_or(0, |vm| vm.ready_capabilities.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FairnessConfig {
        FairnessConfig {
            vm_quantum: FairBudget::new(4, 4_096),
            capability_quantum: FairBudget::new(2, 1_024),
            max_vm_deficit: FairBudget::new(16, 16_384),
            max_capability_deficit: FairBudget::new(8, 4_096),
            max_vms: 8,
            max_capabilities_per_vm: 8,
        }
    }

    fn broker() -> FairWorkBroker {
        FairWorkBroker::new(config(), RuntimeMetrics::new()).expect("fair work broker")
    }

    #[tokio::test]
    async fn broker_applies_vm_ceiling_and_drop_releases_global_turn() {
        let broker = broker();
        let turn = broker
            .acquire(1, 10, FairBudget::new(1, 128))
            .await
            .expect("first turn");
        assert_eq!(turn.allowance(), FairBudget::new(1, 128));
        drop(turn);

        let next = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            broker.acquire(2, 20, FairBudget::new(2, 512)),
        )
        .await
        .expect("dropped turn must not strand scheduler")
        .expect("next turn");
        next.complete(FairBudget::new(1, 64), false)
            .expect("complete next turn");
    }

    #[tokio::test]
    async fn cancelled_waiter_removes_membership_and_cannot_strand_scheduler() {
        let broker = broker();
        let held = broker
            .acquire(1, 10, FairBudget::new(1, 128))
            .await
            .expect("held turn");
        let waiting = tokio::spawn({
            let broker = broker.clone();
            async move { broker.acquire(2, 20, FairBudget::new(1, 128)).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        waiting.abort();
        assert!(waiting.await.expect_err("waiter cancelled").is_cancelled());
        held.complete(FairBudget::new(1, 64), false)
            .expect("complete held turn");

        let next = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            broker.acquire(3, 30, FairBudget::new(1, 128)),
        )
        .await
        .expect("cancelled waiter must not retain the grant")
        .expect("next turn");
        next.complete(FairBudget::new(1, 64), false)
            .expect("complete next turn");
    }

    #[tokio::test]
    async fn concurrent_waiters_for_one_full_duplex_capability_each_receive_a_turn() {
        let broker = broker();
        let held = broker
            .acquire(1, 10, FairBudget::new(1, 128))
            .await
            .expect("hold capability turn");
        let waiter = |broker: FairWorkBroker| {
            tokio::spawn(async move {
                let turn = broker
                    .acquire(1, 10, FairBudget::new(1, 128))
                    .await
                    .expect("full-duplex waiter turn");
                turn.complete(FairBudget::new(1, 1), false)
                    .expect("complete full-duplex waiter turn");
            })
        };
        let reader = waiter(broker.clone());
        let writer = waiter(broker.clone());
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        held.complete(FairBudget::new(1, 1), false)
            .expect("release held capability turn");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            reader.await.expect("reader waiter joins");
            writer.await.expect("writer waiter joins");
        })
        .await
        .expect("same-capability waiters must not lose their shared ready edge");
    }

    #[tokio::test]
    async fn retiring_an_active_turn_defers_removal_and_rejects_stale_reacquire() {
        let broker = broker();
        let turn = broker
            .acquire(7, 11, FairBudget::new(1, 128))
            .await
            .expect("active turn");

        assert!(broker
            .retire_capability(7, 11)
            .expect("begin active capability retirement"));
        assert!(broker
            .inner
            .state
            .lock()
            .expect("fairness state")
            .scheduler
            .snapshot(&7, &11)
            .is_some_and(|snapshot| snapshot.capability_in_flight));

        turn.complete(FairBudget::new(1, 64), true)
            .expect("settle retired turn");
        assert!(broker
            .inner
            .state
            .lock()
            .expect("fairness state")
            .scheduler
            .snapshot(&7, &11)
            .is_none());
        assert_eq!(
            broker
                .acquire(7, 11, FairBudget::new(1, 128))
                .await
                .expect_err("retired capability must not reacquire"),
            FairnessError::CapabilityRetired {
                vm_generation: 7,
                capability_id: 11,
            }
        );
    }

    #[tokio::test]
    async fn retiring_vm_with_active_turn_defers_removal_and_rejects_generation() {
        let broker = broker();
        let turn = broker
            .acquire(7, 11, FairBudget::new(1, 128))
            .await
            .expect("active VM turn");

        assert!(broker.retire_vm(7).expect("begin active VM retirement"));
        {
            let state = broker.inner.state.lock().expect("fairness state");
            assert!(state.retired_vm_generations.contains(7));
            assert!(state
                .scheduler
                .snapshot(&7, &11)
                .is_some_and(|snapshot| snapshot.capability_in_flight));
            assert_eq!(state.scheduler.ready_capability_count(&7), 0);
        }
        assert_eq!(
            broker
                .acquire(7, 12, FairBudget::new(1, 128))
                .await
                .expect_err("retired VM generation must reject new capabilities"),
            FairnessError::CapabilityRetired {
                vm_generation: 7,
                capability_id: 12,
            }
        );

        turn.complete(FairBudget::new(1, 64), true)
            .expect("settle retired VM turn");
        {
            let state = broker.inner.state.lock().expect("fairness state");
            assert!(state.scheduler.snapshot(&7, &11).is_none());
            assert_eq!(state.scheduler.ready_vm_count(), 0);
        }
        assert_eq!(
            broker
                .acquire(7, 11, FairBudget::new(1, 128))
                .await
                .expect_err("settled retired VM must not resurrect"),
            FairnessError::CapabilityRetired {
                vm_generation: 7,
                capability_id: 11,
            }
        );
    }

    #[tokio::test]
    async fn vm_generation_churn_reclaims_membership_and_compacts_tombstones() {
        let bounded = FairnessConfig {
            max_vms: 1,
            max_capabilities_per_vm: 1,
            ..config()
        };
        let broker = FairWorkBroker::new(bounded, RuntimeMetrics::new()).expect("bounded broker");

        for vm_generation in 1..=1_024 {
            let turn = broker
                .acquire(vm_generation, 1, FairBudget::new(1, 128))
                .await
                .expect("VM churn turn");
            assert!(broker.retire_vm(vm_generation).expect("retire churn VM"));
            turn.complete(FairBudget::new(1, 64), true)
                .expect("settle churn VM turn");
        }

        {
            let state = broker.inner.state.lock().expect("fairness state");
            assert_eq!(state.scheduler.ready_vm_count(), 0);
            assert_eq!(
                state.retired_vm_generations.ranges,
                BTreeMap::from([(1, 1_024)])
            );
        }
        assert_eq!(
            broker
                .acquire(512, 2, FairBudget::new(1, 128))
                .await
                .expect_err("retired VM generation must remain tombstoned"),
            FairnessError::CapabilityRetired {
                vm_generation: 512,
                capability_id: 2,
            }
        );
        let next = broker
            .acquire(1_025, 1, FairBudget::new(1, 128))
            .await
            .expect("VM churn must release bounded scheduler membership");
        next.complete(FairBudget::new(1, 64), false)
            .expect("complete post-churn VM turn");
    }

    #[tokio::test]
    async fn capability_churn_reclaims_scheduler_membership_and_compacts_tombstones() {
        let bounded = FairnessConfig {
            max_vms: 1,
            max_capabilities_per_vm: 1,
            ..config()
        };
        let broker = FairWorkBroker::new(bounded, RuntimeMetrics::new()).expect("bounded broker");

        for capability_id in 1..=1_024 {
            let turn = broker
                .acquire(1, capability_id, FairBudget::new(1, 128))
                .await
                .expect("churn turn");
            turn.complete(FairBudget::new(1, 64), false)
                .expect("complete churn turn");
            assert!(broker
                .retire_capability(1, capability_id)
                .expect("retire churn capability"));
        }

        {
            let state = broker.inner.state.lock().expect("fairness state");
            assert_eq!(state.scheduler.ready_capability_count(&1), 0);
            assert!(state.scheduler.snapshot(&1, &1_024).is_none());
            assert_eq!(
                state
                    .retired_capabilities
                    .get(&1)
                    .expect("retired VM range")
                    .ranges,
                BTreeMap::from([(1, 1_024)])
            );
        }

        let next = broker
            .acquire(1, 1_025, FairBudget::new(1, 128))
            .await
            .expect("churn must release bounded scheduler membership");
        next.complete(FairBudget::new(1, 64), false)
            .expect("complete post-churn turn");
    }

    #[test]
    fn hot_vm_and_hot_capability_rotate_deterministically() {
        let mut scheduler = HierarchicalDeficitRoundRobin::new(config()).expect("scheduler");
        assert_eq!(
            scheduler.mark_ready("vm-a", 1),
            Ok(MembershipUpdate::Enqueued)
        );
        assert_eq!(
            scheduler.mark_ready("vm-a", 2),
            Ok(MembershipUpdate::Enqueued)
        );
        assert_eq!(
            scheduler.mark_ready("vm-b", 7),
            Ok(MembershipUpdate::Enqueued)
        );

        let mut order = Vec::new();
        for _ in 0..12 {
            let selection = scheduler
                .select_next()
                .expect("select")
                .expect("ready selection");
            assert_eq!(selection.sequence, order.len() as u64 + 1);
            order.push((selection.vm_id, selection.capability_id));
            scheduler
                .complete(&selection, selection.allowance, true)
                .expect("complete");
        }

        assert_eq!(
            order,
            vec![
                ("vm-a", 1),
                ("vm-b", 7),
                ("vm-a", 2),
                ("vm-b", 7),
                ("vm-a", 1),
                ("vm-b", 7),
                ("vm-a", 2),
                ("vm-b", 7),
                ("vm-a", 1),
                ("vm-b", 7),
                ("vm-a", 2),
                ("vm-b", 7),
            ]
        );
    }

    #[test]
    fn duplicate_membership_is_coalesced() {
        let mut scheduler = HierarchicalDeficitRoundRobin::new(config()).expect("scheduler");
        assert_eq!(scheduler.mark_ready(1, 10), Ok(MembershipUpdate::Enqueued));
        for _ in 0..10_000 {
            assert_eq!(scheduler.mark_ready(1, 10), Ok(MembershipUpdate::Coalesced));
        }
        assert_eq!(scheduler.ready_vm_count(), 1);
        assert_eq!(scheduler.ready_capability_count(&1), 1);
        let selection = scheduler.select_next().expect("select").expect("selection");
        for _ in 0..10_000 {
            assert_eq!(scheduler.mark_ready(1, 10), Ok(MembershipUpdate::Coalesced));
        }
        scheduler
            .complete(&selection, FairBudget::new(1, 1), false)
            .expect("complete");
        assert_eq!(scheduler.ready_vm_count(), 1);
        assert_eq!(scheduler.ready_capability_count(&1), 1);
        let rearmed = scheduler
            .select_next()
            .expect("select rearmed")
            .expect("rearmed selection");
        scheduler
            .complete(&rearmed, FairBudget::new(1, 1), false)
            .expect("complete rearmed");
        assert!(scheduler.select_next().expect("empty selection").is_none());
    }

    #[test]
    fn membership_state_is_bounded() {
        let bounded = FairnessConfig {
            max_vms: 1,
            max_capabilities_per_vm: 1,
            ..config()
        };
        let mut scheduler = HierarchicalDeficitRoundRobin::new(bounded).expect("scheduler");
        scheduler.mark_ready("vm-a", 1).expect("first member");
        assert_eq!(
            scheduler.mark_ready("vm-a", 2),
            Err(FairnessError::CapabilityLimit { limit: 1 })
        );
        assert_eq!(
            scheduler.mark_ready("vm-b", 1),
            Err(FairnessError::VmLimit { limit: 1 })
        );
    }

    #[test]
    fn idle_credit_is_capped_and_does_not_expand_a_turn() {
        let mut scheduler = HierarchicalDeficitRoundRobin::new(config()).expect("scheduler");
        scheduler.mark_ready("idle", 1).expect("mark idle");
        for _ in 0..100 {
            let selection = scheduler
                .select_next()
                .expect("select idle")
                .expect("idle selection");
            scheduler
                .complete(&selection, FairBudget::default(), true)
                .expect("bank bounded credit");
        }
        let selection = scheduler
            .select_next()
            .expect("select idle")
            .expect("idle selection");
        scheduler
            .complete(&selection, FairBudget::default(), false)
            .expect("make idle");

        let snapshot = scheduler.snapshot(&"idle", &1).expect("idle snapshot");
        assert_eq!(snapshot.vm_deficit, config().max_vm_deficit);
        assert_eq!(snapshot.capability_deficit, config().max_capability_deficit);

        scheduler.mark_ready("hot", 2).expect("mark hot");
        scheduler.mark_ready("idle", 1).expect("wake idle");
        let hot = scheduler
            .select_next()
            .expect("select hot")
            .expect("hot selection");
        assert_eq!(hot.vm_id, "hot");
        scheduler
            .complete(&hot, hot.allowance, true)
            .expect("complete hot");
        let idle = scheduler
            .select_next()
            .expect("select idle")
            .expect("idle selection");
        assert_eq!(idle.vm_id, "idle");
        assert_eq!(idle.allowance, config().capability_quantum);
        let capped = scheduler.snapshot(&"idle", &1).expect("capped snapshot");
        assert_eq!(capped.vm_deficit, config().max_vm_deficit);
        assert_eq!(capped.capability_deficit, config().max_capability_deficit);
    }

    #[test]
    fn completion_enforces_both_budget_dimensions() {
        let mut scheduler = HierarchicalDeficitRoundRobin::new(config()).expect("scheduler");
        scheduler.mark_ready(1, 1).expect("mark ready");
        let selection = scheduler.select_next().expect("select").expect("selection");

        assert!(matches!(
            scheduler.complete(
                &selection,
                FairBudget::new(selection.allowance.operations + 1, 0),
                true,
            ),
            Err(FairnessError::OperationBudgetExceeded { .. })
        ));
        assert!(matches!(
            scheduler.complete(
                &selection,
                FairBudget::new(0, selection.allowance.bytes + 1),
                true,
            ),
            Err(FairnessError::ByteBudgetExceeded { .. })
        ));
        let mut forged = selection.clone();
        forged.allowance = FairBudget::new(usize::MAX, usize::MAX);
        assert!(matches!(
            scheduler.complete(
                &forged,
                FairBudget::new(selection.allowance.operations + 1, 0),
                true,
            ),
            Err(FairnessError::OperationBudgetExceeded { .. })
        ));
        scheduler
            .complete(&selection, selection.allowance, false)
            .expect("valid completion remains possible");
    }
}
