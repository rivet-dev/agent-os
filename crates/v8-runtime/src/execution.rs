// Script compilation, CJS/ESM execution, module loading

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroI32;

use crate::bridge::{deserialize_v8_value, serialize_v8_value};
use crate::host_call::BridgeCallContext;
use crate::ipc::ExecutionError;
#[cfg(test)]
use crate::ipc::{OsConfig, ProcessConfig};

/// Cached V8 code cache data for bridge code compilation.
///
/// Stores the compiled bytecode from V8's ScriptCompiler::CreateCodeCache
/// along with a hash of the source for invalidation. On subsequent
/// compilations with the same bridge code, the cache is consumed via
/// CompileOptions::ConsumeCodeCache, skipping parsing and initial compilation.
pub struct BridgeCodeCache {
    /// FNV-1a hash of the bridge code source string
    source_hash: u64,
    /// Raw code cache bytes from UnboundScript::create_code_cache()
    cached_data: Vec<u8>,
}

impl BridgeCodeCache {
    /// Compute FNV-1a hash of bridge code source
    fn hash_source(source: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in source.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

/// Callback that denies all WebAssembly code generation.
extern "C" fn deny_wasm_code_generation(
    _context: v8::Local<v8::Context>,
    _source: v8::Local<v8::String>,
) -> bool {
    false
}

/// Disable WebAssembly compilation on the isolate.
/// Must be called before any code execution.
pub fn disable_wasm(isolate: &mut v8::OwnedIsolate) {
    isolate.set_allow_wasm_code_generation_callback(deny_wasm_code_generation);
}

/// Inject `_processConfig` and `_osConfig` as frozen, non-writable, non-configurable
/// global properties, and harden the context (remove SharedArrayBuffer in freeze mode).
///
/// Must be called within a ContextScope.
#[cfg(test)]
pub fn inject_globals(
    scope: &mut v8::HandleScope,
    process_config: &ProcessConfig,
    os_config: &OsConfig,
) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    // Build and freeze _processConfig
    let pc_obj = build_process_config(scope, process_config);
    pc_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    let pc_key = v8::String::new(scope, "_processConfig").unwrap();
    let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, pc_key.into(), pc_obj.into(), attr);

    // Build and freeze _osConfig
    let os_obj = build_os_config(scope, os_config);
    os_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    let os_key = v8::String::new(scope, "_osConfig").unwrap();
    let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, os_key.into(), os_obj.into(), attr);

    // SharedArrayBuffer removal for timing mitigation is handled by the JS-side
    // bridge code (applyTimingMitigationFreeze), which runs AFTER the bridge bundle
    // loads. The bridge bundle depends on SharedArrayBuffer being available during
    // its initialization (whatwg-url/webidl-conversions uses it).
}

/// Inject globals from a V8-serialized payload containing { processConfig, osConfig }.
///
/// The payload is produced by node:v8.serialize() on the host side.
/// Deserializes into V8, extracts processConfig and osConfig, freezes them,
/// and sets them as non-writable, non-configurable global properties.
pub fn inject_globals_from_payload(scope: &mut v8::HandleScope, payload: &[u8]) {
    let context = scope.get_current_context();
    let global = context.global(scope);

    // Deserialize the V8 payload { processConfig, osConfig }
    let config_val = match deserialize_v8_value(scope, payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to deserialize InjectGlobals payload: {}", e);
            return;
        }
    };

    let config_obj = match config_val.to_object(scope) {
        Some(obj) => obj,
        None => {
            eprintln!("InjectGlobals payload is not an object");
            return;
        }
    };

    // Extract and set _processConfig
    let pc_key = v8::String::new(scope, "processConfig").unwrap();
    if let Some(pc_val) = config_obj.get(scope, pc_key.into()) {
        if let Some(pc_obj) = pc_val.to_object(scope) {
            pc_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
        }
        let global_key = v8::String::new(scope, "_processConfig").unwrap();
        let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
        global.define_own_property(scope, global_key.into(), pc_val, attr);
    }

    // Extract and set _osConfig
    let oc_key = v8::String::new(scope, "osConfig").unwrap();
    if let Some(oc_val) = config_obj.get(scope, oc_key.into()) {
        if let Some(oc_obj) = oc_val.to_object(scope) {
            oc_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
        }
        let global_key = v8::String::new(scope, "_osConfig").unwrap();
        let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
        global.define_own_property(scope, global_key.into(), oc_val, attr);
    }
}

/// Compile and run bridge code as a V8 Script, using code cache if available.
///
/// On cache miss (first compilation or hash mismatch): compiles with
/// NoCompileOptions and creates a code cache from the resulting UnboundScript.
/// On cache hit: compiles with ConsumeCodeCache using the cached bytecode.
/// Creates its own TryCatch scope internally so the caller's scope is released.
/// Returns (exit_code, error) — exit code 0 on success.
fn run_bridge_cached(
    scope: &mut v8::HandleScope,
    bridge_code: &str,
    cache: &mut Option<BridgeCodeCache>,
) -> (i32, Option<ExecutionError>) {
    let tc = &mut v8::TryCatch::new(scope);

    let v8_source = match v8::String::new(tc, bridge_code) {
        Some(s) => s,
        None => {
            return (
                1,
                Some(ExecutionError {
                    error_type: "Error".into(),
                    message: "bridge code string too large for V8".into(),
                    stack: String::new(),
                    code: None,
                }),
            );
        }
    };

    // Resource name for bridge code (needed for code cache to work)
    let resource_name = v8::String::new(tc, "<bridge>").unwrap();
    let origin = v8::ScriptOrigin::new(
        tc,
        resource_name.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        false,
        None,
    );

    let source_hash = BridgeCodeCache::hash_source(bridge_code);

    // Check if cache is valid for this bridge code
    let cache_hit = cache.as_ref().is_some_and(|c| c.source_hash == source_hash);

    let script = if cache_hit {
        // Consume cached bytecode
        let cached_bytes = &cache.as_ref().unwrap().cached_data;
        let cached_data = v8::script_compiler::CachedData::new(cached_bytes);
        let mut source = v8::script_compiler::Source::new_with_cached_data(
            v8_source,
            Some(&origin),
            cached_data,
        );
        let compiled = v8::script_compiler::compile(
            tc,
            &mut source,
            v8::script_compiler::CompileOptions::ConsumeCodeCache,
            v8::script_compiler::NoCacheReason::NoReason,
        );
        // If cache was rejected, invalidate it (will be regenerated next time)
        if source.get_cached_data().is_some_and(|cd| cd.rejected()) {
            *cache = None;
        }
        compiled
    } else {
        // First compilation or cache invalidated — compile without cache
        let mut source = v8::script_compiler::Source::new(v8_source, Some(&origin));
        let compiled = v8::script_compiler::compile(
            tc,
            &mut source,
            v8::script_compiler::CompileOptions::NoCompileOptions,
            v8::script_compiler::NoCacheReason::NoReason,
        );
        // Generate code cache from the compiled script
        if let Some(ref script) = compiled {
            let unbound = script.get_unbound_script(tc);
            if let Some(code_cache) = unbound.create_code_cache() {
                *cache = Some(BridgeCodeCache {
                    source_hash,
                    cached_data: code_cache.to_vec(),
                });
            }
        }
        compiled
    };

    // Run the compiled script
    let script = match script {
        Some(s) => s,
        None => {
            return match tc.exception() {
                Some(e) => {
                    let (c, err) = exception_to_result(tc, e);
                    (c, Some(err))
                }
                None => (1, None),
            };
        }
    };

    if script.run(tc).is_none() {
        return match tc.exception() {
            Some(e) => {
                let (c, err) = exception_to_result(tc, e);
                (c, Some(err))
            }
            None => (1, None),
        };
    }

    (0, None)
}

/// Run a short init script (e.g. post-restore config). Compiles and executes
/// via v8::Script, returning (exit_code, error) on failure. No code caching.
#[cfg(not(test))]
pub fn run_init_script(scope: &mut v8::HandleScope, code: &str) -> (i32, Option<ExecutionError>) {
    if code.is_empty() {
        return (0, None);
    }
    let tc = &mut v8::TryCatch::new(scope);
    let source = match v8::String::new(tc, code) {
        Some(s) => s,
        None => {
            return (
                1,
                Some(ExecutionError {
                    error_type: "Error".into(),
                    message: "init script string too large for V8".into(),
                    stack: String::new(),
                    code: None,
                }),
            );
        }
    };
    let script = match v8::Script::compile(tc, source, None) {
        Some(s) => s,
        None => {
            return match tc.exception() {
                Some(e) => {
                    let (c, err) = exception_to_result(tc, e);
                    (c, Some(err))
                }
                None => (1, None),
            };
        }
    };
    if script.run(tc).is_none() {
        return match tc.exception() {
            Some(e) => {
                let (c, err) = exception_to_result(tc, e);
                (c, Some(err))
            }
            None => (1, None),
        };
    }
    (0, None)
}

/// Execute user code as a CJS script (mode='exec').
///
/// Runs bridge_code as IIFE first (if non-empty), then compiles and runs user_code
/// via v8::Script. Returns (exit_code, error) — exit code 0 on success, 1 on error.
/// The `bridge_cache` parameter enables code caching for repeated bridge compilations.
pub fn execute_script(
    scope: &mut v8::HandleScope,
    bridge_code: &str,
    user_code: &str,
    bridge_cache: &mut Option<BridgeCodeCache>,
) -> (i32, Option<ExecutionError>) {
    // Run bridge code IIFE (with code caching)
    if !bridge_code.is_empty() {
        let (code, err) = run_bridge_cached(scope, bridge_code, bridge_cache);
        if code != 0 {
            return (code, err);
        }
    }

    // Run user code
    {
        let tc = &mut v8::TryCatch::new(scope);
        let source = match v8::String::new(tc, user_code) {
            Some(s) => s,
            None => {
                return (
                    1,
                    Some(ExecutionError {
                        error_type: "Error".into(),
                        message: "user code string too large for V8".into(),
                        stack: String::new(),
                        code: None,
                    }),
                )
            }
        };
        let script = match v8::Script::compile(tc, source, None) {
            Some(s) => s,
            None => {
                return match tc.exception() {
                    Some(e) => {
                        let (c, err) = exception_to_result(tc, e);
                        (c, Some(err))
                    }
                    None => (1, None),
                };
            }
        };
        let completion = match script.run(tc) {
            Some(result) => result,
            None => {
                return match tc.exception() {
                    Some(e) => {
                        let (c, err) = exception_to_result(tc, e);
                        (c, Some(err))
                    }
                    None => (1, None),
                };
            }
        };

        // Flush microtasks once after every exec()-style script so process.nextTick()
        // and zero-delay bridge callbacks run before we decide whether more event-loop
        // work is pending.
        tc.perform_microtask_checkpoint();

        if let Some(exception) = tc.exception() {
            let (c, err) = exception_to_result(tc, exception);
            return (c, Some(err));
        }

        if let Some(state) = tc.get_slot_mut::<crate::isolate::PromiseRejectState>() {
            if let Some((_, err)) = state.unhandled.drain().next() {
                return (1, Some(err));
            }
        }

        // Surface rejected async completions for exec()-style scripts that
        // return a Promise (for example an async IIFE ending in await import()).
        if completion.is_promise() {
            let promise = v8::Local::<v8::Promise>::try_from(completion).unwrap();
            match promise.state() {
                v8::PromiseState::Pending => {
                    set_pending_script_evaluation(tc, promise);
                    return (0, None);
                }
                v8::PromiseState::Rejected => {
                    let rejection = promise.result(tc);
                    let (c, err) = exception_to_result(tc, rejection);
                    return (c, Some(err));
                }
                v8::PromiseState::Fulfilled => {}
            }
        }
    }

    (0, None)
}

/// Check if a V8 exception is a ProcessExitError (has `_isProcessExit: true` sentinel).
/// Returns `Some(exit_code)` if detected, `None` otherwise.
///
/// ProcessExitError is detected by sentinel property, not by regex matching on the
/// error message or constructor name.
pub fn extract_process_exit_code(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
) -> Option<i32> {
    if !exception.is_object() {
        return None;
    }
    let obj = v8::Local::<v8::Object>::try_from(exception).ok()?;
    let sentinel_key = v8::String::new(scope, "_isProcessExit")?;
    let sentinel_val = obj.get(scope, sentinel_key.into())?;
    if !sentinel_val.is_true() {
        return None;
    }
    // Extract numeric exit code from .code property
    let code_key = v8::String::new(scope, "code")?;
    let code_val = obj.get(scope, code_key.into())?;
    if code_val.is_number() {
        Some(code_val.int32_value(scope).unwrap_or(1))
    } else {
        Some(1)
    }
}

/// Extract error info and exit code from a V8 exception.
/// For ProcessExitError (detected via _isProcessExit sentinel), returns the error's exit code.
/// For other errors, returns exit code 1.
pub(crate) fn exception_to_result(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
) -> (i32, ExecutionError) {
    let exit_code = extract_process_exit_code(scope, exception).unwrap_or(1);
    let error = extract_error_info(scope, exception);
    (exit_code, error)
}

/// Extract structured error information from a V8 exception value.
///
/// Reads constructor.name for error type, .message for the message,
/// .stack for the stack trace, and optional .code for Node-style error codes.
pub(crate) fn extract_error_info(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
) -> ExecutionError {
    if !exception.is_object() {
        // Non-object throw (e.g., `throw "string"`)
        return ExecutionError {
            error_type: "Error".into(),
            message: exception.to_rust_string_lossy(scope),
            stack: String::new(),
            code: None,
        };
    }

    let obj = v8::Local::<v8::Object>::try_from(exception).unwrap();

    // Error type from constructor.name
    let error_type = {
        let ctor_key = v8::String::new(scope, "constructor").unwrap();
        let name_key = v8::String::new(scope, "name").unwrap();
        obj.get(scope, ctor_key.into())
            .filter(|v| v.is_object())
            .and_then(|ctor| {
                let ctor_obj = v8::Local::<v8::Object>::try_from(ctor).ok()?;
                ctor_obj.get(scope, name_key.into())
            })
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy(scope))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Error".into())
    };

    // Message from error.message property
    let message = {
        let msg_key = v8::String::new(scope, "message").unwrap();
        obj.get(scope, msg_key.into())
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_else(|| exception.to_rust_string_lossy(scope))
    };

    // Stack trace from error.stack property
    let stack = {
        let stack_key = v8::String::new(scope, "stack").unwrap();
        obj.get(scope, stack_key.into())
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy(scope))
            .unwrap_or_default()
    };

    // Optional error code (e.g., ERR_MODULE_NOT_FOUND)
    let code = {
        let code_key = v8::String::new(scope, "code").unwrap();
        obj.get(scope, code_key.into())
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy(scope))
    };

    ExecutionError {
        error_type,
        message,
        stack,
        code,
    }
}

