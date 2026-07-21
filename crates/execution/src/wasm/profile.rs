//! Engine-independent WebAssembly proposal profile.
//!
//! Both standalone engines validate these exact switches before invoking their
//! own compiler. Engine defaults therefore cannot silently widen or narrow the
//! AgentOS guest surface.

use crate::backend::HostServiceError;
use wasmparser::{Validator, WasmFeatures};

pub fn locked_wasm_features() -> WasmFeatures {
    let mut features = WasmFeatures::empty();
    // Floating-point operators are part of the MVP profile and are emitted by
    // the owned Linux toolchain even for commands whose public behavior is
    // integer-only (for example, coreutils `ls`).
    features.set(WasmFeatures::FLOATS, true);
    features.set(WasmFeatures::MUTABLE_GLOBAL, true);
    features.set(WasmFeatures::SATURATING_FLOAT_TO_INT, true);
    features.set(WasmFeatures::SIGN_EXTENSION, true);
    features.set(WasmFeatures::REFERENCE_TYPES, true);
    features.set(WasmFeatures::MULTI_VALUE, true);
    features.set(WasmFeatures::BULK_MEMORY, true);
    features.set(WasmFeatures::SIMD, true);
    // C++ commands use finalized WebAssembly exception tags and exnref-based
    // instructions. LLVM 19 still emits the Phase-3 encoding, so the owned
    // toolchain translates it with Binaryen before staging the artifact.
    // Wasmtime intentionally cannot compile the legacy encoding.
    features.set(WasmFeatures::EXCEPTIONS, true);
    features.set(WasmFeatures::LEGACY_EXCEPTIONS, false);
    features
}

/// The explicitly selected pthread profile extends the ordinary AgentOS
/// surface with only core shared-memory threads/atomics. All unrelated
/// proposals remain locked to the same values as the single-thread profile.
pub fn locked_threaded_wasm_features() -> WasmFeatures {
    let mut features = locked_wasm_features();
    features.set(WasmFeatures::THREADS, true);
    features
}

pub fn validate_locked_profile(bytes: &[u8]) -> Result<(), HostServiceError> {
    validate_profile(bytes, false)
}

pub fn validate_locked_threaded_profile(bytes: &[u8]) -> Result<(), HostServiceError> {
    validate_profile(bytes, true)
}

fn validate_profile(bytes: &[u8], threaded: bool) -> Result<(), HostServiceError> {
    let features = if threaded {
        locked_threaded_wasm_features()
    } else {
        locked_wasm_features()
    };
    Validator::new_with_features(features)
        .validate_all(bytes)
        .map(|_| ())
        .map_err(|error| {
            eprintln!("ERR_AGENTOS_WASM_PROFILE_VALIDATION: private validator diagnostic: {error}");
            HostServiceError::new(
                "ERR_AGENTOS_WASM_INVALID_MODULE",
                "WebAssembly module violates the AgentOS feature profile",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_profile_accepts_simd_finalized_exceptions_and_rejects_legacy_threads_and_memory64() {
        let features = locked_wasm_features();
        assert!(features.contains(WasmFeatures::EXCEPTIONS));
        assert!(!features.contains(WasmFeatures::LEGACY_EXCEPTIONS));
        validate_locked_profile(&wat::parse_str("(module (func (drop (f64.const 1))))").unwrap())
            .expect("MVP floating point is enabled");
        validate_locked_profile(
            &wat::parse_str("(module (func (drop (v128.const i32x4 1 2 3 4))))").unwrap(),
        )
        .expect("SIMD128 is enabled");
        validate_locked_profile(&wat::parse_str("(module (tag (param i32)))").unwrap())
            .expect("exception tags required by the canonical DuckDB artifact are enabled");
        let threads = wat::parse_str("(module (memory 1 1 shared))").unwrap();
        assert!(validate_locked_profile(&threads).is_err());
        let memory64 = wat::parse_str("(module (memory i64 1))").unwrap();
        assert!(validate_locked_profile(&memory64).is_err());
    }

    #[test]
    fn threaded_profile_accepts_shared_memory_without_widening_other_proposals() {
        let threads = wat::parse_str("(module (memory 1 2 shared))").unwrap();
        validate_locked_threaded_profile(&threads).expect("core threads are enabled");
        assert!(validate_locked_profile(&threads).is_err());

        let memory64 = wat::parse_str("(module (memory i64 1))").unwrap();
        assert!(validate_locked_threaded_profile(&memory64).is_err());
        let tail_call =
            wat::parse_str("(module (func $callee) (func (return_call $callee)))").unwrap();
        assert!(validate_locked_threaded_profile(&tail_call).is_err());
    }

    #[test]
    fn locked_profile_rejects_multi_memory_relaxed_simd_and_tail_calls() {
        let multi_memory = wat::parse_str("(module (memory 1) (memory 1))").unwrap();
        assert!(validate_locked_profile(&multi_memory).is_err());
        let tail_call =
            wat::parse_str("(module (func $callee) (func (export \"run\") (return_call $callee)))")
                .unwrap();
        assert!(validate_locked_profile(&tail_call).is_err());
    }
}
