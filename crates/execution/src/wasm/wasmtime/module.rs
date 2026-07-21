//! Shared-profile module compilation.

use super::super::profile::{validate_locked_profile, validate_locked_threaded_profile};
use super::engine::WasmtimeEngineHandle;
use crate::backend::HostServiceError;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::Module;

pub struct CompiledModule {
    pub module: Arc<Module>,
    pub cache_hit: bool,
    pub profile_validation: Duration,
    pub compilation: Duration,
}

pub fn compile_module(
    engine: &WasmtimeEngineHandle,
    bytes: &[u8],
) -> Result<CompiledModule, HostServiceError> {
    let validation_started = Instant::now();
    match engine.profile().feature_profile {
        super::engine::WasmtimeFeatureProfile::AgentOsOwnedWasiV1 => {
            validate_locked_profile(bytes)?;
        }
        super::engine::WasmtimeFeatureProfile::AgentOsOwnedWasiV1Threads => {
            validate_locked_threaded_profile(bytes)?;
        }
    }
    let profile_validation = validation_started.elapsed();
    let mut modules = engine.modules().lock().map_err(|_| {
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_MODULE_CACHE_POISONED",
            "compiled Module cache lock is poisoned",
        )
    })?;
    let before = modules.metrics();
    let module = modules.get_or_compile(engine.engine(), bytes)?;
    let after = modules.metrics();
    Ok(CompiledModule {
        module,
        cache_hit: after.hits > before.hits,
        profile_validation,
        compilation: after.compile_time.saturating_sub(before.compile_time),
    })
}