/// Build the _processConfig JS object: { cwd, env, timing_mitigation, frozen_time_ms }
#[cfg(test)]
fn build_process_config<'s>(
    scope: &mut v8::HandleScope<'s>,
    config: &ProcessConfig,
) -> v8::Local<'s, v8::Object> {
    let obj = v8::Object::new(scope);

    // cwd
    let key = v8::String::new(scope, "cwd").unwrap();
    let val = v8::String::new(scope, &config.cwd).unwrap();
    obj.set(scope, key.into(), val.into());

    // env (frozen sub-object)
    let env_key = v8::String::new(scope, "env").unwrap();
    let env_obj = v8::Object::new(scope);
    for (k, v) in &config.env {
        let ek = v8::String::new(scope, k).unwrap();
        let ev = v8::String::new(scope, v).unwrap();
        env_obj.set(scope, ek.into(), ev.into());
    }
    env_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    obj.set(scope, env_key.into(), env_obj.into());

    // timing_mitigation
    let key = v8::String::new(scope, "timing_mitigation").unwrap();
    let val = v8::String::new(scope, &config.timing_mitigation).unwrap();
    obj.set(scope, key.into(), val.into());

    // frozen_time_ms (number or null)
    let key = v8::String::new(scope, "frozen_time_ms").unwrap();
    let val: v8::Local<v8::Value> = match config.frozen_time_ms {
        Some(ms) => v8::Number::new(scope, ms).into(),
        None => v8::null(scope).into(),
    };
    obj.set(scope, key.into(), val);

    obj
}

/// Build the _osConfig JS object: { homedir, tmpdir, platform, arch }
#[cfg(test)]
fn build_os_config<'s>(
    scope: &mut v8::HandleScope<'s>,
    config: &OsConfig,
) -> v8::Local<'s, v8::Object> {
    let obj = v8::Object::new(scope);

    for (name, value) in [
        ("homedir", config.homedir.as_str()),
        ("tmpdir", config.tmpdir.as_str()),
        ("platform", config.platform.as_str()),
        ("arch", config.arch.as_str()),
    ] {
        let key = v8::String::new(scope, name).unwrap();
        let val = v8::String::new(scope, value).unwrap();
        obj.set(scope, key.into(), val.into());
    }

    obj
}

// --- ESM module loading ---

/// Thread-local state for module resolution during execute_module.
/// Avoids passing user data through V8's ResolveModuleCallback (which is a plain fn pointer).
struct ModuleResolveState {
    bridge_ctx: *const BridgeCallContext,
    /// identity_hash → resource_name for referrer lookup
    module_names: HashMap<NonZeroI32, String>,
    /// resolved_path and referrer-qualified request keys → Global<Module> cache
    module_cache: HashMap<String, v8::Global<v8::Module>>,
}

// SAFETY: ModuleResolveState is only accessed from the session thread
// (single-threaded per session). The raw pointer is valid for the
// duration of execute_module.
unsafe impl Send for ModuleResolveState {}

/// Deferred root-module completion state for async ESM evaluation.
///
/// When `module.evaluate()` returns a pending promise (for example because the
/// entry module or one of its dependencies uses top-level `await`), the session
/// thread keeps the module + promise alive across the bridge event loop and
/// finalizes exports only after the promise settles.
#[cfg_attr(test, allow(dead_code))]
struct PendingModuleEvaluation {
    module: v8::Global<v8::Module>,
    promise: v8::Global<v8::Promise>,
}

// SAFETY: PendingModuleEvaluation is only accessed from the session thread
// (single-threaded per session).
unsafe impl Send for PendingModuleEvaluation {}

struct PendingScriptEvaluation {
    promise: v8::Global<v8::Promise>,
}

unsafe impl Send for PendingScriptEvaluation {}

thread_local! {
    static MODULE_RESOLVE_STATE: RefCell<Option<ModuleResolveState>> = const { RefCell::new(None) };
    static PENDING_MODULE_EVALUATION: RefCell<Option<PendingModuleEvaluation>> = const { RefCell::new(None) };
    static PENDING_SCRIPT_EVALUATION: RefCell<Option<PendingScriptEvaluation>> = const { RefCell::new(None) };
}

fn module_request_cache_key(specifier: &str, referrer_name: &str) -> String {
    format!("{}\0{}", referrer_name, specifier)
}

#[cfg_attr(test, allow(dead_code))]
pub fn clear_module_state() {
    MODULE_RESOLVE_STATE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub fn clear_pending_module_evaluation() {
    PENDING_MODULE_EVALUATION.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

pub fn clear_pending_script_evaluation() {
    PENDING_SCRIPT_EVALUATION.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

#[cfg_attr(test, allow(dead_code))]
pub fn has_pending_module_evaluation() -> bool {
    PENDING_MODULE_EVALUATION.with(|cell| cell.borrow().is_some())
}

pub fn has_pending_script_evaluation() -> bool {
    PENDING_SCRIPT_EVALUATION.with(|cell| cell.borrow().is_some())
}

pub fn pending_module_evaluation_needs_wait(scope: &mut v8::HandleScope) -> bool {
    PENDING_MODULE_EVALUATION.with(|cell| {
        let borrow = cell.borrow();
        let Some(pending) = borrow.as_ref() else {
            return false;
        };
        let promise = v8::Local::new(scope, &pending.promise);
        promise.state() == v8::PromiseState::Pending
    })
}

pub fn pending_script_evaluation_needs_wait(scope: &mut v8::HandleScope) -> bool {
    PENDING_SCRIPT_EVALUATION.with(|cell| {
        let borrow = cell.borrow();
        let Some(pending) = borrow.as_ref() else {
            return false;
        };
        let promise = v8::Local::new(scope, &pending.promise);
        promise.state() == v8::PromiseState::Pending
    })
}

fn set_pending_module_evaluation(
    scope: &mut v8::HandleScope,
    module: v8::Local<v8::Module>,
    promise: v8::Local<v8::Promise>,
) {
    PENDING_MODULE_EVALUATION.with(|cell| {
        *cell.borrow_mut() = Some(PendingModuleEvaluation {
            module: v8::Global::new(scope, module),
            promise: v8::Global::new(scope, promise),
        });
    });
}

pub fn set_pending_script_evaluation(
    scope: &mut v8::HandleScope,
    promise: v8::Local<v8::Promise>,
) {
    PENDING_SCRIPT_EVALUATION.with(|cell| {
        *cell.borrow_mut() = Some(PendingScriptEvaluation {
            promise: v8::Global::new(scope, promise),
        });
    });
}

pub(crate) fn take_unhandled_promise_rejection(
    scope: &mut v8::HandleScope,
) -> Option<ExecutionError> {
    scope
        .get_slot_mut::<crate::isolate::PromiseRejectState>()
        .and_then(|state| state.unhandled.drain().next().map(|(_, err)| err))
}

pub fn finalize_pending_script_evaluation(
    scope: &mut v8::HandleScope,
) -> Option<(i32, Option<ExecutionError>)> {
    let pending = PENDING_SCRIPT_EVALUATION.with(|cell| cell.borrow_mut().take())?;
    let tc = &mut v8::TryCatch::new(scope);
    let promise = v8::Local::new(tc, &pending.promise);

    tc.perform_microtask_checkpoint();

    if let Some(exception) = tc.exception() {
        let (code, err) = exception_to_result(tc, exception);
        return Some((code, Some(err)));
    }

    if let Some(err) = take_unhandled_promise_rejection(tc) {
        return Some((1, Some(err)));
    }

    match promise.state() {
        v8::PromiseState::Pending => {
            PENDING_SCRIPT_EVALUATION.with(|cell| {
                *cell.borrow_mut() = Some(pending);
            });
            None
        }
        v8::PromiseState::Rejected => {
            let rejection = promise.result(tc);
            let (code, err) = exception_to_result(tc, rejection);
            Some((code, Some(err)))
        }
        v8::PromiseState::Fulfilled => Some((0, None)),
    }
}

fn serialize_module_exports(
    scope: &mut v8::HandleScope,
    module: v8::Local<v8::Module>,
) -> Result<Vec<u8>, ExecutionError> {
    // Serialize module namespace (exports)
    // If the ESM namespace is empty, fall back to globalThis.module.exports
    // for CJS compatibility (code using module.exports = {...}).
    // The module namespace is a V8 exotic object that ValueSerializer can't
    // handle directly, so we copy its properties into a plain object.
    let namespace = module.get_module_namespace();
    let namespace_obj = namespace.to_object(scope).unwrap();
    let prop_names = namespace_obj
        .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
        .unwrap();
    let exports_val: v8::Local<v8::Value> = if prop_names.length() == 0 {
        // No ESM exports — check CJS module.exports fallback
        let ctx = scope.get_current_context();
        let global = ctx.global(scope);
        let module_key = v8::String::new(scope, "module").unwrap();
        let cjs_exports = global
            .get(scope, module_key.into())
            .and_then(|m| m.to_object(scope))
            .and_then(|m| {
                let exports_key = v8::String::new(scope, "exports").unwrap();
                m.get(scope, exports_key.into())
            })
            .filter(|v| !v.is_undefined() && !v.is_null_or_undefined());
        match cjs_exports {
            Some(val) => val,
            None => v8::Object::new(scope).into(),
        }
    } else {
        let plain = v8::Object::new(scope);
        for i in 0..prop_names.length() {
            let key = prop_names.get_index(scope, i).unwrap();
            let val = namespace_obj
                .get(scope, key)
                .unwrap_or_else(|| v8::undefined(scope).into());
            plain.set(scope, key, val);
        }
        plain.into()
    };

    serialize_v8_value(scope, exports_val).map_err(|err| ExecutionError {
        error_type: "Error".into(),
        message: format!("failed to serialize exports: {}", err),
        stack: String::new(),
        code: None,
    })
}

#[cfg_attr(test, allow(dead_code))]
pub fn finalize_pending_module_evaluation(
    scope: &mut v8::HandleScope,
) -> Option<(i32, Option<Vec<u8>>, Option<ExecutionError>)> {
    let pending = PENDING_MODULE_EVALUATION.with(|cell| cell.borrow_mut().take())?;
    let tc = &mut v8::TryCatch::new(scope);
    let module = v8::Local::new(tc, &pending.module);
    let promise = v8::Local::new(tc, &pending.promise);

    tc.perform_microtask_checkpoint();

    if let Some(exception) = tc.exception() {
        let (code, err) = exception_to_result(tc, exception);
        return Some((code, None, Some(err)));
    }

    if let Some(err) = take_unhandled_promise_rejection(tc) {
        return Some((1, None, Some(err)));
    }

    match promise.state() {
        v8::PromiseState::Pending => {
            PENDING_MODULE_EVALUATION.with(|cell| {
                *cell.borrow_mut() = Some(pending);
            });
            None
        }
        v8::PromiseState::Rejected => {
            let rejection = promise.result(tc);
            let (code, err) = exception_to_result(tc, rejection);
            Some((code, None, Some(err)))
        }
        v8::PromiseState::Fulfilled => {
            if module.get_status() == v8::ModuleStatus::Errored {
                let exc = module.get_exception();
                let (code, err) = exception_to_result(tc, exc);
                return Some((code, None, Some(err)));
            }

            match serialize_module_exports(tc, module) {
                Ok(exports) => Some((0, Some(exports), None)),
                Err(err) => Some((1, None, Some(err))),
            }
        }
    }
}

/// Execute user code as an ES module (mode='run').
///
/// Runs bridge_code as CJS IIFE first (if non-empty), then compiles and runs
/// user_code as a v8::Module. The ResolveModuleCallback sends sync-blocking IPC
/// calls via BridgeCallContext to resolve import specifiers and load sources.
/// Returns (exit_code, serialized_exports, error).
/// The `bridge_cache` parameter enables code caching for repeated bridge compilations.
pub fn execute_module(
    scope: &mut v8::HandleScope,
    bridge_ctx: &BridgeCallContext,
    bridge_code: &str,
    user_code: &str,
    file_path: Option<&str>,
    bridge_cache: &mut Option<BridgeCodeCache>,
) -> (i32, Option<Vec<u8>>, Option<ExecutionError>) {
    clear_pending_module_evaluation();

    // Set up thread-local resolve state
    MODULE_RESOLVE_STATE.with(|cell| {
        *cell.borrow_mut() = Some(ModuleResolveState {
            bridge_ctx: bridge_ctx as *const BridgeCallContext,
            module_names: HashMap::new(),
            module_cache: HashMap::new(),
        });
    });

    // Run bridge code IIFE (same as CJS mode, with code caching)
    if !bridge_code.is_empty() {
        let (code, err) = run_bridge_cached(scope, bridge_code, bridge_cache);
        if code != 0 {
            clear_module_state();
            return (code, None, err);
        }
    }

    // Compile and evaluate as ES module
    {
        let tc = &mut v8::TryCatch::new(scope);
        let resource_name_str = file_path.unwrap_or("<user_module>");
        let resource = v8::String::new(tc, resource_name_str).unwrap();
        let origin = v8::ScriptOrigin::new(
            tc,
            resource.into(),
            0,
            0,
            false,
            -1,
            None,
            false,
            false,
            true, // is_module
            None,
        );

        let v8_source = match v8::String::new(tc, user_code) {
            Some(s) => s,
            None => {
                clear_module_state();
                return (
                    1,
                    None,
                    Some(ExecutionError {
                        error_type: "Error".into(),
                        message: "user code string too large for V8".into(),
                        stack: String::new(),
                        code: None,
                    }),
                );
            }
        };

        let mut source = v8::script_compiler::Source::new(v8_source, Some(&origin));
        let module = match v8::script_compiler::compile_module(tc, &mut source) {
            Some(m) => m,
            None => {
                clear_module_state();
                return match tc.exception() {
                    Some(e) => {
                        let (c, err) = exception_to_result(tc, e);
                        (c, None, Some(err))
                    }
                    None => (1, None, None),
                };
            }
        };

        // Store root module name for referrer lookup in resolve callback
        MODULE_RESOLVE_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state
                    .module_names
                    .insert(module.get_identity_hash(), resource_name_str.to_string());
            }
        });

        // Batch-prefetch static imports (BFS) to reduce IPC round-trips.
        // Each level collects uncached specifiers and resolves+loads them in one batch call.
        // The resolve callback then finds everything pre-cached during instantiation.
        prefetch_module_imports(tc, bridge_ctx, module, resource_name_str);

        // Instantiate (calls resolve callback for each import — mostly cache hits now)
        let inst_result = module.instantiate_module(tc, module_resolve_callback);
        if inst_result.is_none()
        {
            clear_module_state();
            return match tc.exception() {
                Some(e) => {
                    let (c, err) = exception_to_result(tc, e);
                    (c, None, Some(err))
                }
                None => (1, None, None),
            };
        }

        // Evaluate
        let eval_result = module.evaluate(tc);
        if eval_result.is_none() {
            clear_module_state();
            return match tc.exception() {
                Some(e) => {
                    let (c, err) = exception_to_result(tc, e);
                    (c, None, Some(err))
                }
                None => (1, None, None),
            };
        }

        // Always flush microtasks after module evaluation so that async
        // operations started during evaluation (e.g. process.stdin listeners,
        // timers) can create their pending bridge promises.  Without this,
        // modules without top-level await exit immediately because the session
        // event loop sees no pending work.
        if eval_result.unwrap().is_promise() {
            let promise = v8::Local::<v8::Promise>::try_from(eval_result.unwrap()).unwrap();
            tc.perform_microtask_checkpoint();

            if let Some(exception) = tc.exception() {
                clear_module_state();
                let (c, err) = exception_to_result(tc, exception);
                return (c, None, Some(err));
            }

            if let Some(err) = take_unhandled_promise_rejection(tc) {
                clear_module_state();
                return (1, None, Some(err));
            }

            match promise.state() {
                v8::PromiseState::Pending => {
                    set_pending_module_evaluation(tc, module, promise);
                    return (0, None, None);
                }
                v8::PromiseState::Rejected => {
                    let rejection = promise.result(tc);
                    clear_module_state();
                    let (exit_code, err) = exception_to_result(tc, rejection);
                    return (exit_code, None, Some(err));
                }
                v8::PromiseState::Fulfilled => {}
            }
        } else {
            // Non-TLA module: still flush microtasks so bridge-initiated
            // async work (stdin reads, handle registration) becomes visible
            // to the session event loop.
            tc.perform_microtask_checkpoint();

            if let Some(exception) = tc.exception() {
                clear_module_state();
                let (c, err) = exception_to_result(tc, exception);
                return (c, None, Some(err));
            }

            if let Some(err) = take_unhandled_promise_rejection(tc) {
                clear_module_state();
                return (1, None, Some(err));
            }
        }

        // Check module status for errors (handles TLA rejection case)
        if module.get_status() == v8::ModuleStatus::Errored {
            let exc = module.get_exception();
            clear_module_state();
            let (exit_code, err) = exception_to_result(tc, exc);
            return (exit_code, None, Some(err));
        }

        let exports_bytes = match serialize_module_exports(tc, module) {
            Ok(bytes) => bytes,
            Err(err) => {
                clear_module_state();
                return (1, None, Some(err));
            }
        };

        // Keep module resolve state available after the initial module finishes.
        // Dynamic imports can still fire later on the same session event loop.
        (0, Some(exports_bytes), None)
    }
}

