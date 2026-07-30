//! Stable AgentOS outcome classification for private Wasmtime diagnostics.

use crate::backend::HostServiceError;

pub(super) fn normalize(
    default_code: &'static str,
    error: &wasmtime::Error,
    cancelled: bool,
) -> HostServiceError {
    // Wasmtime wraps limiter/trap causes with instantiation/call context. Use
    // the complete private chain for classification, but never expose it as an
    // AgentOS API string.
    let diagnostic = format!("{error:#}");
    if diagnostic.contains("forcing trap when growing memory")
        || diagnostic.contains("forcing a memory growth failure to be a trap")
    {
        return HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_MEMORY_LIMIT",
            "WebAssembly linear-memory growth exceeded its configured limit",
        )
        .with_details(serde_json::json!({
            "limitName": "limits.resources.maxWasmMemoryBytes",
        }));
    }
    if diagnostic.contains("forcing trap when growing table")
        || diagnostic.contains("forcing a table growth failure to be a trap")
    {
        return HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_TABLE_LIMIT",
            "WebAssembly table growth exceeded its configured element limit",
        )
        .with_details(serde_json::json!({
            "limitName": "limits.wasm.maxTableElements",
        }));
    }
    if default_code == "ERR_AGENTOS_WASMTIME_INSTANTIATE" {
        if diagnostic.contains("memory minimum size")
            && diagnostic.contains("exceeds memory limits")
        {
            return HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_MEMORY_LIMIT",
                "WebAssembly initial memory exceeds the configured linear-memory limit",
            )
            .with_details(serde_json::json!({
                "limitName": "limits.resources.maxWasmMemoryBytes",
            }));
        }
        if diagnostic.contains("table minimum size") && diagnostic.contains("exceeds table limits")
        {
            return HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_TABLE_LIMIT",
                "WebAssembly initial table exceeds the configured element limit",
            )
            .with_details(serde_json::json!({
                "limitName": "limits.wasm.maxTableElements",
            }));
        }
        return HostServiceError::new(
            "ERR_AGENTOS_WASM_INSTANTIATION",
            "WebAssembly host imports do not match module requirements",
        );
    }
    if cancelled || diagnostic.contains("ERR_AGENTOS_WASMTIME_CANCELED") {
        return HostServiceError::new("ECANCELED", "Wasmtime execution was canceled");
    }
    if diagnostic.contains("ERR_AGENTOS_WASMTIME_ACTIVE_CPU_LIMIT") {
        return HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_ACTIVE_CPU_LIMIT",
            "Wasmtime execution exhausted its active CPU budget",
        );
    }
    if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
        return match trap {
            wasmtime::Trap::OutOfFuel => HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_FUEL_EXHAUSTED",
                "Wasmtime execution exhausted deterministic fuel",
            ),
            wasmtime::Trap::StackOverflow => HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_STACK_EXHAUSTED",
                "Wasmtime execution exhausted its configured stack",
            ),
            wasmtime::Trap::Interrupt => HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_INTERRUPTED",
                "Wasmtime execution was interrupted",
            ),
            wasmtime::Trap::MemoryOutOfBounds => stable_guest_trap(
                "memory-out-of-bounds",
                "WebAssembly accessed memory outside its current bounds",
            ),
            wasmtime::Trap::HeapMisaligned => stable_guest_trap(
                "misaligned-atomic",
                "WebAssembly attempted a misaligned atomic memory operation",
            ),
            wasmtime::Trap::TableOutOfBounds => stable_guest_trap(
                "table-out-of-bounds",
                "WebAssembly accessed a table outside its current bounds",
            ),
            wasmtime::Trap::IndirectCallToNull => stable_guest_trap(
                "null-indirect-call",
                "WebAssembly called an uninitialized table element",
            ),
            wasmtime::Trap::BadSignature => stable_guest_trap(
                "indirect-call-type-mismatch",
                "WebAssembly made an indirect call with the wrong function type",
            ),
            wasmtime::Trap::IntegerOverflow => stable_guest_trap(
                "integer-overflow",
                "WebAssembly integer arithmetic overflowed",
            ),
            wasmtime::Trap::IntegerDivisionByZero => stable_guest_trap(
                "integer-division-by-zero",
                "WebAssembly attempted integer division by zero",
            ),
            wasmtime::Trap::BadConversionToInteger => stable_guest_trap(
                "invalid-float-to-integer",
                "WebAssembly attempted an invalid float-to-integer conversion",
            ),
            wasmtime::Trap::UnreachableCodeReached => stable_guest_trap(
                "unreachable",
                "WebAssembly executed an unreachable instruction",
            ),
            wasmtime::Trap::NullReference => stable_guest_trap(
                "null-reference",
                "WebAssembly dereferenced a null reference",
            ),
            wasmtime::Trap::AllocationTooLarge => stable_guest_trap(
                "allocation-too-large",
                "WebAssembly attempted an allocation that is too large",
            ),
            _ => stable_guest_trap("other", "WebAssembly trapped"),
        };
    }
    HostServiceError::new(default_code, "WebAssembly validation or execution failed")
        .with_details(serde_json::json!({ "engine": "wasmtime" }))
}

fn stable_guest_trap(kind: &'static str, message: &'static str) -> HostServiceError {
    HostServiceError::new("ERR_AGENTOS_WASM_TRAP", message)
        .with_details(serde_json::json!({ "trapKind": kind }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_limit_and_guest_trap_errors_without_engine_strings() {
        let memory = wasmtime::format_err!("forcing trap when growing memory to 131072 bytes");
        let error = normalize("ERR_AGENTOS_WASMTIME_TRAP", &memory, false);
        assert_eq!(error.code, "ERR_AGENTOS_WASMTIME_MEMORY_LIMIT");
        assert_eq!(
            error.details.unwrap()["limitName"],
            "limits.resources.maxWasmMemoryBytes"
        );

        let trap: wasmtime::Error = wasmtime::Trap::IntegerDivisionByZero.into();
        let error = normalize("ERR_AGENTOS_WASMTIME_TRAP", &trap, false);
        assert_eq!(error.code, "ERR_AGENTOS_WASM_TRAP");
        assert_eq!(
            error.details.unwrap()["trapKind"],
            "integer-division-by-zero"
        );
        assert!(!error.message.contains("wasmtime"));
    }
}
