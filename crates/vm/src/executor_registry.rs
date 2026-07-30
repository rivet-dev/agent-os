use std::collections::BTreeSet;

use agentos_executor_contract::backend::HostServiceError;
use agentos_executor_wasm_abi::StandaloneWasmBackend;
use agentos_sidecar_protocol::protocol::GuestRuntimeKind;

/// A concrete execution engine that may be injected into a VM manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutorKind {
    NodeV8,
    PythonV8Pyodide,
    WasmV8,
    WasmWasmtime,
    WasmWasmtimeThreads,
}

impl ExecutorKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NodeV8 => "node-v8",
            Self::PythonV8Pyodide => "python-v8-pyodide",
            Self::WasmV8 => "wasm-v8",
            Self::WasmWasmtime => "wasm-wasmtime",
            Self::WasmWasmtimeThreads => "wasm-wasmtime-threads",
        }
    }
}

/// Engine availability injected by the process composition root.
///
/// An empty registry is a supported embedded-OS configuration. Kernel, VFS,
/// mount, snapshot, and lifecycle operations remain available; only requests
/// that need an engine fail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutorRegistry {
    available: BTreeSet<ExecutorKind>,
}

impl ExecutorRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with(mut self, executor: ExecutorKind) -> Self {
        self.available.insert(executor);
        self
    }

    pub fn insert(&mut self, executor: ExecutorKind) -> bool {
        self.available.insert(executor)
    }

    pub fn contains(&self, executor: ExecutorKind) -> bool {
        self.available.contains(&executor)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = ExecutorKind> + '_ {
        self.available.iter().copied()
    }

    /// Verifies that the registry can serve a requested guest runtime.
    ///
    /// Embedders may call this before issuing an execution request. An empty
    /// registry returns the same typed executor-unavailable error as VM
    /// dispatch while leaving non-execution VM operations available.
    pub fn require(
        &self,
        runtime: GuestRuntimeKind,
        wasm_backend: StandaloneWasmBackend,
    ) -> Result<(), HostServiceError> {
        let executor = match runtime {
            GuestRuntimeKind::JavaScript => ExecutorKind::NodeV8,
            GuestRuntimeKind::Python => ExecutorKind::PythonV8Pyodide,
            GuestRuntimeKind::WebAssembly => match wasm_backend {
                StandaloneWasmBackend::V8 => ExecutorKind::WasmV8,
                StandaloneWasmBackend::Wasmtime => ExecutorKind::WasmWasmtime,
                StandaloneWasmBackend::WasmtimeThreads => ExecutorKind::WasmWasmtimeThreads,
            },
        };
        if self.contains(executor) {
            return Ok(());
        }
        Err(HostServiceError::new(
            "ERR_AGENTOS_EXECUTOR_UNAVAILABLE",
            format!(
                "the {} executor is not registered in this VM manager",
                executor.name()
            ),
        )
        .with_details(serde_json::json!({
            "executor": executor.name(),
            "runtime": match runtime {
                GuestRuntimeKind::JavaScript => "javascript",
                GuestRuntimeKind::Python => "python",
                GuestRuntimeKind::WebAssembly => "webassembly",
            },
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_a_stable_typed_error() {
        let error = ExecutorRegistry::empty()
            .require(GuestRuntimeKind::JavaScript, StandaloneWasmBackend::V8)
            .expect_err("empty registry must reject execution");
        assert_eq!(error.code, "ERR_AGENTOS_EXECUTOR_UNAVAILABLE");
        assert_eq!(
            error.details.as_ref().expect("executor error details")["executor"],
            "node-v8"
        );
    }
}
