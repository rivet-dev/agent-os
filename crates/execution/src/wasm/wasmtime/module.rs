//! Shared-profile module compilation.

use super::super::profile::validate_locked_profile;
use super::engine::WasmtimeEngineHandle;
use crate::backend::HostServiceError;
use std::sync::Arc;
use wasmtime::Module;

pub fn compile_module(
    engine: &WasmtimeEngineHandle,
    bytes: &[u8],
) -> Result<Arc<Module>, HostServiceError> {
    validate_locked_profile(bytes)?;
    engine
        .modules()
        .lock()
        .map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_MODULE_CACHE_POISONED",
                "compiled Module cache lock is poisoned",
            )
        })?
        .get_or_compile(engine.engine(), bytes)
}