/// Extract static import specifiers from a compiled module.
///
/// Returns a list of (specifier, referrer_name) pairs for all imports
/// that are not already in the module cache.
fn extract_uncached_imports(
    scope: &mut v8::HandleScope,
    module: v8::Local<v8::Module>,
    referrer_name: &str,
) -> Vec<(String, String)> {
    let requests = module.get_module_requests();
    let mut uncached = Vec::new();
    for i in 0..requests.length() {
        let data = requests.get(scope, i).unwrap();
        let request: v8::Local<v8::ModuleRequest> = data.cast();
        let specifier = request.get_specifier().to_rust_string_lossy(scope);
        let cache_key = module_request_cache_key(&specifier, referrer_name);

        // Skip if already cached for this referrer-qualified request.
        let already_cached = MODULE_RESOLVE_STATE.with(|cell| {
            let borrow = cell.borrow();
            let state = borrow.as_ref().unwrap();
            state.module_cache.contains_key(&cache_key)
        });
        if !already_cached {
            uncached.push((specifier, referrer_name.to_string()));
        }
    }
    uncached
}

/// Batch-prefetch module imports via a single IPC round-trip.
///
/// Sends _batchResolveModules with all uncached specifiers, receives resolved
/// paths + source code, compiles and caches each module, then recurses (BFS)
/// for any newly discovered imports. Falls back silently if the host doesn't
/// support batch resolution (the resolve callback handles individual resolution).
fn prefetch_module_imports(
    scope: &mut v8::HandleScope,
    bridge_ctx: &BridgeCallContext,
    root_module: v8::Local<v8::Module>,
    root_name: &str,
) {
    // BFS queue: modules whose imports we need to prefetch
    let mut pending: Vec<(v8::Global<v8::Module>, String)> =
        vec![(v8::Global::new(scope, root_module), root_name.to_string())];

    while !pending.is_empty() {
        // Collect all uncached imports from pending modules
        let mut batch: Vec<(String, String)> = Vec::new();
        for (global_mod, referrer) in &pending {
            let local_mod = v8::Local::new(scope, global_mod);
            let imports = extract_uncached_imports(scope, local_mod, referrer);
            for (spec, ref_name) in imports {
                // Deduplicate within this batch by the full request identity.
                if !batch.iter().any(|(s, r)| s == &spec && r == &ref_name) {
                    batch.push((spec, ref_name));
                }
            }
        }

        if batch.is_empty() {
            break;
        }

        // Send batch resolve+load via IPC
        let results = match batch_resolve_via_ipc(scope, bridge_ctx, &batch) {
            Some(r) => r,
            None => break, // Host doesn't support batch or IPC error — fall back to individual
        };

        // Compile and cache each result, collect newly compiled modules for next BFS level
        let mut next_pending: Vec<(v8::Global<v8::Module>, String)> = Vec::new();
        for (i, result) in results.iter().enumerate() {
            if i >= batch.len() {
                break;
            }
            if let Some((resolved_path, source_code)) = result {
                // Check cache again (another entry in this batch may have resolved the same path)
                let already_cached = MODULE_RESOLVE_STATE.with(|cell| {
                    let borrow = cell.borrow();
                    let state = borrow.as_ref().unwrap();
                    state.module_cache.contains_key(resolved_path)
                });
                if already_cached {
                    continue;
                }

                // Detect CJS and wrap in ESM shim if needed
                let is_cjs = is_likely_cjs(source_code);
                let effective_source = if is_cjs {
                    let exports = extract_cjs_export_names(source_code);
                    let mut shim = format!(
                        "const _cjsModule = globalThis._requireFrom(\"{}\", \"/\");\nexport default _cjsModule;\n",
                        resolved_path.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    for name in &exports {
                        shim.push_str(&format!(
                            "export const {} = _cjsModule[\"{}\"];\n",
                            name, name
                        ));
                    }
                    shim
                } else {
                    source_code.clone()
                };

                // Compile the module
                let resource = match v8::String::new(scope, resolved_path) {
                    Some(s) => s,
                    None => continue,
                };
                let origin = v8::ScriptOrigin::new(
                    scope,
                    resource.into(),
                    0,
                    0,
                    false,
                    -1,
                    None,
                    false,
                    false,
                    true, // is_module
                    None,
                );
                let v8_source = match v8::String::new(scope, &effective_source) {
                    Some(s) => s,
                    None => continue,
                };
                let mut compiled = v8::script_compiler::Source::new(v8_source, Some(&origin));
                let module = match v8::script_compiler::compile_module(scope, &mut compiled) {
                    Some(m) => m,
                    None => continue,
                };

                // Cache the module
                let global = v8::Global::new(scope, module);
                MODULE_RESOLVE_STATE.with(|cell| {
                    if let Some(state) = cell.borrow_mut().as_mut() {
                        state
                            .module_names
                            .insert(module.get_identity_hash(), resolved_path.clone());
                        // Cache by both specifier and resolved path
                        state
                            .module_cache
                            .insert(resolved_path.clone(), global.clone());
                        state
                            .module_cache
                            .insert(
                                module_request_cache_key(&batch[i].0, &batch[i].1),
                                global.clone(),
                            );
                    }
                });

                next_pending.push((v8::Global::new(scope, module), resolved_path.clone()));
            }
        }

        pending = next_pending;
    }
}

fn resolve_or_compile_module<'s>(
    scope: &mut v8::HandleScope<'s>,
    specifier_str: &str,
    referrer_name: &str,
) -> Option<v8::Local<'s, v8::Module>> {
    let request_cache_key = module_request_cache_key(specifier_str, referrer_name);

    // Phase 1: Check cache by referrer-qualified request.
    let cached_global = MODULE_RESOLVE_STATE.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow.as_ref()?;
        state.module_cache.get(&request_cache_key).cloned()
    });
    if let Some(cached) = cached_global {
        return Some(v8::Local::new(scope, &cached));
    }

    // Phase 2: Get bridge context.
    let bridge_ctx_ptr = MODULE_RESOLVE_STATE.with(|cell| {
        let borrow = cell.borrow();
        borrow.as_ref().map(|state| state.bridge_ctx)
    });
    let bridge_ctx_ptr = bridge_ctx_ptr?;
    let ctx = unsafe { &*bridge_ctx_ptr };

    // Phase 3: Resolve module path.
    let resolved_path = resolve_module_via_ipc(scope, ctx, specifier_str, referrer_name)?;

    // Phase 4: Check cache by resolved path.
    let cached_global = MODULE_RESOLVE_STATE.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow.as_ref()?;
        state.module_cache.get(&resolved_path).cloned()
    });
    if let Some(cached) = cached_global {
        return Some(v8::Local::new(scope, &cached));
    }

    // Phase 5: Load and compile the module source.
    let raw_source = load_module_via_ipc(scope, ctx, &resolved_path)?;

    // Phase 5b: Detect CJS modules and wrap in ESM shim.
    // CJS modules use module.exports/exports which isn't valid ESM syntax.
    // Real Node.js wraps CJS in a default+named export shim. We do the same
    // by generating: `const _m = globalThis._requireFrom(path, "/"); export default _m; export const {named1, named2, ...} = _m;`
    let source_code = if is_likely_cjs(&raw_source) {
        // Extract potential named exports by scanning for common patterns
        let exports = extract_cjs_export_names(&raw_source);
        let mut shim = format!(
            "const _cjsModule = globalThis._requireFrom({}, \"/\");\nexport default _cjsModule;\n",
            format!("\"{}\"", resolved_path.replace('\\', "\\\\").replace('"', "\\\""))
        );
        if !exports.is_empty() {
            // Re-export each named export from the CJS module
            for name in &exports {
                shim.push_str(&format!(
                    "export const {} = _cjsModule[\"{}\"];\n",
                    name, name
                ));
            }
        }
        shim
    } else {
        raw_source
    };

    let resource = v8::String::new(scope, &resolved_path)?;
    let origin = v8::ScriptOrigin::new(
        scope,
        resource.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        true,
        None,
    );
    let v8_source = match v8::String::new(scope, &source_code) {
        Some(s) => s,
        None => {
            throw_module_error(scope, "module source too large for V8");
            return None;
        }
    };
    let mut compiled = v8::script_compiler::Source::new(v8_source, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut compiled)?;
    MODULE_RESOLVE_STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state
                .module_names
                .insert(module.get_identity_hash(), resolved_path.clone());
            let global = v8::Global::new(scope, module);
            state
                .module_cache
                .insert(request_cache_key.clone(), global.clone());
            state.module_cache.insert(resolved_path, global);
        }
    });

    Some(module)
}

/// Callback invoked by V8 when `import.meta` is accessed in an ES module.
/// Sets `import.meta.url` to a `file://` URL derived from the module's resource name.
#[cfg_attr(test, allow(dead_code))]
pub extern "C" fn import_meta_object_callback(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    let scope = &mut unsafe { v8::CallbackScope::new(context) };

    // Look up the module's resource name from MODULE_RESOLVE_STATE.module_names
    // which maps identity_hash → resource_name.
    let identity_hash = module.get_identity_hash();
    let url_str = MODULE_RESOLVE_STATE.with(|cell| {
        let state_opt = cell.borrow();
        if let Some(ref state) = *state_opt {
            if let Some(name) = state.module_names.get(&identity_hash) {
                let n = name.clone();
                if n.starts_with("file://") {
                    return Some(n);
                } else if n.starts_with("/") {
                    return Some(format!("file://{}", n));
                } else {
                    return Some(n);
                }
            }
        }
        None
    });

    if let Some(url) = url_str {
        let key = v8::String::new(scope, "url").unwrap();
        let value = v8::String::new(scope, &url).unwrap();
        meta.set(scope, key.into(), value.into());
    }
}

#[cfg_attr(test, allow(dead_code))]
fn dynamic_import_namespace_callback(
    _scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    rv.set(args.data());
}

#[cfg_attr(test, allow(dead_code))]
fn dynamic_import_reject_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let reason = args.get(0);
    scope.throw_exception(reason);
    rv.set(reason);
}

