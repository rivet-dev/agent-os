//! Process-wide Engine profiles and feature configuration.

use super::cache::{WasmtimeModuleCache, WasmtimeModuleCacheMetrics};
use crate::backend::HostServiceError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use wasmtime::{Config, Engine, OptLevel, WasmFeatures};

pub const DEFAULT_WASM_STACK_BYTES: usize = 512 * 1024;
pub const HOST_CALL_STACK_HEADROOM_BYTES: usize = 1536 * 1024;
pub const DEFAULT_MAX_ENGINE_PROFILES: usize = 8;
pub const ENGINE_PROFILE_LIMIT_CONFIG_PATH: &str = "limits.wasm.maxEngineProfiles";

/// Low-cardinality process metrics for operator diagnostics and Phase 3
/// measurement. Cache counters are aggregated across exact Engine profiles;
/// RSS is process resident memory and is deliberately distinct from Wasmtime's
/// conservative charged-code estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WasmtimeMetricsSnapshot {
    pub engine_profiles: usize,
    pub module_entries: usize,
    pub module_cache_hits: u64,
    pub module_cache_misses: u64,
    pub module_cache_evictions: u64,
    pub compiled_source_bytes: u64,
    pub charged_module_bytes: usize,
    pub compile_time: Duration,
    pub process_retained_rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmtimeFeatureProfile {
    /// AgentOS-owned Preview1/POSIX ABI with the proposal switches configured
    /// in `build_engine`; changing any switch requires a new keyed variant.
    AgentOsOwnedWasiV1,
    /// AgentOS-owned Preview1/POSIX ABI plus the core WebAssembly threads
    /// proposal. This profile is selected explicitly and never inferred.
    AgentOsOwnedWasiV1Threads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WasmtimeEngineProfile {
    pub feature_profile: WasmtimeFeatureProfile,
    pub wasm_stack_bytes: usize,
}

impl WasmtimeEngineProfile {
    pub fn new(wasm_stack_bytes: Option<u64>) -> Result<Self, HostServiceError> {
        Self::with_feature_profile(wasm_stack_bytes, WasmtimeFeatureProfile::AgentOsOwnedWasiV1)
    }

    pub fn new_threaded(wasm_stack_bytes: Option<u64>) -> Result<Self, HostServiceError> {
        Self::with_feature_profile(
            wasm_stack_bytes,
            WasmtimeFeatureProfile::AgentOsOwnedWasiV1Threads,
        )
    }

    fn with_feature_profile(
        wasm_stack_bytes: Option<u64>,
        feature_profile: WasmtimeFeatureProfile,
    ) -> Result<Self, HostServiceError> {
        let wasm_stack_bytes = wasm_stack_bytes
            .map(usize::try_from)
            .transpose()
            .map_err(|_| invalid_stack("WASM stack limit does not fit this platform"))?
            .unwrap_or(DEFAULT_WASM_STACK_BYTES);
        if wasm_stack_bytes == 0 {
            return Err(invalid_stack("WASM stack limit must be greater than zero"));
        }
        wasm_stack_bytes
            .checked_add(HOST_CALL_STACK_HEADROOM_BYTES)
            .ok_or_else(|| invalid_stack("WASM plus host-call stack reservation overflows"))?;
        Ok(Self {
            feature_profile,
            wasm_stack_bytes,
        })
    }

    pub fn async_stack_bytes(self) -> Result<usize, HostServiceError> {
        self.wasm_stack_bytes
            .checked_add(HOST_CALL_STACK_HEADROOM_BYTES)
            .ok_or_else(|| invalid_stack("WASM plus host-call stack reservation overflows"))
    }
}

fn invalid_stack(message: &'static str) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_WASMTIME_STACK_CONFIG", message)
        .with_details(serde_json::json!({ "configPath": "limits.resources.maxWasmStackBytes" }))
}

pub struct WasmtimeEngineHandle {
    profile: WasmtimeEngineProfile,
    engine: Engine,
    modules: Mutex<WasmtimeModuleCache>,
}

impl std::fmt::Debug for WasmtimeEngineHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmtimeEngineHandle")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl WasmtimeEngineHandle {
    pub fn profile(&self) -> WasmtimeEngineProfile {
        self.profile
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(super) fn modules(&self) -> &Mutex<WasmtimeModuleCache> {
        &self.modules
    }
}

#[derive(Debug, Default)]
struct RegistryState {
    engines: HashMap<WasmtimeEngineProfile, Arc<WasmtimeEngineHandle>>,
    near_limit_warned: bool,
}

#[derive(Debug)]
pub struct WasmtimeEngineRegistry {
    maximum_profiles: usize,
    state: Mutex<RegistryState>,
}

impl WasmtimeEngineRegistry {
    pub fn process() -> &'static Self {
        static PROCESS_REGISTRY: OnceLock<WasmtimeEngineRegistry> = OnceLock::new();
        static EPOCH_TICKER: OnceLock<()> = OnceLock::new();
        let registry = PROCESS_REGISTRY.get_or_init(|| Self::new(DEFAULT_MAX_ENGINE_PROFILES));
        EPOCH_TICKER.get_or_init(|| {
            // AGENTOS_THREAD_SITE: process-wasmtime-epoch-ticker
            std::thread::Builder::new()
                .name(String::from("agentos-wasmtime-epoch"))
                .spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(10));
                    let engines = match registry.state.lock() {
                        Ok(state) => state
                            .engines
                            .values()
                            .map(|handle| handle.engine.clone())
                            .collect::<Vec<_>>(),
                        Err(_) => {
                            eprintln!(
                                "ERR_AGENTOS_WASMTIME_ENGINE_REGISTRY_POISONED: epoch ticker cannot inspect Engine profiles"
                            );
                            continue;
                        }
                    };
                    for engine in engines {
                        engine.increment_epoch();
                    }
                })
                .expect("process Wasmtime epoch ticker must start");
        });
        registry
    }

    pub fn new(maximum_profiles: usize) -> Self {
        assert!(maximum_profiles > 0, "engine-profile limit must be nonzero");
        Self {
            maximum_profiles,
            state: Mutex::new(RegistryState::default()),
        }
    }

    pub fn get_or_create(
        &self,
        profile: WasmtimeEngineProfile,
    ) -> Result<Arc<WasmtimeEngineHandle>, HostServiceError> {
        let mut state = self.state.lock().map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_ENGINE_REGISTRY_POISONED",
                "Wasmtime Engine registry lock is poisoned",
            )
        })?;
        if let Some(engine) = state.engines.get(&profile) {
            return Ok(Arc::clone(engine));
        }
        let observed = state.engines.len().saturating_add(1);
        if observed > self.maximum_profiles {
            return Err(HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_ENGINE_PROFILE_LIMIT",
                "Wasmtime Engine profile limit exceeded",
            )
            .with_details(serde_json::json!({
                "limitName": ENGINE_PROFILE_LIMIT_CONFIG_PATH,
                "limit": self.maximum_profiles,
                "observed": observed,
            })));
        }
        if observed >= near_limit_threshold(self.maximum_profiles) && !state.near_limit_warned {
            state.near_limit_warned = true;
            eprintln!(
                "WARN_AGENTOS_WASMTIME_ENGINE_PROFILES_NEAR_LIMIT: active={} limit={} config={}",
                observed, self.maximum_profiles, ENGINE_PROFILE_LIMIT_CONFIG_PATH
            );
        }

        let engine = Arc::new(build_engine(profile)?);
        state.engines.insert(profile, Arc::clone(&engine));
        Ok(engine)
    }

    pub fn profile_count(&self) -> Result<usize, HostServiceError> {
        self.state
            .lock()
            .map(|state| state.engines.len())
            .map_err(|_| {
                HostServiceError::new(
                    "ERR_AGENTOS_WASMTIME_ENGINE_REGISTRY_POISONED",
                    "Wasmtime Engine registry lock is poisoned",
                )
            })
    }

    pub fn metrics(&self) -> Result<WasmtimeMetricsSnapshot, HostServiceError> {
        let state = self.state.lock().map_err(|_| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_ENGINE_REGISTRY_POISONED",
                "Wasmtime Engine registry lock is poisoned",
            )
        })?;
        let mut result = WasmtimeMetricsSnapshot {
            engine_profiles: state.engines.len(),
            process_retained_rss_bytes: process_retained_rss_bytes(),
            ..WasmtimeMetricsSnapshot::default()
        };
        for handle in state.engines.values() {
            let metrics = handle
                .modules
                .lock()
                .map_err(|_| {
                    HostServiceError::new(
                        "ERR_AGENTOS_WASMTIME_MODULE_CACHE_POISONED",
                        "Wasmtime Module cache lock is poisoned",
                    )
                })?
                .metrics();
            add_cache_metrics(&mut result, metrics);
        }
        Ok(result)
    }
}

