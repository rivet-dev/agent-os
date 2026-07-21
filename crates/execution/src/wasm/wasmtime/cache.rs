//! Bounded compiled-module cache.

use crate::backend::HostServiceError;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::{Engine, Module};

pub const DEFAULT_MODULE_CACHE_ENTRIES: usize = 32;
pub const DEFAULT_MODULE_CACHE_CHARGED_BYTES: usize = 256 * 1024 * 1024;
const MINIMUM_MODULE_CHARGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WasmtimeModuleCacheMetrics {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub source_bytes: u64,
    pub charged_bytes: usize,
    pub compile_time: Duration,
}

#[derive(Debug)]
struct CacheEntry {
    module: Arc<Module>,
    charged_bytes: usize,
}

#[derive(Debug)]
pub struct WasmtimeModuleCache {
    maximum_entries: usize,
    maximum_charged_bytes: usize,
    entries: HashMap<[u8; 32], CacheEntry>,
    lru: VecDeque<[u8; 32]>,
    metrics: WasmtimeModuleCacheMetrics,
    near_limit_warned: bool,
}

impl Default for WasmtimeModuleCache {
    fn default() -> Self {
        Self::new(
            DEFAULT_MODULE_CACHE_ENTRIES,
            DEFAULT_MODULE_CACHE_CHARGED_BYTES,
        )
    }
}

impl WasmtimeModuleCache {
    pub fn new(maximum_entries: usize, maximum_charged_bytes: usize) -> Self {
        assert!(maximum_entries > 0);
        assert!(maximum_charged_bytes > 0);
        Self {
            maximum_entries,
            maximum_charged_bytes,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            metrics: WasmtimeModuleCacheMetrics::default(),
            near_limit_warned: false,
        }
    }

    pub fn get_or_compile(
        &mut self,
        engine: &Engine,
        bytes: &[u8],
    ) -> Result<Arc<Module>, HostServiceError> {
        let key: [u8; 32] = Sha256::digest(bytes).into();
        if let Some(module) = self
            .entries
            .get(&key)
            .map(|entry| Arc::clone(&entry.module))
        {
            self.metrics.hits = self.metrics.hits.saturating_add(1);
            self.touch(key);
            return Ok(module);
        }
        self.metrics.misses = self.metrics.misses.saturating_add(1);
        let charged_bytes = module_charge(bytes.len())?;
        if charged_bytes > self.maximum_charged_bytes {
            return Err(cache_limit_error(
                "limits.wasm.moduleCacheBytes",
                self.maximum_charged_bytes,
                charged_bytes,
            ));
        }
        while self.entries.len() >= self.maximum_entries
            || self.metrics.charged_bytes.saturating_add(charged_bytes) > self.maximum_charged_bytes
        {
            self.evict_lru()?;
        }
        let started = Instant::now();
        let module = Arc::new(Module::new(engine, bytes).map_err(|error| {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_MODULE_COMPILE: private compiler diagnostic: {error:#}"
            );
            HostServiceError::new(
                "ERR_AGENTOS_WASM_INVALID_MODULE",
                "WebAssembly module could not be compiled for the configured feature profile",
            )
        })?);
        self.metrics.compile_time = self.metrics.compile_time.saturating_add(started.elapsed());
        self.metrics.source_bytes = self
            .metrics
            .source_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.metrics.charged_bytes = self.metrics.charged_bytes.saturating_add(charged_bytes);
        self.entries.insert(
            key,
            CacheEntry {
                module: Arc::clone(&module),
                charged_bytes,
            },
        );
        self.lru.push_back(key);
        if self.metrics.charged_bytes >= near_limit_threshold(self.maximum_charged_bytes)
            && !self.near_limit_warned
        {
            self.near_limit_warned = true;
            eprintln!(
                "WARN_AGENTOS_WASMTIME_MODULE_CACHE_NEAR_LIMIT: chargedBytes={} limit={} config=limits.wasm.moduleCacheBytes",
                self.metrics.charged_bytes, self.maximum_charged_bytes
            );
        }
        Ok(module)
    }

    pub fn metrics(&self) -> WasmtimeModuleCacheMetrics {
        WasmtimeModuleCacheMetrics {
            entries: self.entries.len(),
            ..self.metrics
        }
    }

    fn touch(&mut self, key: [u8; 32]) {
        if let Some(index) = self.lru.iter().position(|candidate| *candidate == key) {
            self.lru.remove(index);
        }
        self.lru.push_back(key);
    }

    fn evict_lru(&mut self) -> Result<(), HostServiceError> {
        let key = self.lru.pop_front().ok_or_else(|| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_MODULE_CACHE_ACCOUNTING",
                "module cache cannot free enough charged capacity",
            )
        })?;
        let entry = self.entries.remove(&key).ok_or_else(|| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_MODULE_CACHE_ACCOUNTING",
                "module cache LRU references a missing entry",
            )
        })?;
        self.metrics.charged_bytes = self
            .metrics
            .charged_bytes
            .saturating_sub(entry.charged_bytes);
        self.metrics.evictions = self.metrics.evictions.saturating_add(1);
        if self.metrics.charged_bytes < near_limit_threshold(self.maximum_charged_bytes) {
            self.near_limit_warned = false;
        }
        Ok(())
    }
}

fn module_charge(source_bytes: usize) -> Result<usize, HostServiceError> {
    source_bytes
        .checked_mul(8)
        .map(|bytes| bytes.max(MINIMUM_MODULE_CHARGE_BYTES))
        .ok_or_else(|| {
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_MODULE_CACHE_CHARGE_OVERFLOW",
                "module cache charge overflows this platform",
            )
        })
}

fn cache_limit_error(name: &'static str, limit: usize, observed: usize) -> HostServiceError {
    HostServiceError::new(
        "ERR_AGENTOS_WASMTIME_MODULE_CACHE_LIMIT",
        "compiled Wasmtime Module exceeds the cache admission budget",
    )
    .with_details(serde_json::json!({
        "limitName": name,
        "limit": limit,
        "observed": observed,
    }))
}

fn near_limit_threshold(limit: usize) -> usize {
    limit.saturating_sub(limit / 5).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::Config;

    fn engine() -> Engine {
        let config = Config::new();
        Engine::new(&config).unwrap()
    }

    #[test]
    fn cache_reuses_modules_and_evicts_at_both_bounds() {
        let engine = engine();
        let first = wat::parse_str("(module (func (export \"first\")))").unwrap();
        let second = wat::parse_str("(module (func (export \"second\")))").unwrap();
        let mut cache = WasmtimeModuleCache::new(1, 2 * MINIMUM_MODULE_CHARGE_BYTES);
        let first_module = cache.get_or_compile(&engine, &first).unwrap();
        assert!(Arc::ptr_eq(
            &first_module,
            &cache.get_or_compile(&engine, &first).unwrap()
        ));
        cache.get_or_compile(&engine, &second).unwrap();
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.evictions, 1);
        assert_eq!(metrics.charged_bytes, MINIMUM_MODULE_CHARGE_BYTES);
    }

    #[test]
    fn single_oversized_module_is_rejected_with_named_limit() {
        let engine = engine();
        let mut cache = WasmtimeModuleCache::new(1, MINIMUM_MODULE_CHARGE_BYTES - 1);
        let error = cache
            .get_or_compile(&engine, &wat::parse_str("(module)").unwrap())
            .unwrap_err();
        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_MODULE_CACHE_LIMIT");
        assert_eq!(
            error.details.unwrap()["limitName"],
            "limits.wasm.moduleCacheBytes"
        );
    }
}