#[cfg_attr(test, allow(dead_code))]
pub fn dynamic_import_callback<'a>(
    scope: &mut v8::HandleScope<'a>,
    _host_defined_options: v8::Local<'a, v8::Data>,
    resource_name: v8::Local<'a, v8::Value>,
    specifier: v8::Local<'a, v8::String>,
    _import_attributes: v8::Local<'a, v8::FixedArray>,
) -> Option<v8::Local<'a, v8::Promise>> {
    let tc = &mut v8::TryCatch::new(scope);

    let specifier_str = specifier.to_rust_string_lossy(tc);
    let referrer_name = resource_name.to_rust_string_lossy(tc);
    let module = match resolve_or_compile_module(tc, &specifier_str, &referrer_name) {
        Some(module) => module,
        None => {
            let reason = if let Some(exception) = tc.exception() {
                exception
            } else {
                let msg = v8::String::new(tc, "Cannot dynamically import module").unwrap();
                v8::Exception::error(tc, msg).into()
            };
            return rejected_promise(tc, reason);
        }
    };

    if module.get_status() == v8::ModuleStatus::Uninstantiated
        && module
            .instantiate_module(tc, module_resolve_callback)
            .is_none()
    {
        let reason = if let Some(exception) = tc.exception() {
            exception
        } else {
            let msg =
                v8::String::new(tc, "Cannot instantiate dynamically imported module").unwrap();
            v8::Exception::error(tc, msg).into()
        };
        return rejected_promise(tc, reason);
    }

    if module.get_status() == v8::ModuleStatus::Errored {
        let exception = v8::Global::new(tc, module.get_exception());
        let exception = v8::Local::new(tc, &exception);
        return rejected_promise(tc, exception);
    }

    if module.get_status() == v8::ModuleStatus::Evaluated {
        let namespace = v8::Global::new(tc, module.get_module_namespace());
        let namespace = v8::Local::new(tc, &namespace);
        return resolved_promise(tc, namespace.into());
    }

    let eval_result = match module.evaluate(tc) {
        Some(result) => result,
        None => {
            let reason = if let Some(exception) = tc.exception() {
                exception
            } else {
                let msg =
                    v8::String::new(tc, "Cannot evaluate dynamically imported module").unwrap();
                v8::Exception::error(tc, msg).into()
            };
            return rejected_promise(tc, reason);
        }
    };

    let namespace = v8::Global::new(tc, module.get_module_namespace());
    let namespace = v8::Local::new(tc, &namespace);
    if eval_result.is_promise() {
        let eval_promise = v8::Local::<v8::Promise>::try_from(eval_result).ok()?;
        let on_fulfilled = v8::FunctionTemplate::builder(dynamic_import_namespace_callback)
            .data(namespace.into())
            .build(tc)
            .get_function(tc)?;
        let on_rejected = v8::FunctionTemplate::builder(dynamic_import_reject_callback)
            .build(tc)
            .get_function(tc)?;
        return eval_promise.then2(tc, on_fulfilled, on_rejected);
    }

    resolved_promise(tc, namespace.into())
}

#[cfg_attr(test, allow(dead_code))]
fn resolved_promise<'s>(
    scope: &mut v8::HandleScope<'s>,
    value: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    resolver.resolve(scope, value);
    Some(resolver.get_promise(scope))
}

#[cfg_attr(test, allow(dead_code))]
fn rejected_promise<'s>(
    scope: &mut v8::HandleScope<'s>,
    reason: v8::Local<'s, v8::Value>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    resolver.reject(scope, reason);
    Some(resolver.get_promise(scope))
}

/// Send _batchResolveModules via sync-blocking IPC.
///
/// Sends an array of {specifier, referrer} pairs, receives an array of
/// {resolved, source} results (null entries for unresolvable modules).
/// Returns None if the host doesn't support batch resolution or on IPC error.
fn batch_resolve_via_ipc(
    scope: &mut v8::HandleScope,
    ctx: &BridgeCallContext,
    batch: &[(String, String)],
) -> Option<Vec<Option<(String, String)>>> {
    // Build V8 array of [specifier, referrer] pairs, wrapped in an outer array
    // so the host handler receives the batch as a single argument (args are spread).
    let inner = v8::Array::new(scope, batch.len() as i32);
    for (i, (specifier, referrer)) in batch.iter().enumerate() {
        let pair = v8::Array::new(scope, 2);
        let spec_v8 = v8::String::new(scope, specifier)?;
        let ref_v8 = v8::String::new(scope, referrer)?;
        pair.set_index(scope, 0, spec_v8.into());
        pair.set_index(scope, 1, ref_v8.into());
        inner.set_index(scope, i as u32, pair.into());
    }
    let outer = v8::Array::new(scope, 1);
    outer.set_index(scope, 0, inner.into());
    let args = serialize_v8_value(scope, outer.into()).ok()?;

    let response = ctx.sync_call("_batchResolveModules", args).ok()??;
    let val = deserialize_v8_value(scope, &response).ok()?;

    // Parse response: array of {resolved, source} or null
    let result_arr = v8::Local::<v8::Array>::try_from(val).ok()?;
    let mut results = Vec::with_capacity(batch.len());
    for i in 0..result_arr.length() {
        let entry = result_arr.get_index(scope, i);
        match entry {
            Some(v) if !v.is_null() && !v.is_undefined() => {
                let obj = v8::Local::<v8::Object>::try_from(v).ok();
                if let Some(obj) = obj {
                    let r_key = v8::String::new(scope, "resolved").unwrap();
                    let s_key = v8::String::new(scope, "source").unwrap();
                    let resolved = obj
                        .get(scope, r_key.into())
                        .filter(|v| v.is_string())
                        .map(|v| v.to_rust_string_lossy(scope));
                    let source = obj
                        .get(scope, s_key.into())
                        .filter(|v| v.is_string())
                        .map(|v| v.to_rust_string_lossy(scope));
                    match (resolved, source) {
                        (Some(r), Some(s)) => results.push(Some((r, s))),
                        _ => results.push(None),
                    }
                } else {
                    results.push(None);
                }
            }
            _ => results.push(None),
        }
    }
    Some(results)
}

/// V8 ResolveModuleCallback — called during instantiate_module for each import.
///
/// Sends sync-blocking IPC calls to resolve specifiers and load source code,
/// compiles resolved modules, and caches them.
fn module_resolve_callback<'a>(
    context: v8::Local<'a, v8::Context>,
    specifier: v8::Local<'a, v8::String>,
    _import_attributes: v8::Local<'a, v8::FixedArray>,
    referrer: v8::Local<'a, v8::Module>,
) -> Option<v8::Local<'a, v8::Module>> {
    // SAFETY: CallbackScope can be constructed from Local<Context> within a V8 callback
    let scope = &mut unsafe { v8::CallbackScope::new(context) };

    let specifier_str = specifier.to_rust_string_lossy(scope);
    let referrer_hash = referrer.get_identity_hash();

    let referrer_name = MODULE_RESOLVE_STATE.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow.as_ref()?;
        state.module_names.get(&referrer_hash).cloned()
    });
    let referrer_name = referrer_name?;
    resolve_or_compile_module(scope, &specifier_str, &referrer_name)
}

/// Send _resolveModule(specifier, referrer_path) via sync-blocking IPC.
fn resolve_module_via_ipc(
    scope: &mut v8::HandleScope,
    ctx: &BridgeCallContext,
    specifier: &str,
    referrer: &str,
) -> Option<String> {
    // Serialize [specifier, referrer] as V8 Array
    let spec_v8 = v8::String::new(scope, specifier).unwrap();
    let ref_v8 = v8::String::new(scope, referrer).unwrap();
    let arr = v8::Array::new(scope, 2);
    arr.set_index(scope, 0, spec_v8.into());
    arr.set_index(scope, 1, ref_v8.into());
    let args = match serialize_v8_value(scope, arr.into()) {
        Ok(bytes) => bytes,
        Err(e) => {
            throw_module_error(scope, &format!("_resolveModule serialize error: {}", e));
            return None;
        }
    };

    match ctx.sync_call("_resolveModule", args) {
        Ok(Some(bytes)) => match deserialize_v8_value(scope, &bytes) {
            Ok(val) => {
                if val.is_string() {
                    Some(val.to_rust_string_lossy(scope))
                } else {
                    throw_module_error(
                        scope,
                        &format!("_resolveModule returned non-string for '{}'", specifier),
                    );
                    None
                }
            }
            Err(e) => {
                throw_module_error(scope, &format!("_resolveModule decode error: {}", e));
                None
            }
        },
        Ok(None) => {
            throw_module_error(scope, &format!("Cannot resolve module '{}'", specifier));
            None
        }
        Err(e) => {
            throw_module_error(scope, &e);
            None
        }
    }
}

/// Send _loadFile(resolved_path) via sync-blocking IPC.
fn load_module_via_ipc(
    scope: &mut v8::HandleScope,
    ctx: &BridgeCallContext,
    resolved_path: &str,
) -> Option<String> {
    // Serialize [resolved_path] as V8 Array
    let path_v8 = v8::String::new(scope, resolved_path).unwrap();
    let arr = v8::Array::new(scope, 1);
    arr.set_index(scope, 0, path_v8.into());
    let args = match serialize_v8_value(scope, arr.into()) {
        Ok(bytes) => bytes,
        Err(e) => {
            throw_module_error(scope, &format!("_loadFile serialize error: {}", e));
            return None;
        }
    };

    let ipc_result = ctx.sync_call("_loadFile", args);
    match ipc_result {
        Ok(Some(bytes)) => match deserialize_v8_value(scope, &bytes) {
            Ok(val) => {
                if val.is_string() {
                    Some(val.to_rust_string_lossy(scope))
                } else {
                    throw_module_error(
                        scope,
                        &format!("_loadFile returned non-string for '{}'", resolved_path),
                    );
                    None
                }
            }
            Err(e) => {
                throw_module_error(scope, &format!("_loadFile decode error: {}", e));
                None
            }
        },
        Ok(None) => {
            throw_module_error(scope, &format!("Cannot load module '{}'", resolved_path));
            None
        }
        Err(e) => {
            throw_module_error(scope, &e);
            None
        }
    }
}

