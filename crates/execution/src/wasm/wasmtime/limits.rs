//! Store, memory, table, instance, stack, CPU, and cache limits.

use super::super::WasmExecutionLimits;
use crate::backend::HostServiceError;
use wasmtime::{StoreLimits, StoreLimitsBuilder};

pub const DEFAULT_MAX_WASM_MEMORY_BYTES: usize = 128 * 1024 * 1024;
pub const DEFAULT_MAX_TABLE_ELEMENTS: usize = 1_000_000;
pub const DEFAULT_TABLE_ACCOUNTING_BYTES: usize =
    DEFAULT_MAX_TABLE_ELEMENTS * std::mem::size_of::<usize>();
pub const DEFAULT_MAX_INSTANCES: usize = 1;
pub const DEFAULT_MAX_TABLES: usize = 1;
pub const DEFAULT_MAX_MEMORIES: usize = 1;

pub fn store_limits(limits: &WasmExecutionLimits) -> Result<StoreLimits, HostServiceError> {
    let memory_bytes = limits
        .max_memory_bytes
        .map(usize::try_from)
        .transpose()
        .map_err(|_| limit_overflow("limits.resources.maxWasmMemoryBytes"))?
        .unwrap_or(DEFAULT_MAX_WASM_MEMORY_BYTES);
    Ok(StoreLimitsBuilder::new()
        .memory_size(memory_bytes)
        .table_elements(DEFAULT_MAX_TABLE_ELEMENTS)
        .instances(DEFAULT_MAX_INSTANCES)
        .tables(DEFAULT_MAX_TABLES)
        .memories(DEFAULT_MAX_MEMORIES)
        .trap_on_grow_failure(true)
        .build())
}

pub fn max_memory_bytes(limits: &WasmExecutionLimits) -> Result<usize, HostServiceError> {
    limits
        .max_memory_bytes
        .map(usize::try_from)
        .transpose()
        .map_err(|_| limit_overflow("limits.resources.maxWasmMemoryBytes"))
        .map(|value| value.unwrap_or(DEFAULT_MAX_WASM_MEMORY_BYTES))
}

pub fn aggregate_store_memory_bytes(
    limits: &WasmExecutionLimits,
) -> Result<usize, HostServiceError> {
    max_memory_bytes(limits)?
        .checked_add(DEFAULT_TABLE_ACCOUNTING_BYTES)
        .ok_or_else(|| limit_overflow("limits.resources.maxWasmMemoryBytes"))
}

fn limit_overflow(name: &'static str) -> HostServiceError {
    HostServiceError::new(
        "ERR_AGENTOS_WASMTIME_LIMIT_CONFIG",
        format!("{name} does not fit this platform"),
    )
    .with_details(serde_json::json!({ "limitName": name }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Engine, Instance, Module, Store};

    fn instantiate_with(limits: StoreLimits, wat: &str) -> Result<(), wasmtime::Error> {
        let engine = Engine::default();
        let bytes = wat::parse_str(wat)?;
        let module = Module::new(&engine, bytes)?;
        let mut store = Store::new(&engine, limits);
        store.limiter(|limits| limits);
        Instance::new(&mut store, &module, &[])?;
        Ok(())
    }

    #[test]
    fn store_limits_accept_exact_memory_and_table_bounds_and_reject_overflow() {
        let request = WasmExecutionLimits {
            max_memory_bytes: Some(65_536),
            ..WasmExecutionLimits::default()
        };
        instantiate_with(
            store_limits(&request).expect("memory limits"),
            "(module (memory 1))",
        )
        .expect("exact memory bound");
        assert!(instantiate_with(
            store_limits(&request).expect("memory limits"),
            "(module (memory 2))",
        )
        .is_err());

        instantiate_with(
            store_limits(&WasmExecutionLimits::default()).expect("table limits"),
            &format!("(module (table {DEFAULT_MAX_TABLE_ELEMENTS} funcref))"),
        )
        .expect("exact table bound");
        assert!(instantiate_with(
            store_limits(&WasmExecutionLimits::default()).expect("table limits"),
            &format!(
                "(module (table {} funcref))",
                DEFAULT_MAX_TABLE_ELEMENTS + 1
            ),
        )
        .is_err());
    }

    #[test]
    fn store_instance_count_is_bounded() {
        let engine = Engine::default();
        let module = Module::new(
            &engine,
            wat::parse_str("(module)").expect("empty module bytes"),
        )
        .expect("empty module");
        let mut store = Store::new(&engine, StoreLimitsBuilder::new().instances(1).build());
        store.limiter(|limits| limits);
        Instance::new(&mut store, &module, &[]).expect("first instance");
        assert!(Instance::new(&mut store, &module, &[]).is_err());
    }
}
