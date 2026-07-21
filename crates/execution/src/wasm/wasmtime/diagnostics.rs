//! Opt-in, best-effort per-execution benchmark diagnostics.

use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PHASE_METRICS_PREFIX: &str = "__AGENTOS_WASM_PHASE_METRICS__:";

#[derive(Debug, Default)]
struct DiagnosticState {
    phases: Vec<(&'static str, Duration)>,
    first_host_call: Option<Duration>,
    first_guest_host_call: Option<Duration>,
    first_output: Option<Duration>,
    module_bytes: Option<usize>,
    module_cache_hit: Option<bool>,
    guest_linear_memory_bytes: usize,
    async_stack_bytes: usize,
    reserved_store_bytes: usize,
}

#[derive(Debug)]
pub struct ExecutionDiagnostics {
    enabled: bool,
    started: Instant,
    state: Mutex<DiagnosticState>,
}

impl ExecutionDiagnostics {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Instant::now(),
            state: Mutex::new(DiagnosticState::default()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn phase(&self, name: &'static str, elapsed: Duration) {
        if !self.enabled {
            return;
        }
        self.with_state(|state| state.phases.push((name, elapsed)));
    }

    pub fn first_host_call(&self) {
        if self.enabled {
            let elapsed = self.started.elapsed();
            self.with_state(|state| {
                state.first_host_call.get_or_insert(elapsed);
            });
        }
    }

    pub fn first_guest_host_call(&self) {
        if self.enabled {
            let elapsed = self.started.elapsed();
            self.with_state(|state| {
                state.first_guest_host_call.get_or_insert(elapsed);
            });
        }
    }

    pub fn first_output(&self) {
        if self.enabled {
            let elapsed = self.started.elapsed();
            self.with_state(|state| {
                state.first_output.get_or_insert(elapsed);
            });
        }
    }

    pub fn module(&self, bytes: usize, cache_hit: bool) {
        if self.enabled {
            self.with_state(|state| {
                state.module_bytes = Some(bytes);
                state.module_cache_hit = Some(cache_hit);
            });
        }
    }

    pub fn store_memory(
        &self,
        guest_linear_memory_bytes: usize,
        async_stack_bytes: usize,
        reserved_store_bytes: usize,
    ) {
        if self.enabled {
            self.with_state(|state| {
                state.guest_linear_memory_bytes = state
                    .guest_linear_memory_bytes
                    .max(guest_linear_memory_bytes);
                state.async_stack_bytes = state.async_stack_bytes.max(async_stack_bytes);
                state.reserved_store_bytes = state.reserved_store_bytes.max(reserved_store_bytes);
            });
        }
    }

    pub fn line(&self, reason: &str, module_path: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        let state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_DIAGNOSTICS_POISONED: recovering phase diagnostic state"
            );
            poisoned.into_inner()
        });
        let phases = state
            .phases
            .iter()
            .map(|(name, elapsed)| json!({ "name": name, "ms": millis(*elapsed) }))
            .collect::<Vec<_>>();
        let payload = json!({
            "reason": reason,
            "backend": "wasmtime",
            "modulePath": module_path,
            "sourceModuleBytes": state.module_bytes,
            "moduleBytes": state.module_bytes,
            "moduleCacheHit": state.module_cache_hit,
            "memoryAllocation": "on-demand",
            "memoryInitCow": true,
            "memoryInitializationIncludedInPhase": "Instance",
            "firstHostCallMs": state.first_host_call.map(millis),
            "firstGuestHostCallMs": state.first_guest_host_call.map(millis),
            "firstOutputMs": state.first_output.map(millis),
            "guestLinearMemoryBytes": state.guest_linear_memory_bytes,
            "asyncStackBytes": state.async_stack_bytes,
            "reservedStoreBytes": state.reserved_store_bytes,
            "totalMs": millis(self.started.elapsed()),
            "phases": phases,
        });
        serde_json::to_vec(&payload).ok().map(|mut bytes| {
            let mut line = PHASE_METRICS_PREFIX.as_bytes().to_vec();
            line.append(&mut bytes);
            line.push(b'\n');
            line
        })
    }

    fn with_state<T>(&self, update: impl FnOnce(&mut DiagnosticState) -> T) -> T {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_DIAGNOSTICS_POISONED: recovering phase diagnostic state"
            );
            poisoned.into_inner()
        });
        update(&mut state)
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_diagnostics_use_the_shared_phase_marker_and_stable_fields() {
        let diagnostics = ExecutionDiagnostics::new(true);
        diagnostics.phase("moduleRead", Duration::from_millis(2));
        diagnostics.first_host_call();
        diagnostics.first_guest_host_call();
        diagnostics.first_output();
        diagnostics.module(1234, true);
        diagnostics.store_memory(65_536, 2 * 1024 * 1024, 128 * 1024 * 1024);
        let line = String::from_utf8(
            diagnostics
                .line("completed", "/bin/example")
                .expect("enabled line"),
        )
        .expect("utf8 diagnostics");
        let payload: serde_json::Value = serde_json::from_str(
            line.strip_prefix(PHASE_METRICS_PREFIX)
                .expect("shared phase prefix"),
        )
        .expect("phase JSON");
        assert_eq!(payload["backend"], "wasmtime");
        assert_eq!(payload["modulePath"], "/bin/example");
        assert_eq!(payload["sourceModuleBytes"], 1234);
        assert_eq!(payload["moduleBytes"], 1234);
        assert_eq!(payload["moduleCacheHit"], true);
        assert_eq!(payload["memoryAllocation"], "on-demand");
        assert_eq!(payload["memoryInitCow"], true);
        assert_eq!(payload["guestLinearMemoryBytes"], 65_536);
        assert_eq!(payload["phases"][0]["name"], "moduleRead");
    }

    #[test]
    fn disabled_diagnostics_emit_nothing() {
        assert!(ExecutionDiagnostics::new(false)
            .line("completed", "/bin/example")
            .is_none());
    }
}