/// Throw a V8 exception for module resolution errors.
fn throw_module_error(scope: &mut v8::HandleScope, message: &str) {
    let msg = v8::String::new(scope, message).unwrap();
    let exc = v8::Exception::error(scope, msg);
    scope.throw_exception(exc);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge;
    use crate::host_call::BridgeCallContext;
    use crate::isolate;
    use std::collections::HashMap;
    use std::io::{Cursor, Write};
    use std::sync::{Arc, Mutex};

    /// Shared writer that captures output for test inspection
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    /// Helper: serialize a V8 string value for test BridgeResponse payloads
    fn v8_serialize_str(
        iso: &mut v8::OwnedIsolate,
        ctx: &v8::Global<v8::Context>,
        s: &str,
    ) -> Vec<u8> {
        let scope = &mut v8::HandleScope::new(iso);
        let local = v8::Local::new(scope, ctx);
        let scope = &mut v8::ContextScope::new(scope, local);
        let val = v8::String::new(scope, s).unwrap();
        crate::bridge::serialize_v8_value(scope, val.into()).unwrap()
    }

    /// Helper: serialize a V8 integer value for test BridgeResponse payloads
    fn v8_serialize_int(
        iso: &mut v8::OwnedIsolate,
        ctx: &v8::Global<v8::Context>,
        n: i64,
    ) -> Vec<u8> {
        let scope = &mut v8::HandleScope::new(iso);
        let local = v8::Local::new(scope, ctx);
        let scope = &mut v8::ContextScope::new(scope, local);
        let val = v8::Number::new(scope, n as f64);
        crate::bridge::serialize_v8_value(scope, val.into()).unwrap()
    }

    /// Helper: serialize a V8 null value for test BridgeResponse payloads
    fn v8_serialize_null(iso: &mut v8::OwnedIsolate, ctx: &v8::Global<v8::Context>) -> Vec<u8> {
        let scope = &mut v8::HandleScope::new(iso);
        let local = v8::Local::new(scope, ctx);
        let scope = &mut v8::ContextScope::new(scope, local);
        let val = v8::null(scope);
        crate::bridge::serialize_v8_value(scope, val.into()).unwrap()
    }

    /// Helper: serialize a V8 object (from JS expression) for test BridgeResponse payloads
    fn v8_serialize_eval(
        iso: &mut v8::OwnedIsolate,
        ctx: &v8::Global<v8::Context>,
        expr: &str,
    ) -> Vec<u8> {
        let scope = &mut v8::HandleScope::new(iso);
        let local = v8::Local::new(scope, ctx);
        let scope = &mut v8::ContextScope::new(scope, local);
        let source = v8::String::new(scope, expr).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let val = script.run(scope).unwrap();
        crate::bridge::serialize_v8_value(scope, val).unwrap()
    }

    /// Enter a context, run JS, return the string result.
    fn eval(
        isolate: &mut v8::OwnedIsolate,
        context: &v8::Global<v8::Context>,
        code: &str,
    ) -> String {
        let scope = &mut v8::HandleScope::new(isolate);
        let local = v8::Local::new(scope, context);
        let scope = &mut v8::ContextScope::new(scope, local);
        let source = v8::String::new(scope, code).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap();
        result.to_rust_string_lossy(scope)
    }

    /// Enter a context, run JS, return true if the result is truthy.
    fn eval_bool(
        isolate: &mut v8::OwnedIsolate,
        context: &v8::Global<v8::Context>,
        code: &str,
    ) -> bool {
        let scope = &mut v8::HandleScope::new(isolate);
        let local = v8::Local::new(scope, context);
        let scope = &mut v8::ContextScope::new(scope, local);
        let source = v8::String::new(scope, code).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap();
        result.boolean_value(scope)
    }

    /// Enter a context, run JS, return true if an exception was thrown.
    fn eval_throws(
        isolate: &mut v8::OwnedIsolate,
        context: &v8::Global<v8::Context>,
        code: &str,
    ) -> bool {
        let scope = &mut v8::HandleScope::new(isolate);
        let local = v8::Local::new(scope, context);
        let scope = &mut v8::ContextScope::new(scope, local);
        let tc = &mut v8::TryCatch::new(scope);
        let source = v8::String::new(tc, code).unwrap();
        if let Some(script) = v8::Script::compile(tc, source, None) {
            script.run(tc);
        }
        tc.has_caught()
    }

    #[test]
    fn v8_consolidated_tests() {
        isolate::init_v8_platform();

        // --- Isolate lifecycle (moved from isolate::tests to consolidate V8 tests) ---
        // Create and destroy 3 isolates sequentially without crash
        for i in 0..3 {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let result = eval(&mut isolate, &context, &format!("{} + 1", i));
            assert_eq!(result, format!("{}", i + 1));
        }
        // Isolate with heap limit
        {
            let mut isolate = isolate::create_isolate(Some(16));
            let context = isolate::create_context(&mut isolate);
            assert_eq!(eval(&mut isolate, &context, "1 + 2"), "3");
        }
        // Isolate without heap limit
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            assert_eq!(
                eval(&mut isolate, &context, "'hello' + ' world'"),
                "hello world"
            );
        }
        // Global context handle persists state
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            eval(&mut isolate, &context, "var x = 42;");
            assert_eq!(eval(&mut isolate, &context, "x"), "42");
        }

        // --- Part 1: InjectGlobals sets _processConfig and _osConfig ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let mut env = HashMap::new();
            env.insert("HOME".into(), "/home/user".into());
            env.insert("PATH".into(), "/usr/bin".into());

            let process_config = ProcessConfig {
                cwd: "/app".into(),
                env,
                timing_mitigation: "none".into(),
                frozen_time_ms: Some(1700000000000.0),
            };
            let os_config = OsConfig {
                homedir: "/home/user".into(),
                tmpdir: "/tmp".into(),
                platform: "linux".into(),
                arch: "x64".into(),
            };

            // Inject globals
            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals(scope, &process_config, &os_config);
            }

            // Verify _processConfig values
            assert_eq!(eval(&mut isolate, &context, "_processConfig.cwd"), "/app");
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.timing_mitigation"),
                "none"
            );
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.frozen_time_ms"),
                "1700000000000"
            );
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.env.HOME"),
                "/home/user"
            );
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.env.PATH"),
                "/usr/bin"
            );

            // Verify _osConfig values
            assert_eq!(
                eval(&mut isolate, &context, "_osConfig.homedir"),
                "/home/user"
            );
            assert_eq!(eval(&mut isolate, &context, "_osConfig.tmpdir"), "/tmp");
            assert_eq!(eval(&mut isolate, &context, "_osConfig.platform"), "linux");
            assert_eq!(eval(&mut isolate, &context, "_osConfig.arch"), "x64");
        }

        // --- Part 2: frozen_time_ms null when None ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let process_config = ProcessConfig {
                cwd: "/".into(),
                env: HashMap::new(),
                timing_mitigation: "none".into(),
                frozen_time_ms: None,
            };
            let os_config = OsConfig {
                homedir: "/root".into(),
                tmpdir: "/tmp".into(),
                platform: "linux".into(),
                arch: "x64".into(),
            };

            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals(scope, &process_config, &os_config);
            }

            assert_eq!(
                eval(
                    &mut isolate,
                    &context,
                    "_processConfig.frozen_time_ms === null"
                ),
                "true"
            );
        }

        // --- Part 3: Objects are frozen (immutable) ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let process_config = ProcessConfig {
                cwd: "/app".into(),
                env: HashMap::new(),
                timing_mitigation: "none".into(),
                frozen_time_ms: None,
            };
            let os_config = OsConfig {
                homedir: "/home".into(),
                tmpdir: "/tmp".into(),
                platform: "linux".into(),
                arch: "x64".into(),
            };

            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals(scope, &process_config, &os_config);
            }

            // Verify Object.isFrozen
            assert!(eval_bool(
                &mut isolate,
                &context,
                "Object.isFrozen(_processConfig)"
            ));
            assert!(eval_bool(
                &mut isolate,
                &context,
                "Object.isFrozen(_osConfig)"
            ));
            assert!(eval_bool(
                &mut isolate,
                &context,
                "Object.isFrozen(_processConfig.env)"
            ));

            // Verify non-writable: assignment in strict mode throws
            assert!(eval_throws(
                &mut isolate,
                &context,
                "'use strict'; _processConfig.cwd = '/hacked'"
            ));
            assert!(eval_throws(
                &mut isolate,
                &context,
                "'use strict'; _osConfig.platform = 'hacked'"
            ));

            // Verify non-configurable: cannot delete or redefine
            assert!(eval_throws(
                &mut isolate,
                &context,
                "'use strict'; delete _processConfig"
            ));
            assert!(eval_throws(
                &mut isolate,
                &context,
                "Object.defineProperty(globalThis, '_processConfig', { value: {} })"
            ));
            assert!(eval_throws(
                &mut isolate,
                &context,
                "Object.defineProperty(globalThis, '_osConfig', { value: {} })"
            ));
        }

        // --- Part 4: SharedArrayBuffer NOT removed by inject_globals ---
        // SharedArrayBuffer removal is handled by JS bridge code (applyTimingMitigationFreeze),
        // not by inject_globals. The bridge bundle depends on SharedArrayBuffer being available
        // during initialization. inject_globals stores timing_mitigation in _processConfig
        // for the bridge to read.
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let process_config = ProcessConfig {
                cwd: "/".into(),
                env: HashMap::new(),
                timing_mitigation: "freeze".into(),
                frozen_time_ms: None,
            };
            let os_config = OsConfig {
                homedir: "/root".into(),
                tmpdir: "/tmp".into(),
                platform: "linux".into(),
                arch: "x64".into(),
            };

            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals(scope, &process_config, &os_config);
            }

            // SharedArrayBuffer should still exist — removal is done by JS bridge
            assert!(eval_bool(
                &mut isolate,
                &context,
                "typeof SharedArrayBuffer !== 'undefined'"
            ));
            // timing_mitigation is stored for the bridge to act on
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.timing_mitigation"),
                "freeze"
            );
        }

        // --- Part 5: SharedArrayBuffer preserved when timing_mitigation is 'none' ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let process_config = ProcessConfig {
                cwd: "/".into(),
                env: HashMap::new(),
                timing_mitigation: "none".into(),
                frozen_time_ms: None,
            };
            let os_config = OsConfig {
                homedir: "/root".into(),
                tmpdir: "/tmp".into(),
                platform: "linux".into(),
                arch: "x64".into(),
            };

            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals(scope, &process_config, &os_config);
            }

            // SharedArrayBuffer should still exist
            assert!(eval_bool(
                &mut isolate,
                &context,
                "typeof SharedArrayBuffer !== 'undefined'"
            ));
        }

        // --- Part 6: WASM disabled ---
        {
            let mut isolate = isolate::create_isolate(None);
            disable_wasm(&mut isolate);
            let context = isolate::create_context(&mut isolate);

            // Attempting to compile WASM should throw
            assert!(eval_throws(
                &mut isolate,
                &context,
                "new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]))"
            ));
        }

        // --- Part 7: WASM works without disable_wasm ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            // WASM should work by default (minimal valid WASM module)
            assert!(!eval_throws(
                &mut isolate,
                &context,
                "new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]))"
            ));
        }

        // --- Part 8: Sync bridge call returns value ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            // Prepare BridgeResponse: call_id=1, result="hello world"
            let result_v8 = v8_serialize_str(&mut iso, &ctx, "hello world");

            let mut response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: result_v8,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_testBridge"],
                );
            }

            assert_eq!(eval(&mut iso, &ctx, "_testBridge('arg1')"), "hello world");
        }

        // --- Part 9: Bridge call error throws V8 exception ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 1,
                    payload: "ENOENT: file not found".as_bytes().to_vec(),
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_testBridge"],
                );
            }

            assert!(eval_throws(&mut iso, &ctx, "_testBridge('arg')"));
        }

        // --- Part 10: Multiple bridge functions with argument passing ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            // Prepare two BridgeResponses (call_id=1 for _fn1, call_id=2 for _fn2)
            let r1_bytes = v8_serialize_str(&mut iso, &ctx, "result-one");
            let r2_bytes = v8_serialize_int(&mut iso, &ctx, 42);

            let mut response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r1_bytes,
                },
            )
            .unwrap();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: r2_bytes,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_fn1", "_fn2"],
                );
            }

            assert_eq!(eval(&mut iso, &ctx, "_fn1('x')"), "result-one");
            assert_eq!(eval(&mut iso, &ctx, "_fn2(1, 2, 3)"), "42");
        }

        // --- Part 11: Bridge call with null result returns undefined ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: vec![],
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_testBridge"],
                );
            }

            assert!(eval_bool(&mut iso, &ctx, "_testBridge() === undefined"));
        }

        // --- Part 12: Async bridge call returns pending promise, resolved successfully ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let writer_buf = Arc::new(Mutex::new(Vec::new()));
            let bridge_ctx = BridgeCallContext::new(
                Box::new(SharedWriter(Arc::clone(&writer_buf))),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Call the async function
            eval(&mut iso, &ctx, "var _promise = _asyncFn('arg1')");

            // Verify a BridgeCall was sent
            {
                let written = writer_buf.lock().unwrap();
                let call = crate::ipc_binary::read_frame(&mut Cursor::new(&*written)).unwrap();
                match call {
                    crate::ipc_binary::BinaryFrame::BridgeCall {
                        call_id, method, ..
                    } => {
                        assert_eq!(call_id, 1);
                        assert_eq!(method, "_asyncFn");
                    }
                    _ => panic!("expected BridgeCall"),
                }
            }

            // Promise should be pending with 1 pending promise
            assert_eq!(pending.len(), 1);
            assert!(eval_bool(&mut iso, &ctx, "_promise instanceof Promise"));

            // Resolve the promise
            let result_v8 = v8_serialize_str(&mut iso, &ctx, "async result");

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, Some(result_v8), None).unwrap();
            }

            assert_eq!(pending.len(), 0);

            // Verify promise is fulfilled with correct value
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let source = v8::String::new(scope, "_promise").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
                assert_eq!(
                    promise.result(scope).to_rust_string_lossy(scope),
                    "async result"
                );
            }
        }

        // --- Part 13: Async bridge call promise rejected on error ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "var _promise = _asyncFn('arg')");
            assert_eq!(pending.len(), 1);

            // Reject the promise
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(
                    scope,
                    &pending,
                    1,
                    None,
                    Some("ENOENT: file not found".into()),
                )
                .unwrap();
            }

            assert_eq!(pending.len(), 0);

            // Verify promise is rejected with error
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let source = v8::String::new(scope, "_promise").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                assert_eq!(promise.state(), v8::PromiseState::Rejected);
                let rejection = promise.result(scope);
                let obj = v8::Local::<v8::Object>::try_from(rejection).unwrap();
                let msg_key = v8::String::new(scope, "message").unwrap();
                let msg_val = obj.get(scope, msg_key.into()).unwrap();
                assert_eq!(
                    msg_val.to_rust_string_lossy(scope),
                    "ENOENT: file not found"
                );
            }
        }

        // --- Part 14: Multiple async functions with out-of-order resolution ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_fetch", "_dns"],
                );
            }

            eval(
                &mut iso,
                &ctx,
                "var _p1 = _fetch('url'); var _p2 = _dns('host')",
            );
            assert_eq!(pending.len(), 2);

            // Resolve in reverse order (p2 first, then p1)
            let r2 = v8_serialize_str(&mut iso, &ctx, "dns-result");
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 2, Some(r2), None).unwrap();
            }
            assert_eq!(pending.len(), 1);

            let r1 = v8_serialize_str(&mut iso, &ctx, "fetch-result");
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, Some(r1), None).unwrap();
            }
            assert_eq!(pending.len(), 0);

            // Verify both promises fulfilled correctly
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);

                let source = v8::String::new(scope, "_p1").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
                assert_eq!(
                    promise.result(scope).to_rust_string_lossy(scope),
                    "fetch-result"
                );

                let source = v8::String::new(scope, "_p2").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
                assert_eq!(
                    promise.result(scope).to_rust_string_lossy(scope),
                    "dns-result"
                );
            }
        }

        // --- Part 15: Async bridge call with null result resolves to undefined ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "var _promise = _asyncFn()");

            // Resolve with None (null result)
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, None, None).unwrap();
            }

            // Promise should be fulfilled with undefined
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let source = v8::String::new(scope, "_promise").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
                assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
                assert!(promise.result(scope).is_undefined());
            }
        }

        // --- Part 16: Microtasks flushed after promise resolution ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Set up .then handler that sets a global variable
            eval(
                &mut iso,
                &ctx,
                "var _thenRan = false; _asyncFn().then(function() { _thenRan = true; })",
            );

            // Before resolution, _thenRan should be false
            assert!(eval_bool(&mut iso, &ctx, "_thenRan === false"));

            // Resolve the promise (microtasks flushed inside resolve_pending_promise)
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, None, None).unwrap();
            }

            // After resolution + microtask flush, _thenRan should be true
            assert!(eval_bool(&mut iso, &ctx, "_thenRan === true"));
        }

        // --- Part 17: CJS execution — successful execution returns exit code 0 ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "var x = 1 + 2;", &mut None)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            // Verify the code actually ran
            assert_eq!(eval(&mut iso, &ctx, "x"), "3");
        }

        // --- Part 18: Bridge code IIFE executed before user code ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge = "(function() { globalThis._bridgeReady = true; })()";
            let user = "var _sawBridge = _bridgeReady;";
            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, bridge, user, &mut None)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            assert!(eval_bool(&mut iso, &ctx, "_sawBridge === true"));
            assert!(eval_bool(&mut iso, &ctx, "_bridgeReady === true"));
        }

        // --- Part 18b: Rejected async script completion returns structured error ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(
                    scope,
                    "",
                    "(async function () { throw new Error('async failure'); })()",
                    &mut None,
                )
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "async failure");
        }

        // --- Part 19: SyntaxError in user code returns structured error ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "var x = {;", &mut None)
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "SyntaxError");
            assert!(!err.message.is_empty());
        }

        // --- Part 20: Runtime TypeError returns structured error ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "null.foo", &mut None)
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "TypeError");
            assert!(!err.message.is_empty());
            assert!(!err.stack.is_empty());
        }

        // --- Part 21: SyntaxError in bridge code returns error ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "function {", "var x = 1;", &mut None)
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "SyntaxError");
            // User code should NOT have run
            assert!(eval_bool(&mut iso, &ctx, "typeof x === 'undefined'"));
        }

        // --- Part 22: Empty bridge code is skipped ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "'hello'", &mut None)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
        }

        // --- Part 23: Runtime error with error code ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(
                    scope,
                    "",
                    "var e = new Error('not found'); e.code = 'ERR_MODULE_NOT_FOUND'; throw e;",
                    &mut None,
                )
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "not found");
            assert_eq!(err.code, Some("ERR_MODULE_NOT_FOUND".into()));
        }

        // --- Part 24: Thrown string (non-Error object) handled ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "throw 'raw string error';", &mut None)
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "raw string error");
            assert!(err.stack.is_empty());
            assert!(err.code.is_none());
        }

        // --- Part 25: ESM — simple module with exports ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let user_code = "export const x = 42;\nexport const msg = 'hello';";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(scope, &bridge_ctx, "", user_code, None, &mut None)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                assert!(val.is_object());
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "x").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .int32_value(scope)
                        .unwrap(),
                    42
                );
                let k = v8::String::new(scope, "msg").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .to_rust_string_lossy(scope),
                    "hello"
                );
            }
        }

        // --- Part 26: ESM — default export ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    "export default 'world';",
                    None,
                    &mut None,
                )
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                assert!(val.is_object());
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "default").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .to_rust_string_lossy(scope),
                    "world"
                );
            }
        }

        // --- Part 27: ESM — SyntaxError ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let (code, _exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    "export const x = {;",
                    None,
                    &mut None,
                )
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "SyntaxError");
        }

        // --- Part 28: ESM — runtime TypeError ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let (code, _exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    "const x = null; x.foo;",
                    None,
                    &mut None,
                )
            };

            assert_eq!(code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "TypeError");
        }

        // --- Part 29: ESM — bridge code IIFE runs before module ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let bridge = "(function() { globalThis._bridgeReady = true; })()";
            let user = "export const saw = _bridgeReady;";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(scope, &bridge_ctx, bridge, user, None, &mut None)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                assert!(val.is_object());
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "saw").unwrap();
                assert!(obj.get(scope, k.into()).unwrap().is_true());
            }
        }

        // --- Part 30: ESM — import from dependency via batch resolve ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            // Prepare BridgeResponse for _batchResolveModules (batch prefetch).
            // The batch call (call_id=1) returns an array of {resolved, source}.
            let mut response_buf = Vec::new();

            let batch_result = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/dep.mjs', source: 'export const dep_val = 99;'}]",
            );
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: batch_result,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let user_code =
                "import { dep_val } from './dep.mjs';\nexport const result = dep_val + 1;";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    user_code,
                    Some("/app/main.mjs"),
                    &mut None,
                )
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                assert!(val.is_object());
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "result").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .int32_value(scope)
                        .unwrap(),
                    100
                );
            }
        }

        // --- Part 31: Event loop — BridgeResponse resolves pending promise ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            // Register async bridge function
            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Call async function from V8 — creates pending promise
            eval(
                &mut iso,
                &ctx,
                "var _eventLoopResult = 'pending'; _asyncFn('test').then(function(v) { _eventLoopResult = v; })",
            );
            assert_eq!(pending.len(), 1);
            assert_eq!(eval(&mut iso, &ctx, "_eventLoopResult"), "pending");

            // Create channel and send BridgeResponse
            let (tx, rx) = crossbeam_channel::unbounded();
            let result_v8 = v8_serialize_str(&mut iso, &ctx, "event-loop-resolved");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: result_v8,
                },
            ))
            .unwrap();

            // Run event loop
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed, "event loop should complete normally");
            assert_eq!(pending.len(), 0);
            assert_eq!(
                eval(&mut iso, &ctx, "_eventLoopResult"),
                "event-loop-resolved"
            );
        }

        // --- Part 32: Event loop — multiple BridgeResponses resolved in sequence ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_fetch", "_dns"],
                );
            }

            // Create two pending promises
            eval(
                &mut iso,
                &ctx,
                "var _r1 = 'pending'; var _r2 = 'pending'; \
                 _fetch('url').then(function(v) { _r1 = v; }); \
                 _dns('host').then(function(v) { _r2 = v; })",
            );
            assert_eq!(pending.len(), 2);

            // Create channel and send both responses
            let (tx, rx) = crossbeam_channel::unbounded();
            // Resolve in reverse order
            let r2 = v8_serialize_str(&mut iso, &ctx, "dns-result");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: r2,
                },
            ))
            .unwrap();
            let r1 = v8_serialize_str(&mut iso, &ctx, "fetch-result");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r1,
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
            assert_eq!(pending.len(), 0);
            assert_eq!(eval(&mut iso, &ctx, "_r1"), "fetch-result");
            assert_eq!(eval(&mut iso, &ctx, "_r2"), "dns-result");
        }

        // --- Part 33: Event loop — TerminateExecution breaks loop ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "_asyncFn('test')");
            assert_eq!(pending.len(), 1);

            // Send TerminateExecution
            let (tx, rx) = crossbeam_channel::unbounded();
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::TerminateExecution {
                    session_id: "test-session".into(),
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(!completed, "event loop should return false on termination");
            // Promise is still pending (not resolved)
            assert_eq!(pending.len(), 1);

            // Cancel termination so isolate is usable again
            iso.cancel_terminate_execution();
        }

        // --- Part 34: Event loop — Shutdown breaks loop ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "_asyncFn('test')");
            assert_eq!(pending.len(), 1);

            // Send Shutdown
            let (tx, rx) = crossbeam_channel::unbounded();
            tx.send(crate::session::SessionCommand::Shutdown).unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(!completed, "event loop should return false on shutdown");
        }

        // --- Part 35: Event loop — exits immediately when no pending promises ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let pending = bridge::PendingPromises::new();

            let (_tx, rx) = crossbeam_channel::unbounded::<crate::session::SessionCommand>();

            // No pending promises — event loop should exit immediately
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
        }

        // --- Part 36: Event loop — StreamEvent dispatches to V8 callback ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Register dispatch callback and create pending promise
            eval(
                &mut iso,
                &ctx,
                "var _streamEvents = []; \
                 globalThis._childProcessDispatch = function(eventType, payload) { \
                     _streamEvents.push({ type: eventType, data: payload }); \
                 }; \
                 _asyncFn('keep-alive')",
            );
            assert_eq!(pending.len(), 1);

            // Send StreamEvent followed by BridgeResponse
            let (tx, rx) = crossbeam_channel::unbounded();

            // Encode payload as V8-serialized string
            let payload_bytes = v8_serialize_str(&mut iso, &ctx, "hello from child");

            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "child_stdout".into(),
                    payload: payload_bytes,
                },
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
            assert_eq!(pending.len(), 0);

            // Verify stream event was dispatched
            assert_eq!(eval(&mut iso, &ctx, "_streamEvents.length"), "1");
            assert_eq!(
                eval(&mut iso, &ctx, "_streamEvents[0].type"),
                "child_stdout"
            );
            assert_eq!(
                eval(&mut iso, &ctx, "_streamEvents[0].data"),
                "hello from child"
            );
        }

        // --- Part 37: Event loop — microtasks flushed after BridgeResponse ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Set up .then handler that mutates global state
            eval(
                &mut iso,
                &ctx,
                "var _microtaskRan = false; \
                 _asyncFn('test').then(function() { _microtaskRan = true; })",
            );
            assert!(eval_bool(&mut iso, &ctx, "_microtaskRan === false"));

            let (tx, rx) = crossbeam_channel::unbounded();
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None);
            }

            // .then handler should have run (microtasks flushed)
            assert!(eval_bool(&mut iso, &ctx, "_microtaskRan === true"));
        }

        // --- Part 38: StreamEvent dispatches child_stderr and child_exit ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Register child process dispatch and create pending promise
            eval(
                &mut iso,
                &ctx,
                "var _childEvents = []; \
                 globalThis._childProcessDispatch = function(eventType, payload) { \
                     _childEvents.push({ type: eventType, data: payload }); \
                 }; \
                 _asyncFn('keep-alive')",
            );
            assert_eq!(pending.len(), 1);

            let (tx, rx) = crossbeam_channel::unbounded();

            // Send child_stderr event
            let stderr_payload = v8_serialize_str(&mut iso, &ctx, "error output");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "child_stderr".into(),
                    payload: stderr_payload,
                },
            ))
            .unwrap();

            // Send child_exit event with exit code
            let exit_payload = v8_serialize_int(&mut iso, &ctx, 1);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "child_exit".into(),
                    payload: exit_payload,
                },
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
            assert_eq!(eval(&mut iso, &ctx, "_childEvents.length"), "2");
            assert_eq!(eval(&mut iso, &ctx, "_childEvents[0].type"), "child_stderr");
            assert_eq!(eval(&mut iso, &ctx, "_childEvents[0].data"), "error output");
            assert_eq!(eval(&mut iso, &ctx, "_childEvents[1].type"), "child_exit");
            assert_eq!(eval(&mut iso, &ctx, "_childEvents[1].data"), "1");
        }

        // --- Part 39: StreamEvent dispatches http_request to _httpServerDispatch ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Register HTTP dispatch and create pending promise
            eval(
                &mut iso,
                &ctx,
                "var _httpEvents = []; \
                 globalThis._httpServerDispatch = function(eventType, payload) { \
                     _httpEvents.push({ type: eventType, data: payload }); \
                 }; \
                 _asyncFn('keep-alive')",
            );
            assert_eq!(pending.len(), 1);

            let (tx, rx) = crossbeam_channel::unbounded();

            // Send http_request event with request data
            let http_payload =
                v8_serialize_eval(&mut iso, &ctx, "({method: 'GET', url: '/api/test'})");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "http_request".into(),
                    payload: http_payload,
                },
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
            assert_eq!(eval(&mut iso, &ctx, "_httpEvents.length"), "1");
            assert_eq!(eval(&mut iso, &ctx, "_httpEvents[0].type"), "http_request");
            assert_eq!(eval(&mut iso, &ctx, "_httpEvents[0].data.method"), "GET");
            assert_eq!(eval(&mut iso, &ctx, "_httpEvents[0].data.url"), "/api/test");
        }

        // --- Part 40: StreamEvent with unknown event_type is ignored ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            eval(
                &mut iso,
                &ctx,
                "var _anyDispatched = false; \
                 globalThis._childProcessDispatch = function() { _anyDispatched = true; }; \
                 globalThis._httpServerDispatch = function() { _anyDispatched = true; }; \
                 _asyncFn('keep-alive')",
            );
            assert_eq!(pending.len(), 1);

            let (tx, rx) = crossbeam_channel::unbounded();

            // Send unknown event type
            let payload = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "unknown_event".into(),
                    payload,
                },
            ))
            .unwrap();

            // Resolve pending promise to exit loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
            // Unknown event should NOT have dispatched to any handler
            assert!(eval_bool(&mut iso, &ctx, "_anyDispatched === false"));
        }

        // --- Part 41: StreamEvent dispatch with missing callback is safe (no crash) ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // No dispatch functions registered, just create a pending promise
            eval(&mut iso, &ctx, "_asyncFn('keep-alive')");
            assert_eq!(pending.len(), 1);

            let (tx, rx) = crossbeam_channel::unbounded();

            // Send child_stdout without _childProcessDispatch registered
            let payload = v8_serialize_str(&mut iso, &ctx, "data");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "child_stdout".into(),
                    payload,
                },
            ))
            .unwrap();

            // Resolve pending promise
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            // Should not crash even without dispatch function registered
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None)
            };

            assert!(completed);
        }

        // --- Part 42: StreamEvent microtasks flushed after dispatch ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );
            let pending = bridge::PendingPromises::new();

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_asyncFn"],
                );
            }

            // Set up dispatch that enqueues a microtask via Promise.resolve().then()
            eval(
                &mut iso,
                &ctx,
                "var _microtaskRanFromStream = false; \
                 globalThis._childProcessDispatch = function(eventType, payload) { \
                     Promise.resolve().then(function() { _microtaskRanFromStream = true; }); \
                 }; \
                 _asyncFn('keep-alive')",
            );
            assert_eq!(pending.len(), 1);

            let (tx, rx) = crossbeam_channel::unbounded();

            let payload = v8_serialize_str(&mut iso, &ctx, "data");
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::StreamEvent {
                    session_id: "test-session".into(),
                    event_type: "child_stdout".into(),
                    payload,
                },
            ))
            .unwrap();

            // Resolve pending promise
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: r,
                },
            ))
            .unwrap();

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None);
            }

            // Microtask enqueued by the dispatch callback should have run
            assert!(eval_bool(
                &mut iso,
                &ctx,
                "_microtaskRanFromStream === true"
            ));
        }

        // --- Part 43: Timeout terminates infinite loop ---
        {
            let mut iso = isolate::create_isolate(None);
            disable_wasm(&mut iso);
            let ctx = isolate::create_context(&mut iso);

            // Create abort channel for timeout
            let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);

            // Get isolate handle for the timeout guard
            let iso_handle = iso.thread_safe_handle();

            // Start a 50ms timeout
            let mut guard = crate::timeout::TimeoutGuard::new(50, iso_handle, abort_tx);

            // Run an infinite loop — timeout should terminate it
            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "while(true) {}", &mut None)
            };

            assert!(guard.timed_out(), "timeout should have fired");
            // V8 termination causes an error
            assert_eq!(code, 1);
            assert!(error.is_some());

            guard.cancel();
        }

        // --- Part 44: Timeout cancelled when execution completes before deadline ---
        {
            let mut iso = isolate::create_isolate(None);
            disable_wasm(&mut iso);
            let ctx = isolate::create_context(&mut iso);

            let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);
            let iso_handle = iso.thread_safe_handle();

            // 5 second timeout — execution completes well before
            let mut guard = crate::timeout::TimeoutGuard::new(5000, iso_handle, abort_tx);

            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "1 + 1", &mut None)
            };

            assert!(!guard.timed_out(), "timeout should not have fired");
            assert_eq!(code, 0);
            assert!(error.is_none());

            guard.cancel();
        }

        // --- Part 45: Timeout fires during sync bridge call (unblocks channel reader) ---
        {
            let mut iso = isolate::create_isolate(None);
            disable_wasm(&mut iso);
            let ctx = isolate::create_context(&mut iso);

            // Set up abort channel for timeout
            let (abort_tx, abort_rx) = crossbeam_channel::bounded::<()>(0);
            let iso_handle = iso.thread_safe_handle();

            // Create a BridgeCallContext with a channel reader that monitors abort_rx
            // Simulate: JS calls a sync bridge function, but no response comes back.
            // The timeout should unblock the reader via abort channel.
            let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<crate::session::SessionCommand>();

            // Writer goes to a buffer (we don't care about outgoing messages)
            let writer_buf = Arc::new(Mutex::new(Vec::new()));

            // Create the bridge context with a channel-based reader
            // We can't use ChannelMessageReader directly (it's #[cfg(not(test))])
            // Instead, test the abort_rx behavior through run_event_loop

            let pending = bridge::PendingPromises::new();

            // Register an async bridge function that sends a BridgeCall
            let bridge_ctx = BridgeCallContext::new(
                Box::new(SharedWriter(Arc::clone(&writer_buf))),
                Box::new(Cursor::new(Vec::new())), // unused for async
                "test-session".into(),
            );
            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            let _async_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _async_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &session_buffers as *const std::cell::RefCell<bridge::SessionBuffers>,
                    &["_slowFn"],
                );
            }

            // Execute code that calls async bridge function (creates a pending promise)
            let (_code, _error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "_slowFn('never-responds')", &mut None)
            };

            assert_eq!(pending.len(), 1, "should have 1 pending promise");

            // Start a 50ms timeout
            let mut guard = crate::timeout::TimeoutGuard::new(50, iso_handle, abort_tx);

            // Run event loop — it should be terminated by the timeout
            // (no messages on cmd_rx, so it blocks until abort_rx fires)
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &cmd_rx, &pending, Some(&abort_rx), None)
            };

            assert!(!completed, "event loop should have been terminated");
            assert!(guard.timed_out(), "timeout should have fired");

            guard.cancel();
            drop(cmd_tx); // clean up
        }

        // --- Part 46: Timeout error message structure ---
        {
            // Verify that the timeout error produced by the session matches expectations.
            // This tests the ipc::ExecutionError structure, not V8 directly.
            let err = crate::ipc::ExecutionError {
                error_type: "Error".into(),
                message: "Script execution timed out".into(),
                stack: String::new(),
                code: Some("ERR_SCRIPT_EXECUTION_TIMEOUT".into()),
            };
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "Script execution timed out");
            assert_eq!(err.code, Some("ERR_SCRIPT_EXECUTION_TIMEOUT".into()));
        }

        // --- Part 47: ProcessExitError detected via _isProcessExit sentinel ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Simulate ProcessExitError: an Error object with _isProcessExit: true and code: 42
            let code = r#"
                var err = new Error("process.exit(42)");
                err._isProcessExit = true;
                err.code = 42;
                throw err;
            "#;

            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(
                exit_code, 42,
                "ProcessExitError should return the error's exit code"
            );
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert!(err.message.contains("process.exit(42)"));
            // Numeric .code should NOT appear in the string code field
            assert_eq!(err.code, None);
        }

        // --- Part 48: ProcessExitError with exit code 0 ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let code = r#"
                var err = new Error("process.exit(0)");
                err._isProcessExit = true;
                err.code = 0;
                throw err;
            "#;

            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(
                exit_code, 0,
                "ProcessExitError code 0 should return exit code 0"
            );
            assert!(error.is_some());
        }

        // --- Part 49: Non-ProcessExitError returns exit code 1 ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Regular error without _isProcessExit sentinel
            let code = r#"throw new TypeError("not a process exit")"#;

            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(exit_code, 1, "Regular errors should return exit code 1");
            let err = error.unwrap();
            assert_eq!(err.error_type, "TypeError");
            assert_eq!(err.message, "not a process exit");
        }

        // --- Part 50: ProcessExitError with custom constructor name ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Custom ProcessExitError class
            let code = r#"
                class ProcessExitError extends Error {
                    constructor(exitCode) {
                        super("process exited with code " + exitCode);
                        this._isProcessExit = true;
                        this.code = exitCode;
                    }
                }
                throw new ProcessExitError(7);
            "#;

            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(exit_code, 7);
            let err = error.unwrap();
            assert_eq!(err.error_type, "ProcessExitError");
            assert!(err.message.contains("process exited with code 7"));
        }

        // --- Part 51: extract_process_exit_code returns None for non-objects ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Thrown string — not an object, should not be detected as ProcessExitError
            let code = r#"throw "just a string""#;
            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(exit_code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "just a string");

            // Object without _isProcessExit sentinel
            let code2 = r#"
                var obj = new Error("no sentinel");
                obj._isProcessExit = false;
                obj.code = 99;
                throw obj;
            "#;
            let (exit_code2, error2) = execute_script(scope, "", code2, &mut None);
            assert_eq!(exit_code2, 1, "_isProcessExit:false should not be detected");
            assert!(error2.is_some());
        }

        // --- Part 52: Error with string code field (Node-style) preserved ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let code = r#"
                var err = new Error("Cannot find module './missing'");
                err.code = "ERR_MODULE_NOT_FOUND";
                throw err;
            "#;

            let (exit_code, error) = execute_script(scope, "", code, &mut None);
            assert_eq!(exit_code, 1);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.code, Some("ERR_MODULE_NOT_FOUND".into()));
        }

        // --- Part 53: Error type from constructor name for standard errors ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // SyntaxError
            let (_, err) = execute_script(scope, "", "eval('function(')", &mut None);
            let err = err.unwrap();
            assert_eq!(err.error_type, "SyntaxError");

            // RangeError
            let (_, err2) = execute_script(scope, "", "new Array(-1)", &mut None);
            let err2 = err2.unwrap();
            assert_eq!(err2.error_type, "RangeError");

            // ReferenceError
            let (_, err3) = execute_script(scope, "", "undefinedVariable", &mut None);
            let err3 = err3.unwrap();
            assert_eq!(err3.error_type, "ReferenceError");
        }

        // --- Part 54: Stack trace extracted from error.stack property ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let code = r#"
                function innerFn() { throw new Error("deep error"); }
                function outerFn() { innerFn(); }
                outerFn();
            "#;

            let (_, error) = execute_script(scope, "", code, &mut None);
            let err = error.unwrap();
            assert_eq!(err.error_type, "Error");
            assert_eq!(err.message, "deep error");
            assert!(
                err.stack.contains("innerFn"),
                "stack should contain innerFn"
            );
            assert!(
                err.stack.contains("outerFn"),
                "stack should contain outerFn"
            );
        }

        // --- V8 ValueSerializer/ValueDeserializer round-trip tests ---

        // Part 55: Primitives round-trip (null, undefined, true, false, integers, floats)
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // null
            let null_val = v8::null(scope).into();
            let bytes = serialize_v8_value(scope, null_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_null());

            // undefined
            let undef_val = v8::undefined(scope).into();
            let bytes = serialize_v8_value(scope, undef_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_undefined());

            // true
            let bool_val = v8::Boolean::new(scope, true).into();
            let bytes = serialize_v8_value(scope, bool_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_true());

            // false
            let bool_val = v8::Boolean::new(scope, false).into();
            let bytes = serialize_v8_value(scope, bool_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_false());

            // integer
            let num_val: v8::Local<v8::Value> = v8::Integer::new(scope, 42).into();
            let bytes = serialize_v8_value(scope, num_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert_eq!(out.int32_value(scope).unwrap(), 42);

            // negative integer
            let num_val: v8::Local<v8::Value> = v8::Integer::new(scope, -7).into();
            let bytes = serialize_v8_value(scope, num_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert_eq!(out.int32_value(scope).unwrap(), -7);

            // float
            let num_val: v8::Local<v8::Value> = v8::Number::new(scope, 3.125).into();
            let bytes = serialize_v8_value(scope, num_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!((out.number_value(scope).unwrap() - 3.125).abs() < 1e-10);
        }

        // Part 56: Strings round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // ASCII string
            let s = v8::String::new(scope, "hello world").unwrap();
            let bytes = serialize_v8_value(scope, s.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_string());
            assert_eq!(out.to_rust_string_lossy(scope), "hello world");

            // Empty string
            let s = v8::String::new(scope, "").unwrap();
            let bytes = serialize_v8_value(scope, s.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_string());
            assert_eq!(out.to_rust_string_lossy(scope), "");

            // Unicode string
            let s = v8::String::new(scope, "hello 🌍 world").unwrap();
            let bytes = serialize_v8_value(scope, s.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert_eq!(out.to_rust_string_lossy(scope), "hello 🌍 world");
        }

        // Part 57: Arrays round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // [1, "two", true, null]
            let arr = v8::Array::new(scope, 4);
            let v1: v8::Local<v8::Value> = v8::Integer::new(scope, 1).into();
            let v2: v8::Local<v8::Value> = v8::String::new(scope, "two").unwrap().into();
            let v3: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            let v4: v8::Local<v8::Value> = v8::null(scope).into();
            arr.set_index(scope, 0, v1);
            arr.set_index(scope, 1, v2);
            arr.set_index(scope, 2, v3);
            arr.set_index(scope, 3, v4);

            let bytes = serialize_v8_value(scope, arr.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_array());
            let out_arr = v8::Local::<v8::Array>::try_from(out).unwrap();
            assert_eq!(out_arr.length(), 4);
            assert_eq!(
                out_arr
                    .get_index(scope, 0)
                    .unwrap()
                    .int32_value(scope)
                    .unwrap(),
                1
            );
            assert_eq!(
                out_arr
                    .get_index(scope, 1)
                    .unwrap()
                    .to_rust_string_lossy(scope),
                "two"
            );
            assert!(out_arr.get_index(scope, 2).unwrap().is_true());
            assert!(out_arr.get_index(scope, 3).unwrap().is_null());

            // Empty array
            let empty_arr = v8::Array::new(scope, 0);
            let bytes = serialize_v8_value(scope, empty_arr.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_array());
            assert_eq!(v8::Local::<v8::Array>::try_from(out).unwrap().length(), 0);
        }

        // Part 58: Objects round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // { name: "test", count: 42, active: true }
            let obj = v8::Object::new(scope);
            let k1 = v8::String::new(scope, "name").unwrap();
            let v1: v8::Local<v8::Value> = v8::String::new(scope, "test").unwrap().into();
            let k2 = v8::String::new(scope, "count").unwrap();
            let v2: v8::Local<v8::Value> = v8::Integer::new(scope, 42).into();
            let k3 = v8::String::new(scope, "active").unwrap();
            let v3: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            obj.set(scope, k1.into(), v1);
            obj.set(scope, k2.into(), v2);
            obj.set(scope, k3.into(), v3);

            let bytes = serialize_v8_value(scope, obj.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_object());
            let out_obj = v8::Local::<v8::Object>::try_from(out).unwrap();
            let k = v8::String::new(scope, "name").unwrap();
            assert_eq!(
                out_obj
                    .get(scope, k.into())
                    .unwrap()
                    .to_rust_string_lossy(scope),
                "test"
            );
            let k = v8::String::new(scope, "count").unwrap();
            assert_eq!(
                out_obj
                    .get(scope, k.into())
                    .unwrap()
                    .int32_value(scope)
                    .unwrap(),
                42
            );
            let k = v8::String::new(scope, "active").unwrap();
            assert!(out_obj.get(scope, k.into()).unwrap().is_true());
        }

        // Part 59: Uint8Array round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let data = [0u8, 1, 2, 255, 128, 64];
            let ab = v8::ArrayBuffer::new(scope, data.len());
            {
                let bs = ab.get_backing_store();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr(),
                        bs.data().unwrap().as_ptr() as *mut u8,
                        data.len(),
                    );
                }
            }
            let u8arr = v8::Uint8Array::new(scope, ab, 0, data.len()).unwrap();

            let bytes = serialize_v8_value(scope, u8arr.into()).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_uint8_array());
            let out_arr = v8::Local::<v8::Uint8Array>::try_from(out).unwrap();
            assert_eq!(out_arr.byte_length(), 6);
            let mut buf = vec![0u8; 6];
            out_arr.copy_contents(&mut buf);
            assert_eq!(buf, vec![0, 1, 2, 255, 128, 64]);
        }

        // Part 60: Nested structures round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Build via JS: { items: [1, { nested: "value" }], flag: false }
            let code = r#"
                ({
                    items: [1, { nested: "value" }],
                    flag: false
                })
            "#;
            let source = v8::String::new(scope, code).unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let val = script.run(scope).unwrap();

            let bytes = serialize_v8_value(scope, val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_object());
            let out_obj = v8::Local::<v8::Object>::try_from(out).unwrap();

            // Check items array
            let k = v8::String::new(scope, "items").unwrap();
            let items = out_obj.get(scope, k.into()).unwrap();
            assert!(items.is_array());
            let items_arr = v8::Local::<v8::Array>::try_from(items).unwrap();
            assert_eq!(items_arr.length(), 2);
            assert_eq!(
                items_arr
                    .get_index(scope, 0)
                    .unwrap()
                    .int32_value(scope)
                    .unwrap(),
                1
            );
            let inner = items_arr.get_index(scope, 1).unwrap();
            assert!(inner.is_object());
            let inner_obj = v8::Local::<v8::Object>::try_from(inner).unwrap();
            let k = v8::String::new(scope, "nested").unwrap();
            assert_eq!(
                inner_obj
                    .get(scope, k.into())
                    .unwrap()
                    .to_rust_string_lossy(scope),
                "value"
            );

            // Check flag
            let k = v8::String::new(scope, "flag").unwrap();
            assert!(out_obj.get(scope, k.into()).unwrap().is_false());
        }

        // Part 61: Date, RegExp, Map, Set, Error round-trip via JS eval
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Date
            let source = v8::String::new(scope, "new Date(1700000000000)").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let date_val = script.run(scope).unwrap();
            let bytes = serialize_v8_value(scope, date_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_date());
            let date = v8::Local::<v8::Date>::try_from(out).unwrap();
            assert_eq!(date.value_of(), 1700000000000.0);

            // RegExp
            let source = v8::String::new(scope, "/abc/gi").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let re_val = script.run(scope).unwrap();
            let bytes = serialize_v8_value(scope, re_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_reg_exp());

            // Map
            let source = v8::String::new(scope, "new Map([['a', 1], ['b', 2]])").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let map_val = script.run(scope).unwrap();
            let bytes = serialize_v8_value(scope, map_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_map());
            let map = v8::Local::<v8::Map>::try_from(out).unwrap();
            assert_eq!(map.size(), 2);

            // Set
            let source = v8::String::new(scope, "new Set([10, 20, 30])").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let set_val = script.run(scope).unwrap();
            let bytes = serialize_v8_value(scope, set_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_set());
            let set = v8::Local::<v8::Set>::try_from(out).unwrap();
            assert_eq!(set.size(), 3);

            // Error
            let source = v8::String::new(scope, "new TypeError('oops')").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let err_val = script.run(scope).unwrap();
            let bytes = serialize_v8_value(scope, err_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            // Error is serialized as a plain object with message property
            assert!(out.is_object());
            let out_obj = v8::Local::<v8::Object>::try_from(out).unwrap();
            let k = v8::String::new(scope, "message").unwrap();
            let msg = out_obj.get(scope, k.into()).unwrap();
            assert_eq!(msg.to_rust_string_lossy(scope), "oops");
        }

        // Part 62: Circular references round-trip
        {
            use crate::bridge::{deserialize_v8_value, serialize_v8_value};

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            // Build circular reference via JS
            let source = v8::String::new(scope, "var o = { a: 1 }; o.self = o; o").unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let circ_val = script.run(scope).unwrap();

            let bytes = serialize_v8_value(scope, circ_val).unwrap();
            let out = deserialize_v8_value(scope, &bytes).unwrap();
            assert!(out.is_object());
            let out_obj = v8::Local::<v8::Object>::try_from(out).unwrap();

            // Verify the self-reference resolves
            let k = v8::String::new(scope, "a").unwrap();
            assert_eq!(
                out_obj
                    .get(scope, k.into())
                    .unwrap()
                    .int32_value(scope)
                    .unwrap(),
                1
            );
            let k = v8::String::new(scope, "self").unwrap();
            let self_ref = out_obj.get(scope, k.into()).unwrap();
            assert!(self_ref.is_object());
            // The self reference should point back to the same structure
            let self_obj = v8::Local::<v8::Object>::try_from(self_ref).unwrap();
            let k = v8::String::new(scope, "a").unwrap();
            assert_eq!(
                self_obj
                    .get(scope, k.into())
                    .unwrap()
                    .int32_value(scope)
                    .unwrap(),
                1
            );
        }

        // --- V8 Code Caching tests ---

        // Part 60: First execution populates the cache
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let mut cache: Option<BridgeCodeCache> = None;

            let bridge = "(function() { globalThis._cached = 'yes'; })()";
            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, bridge, "var _saw = _cached;", &mut cache)
            };

            assert_eq!(code, 0);
            assert!(error.is_none());
            assert_eq!(eval(&mut iso, &ctx, "_saw"), "yes");
            // Cache should be populated after first compile
            assert!(
                cache.is_some(),
                "cache should be populated after first execution"
            );
            assert!(!cache.as_ref().unwrap().cached_data.is_empty());
        }

        // Part 61: Second execution uses the cache and produces correct results
        {
            let mut iso = isolate::create_isolate(None);
            let mut cache: Option<BridgeCodeCache> = None;
            let bridge = "(function() { globalThis._counter = (globalThis._counter || 0) + 1; })()";

            // First execution — populates cache
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_script(scope, bridge, "", &mut cache)
                };
                assert_eq!(code, 0);
                assert!(cache.is_some());
            }

            let cached_data_len = cache.as_ref().unwrap().cached_data.len();

            // Second execution — consumes cache (fresh context)
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_script(scope, bridge, "", &mut cache)
                };
                assert_eq!(code, 0);
                // Cache should still be present (not invalidated)
                assert!(
                    cache.is_some(),
                    "cache should persist after second execution"
                );
                // Cached data should be same size (same code, same cache)
                assert_eq!(cache.as_ref().unwrap().cached_data.len(), cached_data_len);
                // Bridge code executed correctly
                assert_eq!(eval(&mut iso, &ctx, "String(_counter)"), "1");
            }
        }

        // Part 62: Cache is invalidated when bridge code changes
        {
            let mut iso = isolate::create_isolate(None);
            let mut cache: Option<BridgeCodeCache> = None;

            // Populate cache with bridge A
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_script(
                        scope,
                        "(function() { globalThis.x = 'A'; })()",
                        "",
                        &mut cache,
                    )
                };
                assert_eq!(code, 0);
                assert!(cache.is_some());
            }

            let hash_a = cache.as_ref().unwrap().source_hash;

            // Execute with different bridge code — cache should be replaced
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_script(
                        scope,
                        "(function() { globalThis.x = 'B'; })()",
                        "",
                        &mut cache,
                    )
                };
                assert_eq!(code, 0);
                assert!(cache.is_some());
                // Hash should be different
                assert_ne!(cache.as_ref().unwrap().source_hash, hash_a);
                // Code should have executed correctly
                assert_eq!(eval(&mut iso, &ctx, "x"), "B");
            }
        }

        // Part 63: Code caching works with execute_module
        {
            let mut iso = isolate::create_isolate(None);
            let mut cache: Option<BridgeCodeCache> = None;

            let output = Arc::new(Mutex::new(Vec::new()));
            let writer = SharedWriter(Arc::clone(&output));
            let reader = Cursor::new(Vec::new());
            let bridge_ctx =
                BridgeCallContext::new(Box::new(writer), Box::new(reader), "test-session".into());

            let bridge = "(function() { globalThis._moduleBridge = true; })()";

            // First execution populates cache
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, _, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_module(
                        scope,
                        &bridge_ctx,
                        bridge,
                        "export const a = 1;",
                        None,
                        &mut cache,
                    )
                };
                assert_eq!(code, 0);
                assert!(cache.is_some());
            }

            // Second execution consumes cache
            {
                let ctx = isolate::create_context(&mut iso);
                let (code, exports, _) = {
                    let scope = &mut v8::HandleScope::new(&mut iso);
                    let local = v8::Local::new(scope, &ctx);
                    let scope = &mut v8::ContextScope::new(scope, local);
                    execute_module(
                        scope,
                        &bridge_ctx,
                        bridge,
                        "export const b = 2;",
                        None,
                        &mut cache,
                    )
                };
                assert_eq!(code, 0);
                assert!(exports.is_some());
                assert!(cache.is_some());
            }
        }

        // Part 64: Empty bridge code does not populate cache
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let mut cache: Option<BridgeCodeCache> = None;

            let (code, _) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_script(scope, "", "var x = 1;", &mut cache)
            };

            assert_eq!(code, 0);
            assert!(
                cache.is_none(),
                "cache should not be populated for empty bridge code"
            );
        }

        // Part 65: Batch resolve — multiple imports prefetched in one round-trip
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();

            // Batch response (call_id=1): two resolved modules
            let batch_result = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/a.mjs', source: 'export const a = 1;'}, {resolved: '/b.mjs', source: 'export const b = 2;'}]",
            );
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: batch_result,
                },
            )
            .unwrap();

            let writer_buf = Arc::new(Mutex::new(Vec::new()));
            let bridge_ctx = BridgeCallContext::new(
                Box::new(SharedWriter(Arc::clone(&writer_buf))),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let user_code = "import { a } from './a.mjs';\nimport { b } from './b.mjs';\nexport const sum = a + b;";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    user_code,
                    Some("/app/main.mjs"),
                    &mut None,
                )
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "sum").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .int32_value(scope)
                        .unwrap(),
                    3
                );
            }

            // Verify only one BridgeCall was sent (the batch call, not individual calls)
            let written = writer_buf.lock().unwrap();
            let call = crate::ipc_binary::read_frame(&mut Cursor::new(&*written)).unwrap();
            match call {
                crate::ipc_binary::BinaryFrame::BridgeCall { method, .. } => {
                    assert_eq!(method, "_batchResolveModules");
                }
                _ => panic!("expected BridgeCall for _batchResolveModules"),
            }
        }

        // Part 66: Batch resolve — fallback to individual resolution when batch fails
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();

            // Batch response (call_id=1): error (simulating unsupported batch method)
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 1,
                    payload: "No handler for bridge method: _batchResolveModules"
                        .as_bytes()
                        .to_vec(),
                },
            )
            .unwrap();

            // Individual fallback: _resolveModule (call_id=2) returns "/dep.mjs"
            let resolve_result = v8_serialize_str(&mut iso, &ctx, "/dep.mjs");
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: resolve_result,
                },
            )
            .unwrap();

            // Individual fallback: _loadFile (call_id=3) returns source
            let load_result = v8_serialize_str(&mut iso, &ctx, "export const val = 42;");
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 3,
                    status: 0,
                    payload: load_result,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let user_code = "import { val } from './dep.mjs';\nexport const result = val;";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    user_code,
                    Some("/app/main.mjs"),
                    &mut None,
                )
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "result").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .int32_value(scope)
                        .unwrap(),
                    42
                );
            }
        }

        // Part 67: Batch resolve — nested imports resolved via BFS prefetch
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();

            // Level 1 batch (call_id=1): root imports ./a.mjs which imports ./b.mjs
            let batch1 = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/a.mjs', source: \"import { b } from './b.mjs'; export const a = b + 1;\"}]",
            );
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: batch1,
                },
            )
            .unwrap();

            // Level 2 batch (call_id=2): ./b.mjs has no further imports
            let batch2 = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/b.mjs', source: 'export const b = 10;'}]",
            );
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: batch2,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let user_code = "import { a } from './a.mjs';\nexport const result = a;";
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    user_code,
                    Some("/app/main.mjs"),
                    &mut None,
                )
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());
            let exports = exports.unwrap();
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = crate::bridge::deserialize_v8_value(scope, &exports).unwrap();
                let obj = v8::Local::<v8::Object>::try_from(val).unwrap();
                let k = v8::String::new(scope, "result").unwrap();
                assert_eq!(
                    obj.get(scope, k.into())
                        .unwrap()
                        .int32_value(scope)
                        .unwrap(),
                    11
                );
            }
        }

        // Part 68: Batch resolve — module with no imports skips batch call
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let writer_buf = Arc::new(Mutex::new(Vec::new()));
            let bridge_ctx = BridgeCallContext::new(
                Box::new(SharedWriter(Arc::clone(&writer_buf))),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let user_code = "export const x = 42;";
            let (code, _exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(scope, &bridge_ctx, "", user_code, None, &mut None)
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());

            // No BridgeCall should have been sent (no imports to resolve)
            let written = writer_buf.lock().unwrap();
            assert!(
                written.is_empty(),
                "no IPC calls expected for module with no imports"
            );
        }

        // Part 69: Dynamic import works after execute_module returns
        {
            let mut iso = isolate::create_isolate(None);
            iso.set_host_import_module_dynamically_callback(dynamic_import_callback);
            iso.set_host_initialize_import_meta_object_callback(import_meta_object_callback);
            let ctx = isolate::create_context(&mut iso);

            let mut response_buf = Vec::new();

            let resolve_result = v8_serialize_str(&mut iso, &ctx, "/dep.mjs");
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: resolve_result,
                },
            )
            .unwrap();

            let load_result = v8_serialize_str(&mut iso, &ctx, "export const value = 42;");
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: load_result,
                },
            )
            .unwrap();

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let user_code = r#"
                globalThis.loadDep = async () => (await import("./dep.mjs")).value;
                export const ready = true;
            "#;
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(
                    scope,
                    &bridge_ctx,
                    "",
                    user_code,
                    Some("/app/main.mjs"),
                    &mut None,
                )
            };

            assert_eq!(code, 0, "error: {:?}", error);
            assert!(error.is_none());
            assert!(exports.is_some());

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let tc = &mut v8::TryCatch::new(scope);
                let source = v8::String::new(
                    tc,
                    "globalThis.__depPromise = globalThis.loadDep().then((value) => { globalThis.__depValue = value; return value; });",
                )
                .unwrap();
                let script = v8::Script::compile(tc, source, None).unwrap();
                assert!(script.run(tc).is_some());
                tc.perform_microtask_checkpoint();
                assert!(tc.exception().is_none());
            }

            assert_eq!(eval(&mut iso, &ctx, "String(globalThis.__depValue)"), "42");
            clear_module_state();
        }

        // --- Part 57: serialize_v8_value_into reuses buffer capacity ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let mut buf = Vec::new();

            // First serialization grows the buffer
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = v8::String::new(scope, "hello world").unwrap();
                bridge::serialize_v8_value_into(scope, val.into(), &mut buf).expect("serialize");
            }
            assert!(!buf.is_empty());
            let cap_after_first = buf.capacity();

            // Second serialization (smaller value) reuses capacity
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = v8::Integer::new(scope, 42);
                bridge::serialize_v8_value_into(scope, val.into(), &mut buf).expect("serialize");
            }
            assert_eq!(
                buf.capacity(),
                cap_after_first,
                "capacity should stay at high-water mark"
            );

            // Third serialization (larger value) grows buffer
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let long_str = "x".repeat(1024);
                let val = v8::String::new(scope, &long_str).unwrap();
                bridge::serialize_v8_value_into(scope, val.into(), &mut buf).expect("serialize");
            }
            assert!(
                buf.capacity() >= cap_after_first,
                "capacity should grow for larger values"
            );
            let cap_after_large = buf.capacity();

            // Fourth serialization (small again) stays at high-water mark
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let val = v8::Boolean::new(scope, true);
                bridge::serialize_v8_value_into(scope, val.into(), &mut buf).expect("serialize");
            }
            assert_eq!(
                buf.capacity(),
                cap_after_large,
                "capacity stays at high-water mark"
            );

            // Verify the serialized data is correct (round-trip)
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                let deserialized = bridge::deserialize_v8_value(scope, &buf).expect("deserialize");
                assert!(deserialized.is_true(), "should deserialize to true");
            }
        }

        // --- Part 58: SessionBuffers ser_buf grows to high-water mark across bridge calls ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let session_buffers = std::cell::RefCell::new(bridge::SessionBuffers::new());
            assert!(
                session_buffers.borrow().ser_buf.capacity() >= 256,
                "initial capacity should be >= 256"
            );

            // Simulate multiple serializations through SessionBuffers
            for i in 0..5 {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);

                // Create varying-size values
                let val_str = "a".repeat(100 * (i + 1));
                let val = v8::String::new(scope, &val_str).unwrap();
                let mut bufs = session_buffers.borrow_mut();
                bridge::serialize_v8_value_into(scope, val.into(), &mut bufs.ser_buf)
                    .expect("serialize");
            }

            // Buffer capacity should be at least as large as the last (largest) serialization
            let bufs = session_buffers.borrow();
            assert!(!bufs.ser_buf.is_empty(), "should contain serialized data");

            // Verify the buffer hasn't been dropped/reallocated to smaller size
            let final_cap = bufs.ser_buf.capacity();
            assert!(final_cap >= bufs.ser_buf.len(), "capacity >= len");
        }
    }
}