fn add_cache_metrics(result: &mut WasmtimeMetricsSnapshot, metrics: WasmtimeModuleCacheMetrics) {
    result.module_entries = result.module_entries.saturating_add(metrics.entries);
    result.module_cache_hits = result.module_cache_hits.saturating_add(metrics.hits);
    result.module_cache_misses = result.module_cache_misses.saturating_add(metrics.misses);
    result.module_cache_evictions = result
        .module_cache_evictions
        .saturating_add(metrics.evictions);
    result.compiled_source_bytes = result
        .compiled_source_bytes
        .saturating_add(metrics.source_bytes);
    result.charged_module_bytes = result
        .charged_module_bytes
        .saturating_add(metrics.charged_bytes);
    result.compile_time = result.compile_time.saturating_add(metrics.compile_time);
}

#[cfg(target_os = "linux")]
fn process_retained_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kibibytes.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn process_retained_rss_bytes() -> Option<u64> {
    None
}

fn build_engine(profile: WasmtimeEngineProfile) -> Result<WasmtimeEngineHandle, HostServiceError> {
    let threaded = matches!(
        profile.feature_profile,
        WasmtimeFeatureProfile::AgentOsOwnedWasiV1Threads
    );
    let mut config = Config::new();
    config
        .epoch_interruption(true)
        .consume_fuel(true)
        .max_wasm_stack(profile.wasm_stack_bytes)
        .async_stack_size(profile.async_stack_bytes()?)
        .async_stack_zeroing(true)
        .cranelift_opt_level(OptLevel::Speed)
        .memory_init_cow(true)
        .shared_memory(threaded)
        .wasm_features(WasmFeatures::TAIL_CALL, false)
        .wasm_features(WasmFeatures::CUSTOM_PAGE_SIZES, false)
        .wasm_features(WasmFeatures::THREADS, threaded)
        .wasm_features(WasmFeatures::SHARED_EVERYTHING_THREADS, false)
        .wasm_features(WasmFeatures::REFERENCE_TYPES, true)
        .wasm_features(WasmFeatures::FUNCTION_REFERENCES, false)
        .wasm_features(WasmFeatures::GC, false)
        .wasm_features(WasmFeatures::SIMD, true)
        .wasm_features(WasmFeatures::RELAXED_SIMD, false)
        .wasm_features(WasmFeatures::BULK_MEMORY, true)
        .wasm_features(WasmFeatures::MULTI_VALUE, true)
        .wasm_features(WasmFeatures::MULTI_MEMORY, false)
        .wasm_features(WasmFeatures::MEMORY64, false)
        .wasm_features(WasmFeatures::EXCEPTIONS, true)
        .wasm_features(WasmFeatures::LEGACY_EXCEPTIONS, false)
        .wasm_features(WasmFeatures::COMPONENT_MODEL, false)
        .wasm_features(WasmFeatures::STACK_SWITCHING, false)
        .wasm_features(WasmFeatures::WIDE_ARITHMETIC, false);
    let engine = Engine::new(&config).map_err(|error| {
        eprintln!("ERR_AGENTOS_WASMTIME_ENGINE_CONFIG: private engine diagnostic: {error:#}");
        HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_ENGINE_CONFIG",
            "failed to construct the configured WebAssembly engine",
        )
    })?;
    Ok(WasmtimeEngineHandle {
        profile,
        engine,
        modules: Mutex::new(WasmtimeModuleCache::default()),
    })
}

