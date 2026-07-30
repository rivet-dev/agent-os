use std::fmt;
use std::sync::{Arc, Mutex};

use crate::metrics::{ExecutorMetricClass, RuntimeMetrics};

pub const VM_EXECUTOR_LIMIT_CONFIG_PATH: &str = "runtime.executor.maxActiveVms";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmExecutorAdmissionSnapshot {
    pub active: usize,
    pub maximum: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmExecutorAdmissionError {
    Limit { active: usize, maximum: usize },
    Poisoned,
}

impl VmExecutorAdmissionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Limit { .. } => "ERR_AGENTOS_VM_EXECUTOR_LIMIT",
            Self::Poisoned => "ERR_AGENTOS_VM_EXECUTOR_POISONED",
        }
    }

    pub fn config_path(&self) -> &'static str {
        VM_EXECUTOR_LIMIT_CONFIG_PATH
    }
}

impl fmt::Display for VmExecutorAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit { active, maximum } => write!(
                formatter,
                "{}: active guest executors reached limit of {maximum} (active={active}); raise {}",
                self.code(),
                self.config_path()
            ),
            Self::Poisoned => write!(
                formatter,
                "{}: process VM executor admission lock poisoned",
                self.code()
            ),
        }
    }
}

impl std::error::Error for VmExecutorAdmissionError {}

#[derive(Debug)]
struct VmExecutorAdmissionInner {
    state: Mutex<VmExecutorAdmissionState>,
    maximum: usize,
    metrics: RuntimeMetrics,
}

#[derive(Debug, Default)]
struct VmExecutorAdmissionState {
    active: usize,
    near_limit_warning_emitted: bool,
}

fn near_limit_threshold(maximum: usize) -> usize {
    maximum.saturating_sub(maximum / 5).max(1)
}

/// Process-wide admission for dedicated guest executor threads.
///
/// Clones share one active count across every engine. The permit must remain
/// owned by the executor generation until its OS thread exits; a detached or
/// stale generation therefore stays charged while it is quarantined.
#[derive(Clone, Debug)]
pub struct VmExecutorAdmission {
    inner: Arc<VmExecutorAdmissionInner>,
}

impl VmExecutorAdmission {
    pub(crate) fn new(maximum: usize, metrics: RuntimeMetrics) -> Self {
        Self {
            inner: Arc::new(VmExecutorAdmissionInner {
                state: Mutex::new(VmExecutorAdmissionState::default()),
                maximum,
                metrics,
            }),
        }
    }

    pub fn maximum(&self) -> usize {
        self.inner.maximum
    }

    pub fn snapshot(&self) -> VmExecutorAdmissionSnapshot {
        let active = self
            .inner
            .state
            .lock()
            .map(|state| state.active)
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_VM_EXECUTOR_POISONED: process VM executor admission snapshot failed"
                );
                self.inner.maximum
            });
        VmExecutorAdmissionSnapshot {
            active,
            maximum: self.inner.maximum,
        }
    }

    pub fn try_acquire(&self) -> Result<VmExecutorPermit, VmExecutorAdmissionError> {
        self.try_acquire_at_most(self.inner.maximum)
    }

    /// Acquire against a caller-requested ceiling without creating a second
    /// counter. The process configuration remains the hard upper bound.
    pub fn try_acquire_at_most(
        &self,
        requested_maximum: usize,
    ) -> Result<VmExecutorPermit, VmExecutorAdmissionError> {
        let maximum = requested_maximum.min(self.inner.maximum);
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VmExecutorAdmissionError::Poisoned)?;
        if state.active >= maximum {
            return Err(VmExecutorAdmissionError::Limit {
                active: state.active,
                maximum,
            });
        }
        state.active += 1;
        let process_warning_threshold = near_limit_threshold(self.inner.maximum);
        if state.active >= process_warning_threshold && !state.near_limit_warning_emitted {
            state.near_limit_warning_emitted = true;
            eprintln!(
                "WARN_AGENTOS_VM_EXECUTOR_NEAR_LIMIT: active={} limit={} threshold={}; raise {} before sustained saturation",
                state.active,
                self.inner.maximum,
                process_warning_threshold,
                VM_EXECUTOR_LIMIT_CONFIG_PATH
            );
        }
        self.inner
            .metrics
            .observe_executor(ExecutorMetricClass::Vm, state.active, 0);
        Ok(VmExecutorPermit {
            admission: self.clone(),
        })
    }
}

