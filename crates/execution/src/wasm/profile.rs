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
    features
}

pub fn validate_locked_profile(bytes: &[u8]) -> Result<(), HostServiceError> {
    Validator::new_with_features(locked_wasm_features())
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
    fn locked_profile_accepts_simd_and_rejects_threads_and_memory64() {
        validate_locked_profile(&wat::parse_str("(module (func (drop (f64.const 1))))").unwrap())
            .expect("MVP floating point is enabled");
        validate_locked_profile(
            &wat::parse_str("(module (func (drop (v128.const i32x4 1 2 3 4))))").unwrap(),
        )
        .expect("SIMD128 is enabled");
        let threads = wat::parse_str("(module (memory 1 1 shared))").unwrap();
        assert!(validate_locked_profile(&threads).is_err());
        let memory64 = wat::parse_str("(module (memory i64 1))").unwrap();
        assert!(validate_locked_profile(&memory64).is_err());
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