fn near_limit_threshold(limit: usize) -> usize {
    limit.saturating_sub(limit / 5).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_profile_reserves_locked_host_headroom() {
        let profile = WasmtimeEngineProfile::new(None).expect("default profile");
        assert_eq!(profile.wasm_stack_bytes, 512 * 1024);
        assert_eq!(profile.async_stack_bytes().unwrap(), 2 * 1024 * 1024);
        assert!(WasmtimeEngineProfile::new(Some(0)).is_err());
    }

    #[test]
    fn threaded_profile_is_a_distinct_exact_engine_key() {
        let ordinary = WasmtimeEngineProfile::new(None).unwrap();
        let threaded = WasmtimeEngineProfile::new_threaded(None).unwrap();
        assert_ne!(ordinary, threaded);
        assert_eq!(
            threaded.feature_profile,
            WasmtimeFeatureProfile::AgentOsOwnedWasiV1Threads
        );
    }

    #[test]
    fn registry_is_exact_profile_keyed_and_bounded() {
        let registry = WasmtimeEngineRegistry::new(2);
        let default = WasmtimeEngineProfile::new(None).unwrap();
        let first = registry.get_or_create(default).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &registry.get_or_create(default).unwrap()
        ));
        registry
            .get_or_create(WasmtimeEngineProfile::new(Some(1024 * 1024)).unwrap())
            .unwrap();
        let error = registry
            .get_or_create(WasmtimeEngineProfile::new(Some(2 * 1024 * 1024)).unwrap())
            .unwrap_err();
        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_ENGINE_PROFILE_LIMIT");
        assert_eq!(
            error.details.unwrap()["limitName"],
            ENGINE_PROFILE_LIMIT_CONFIG_PATH
        );
        let metrics = registry.metrics().unwrap();
        assert_eq!(metrics.engine_profiles, 2);
        assert_eq!(metrics.module_entries, 0);
    }
}