/// RAII ownership of one process-wide guest executor slot.
#[derive(Debug)]
pub struct VmExecutorPermit {
    admission: VmExecutorAdmission,
}

impl Drop for VmExecutorPermit {
    fn drop(&mut self) {
        match self.admission.inner.state.lock() {
            Ok(mut state) if state.active > 0 => {
                state.active -= 1;
                if state.active < near_limit_threshold(self.admission.inner.maximum) {
                    state.near_limit_warning_emitted = false;
                }
                self.admission.inner.metrics.observe_executor(
                    ExecutorMetricClass::Vm,
                    state.active,
                    0,
                );
            }
            Ok(_) => eprintln!(
                "ERR_AGENTOS_VM_EXECUTOR_ACCOUNTING_UNDERFLOW: executor permit released at zero"
            ),
            Err(_) => {
                eprintln!("ERR_AGENTOS_VM_EXECUTOR_POISONED: executor permit could not be released")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_saturation_and_permit_drop_releases_capacity() {
        let metrics = RuntimeMetrics::new();
        let admission = VmExecutorAdmission::new(2, metrics.clone());
        let clone = admission.clone();

        let first = admission.try_acquire().expect("first executor permit");
        let second = clone.try_acquire().expect("second executor permit");
        let error = admission
            .try_acquire()
            .expect_err("third executor must saturate the process quota");
        assert_eq!(
            error,
            VmExecutorAdmissionError::Limit {
                active: 2,
                maximum: 2,
            }
        );
        assert_eq!(error.code(), "ERR_AGENTOS_VM_EXECUTOR_LIMIT");
        assert_eq!(error.config_path(), "runtime.executor.maxActiveVms");

        let active = metrics.snapshot().executors[ExecutorMetricClass::Vm.index()].active;
        assert_eq!(active.current, 2);
        assert_eq!(active.high_water, 2);

        drop(first);
        assert_eq!(admission.snapshot().active, 1);
        let replacement = clone
            .try_acquire()
            .expect("dropped permit must release shared capacity");
        drop(second);
        drop(replacement);
        assert_eq!(admission.snapshot().active, 0);
        let active = metrics.snapshot().executors[ExecutorMetricClass::Vm.index()].active;
        assert_eq!(active.current, 0);
        assert_eq!(active.high_water, 2);
    }

    #[test]
    fn requested_ceiling_uses_the_process_counter_and_never_raises_process_limit() {
        let admission = VmExecutorAdmission::new(3, RuntimeMetrics::new());
        let first = admission
            .try_acquire_at_most(1)
            .expect("requested sub-ceiling admits first executor");
        assert!(matches!(
            admission.try_acquire_at_most(1),
            Err(VmExecutorAdmissionError::Limit {
                active: 1,
                maximum: 1
            })
        ));
        let second = admission
            .try_acquire_at_most(usize::MAX)
            .expect("larger request remains bounded by process quota");
        let third = admission
            .try_acquire_at_most(usize::MAX)
            .expect("fill process quota");
        assert!(matches!(
            admission.try_acquire_at_most(usize::MAX),
            Err(VmExecutorAdmissionError::Limit {
                active: 3,
                maximum: 3
            })
        ));
        drop((first, second, third));
        assert_eq!(admission.snapshot().active, 0);
    }

    #[test]
    fn near_limit_warning_is_coalesced_and_rearmed_below_eighty_percent() {
        let admission = VmExecutorAdmission::new(5, RuntimeMetrics::new());
        let mut permits = (0..4)
            .map(|_| admission.try_acquire().expect("fill to warning threshold"))
            .collect::<Vec<_>>();
        assert!(
            admission
                .inner
                .state
                .lock()
                .expect("admission state")
                .near_limit_warning_emitted
        );

        permits.pop();
        assert!(
            !admission
                .inner
                .state
                .lock()
                .expect("admission state")
                .near_limit_warning_emitted,
            "falling below 80% must rearm the warning"
        );
        permits.push(admission.try_acquire().expect("re-enter warning threshold"));
        assert!(
            admission
                .inner
                .state
                .lock()
                .expect("admission state")
                .near_limit_warning_emitted
        );
    }
}