/// Detect if source code is likely CommonJS (not ESM).
/// Checks for module.exports, exports.X, or require() patterns without ESM import/export.
fn is_likely_cjs(source: &str) -> bool {
    // If it has ESM syntax, it's not CJS
    if source.contains("export ") || source.contains("import ") {
        // Check for actual ESM: `export default`, `export {`, `export const`, `import {`, `import "`, `import(`
        let has_esm_export = source.contains("export default")
            || source.contains("export {")
            || source.contains("export const")
            || source.contains("export function")
            || source.contains("export class")
            || source.contains("export var")
            || source.contains("export let");
        let has_esm_import = source.contains("import {")
            || source.contains("import \"")
            || source.contains("import '")
            || source.contains("import *");
        if has_esm_export || has_esm_import {
            return false;
        }
    }
    // CJS indicators
    source.contains("module.exports") || source.contains("exports.") || source.contains("require(")
}

/// Extract named export names from CJS source by scanning for `exports.X =` and
/// `module.exports = { X: ... }` patterns. Returns a list of valid JS identifiers.
fn extract_cjs_export_names(source: &str) -> Vec<String> {
    use std::collections::HashSet;
    let mut names = HashSet::new();

    // Pattern 1: exports.NAME = ...
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("exports.") {
            if let Some(eq_pos) = rest.find('=') {
                let name = rest[..eq_pos].trim();
                if is_valid_js_ident(name) && name != "default" {
                    names.insert(name.to_string());
                }
            }
        }
        // Pattern 2: Object.defineProperty(exports, "NAME", ...)
        if trimmed.contains("Object.defineProperty(exports") {
            if let Some(start) = trimmed.find('"').or_else(|| trimmed.find('\'')) {
                let rest = &trimmed[start + 1..];
                if let Some(end) = rest.find('"').or_else(|| rest.find('\'')) {
                    let name = &rest[..end];
                    if is_valid_js_ident(name) && name != "default" && name != "__esModule" {
                        names.insert(name.to_string());
                    }
                }
            }
        }
    }

    let mut result: Vec<String> = names.into_iter().collect();
    result.sort();
    result
}

fn is_valid_js_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_alphabetic() && first != '_' && first != '$' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}
