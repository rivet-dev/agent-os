//! Tests for typed create-VM limits config defaults, overrides, and validation.

use agentos_native_sidecar::limits::{vm_limits_from_config, VmLimits};
use agentos_vm_config::{
    BindingLimitsConfig, HttpLimitsConfig, JsRuntimeLimitsConfig, PythonLimitsConfig,
    ResourceLimitsConfig, VmLimitsConfig, WasmLimitsConfig,
};
use serde_json::json;

// Must match the production sidecar wire frame cap (wire::DEFAULT_MAX_FRAME_BYTES),
// which is what vm_limits_from_config is called with at runtime (lib.rs/state.rs).
const SIDECAR_FRAME_CAP: usize = 16 * 1024 * 1024;

#[test]
fn defaults_match_struct_default() {
    let parsed =
        vm_limits_from_config(None, SIDECAR_FRAME_CAP).expect("empty config parses to defaults");
    assert_eq!(parsed, VmLimits::default());
    assert_eq!(
        parsed.js_runtime.v8_heap_limit_mb,
        Some(128),
        "JavaScript heap must be bounded by default"
    );
    assert_eq!(
        parsed.resources.max_wasm_memory_bytes,
        Some(128 * 1024 * 1024),
        "WASM memory must be bounded by default"
    );
    assert_eq!(
        parsed.wasm.active_cpu_time_limit_ms, 30_000,
        "WASM active CPU must have a default runaway safeguard"
    );
    assert_eq!(
        parsed.wasm.wall_clock_limit_ms, None,
        "WASM wall-clock cutoff must remain opt-in"
    );
    assert_eq!(parsed.wasm.deterministic_fuel, None);
}

#[test]
fn overrides_only_present_keys() {
    let config = VmLimitsConfig {
        bindings: Some(BindingLimitsConfig {
            max_binding_schema_bytes: Some(4096),
            ..Default::default()
        }),
        wasm: Some(WasmLimitsConfig {
            max_module_file_bytes: Some(1_048_576),
            active_cpu_time_limit_ms: Some(90_000),
            wall_clock_limit_ms: Some(120_000),
            deterministic_fuel: Some(1_000_000),
            ..Default::default()
        }),
        js_runtime: Some(JsRuntimeLimitsConfig {
            v8_heap_limit_mb: Some(256),
            ..Default::default()
        }),
        python: Some(PythonLimitsConfig {
            execution_timeout_ms: Some(1000),
            ..Default::default()
        }),
        http: Some(HttpLimitsConfig {
            max_fetch_response_bytes: Some(65_536),
        }),
        ..Default::default()
    };
    let parsed = vm_limits_from_config(Some(&config), SIDECAR_FRAME_CAP).expect("valid overrides");

    assert_eq!(parsed.bindings.max_binding_schema_bytes, 4096);
    assert_eq!(parsed.wasm.max_module_file_bytes, 1_048_576);
    assert_eq!(parsed.wasm.active_cpu_time_limit_ms, 90_000);
    assert_eq!(parsed.wasm.wall_clock_limit_ms, Some(120_000));
    assert_eq!(parsed.wasm.deterministic_fuel, Some(1_000_000));
    assert_eq!(parsed.js_runtime.v8_heap_limit_mb, Some(256));
    assert_eq!(parsed.python.execution_timeout_ms, 1000);
    assert_eq!(parsed.http.max_fetch_response_bytes, 65536);

    // Unspecified fields keep defaults.
    let defaults = VmLimits::default();
    assert_eq!(
        parsed.bindings.max_registered_collections,
        defaults.bindings.max_registered_collections
    );
    assert_eq!(
        parsed.wasm.sync_read_limit_bytes,
        defaults.wasm.sync_read_limit_bytes
    );
}

#[test]
fn resources_subset_threads_through() {
    let config = VmLimitsConfig {
        resources: Some(ResourceLimitsConfig {
            max_processes: Some(8),
            ..Default::default()
        }),
        ..Default::default()
    };
    let parsed =
        vm_limits_from_config(Some(&config), SIDECAR_FRAME_CAP).expect("resources thread through");
    assert_eq!(parsed.resources.max_processes, Some(8));
}

#[test]
fn rejects_unparseable_value() {
    let error = serde_json::from_value::<VmLimitsConfig>(json!({
        "bindings": { "maxBindingSchemaBytes": "not-a-number" }
    }))
    .expect_err("unparseable value rejected");
    assert!(error.to_string().contains("invalid type"));
}

#[test]
fn rejects_fetch_body_exceeding_frame_cap() {
    let config = VmLimitsConfig {
        http: Some(HttpLimitsConfig {
            max_fetch_response_bytes: Some((SIDECAR_FRAME_CAP + 1) as u64),
        }),
        ..Default::default()
    };
    let error = vm_limits_from_config(Some(&config), SIDECAR_FRAME_CAP)
        .expect_err("oversized fetch body rejected");
    assert!(error.to_string().contains("wire frame cap"));
}

#[test]
fn rejects_default_timeout_above_max() {
    let config = VmLimitsConfig {
        bindings: Some(BindingLimitsConfig {
            default_binding_timeout_ms: Some(60_000),
            max_binding_timeout_ms: Some(30_000),
            ..Default::default()
        }),
        ..Default::default()
    };
    let error =
        vm_limits_from_config(Some(&config), SIDECAR_FRAME_CAP).expect_err("default above max");
    assert!(error.to_string().contains("max_binding_timeout_ms"));
}

#[test]
fn rejects_zero_buffer_cap() {
    let config = VmLimitsConfig {
        js_runtime: Some(JsRuntimeLimitsConfig {
            captured_output_limit_bytes: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let error =
        vm_limits_from_config(Some(&config), SIDECAR_FRAME_CAP).expect_err("zero buffer cap");
    assert!(error.to_string().contains("captured_output_limit_bytes"));
}
