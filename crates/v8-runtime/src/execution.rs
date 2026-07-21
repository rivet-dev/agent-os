// Script compilation, CJS/ESM execution, module loading

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::num::NonZeroI32;
use std::time::Instant;

// ── Module-load read/compile split (opt-in via AGENTOS_MODULE_TRACE=1) ──
// Per module miss: resolve IPC + load (read) IPC + format IPC + V8 compile.
// Accumulates ns per category and writes a running total to
// AGENTOS_MODULE_TRACE_FILE so we can see whether the VM module-load tax is
// IPC (read) or V8 compile bound. Index: 0=count 1=resolve 2=load 3=format 4=compile.
static MOD_TRACE: std::sync::OnceLock<std::sync::Mutex<[u64; 5]>> = std::sync::OnceLock::new();

fn mod_trace_enabled() -> bool {
    std::env::var("AGENTOS_MODULE_TRACE").as_deref() == Ok("1")
}

fn record_mod(idx: usize, ns: u64) {
    let m = MOD_TRACE.get_or_init(|| std::sync::Mutex::new([0u64; 5]));
    let Ok(mut a) = m.lock() else {
        return;
    };
    a[idx] = a[idx].wrapping_add(ns);
    if idx == 4 {
        a[0] += 1;
        if a[0] % 25 == 0 {
            if let Ok(path) = std::env::var("AGENTOS_MODULE_TRACE_FILE") {
                let _ = std::fs::write(
                    &path,
                    format!(
                        "modules={} resolve_ms={} load_ms={} format_ms={} compile_ms={}\n",
                        a[0],
                        a[1] / 1_000_000,
                        a[2] / 1_000_000,
                        a[3] / 1_000_000,
                        a[4] / 1_000_000,
                    ),
                );
            }
        }
    }
}

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

pub fn install_high_resolution_time_global(scope: &mut v8::HandleScope, origin: *const Instant) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let external = v8::External::new(scope, origin as *mut c_void);
    let template = v8::FunctionTemplate::builder(high_resolution_time_callback)
        .data(external.into())
        .build(scope);
    let Some(func) = template.get_function(scope) else {
        return;
    };
    let key = v8::String::new(scope, "__agentOsHrNowUs").unwrap();
    let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, key.into(), func.into(), attr);
}

pub fn install_require_esm_sync_global(scope: &mut v8::HandleScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let template = v8::FunctionTemplate::builder(require_esm_sync_callback).build(scope);
    let Some(function) = template.get_function(scope) else {
        return;
    };
    let key = v8::String::new(scope, "__agentOsRequireEsmSync").unwrap();
    let attributes = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, key.into(), function.into(), attributes);
}

fn require_esm_sync_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let specifier = args.get(0).to_rust_string_lossy(scope);
    let referrer = args.get(1).to_rust_string_lossy(scope);
    if specifier.is_empty() {
        throw_module_error_with_code(
            scope,
            "require() expected a non-empty ES module filename",
            "ERR_INVALID_ARG_VALUE",
        );
        return;
    }

    let Some(module) = resolve_or_compile_module(scope, &specifier, &referrer) else {
        return;
    };
    if module.get_status() == v8::ModuleStatus::Uninstantiated
        && module
            .instantiate_module(scope, module_resolve_callback)
            .is_none()
    {
        return;
    }
    if module.get_status() == v8::ModuleStatus::Errored {
        let exception = module.get_exception();
        scope.throw_exception(exception);
        return;
    }
    if module.is_graph_async() {
        throw_module_error_with_code(
            scope,
            &format!("require() cannot be used on an ESM graph with top-level await: {specifier}"),
            "ERR_REQUIRE_ASYNC_MODULE",
        );
        return;
    }

    if module.get_status() != v8::ModuleStatus::Evaluated {
        let Some(result) = module.evaluate(scope) else {
            return;
        };
        if module.is_graph_async() {
            throw_module_error_with_code(
                scope,
                &format!(
                    "require() cannot be used on an ESM graph with top-level await: {specifier}"
                ),
                "ERR_REQUIRE_ASYNC_MODULE",
            );
            return;
        }
        if result.is_promise() {
            let Ok(promise) = v8::Local::<v8::Promise>::try_from(result) else {
                throw_module_error(scope, "ES module evaluation returned an invalid promise");
                return;
            };
            scope.perform_microtask_checkpoint();
            match promise.state() {
                v8::PromiseState::Pending => {
                    throw_module_error_with_code(
                        scope,
                        &format!(
                            "require() cannot be used on an ESM graph with top-level await: {specifier}"
                        ),
                        "ERR_REQUIRE_ASYNC_MODULE",
                    );
                    return;
                }
                v8::PromiseState::Rejected => {
                    let rejection = promise.result(scope);
                    scope.throw_exception(rejection);
                    return;
                }
                v8::PromiseState::Fulfilled => {}
            }
        }
    }
    if module.get_status() == v8::ModuleStatus::Errored {
        let exception = module.get_exception();
        scope.throw_exception(exception);
        return;
    }
    rv.set(module.get_module_namespace());
}

fn high_resolution_time_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let external = match v8::Local::<v8::External>::try_from(args.data()) {
        Ok(ext) => ext,
        Err(_) => {
            let msg = v8::String::new(scope, "internal error: missing hrtime origin").unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };
    // SAFETY: the pointer targets the session thread's per-isolate Instant,
    // which is kept alive for the lifetime of the V8 session thread.
    let origin = unsafe { &*(external.value() as *const Instant) };
    let micros = origin.elapsed().as_secs_f64() * 1_000_000.0;
    rv.set(v8::Number::new(scope, micros).into());
}

/// Inject globals from a V8-serialized payload containing { processConfig, osConfig }.
///
/// The payload is produced by node:v8.serialize() on the host side.
/// Deserializes into V8, extracts processConfig and osConfig, freezes them,
/// and sets them as non-writable, non-configurable global properties.
pub fn inject_globals_from_payload(
    scope: &mut v8::HandleScope,
    payload: &[u8],
) -> Result<(), ExecutionError> {
    let context = scope.get_current_context();
    let global = context.global(scope);

    // Deserialize the V8 payload { processConfig, osConfig }
    let config_val = deserialize_v8_value(scope, payload)
        .map_err(|err| invalid_globals_payload_error(format!("decode failed: {err}")))?;

    if !config_val.is_object() {
        return Err(invalid_globals_payload_error("payload is not an object"));
    }
    let config_obj = v8::Local::<v8::Object>::try_from(config_val)
        .map_err(|_| invalid_globals_payload_error("payload is not an object"))?;
    if !is_plain_config_object(scope, config_obj) {
        return Err(invalid_globals_payload_error(
            "payload is not a plain object",
        ));
    }

    // Validate both config objects before mutating globals so malformed payloads
    // cannot leave a partially injected execution context.
    let (pc_val, pc_obj) = required_object_property(scope, config_obj, "processConfig")?;
    let (oc_val, oc_obj) = required_object_property(scope, config_obj, "osConfig")?;

    let (_env_val, env_obj) =
        required_object_property_with_label(scope, pc_obj, "env", "processConfig.env")?;
    freeze_config_object(scope, env_obj, "processConfig.env")?;
    freeze_config_object(scope, pc_obj, "processConfig")?;
    freeze_config_object(scope, oc_obj, "osConfig")?;
    let global_key = v8::String::new(scope, "_processConfig").unwrap();
    let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, global_key.into(), pc_val, attr);

    let global_key = v8::String::new(scope, "_osConfig").unwrap();
    let attr = v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    global.define_own_property(scope, global_key.into(), oc_val, attr);

    Ok(())
}

fn required_object_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    obj: v8::Local<'s, v8::Object>,
    name: &str,
) -> Result<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>), ExecutionError> {
    required_object_property_with_label(scope, obj, name, name)
}

fn required_object_property_with_label<'s>(
    scope: &mut v8::HandleScope<'s>,
    obj: v8::Local<'s, v8::Object>,
    name: &str,
    error_label: &str,
) -> Result<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Object>), ExecutionError> {
    let key = v8::String::new(scope, name).unwrap();
    let value = obj
        .get(scope, key.into())
        .filter(|value| !value.is_null_or_undefined())
        .ok_or_else(|| invalid_globals_payload_error(format!("missing {error_label}")))?;
    if !value.is_object() {
        return Err(invalid_globals_payload_error(format!(
            "{error_label} is not an object"
        )));
    }
    let object = v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| invalid_globals_payload_error(format!("{error_label} is not an object")))?;
    if !is_plain_config_object(scope, object) {
        return Err(invalid_globals_payload_error(format!(
            "{error_label} is not a plain object"
        )));
    }
    Ok((value, object))
}

fn is_plain_config_object(scope: &mut v8::HandleScope, object: v8::Local<v8::Object>) -> bool {
    let Some(prototype) = object.get_prototype(scope) else {
        return false;
    };
    if prototype.is_null() {
        return true;
    }
    if !prototype.is_object() {
        return false;
    }
    let Ok(prototype_object) = v8::Local::<v8::Object>::try_from(prototype) else {
        return false;
    };
    prototype_object
        .get_prototype(scope)
        .is_some_and(|parent| parent.is_null())
}

fn freeze_config_object(
    scope: &mut v8::HandleScope,
    object: v8::Local<v8::Object>,
    label: &str,
) -> Result<(), ExecutionError> {
    match object.set_integrity_level(scope, v8::IntegrityLevel::Frozen) {
        Some(true) => Ok(()),
        Some(false) | None => Err(invalid_globals_payload_error(format!(
            "failed to freeze {label}"
        ))),
    }
}

fn invalid_globals_payload_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError {
        error_type: "Error".into(),
        message: format!("invalid InjectGlobals payload: {}", message.into()),
        stack: String::new(),
        code: Some("ERR_INVALID_GLOBALS_PAYLOAD".into()),
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
    execute_script_with_options(scope, None, bridge_code, user_code, None, bridge_cache)
}

pub fn execute_script_with_options(
    scope: &mut v8::HandleScope,
    bridge_ctx: Option<&BridgeCallContext>,
    bridge_code: &str,
    user_code: &str,
    file_path: Option<&str>,
    bridge_cache: &mut Option<BridgeCodeCache>,
) -> (i32, Option<ExecutionError>) {
    if let Some(bridge_ctx) = bridge_ctx {
        MODULE_RESOLVE_STATE.with(|cell| {
            *cell.borrow_mut() = Some(ModuleResolveState {
                bridge_ctx: bridge_ctx as *const BridgeCallContext,
                module_names: HashMap::new(),
                module_cache: HashMap::new(),
                guest_reader: None,
            });
        });
    }

    // Run bridge code IIFE (with code caching)
    if !bridge_code.is_empty() {
        let (code, err) = run_bridge_cached(scope, bridge_code, bridge_cache);
        if code != 0 {
            if bridge_ctx.is_some() {
                clear_module_state();
            }
            return (code, err);
        }
    }

    if bridge_ctx.is_some() {
        install_require_esm_sync_global(scope);
    }

    // Run user code
    {
        let tc = &mut v8::TryCatch::new(scope);
        let source = match v8::String::new(tc, user_code) {
            Some(s) => s,
            None => {
                if bridge_ctx.is_some() {
                    clear_module_state();
                }
                return (
                    1,
                    Some(ExecutionError {
                        error_type: "Error".into(),
                        message: "user code string too large for V8".into(),
                        stack: String::new(),
                        code: None,
                    }),
                );
            }
        };
        let origin = file_path.and_then(|path| {
            let resource = v8::String::new(tc, path)?;
            Some(v8::ScriptOrigin::new(
                tc,
                resource.into(),
                0,
                0,
                false,
                -1,
                None,
                false,
                false,
                false,
                None,
            ))
        });
        let script = match v8::Script::compile(tc, source, origin.as_ref()) {
            Some(s) => s,
            None => {
                if bridge_ctx.is_some() {
                    clear_module_state();
                }
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
                if bridge_ctx.is_some() {
                    clear_module_state();
                }
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
            if bridge_ctx.is_some() {
                clear_module_state();
            }
            let (c, err) = exception_to_result(tc, exception);
            return (c, Some(err));
        }

        if let Some(err) = take_unhandled_promise_rejection(tc) {
            if bridge_ctx.is_some() {
                clear_module_state();
            }
            return (1, Some(err));
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
                    if bridge_ctx.is_some() {
                        clear_module_state();
                    }
                    let (c, err) = exception_to_result(tc, rejection);
                    return (c, Some(err));
                }
                v8::PromiseState::Fulfilled => {
                    return (extract_global_process_exit_code(tc).unwrap_or(0), None);
                }
            }
        }
    }

    (extract_global_process_exit_code(scope).unwrap_or(0), None)
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
    if code_val.is_undefined() || code_val.is_null() {
        Some(0)
    } else if code_val.is_number() {
        Some(code_val.int32_value(scope).unwrap_or(0))
    } else {
        Some(1)
    }
}

pub(crate) fn extract_global_process_exit_code(scope: &mut v8::HandleScope) -> Option<i32> {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let process_key = v8::String::new(scope, "process")?;
    let process_val = global.get(scope, process_key.into())?;
    if !process_val.is_object() {
        return None;
    }

    let process_obj = v8::Local::<v8::Object>::try_from(process_val).ok()?;
    let exit_code_key = v8::String::new(scope, "exitCode")?;
    let exit_code_val = process_obj.get(scope, exit_code_key.into())?;
    if exit_code_val.is_undefined() || exit_code_val.is_null() {
        None
    } else if exit_code_val.is_number() {
        Some(exit_code_val.int32_value(scope).unwrap_or(0))
    } else {
        None
    }
}

/// Extract error info and exit code from a V8 exception.
/// For ProcessExitError (detected via _isProcessExit sentinel), returns the error's exit code.
/// For other errors, returns exit code 1.
pub(crate) fn exception_to_result(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
) -> (i32, ExecutionError) {
    let error = extract_error_info(scope, exception);
    let exit_code = extract_process_exit_code(scope, exception)
        .or_else(|| parse_process_exit_code_from_error(&error))
        .unwrap_or(1);
    (exit_code, error)
}

fn parse_process_exit_code_from_error(error: &ExecutionError) -> Option<i32> {
    if error.error_type != "ProcessExitError" && !error.message.starts_with("process.exit(") {
        return None;
    }
    let code = error
        .message
        .strip_prefix("process.exit(")?
        .strip_suffix(')')?;
    code.parse::<i32>().ok()
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

/// Build the _processConfig JS object: { cwd, env, timing_mitigation, frozen_time_ms, high_resolution_time }
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

    // high_resolution_time
    let key = v8::String::new(scope, "high_resolution_time").unwrap();
    let val = v8::Boolean::new(scope, config.high_resolution_time);
    obj.set(scope, key.into(), val.into());

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
/// Direct, in-process module source reader living on the V8 session thread.
///
/// The V8 module callback (resolve_or_compile_module) runs in this crate
/// (v8-runtime), but the module reader/resolver lives in the higher `execution`
/// crate, so a direct call would be a circular dependency — today every module
/// resolve/load/format is a sync bridge round-trip (~139us × ~5,100 calls ≈ all
/// of loadPiSdkRuntime). This trait is owned here and implemented in the higher
/// crate over the mounted `HostDirModuleReader`, then handed down to the session
/// thread so module source can be read directly, skipping the round-trip. It is
/// confined to the same mounts the guest sees (the impl keeps the reader's
/// `openat2(RESOLVE_BENEATH)` confinement).
pub trait GuestModuleReader: Send {
    /// Read the source for an already-resolved guest module path, or `None` if
    /// the path isn't served by this reader (caller falls back to the bridge IPC).
    fn read_module_source(&mut self, resolved_guest_path: &str) -> Option<String>;

    /// Resolve a module specifier (import mode) to a resolved guest path directly,
    /// skipping the bridge `_resolveModule` round-trip. `None` => fall back to IPC.
    /// Implementations must match the bridge resolver exactly (same cache, same
    /// ESM/CJS/exports/symlink semantics).
    fn resolve_module(&mut self, specifier: &str, referrer: &str) -> Option<String> {
        let _ = (specifier, referrer);
        None
    }
}

/// Install (or clear) the direct module reader for the current session thread.
/// Called by the session thread when it receives a `SetModuleReader` command; the
/// next `execute_module` moves it into the resolve state. Must be called on the
/// session/isolate thread.
pub fn install_session_guest_reader(reader: Option<Box<dyn GuestModuleReader>>) {
    SESSION_GUEST_READER.with(|cell| *cell.borrow_mut() = reader);
}

struct ModuleResolveState {
    bridge_ctx: *const BridgeCallContext,
    /// identity_hash → resource_name for referrer lookup
    module_names: HashMap<NonZeroI32, String>,
    /// resolved_path and referrer-qualified request keys → Global<Module> cache
    module_cache: HashMap<String, v8::Global<v8::Module>>,
    /// Optional direct module-source reader (session-thread local). When present,
    /// module loads read source directly instead of via the bridge round-trip.
    guest_reader: Option<Box<dyn GuestModuleReader>>,
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
    /// Session-thread-local handoff: a SetModuleReader command stashes the reader
    /// here, and the next execute_module moves it into ModuleResolveState so module
    /// source loads read directly on this thread instead of round-tripping the bridge.
    static SESSION_GUEST_READER: RefCell<Option<Box<dyn GuestModuleReader>>> = const { RefCell::new(None) };
    static PENDING_MODULE_EVALUATION: RefCell<Option<PendingModuleEvaluation>> = const { RefCell::new(None) };
    static PENDING_SCRIPT_EVALUATION: RefCell<Option<PendingScriptEvaluation>> = const { RefCell::new(None) };
    static CJS_RUNTIME_EXTRACTION_IN_PROGRESS: RefCell<HashSet<String>> =
        RefCell::new(HashSet::new());
}

// Framework build graphs routinely cross one thousand ESM modules. Keep the
// cache bounded, but leave enough headroom for Astro/Vite production builds.
const MAX_MODULE_RESOLVE_MODULES: usize = 4096;
const MAX_MODULE_RESOLVE_CACHE_ENTRIES: usize = 16384;
const MAX_MODULE_PREFETCH_GRAPH_MODULES: usize = 4096;
const MAX_MODULE_PREFETCH_BATCH_SIZE: usize = 256;
const MAX_MODULE_BATCH_RESOLVE_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CJS_NAMED_EXPORTS: usize = 1024;
const MAX_CJS_RUNTIME_EXPORT_NAME_LEN: usize = 512;

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

pub fn set_pending_script_evaluation(scope: &mut v8::HandleScope, promise: v8::Local<v8::Promise>) {
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
        .and_then(|state| state.take_next_unhandled())
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
        v8::PromiseState::Fulfilled => {
            Some((extract_global_process_exit_code(tc).unwrap_or(0), None))
        }
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
                Ok(exports) => Some((
                    extract_global_process_exit_code(tc).unwrap_or(0),
                    Some(exports),
                    None,
                )),
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

    // Set up thread-local resolve state, taking any reader the session thread
    // stashed via a SetModuleReader command so module loads read source directly.
    let guest_reader = SESSION_GUEST_READER.with(|cell| cell.borrow_mut().take());
    MODULE_RESOLVE_STATE.with(|cell| {
        *cell.borrow_mut() = Some(ModuleResolveState {
            bridge_ctx: bridge_ctx as *const BridgeCallContext,
            module_names: HashMap::new(),
            module_cache: HashMap::new(),
            guest_reader,
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

    install_require_esm_sync_global(scope);

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

        let effective_user_code = add_esm_runtime_prelude(user_code);
        let v8_source = match v8::String::new(tc, &effective_user_code) {
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
        if inst_result.is_none() {
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
        (
            extract_global_process_exit_code(tc).unwrap_or(0),
            Some(exports_bytes),
            None,
        )
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
        if uncached.len() >= MAX_MODULE_PREFETCH_BATCH_SIZE {
            break;
        }
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
    let mut visited_modules = 0usize;

    while !pending.is_empty() && visited_modules < MAX_MODULE_PREFETCH_GRAPH_MODULES {
        let remaining_modules = MAX_MODULE_PREFETCH_GRAPH_MODULES - visited_modules;
        let current_len = pending.len().min(remaining_modules);
        let current: Vec<_> = pending.drain(..current_len).collect();
        visited_modules += current.len();

        // Collect all uncached imports from pending modules
        let mut batch: Vec<(String, String)> = Vec::new();
        for (global_mod, referrer) in &current {
            let local_mod = v8::Local::new(scope, global_mod);
            let imports = extract_uncached_imports(scope, local_mod, referrer);
            for (spec, ref_name) in imports {
                if batch.len() >= MAX_MODULE_PREFETCH_BATCH_SIZE {
                    break;
                }
                // Deduplicate within this batch by the full request identity.
                if !batch.iter().any(|(s, r)| s == &spec && r == &ref_name) {
                    batch.push((spec, ref_name));
                }
            }
            if batch.len() >= MAX_MODULE_PREFETCH_BATCH_SIZE {
                break;
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

                let module_format = lookup_module_format_via_ipc(scope, bridge_ctx, resolved_path);
                let effective_source =
                    build_module_source(scope, source_code, resolved_path, module_format);

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
                if !cache_resolved_module(
                    module,
                    global,
                    resolved_path.clone(),
                    Some(module_request_cache_key(&batch[i].0, &batch[i].1)),
                ) {
                    return;
                }

                if visited_modules + next_pending.len() < MAX_MODULE_PREFETCH_GRAPH_MODULES {
                    next_pending.push((v8::Global::new(scope, module), resolved_path.clone()));
                }
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

    // Phase 3: Resolve module path — directly on this thread via the session
    // reader when present (skips the bridge `_resolveModule` round-trip), else IPC.
    let trace = mod_trace_enabled();
    let t = trace.then(Instant::now);
    let direct_resolved = MODULE_RESOLVE_STATE.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .and_then(|state| state.guest_reader.as_mut())
            .and_then(|reader| reader.resolve_module(specifier_str, referrer_name))
    });
    let resolved_path = match direct_resolved {
        Some(path) => path,
        None => resolve_module_via_ipc(scope, ctx, specifier_str, referrer_name)?,
    };
    if let Some(t) = t {
        record_mod(1, t.elapsed().as_nanos() as u64);
    }

    // Phase 4: Check cache by resolved path.
    let cached_global = MODULE_RESOLVE_STATE.with(|cell| {
        let borrow = cell.borrow();
        let state = borrow.as_ref()?;
        state.module_cache.get(&resolved_path).cloned()
    });
    if let Some(cached) = cached_global {
        return Some(v8::Local::new(scope, &cached));
    }

    // Phase 5: Load the module source — directly via the session-thread reader
    // when present (skips the ~139us bridge round-trip), else via the bridge IPC.
    // guest_reader is None until the higher crate plumbs the reader down, so this
    // is currently a no-op fall-through to the IPC path (zero behavior change).
    let t = trace.then(Instant::now);
    let direct_source = MODULE_RESOLVE_STATE.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .and_then(|state| state.guest_reader.as_mut())
            .and_then(|reader| reader.read_module_source(&resolved_path))
    });
    let raw_source = match direct_source {
        Some(source) => source,
        None => load_module_via_ipc(scope, ctx, &resolved_path)?,
    };
    if let Some(t) = t {
        record_mod(2, t.elapsed().as_nanos() as u64);
    }
    let t = trace.then(Instant::now);
    let module_format = lookup_module_format_via_ipc(scope, ctx, &resolved_path);
    if let Some(t) = t {
        record_mod(3, t.elapsed().as_nanos() as u64);
    }
    let source_code = build_module_source(scope, &raw_source, &resolved_path, module_format);

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
    let t = trace.then(Instant::now);
    let module = v8::script_compiler::compile_module(scope, &mut compiled)?;
    if let Some(t) = t {
        record_mod(4, t.elapsed().as_nanos() as u64);
    }
    let global = v8::Global::new(scope, module);
    if !cache_resolved_module(module, global, resolved_path, Some(request_cache_key)) {
        throw_module_error(scope, "module resolution cache limit exceeded");
        return None;
    }

    Some(module)
}

fn cache_resolved_module(
    module: v8::Local<v8::Module>,
    global: v8::Global<v8::Module>,
    resolved_path: String,
    request_cache_key: Option<String>,
) -> bool {
    MODULE_RESOLVE_STATE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(state) = borrow.as_mut() else {
            return true;
        };

        let identity_hash = module.get_identity_hash();
        let new_module_name = !state.module_names.contains_key(&identity_hash);
        let new_resolved_path = !state.module_cache.contains_key(&resolved_path);
        let new_request_key = request_cache_key
            .as_ref()
            .is_some_and(|key| !state.module_cache.contains_key(key));

        let next_module_count = state.module_names.len() + usize::from(new_module_name);
        let next_cache_count = state.module_cache.len()
            + usize::from(new_resolved_path)
            + usize::from(new_request_key);
        if next_module_count > MAX_MODULE_RESOLVE_MODULES
            || next_cache_count > MAX_MODULE_RESOLVE_CACHE_ENTRIES
        {
            return false;
        }

        state
            .module_names
            .insert(identity_hash, resolved_path.clone());
        state
            .module_cache
            .insert(resolved_path.clone(), global.clone());
        if let Some(request_cache_key) = request_cache_key {
            state.module_cache.insert(request_cache_key, global);
        }
        true
    })
}

fn import_meta_resolve_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let specifier = args.get(0);
    if specifier.is_undefined() {
        let message = v8::String::new(scope, "import.meta.resolve requires a specifier").unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return;
    }

    let specifier = specifier.to_rust_string_lossy(scope);
    let referrer = args.data().to_rust_string_lossy(scope);
    let bridge_ctx_ptr = MODULE_RESOLVE_STATE.with(|cell| {
        let state = cell.borrow();
        state.as_ref().map(|state| state.bridge_ctx)
    });
    let Some(bridge_ctx_ptr) = bridge_ctx_ptr else {
        throw_module_error(scope, "module resolver is unavailable");
        return;
    };

    let direct_resolved = MODULE_RESOLVE_STATE.with(|cell| {
        cell.borrow_mut()
            .as_mut()
            .and_then(|state| state.guest_reader.as_mut())
            .and_then(|reader| reader.resolve_module(&specifier, &referrer))
    });
    let resolved = match direct_resolved {
        Some(resolved) => resolved,
        None => {
            // SAFETY: ModuleResolveState owns this pointer for the lifetime of the
            // active module execution, and import.meta callbacks run synchronously
            // on that same session thread.
            let bridge_ctx = unsafe { &*bridge_ctx_ptr };
            let Some(resolved) = resolve_module_via_ipc(scope, bridge_ctx, &specifier, &referrer)
            else {
                return;
            };
            resolved
        }
    };

    let resolved_url = if resolved.starts_with('/') {
        format!("file://{resolved}")
    } else {
        resolved
    };
    let Some(value) = v8::String::new(scope, &resolved_url) else {
        throw_module_error(scope, "resolved module URL is too large for V8");
        return;
    };
    rv.set(value.into());
}

/// Callback invoked by V8 when `import.meta` is accessed in an ES module.
/// Sets `import.meta.url` and Node-compatible `import.meta.resolve` values.
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
    let module_location = MODULE_RESOLVE_STATE.with(|cell| {
        let state_opt = cell.borrow();
        if let Some(ref state) = *state_opt {
            if let Some(name) = state.module_names.get(&identity_hash) {
                let n = name.clone();
                let url = if n.starts_with("file://") {
                    n.clone()
                } else if n.starts_with("/") {
                    format!("file://{n}")
                } else {
                    n.clone()
                };
                return Some((n, url));
            }
        }
        None
    });

    if let Some((referrer, url)) = module_location {
        let key = v8::String::new(scope, "url").unwrap();
        let value = v8::String::new(scope, &url).unwrap();
        meta.set(scope, key.into(), value.into());

        let data = v8::String::new(scope, &referrer).unwrap();
        let template = v8::FunctionTemplate::builder(import_meta_resolve_callback)
            .data(data.into())
            .build(scope);
        if let Some(resolve) = template.get_function(scope) {
            let key = v8::String::new(scope, "resolve").unwrap();
            meta.set(scope, key.into(), resolve.into());
        }
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
    let referrer_name = resolve_dynamic_import_referrer_name(tc, resource_name);
    let module = match resolve_or_compile_module(tc, &specifier_str, &referrer_name) {
        Some(module) => module,
        None => {
            let reason = if let Some(exception) = tc.exception() {
                exception
            } else {
                let msg = v8::String::new(tc, "Cannot dynamically import module").unwrap();
                v8::Exception::error(tc, msg)
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
            v8::Exception::error(tc, msg)
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
        return resolved_promise(tc, namespace);
    }

    let eval_result = match module.evaluate(tc) {
        Some(result) => result,
        None => {
            let reason = if let Some(exception) = tc.exception() {
                exception
            } else {
                let msg =
                    v8::String::new(tc, "Cannot evaluate dynamically imported module").unwrap();
                v8::Exception::error(tc, msg)
            };
            return rejected_promise(tc, reason);
        }
    };

    let namespace = v8::Global::new(tc, module.get_module_namespace());
    let namespace = v8::Local::new(tc, &namespace);
    if eval_result.is_promise() {
        let eval_promise = v8::Local::<v8::Promise>::try_from(eval_result).ok()?;
        let on_fulfilled = v8::FunctionTemplate::builder(dynamic_import_namespace_callback)
            .data(namespace)
            .build(tc)
            .get_function(tc)?;
        let on_rejected = v8::FunctionTemplate::builder(dynamic_import_reject_callback)
            .build(tc)
            .get_function(tc)?;
        return eval_promise.then2(tc, on_fulfilled, on_rejected);
    }

    resolved_promise(tc, namespace)
}

fn resolve_dynamic_import_referrer_name(
    scope: &mut v8::HandleScope,
    resource_name: v8::Local<v8::Value>,
) -> String {
    let candidate = resource_name.to_rust_string_lossy(scope);
    // CommonJS modules execute through a synthetic script whose V8 resource
    // name is the entry placeholder. Dynamic imports made by a nested CJS
    // module must resolve relative to that module, not to the placeholder.
    if candidate != "/<entry>.js"
        && (candidate.starts_with('/') || candidate.starts_with("file://"))
    {
        return candidate;
    }

    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = match v8::String::new(scope, "_currentModule") {
        Some(key) => key,
        None => return candidate,
    };
    let current_module = match global.get(scope, key.into()) {
        Some(value) if value.is_object() => value,
        _ => return candidate,
    };
    let current_module = match v8::Local::<v8::Object>::try_from(current_module) {
        Ok(object) => object,
        Err(_) => return candidate,
    };
    let filename_key = match v8::String::new(scope, "filename") {
        Some(key) => key,
        None => return candidate,
    };
    match current_module.get(scope, filename_key.into()) {
        Some(value) if value.is_string() => value.to_rust_string_lossy(scope),
        _ => candidate,
    }
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

    let response = ctx
        .sync_call_response("_batchResolveModules", args)
        .ok()??;
    if response.payload.len() > MAX_MODULE_BATCH_RESOLVE_RESPONSE_BYTES {
        return None;
    }
    let val = deserialize_v8_value(scope, &response.payload).ok()?;

    // Parse response: array of {resolved, source} or null
    let result_arr = v8::Local::<v8::Array>::try_from(val).ok()?;
    let mut results = Vec::with_capacity(batch.len());
    for i in 0..result_arr.length().min(batch.len() as u32) {
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

    match ctx.sync_call_response("_resolveModule", args) {
        Ok(Some(response)) => match deserialize_v8_value(scope, &response.payload) {
            Ok(val) => {
                if val.is_string() {
                    Some(val.to_rust_string_lossy(scope))
                } else {
                    // A non-string (null) return means the host resolver found no
                    // match — i.e. the module could not be located, NOT a type error.
                    // Name the importer so node_modules layout/discovery problems are
                    // diagnosable (e.g. a bare package installed off the importer's
                    // ancestor chain), since that is the common real cause here.
                    //
                    // Call out the host-mounted node_modules case too: a host_dir
                    // mount (what NodeRuntime `nodeModules` projects) confines reads
                    // to the mount root, so a package symlinked OUT of the mounted
                    // tree (pnpm/yarn workspace or `file:` deps that link to the
                    // workspace root or an external store) cannot be followed and
                    // surfaces here as not-found.
                    throw_module_error(
                        scope,
                        &format!(
                            "Cannot resolve module '{specifier}' (imported from \
                             '{referrer}'): not found. For a bare package, ensure it is \
                             installed in a node_modules directory on an ancestor of the \
                             importer (or bundle the entrypoint). If you mounted a host \
                             node_modules, point it at a directory that contains every \
                             symlink target (e.g. the workspace root): symlinks that \
                             escape the mount root are not followed."
                        ),
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

    let ipc_result = ctx.sync_call_response("_loadFile", args);
    match ipc_result {
        Ok(Some(response)) => match deserialize_v8_value(scope, &response.payload) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolvedModuleFormat {
    Module,
    Commonjs,
    Json,
}

fn lookup_module_format_via_ipc(
    scope: &mut v8::HandleScope,
    ctx: &BridgeCallContext,
    resolved_path: &str,
) -> Option<ResolvedModuleFormat> {
    let path_v8 = v8::String::new(scope, resolved_path).unwrap();
    let arr = v8::Array::new(scope, 1);
    arr.set_index(scope, 0, path_v8.into());
    let args = match serialize_v8_value(scope, arr.into()) {
        Ok(bytes) => bytes,
        Err(e) => {
            throw_module_error(scope, &format!("_moduleFormat serialize error: {}", e));
            return None;
        }
    };

    match ctx.sync_call_response("_moduleFormat", args) {
        Ok(Some(response)) => match deserialize_v8_value(scope, &response.payload) {
            Ok(val) if val.is_string() => match val.to_rust_string_lossy(scope).as_str() {
                "module" => Some(ResolvedModuleFormat::Module),
                "commonjs" => Some(ResolvedModuleFormat::Commonjs),
                "json" => Some(ResolvedModuleFormat::Json),
                _ => None,
            },
            Ok(val) if val.is_null_or_undefined() => None,
            Ok(_) => {
                throw_module_error(
                    scope,
                    &format!("_moduleFormat returned non-string for '{}'", resolved_path),
                );
                None
            }
            Err(e) => {
                throw_module_error(scope, &format!("_moduleFormat decode error: {}", e));
                None
            }
        },
        Ok(None) => None,
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

fn throw_module_error_with_code(scope: &mut v8::HandleScope, message: &str, code: &str) {
    let message = v8::String::new(scope, message).unwrap();
    let exception = v8::Exception::error(scope, message);
    if let Ok(object) = v8::Local::<v8::Object>::try_from(exception) {
        let code_key = v8::String::new(scope, "code").unwrap();
        let code_value = v8::String::new(scope, code).unwrap();
        object.set(scope, code_key.into(), code_value.into());
    }
    scope.throw_exception(exception);
}

/// Detect if source code is likely CommonJS (not ESM).
/// Checks for module.exports, exports.X, or require() patterns without ESM import/export.
/// Node strips a leading shebang (`#!`) line before parsing a module. The guest
/// loader must match, or modules shipped as executables (CLI/SDK bundles that
/// begin with `#!/usr/bin/env node`) fail with "Invalid or unexpected token" on
/// the `#`. The newline is preserved so line numbers in stack traces stay aligned.
fn strip_leading_shebang(source: &str) -> &str {
    match source.strip_prefix("#!") {
        Some(rest) => match rest.find('\n') {
            Some(idx) => &rest[idx..],
            None => "",
        },
        None => source,
    }
}

fn build_module_source(
    scope: &mut v8::HandleScope,
    raw_source: &str,
    resolved_path: &str,
    module_format: Option<ResolvedModuleFormat>,
) -> String {
    let raw_source = strip_leading_shebang(raw_source);
    let normalized_path = resolved_path.to_ascii_lowercase();
    if normalized_path.ends_with(".json") || module_format == Some(ResolvedModuleFormat::Json) {
        return build_json_esm_shim(resolved_path);
    }
    if (module_format == Some(ResolvedModuleFormat::Commonjs)
        && !has_probable_esm_syntax(raw_source))
        || is_likely_cjs(raw_source, resolved_path, module_format)
    {
        return build_cjs_esm_shim(scope, raw_source, resolved_path);
    }
    add_esm_runtime_prelude(raw_source)
}

fn build_json_esm_shim(resolved_path: &str) -> String {
    format!(
        "const _jsonModule = globalThis._requireFrom({}, \"/\");\nexport default _jsonModule;\n",
        quoted_module_path(resolved_path)
    )
}

fn build_cjs_esm_shim(
    scope: &mut v8::HandleScope,
    raw_source: &str,
    resolved_path: &str,
) -> String {
    // Static scanning only sees exports assigned with literal `exports.X =` /
    // `Object.defineProperty(exports, "X", ...)` patterns in this file. It misses names introduced at
    // runtime, e.g. tsc's `__exportStar(require("./sub"), exports)` re-export helper (used by
    // `@sinclair/typebox/compiler` to surface `TypeCompiler`) or `Object.assign(exports, ...)`. When
    // such a dynamic re-export pattern is present the static set is provably incomplete, so fall back
    // to runtime extraction (require the module and enumerate the real `Object.keys(module.exports)`)
    // and union the two. Only do this when static finds nothing or a dynamic re-export is detected:
    // eagerly requiring every CJS module would add avoidable work and trigger side effects earlier
    // than intended (see crates/execution/CLAUDE.md). Static still back-fills names that a
    // partially-evaluated circular require may not have added to the exports object yet.
    let mut names = extract_cjs_export_names(raw_source)
        .into_iter()
        .collect::<HashSet<_>>();
    if names.is_empty() || source_has_dynamic_cjs_reexports(raw_source) {
        names.extend(extract_runtime_cjs_export_names(scope, resolved_path));
    }

    let mut exports = names.into_iter().collect::<Vec<_>>();
    exports.sort();
    exports.truncate(MAX_CJS_NAMED_EXPORTS);

    let mut shim = format!(
        "const _cjsModule = globalThis._requireFrom({}, \"/\");\nexport default _cjsModule;\n",
        quoted_module_path(resolved_path)
    );
    for name in exports {
        shim.push_str(&format!(
            "export const {} = _cjsModule[\"{}\"];\n",
            name, name
        ));
    }
    shim
}

/// Runtime fallback for CJS named export extraction. Evaluates the module via
/// `globalThis._requireFrom` and enumerates `Object.keys(module.exports)` so
/// dynamically computed exports still support named ESM imports. A thread-local
/// in-progress set guards against pathological reentrancy: if shim construction
/// for a path somehow re-enters extraction for the same path, the inner call
/// returns an empty list instead of recursing.
fn extract_runtime_cjs_export_names(
    scope: &mut v8::HandleScope,
    resolved_path: &str,
) -> Vec<String> {
    let already_in_progress = CJS_RUNTIME_EXTRACTION_IN_PROGRESS.with(|cell| {
        let mut in_progress = cell.borrow_mut();
        !in_progress.insert(resolved_path.to_string())
    });
    if already_in_progress {
        return Vec::new();
    }
    let names = extract_runtime_cjs_export_names_inner(scope, resolved_path);
    CJS_RUNTIME_EXTRACTION_IN_PROGRESS.with(|cell| {
        cell.borrow_mut().remove(resolved_path);
    });
    names
}

fn extract_runtime_cjs_export_names_inner(
    scope: &mut v8::HandleScope,
    resolved_path: &str,
) -> Vec<String> {
    let tc = &mut v8::TryCatch::new(scope);
    let context = tc.get_current_context();
    let global = context.global(tc);

    let require_key = match v8::String::new(tc, "_requireFrom") {
        Some(key) => key,
        None => return Vec::new(),
    };
    let require_fn = match global
        .get(tc, require_key.into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        Some(function) => function,
        None => return Vec::new(),
    };

    let module_path = match v8::String::new(tc, resolved_path) {
        Some(path) => path,
        None => return Vec::new(),
    };
    let root = match v8::String::new(tc, "/") {
        Some(path) => path,
        None => return Vec::new(),
    };
    let require_args = [module_path.into(), root.into()];
    let receiver = v8::undefined(tc).into();
    let required_module = match require_fn.call(tc, receiver, &require_args) {
        Some(value) => value,
        None => return Vec::new(),
    };
    if required_module.is_null_or_undefined() || !required_module.is_object() {
        return Vec::new();
    }

    let object_key = match v8::String::new(tc, "Object") {
        Some(key) => key,
        None => return Vec::new(),
    };
    let object_ctor = match global
        .get(tc, object_key.into())
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
    {
        Some(object) => object,
        None => return Vec::new(),
    };

    let keys_key = match v8::String::new(tc, "keys") {
        Some(key) => key,
        None => return Vec::new(),
    };
    let keys_fn = match object_ctor
        .get(tc, keys_key.into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        Some(function) => function,
        None => return Vec::new(),
    };

    let keys_args = [required_module];
    let keys = match keys_fn
        .call(tc, object_ctor.into(), &keys_args)
        .and_then(|value| v8::Local::<v8::Array>::try_from(value).ok())
    {
        Some(array) => array,
        None => return Vec::new(),
    };

    let mut names = Vec::new();
    for index in 0..keys.length() {
        if names.len() >= MAX_CJS_NAMED_EXPORTS {
            break;
        }
        let Some(value) = keys.get_index(tc, index) else {
            continue;
        };
        if !value.is_string() {
            continue;
        }
        let name = value.to_rust_string_lossy(tc);
        if name.len() > MAX_CJS_RUNTIME_EXPORT_NAME_LEN {
            continue;
        }
        if is_valid_js_ident(&name) && name != "default" && name != "__esModule" {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    names
}

fn quoted_module_path(resolved_path: &str) -> String {
    format!(
        "\"{}\"",
        resolved_path.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn is_likely_cjs(
    source: &str,
    resolved_path: &str,
    module_format: Option<ResolvedModuleFormat>,
) -> bool {
    let normalized_path = resolved_path.to_ascii_lowercase();
    if normalized_path.ends_with(".mjs") || normalized_path.ends_with(".mts") {
        return false;
    }
    if normalized_path.ends_with(".cjs") || normalized_path.ends_with(".cts") {
        return true;
    }
    if module_format == Some(ResolvedModuleFormat::Module) {
        return false;
    }
    if has_probable_esm_syntax(source) {
        return false;
    }
    // CJS indicators
    source.contains("module.exports") || source.contains("exports.") || source.contains("require(")
}

fn has_probable_esm_syntax(source: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Code,
        LineComment,
        BlockComment,
        SingleQuote,
        DoubleQuote,
        Template,
    }

    let bytes = source.as_bytes();
    let mut state = ScanState::Code;
    let mut index = 0usize;
    let mut brace_depth = 0u32;
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;

    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();

        match state {
            ScanState::Code => {
                if index == 0 && byte == b'#' && next == Some(b'!') {
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && next == Some(b'/') {
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && next == Some(b'*') {
                    state = ScanState::BlockComment;
                    index += 2;
                    continue;
                }
                if byte == b'\'' {
                    state = ScanState::SingleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'"' {
                    state = ScanState::DoubleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'`' {
                    state = ScanState::Template;
                    index += 1;
                    continue;
                }

                match byte {
                    b'{' => brace_depth = brace_depth.saturating_add(1),
                    b'}' => brace_depth = brace_depth.saturating_sub(1),
                    b'(' => paren_depth = paren_depth.saturating_add(1),
                    b')' => paren_depth = paren_depth.saturating_sub(1),
                    b'[' => bracket_depth = bracket_depth.saturating_add(1),
                    b']' => bracket_depth = bracket_depth.saturating_sub(1),
                    _ => {}
                }

                if brace_depth == 0
                    && paren_depth == 0
                    && bracket_depth == 0
                    && is_js_ident_start(byte)
                {
                    let start = index;
                    index += 1;
                    while index < bytes.len() && is_js_ident_continue(bytes[index]) {
                        index += 1;
                    }

                    let token = &source[start..index];
                    if token == "export" {
                        return true;
                    }
                    if token == "import" {
                        let mut cursor = index;
                        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                            cursor += 1;
                        }
                        if bytes.get(cursor).copied() != Some(b'(') {
                            return true;
                        }
                    }

                    continue;
                }

                index += 1;
            }
            ScanState::LineComment => {
                if byte == b'\n' {
                    state = ScanState::Code;
                }
                index += 1;
            }
            ScanState::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    state = ScanState::Code;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            ScanState::SingleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'\'' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            ScanState::DoubleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'"' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            ScanState::Template => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'`' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
        }
    }

    false
}

fn is_js_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_js_ident_continue(byte: u8) -> bool {
    is_js_ident_start(byte) || byte.is_ascii_digit()
}

/// Extract named export names from CJS source by scanning for `exports.X =` and
/// `module.exports = { X: ... }` patterns. Returns a list of valid JS identifiers.
fn extract_cjs_export_names(source: &str) -> Vec<String> {
    let mut names = HashSet::new();

    collect_cjs_property_assignment_names(source, &mut names);
    collect_cjs_define_property_names(source, &mut names);
    collect_cjs_object_literal_export_names(source, &mut names);

    let mut result: Vec<String> = names.into_iter().collect();
    result.sort();
    result
}

fn collect_cjs_property_assignment_names(
    source: &str,
    names: &mut std::collections::HashSet<String>,
) {
    for prefix in ["exports.", "module.exports."] {
        let mut cursor = 0usize;
        while names.len() < MAX_CJS_NAMED_EXPORTS {
            let Some(start) = find_code_pattern(source, prefix, cursor) else {
                break;
            };
            let name_start = start + prefix.len();
            let mut index = name_start;
            while source
                .as_bytes()
                .get(index)
                .is_some_and(|byte| is_js_ident_continue(*byte))
            {
                index += 1;
            }
            let name = &source[name_start..index];
            let next = skip_ascii_whitespace(source, index);
            if source.as_bytes().get(next) == Some(&b'=')
                && is_valid_js_ident(name)
                && name != "default"
                && name != "__esModule"
            {
                names.insert(name.to_string());
            }
            cursor = index.max(start + prefix.len());
        }
    }
}

fn collect_cjs_define_property_names(source: &str, names: &mut std::collections::HashSet<String>) {
    let mut cursor = 0usize;
    while names.len() < MAX_CJS_NAMED_EXPORTS {
        let Some(start) = find_code_pattern(source, "Object.defineProperty", cursor) else {
            break;
        };
        let mut index = skip_ascii_whitespace(source, start + "Object.defineProperty".len());
        if source.as_bytes().get(index) != Some(&b'(') {
            cursor = start + "Object.defineProperty".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        if !source.as_bytes()[index..].starts_with(b"exports") {
            cursor = start + "Object.defineProperty".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + "exports".len());
        if source.as_bytes().get(index) != Some(&b',') {
            cursor = start + "Object.defineProperty".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        if let Some((name, end)) = parse_quoted_string_literal(source, index) {
            if is_valid_js_ident(name) && name != "default" && name != "__esModule" {
                names.insert(name.to_string());
                cursor = end;
                continue;
            }
        }
        cursor = start + "Object.defineProperty".len();
    }
}

fn collect_cjs_object_literal_export_names(
    source: &str,
    names: &mut std::collections::HashSet<String>,
) {
    collect_module_exports_assignments(source, names);
    collect_object_assign_module_exports(source, names);
}

fn collect_module_exports_assignments(source: &str, names: &mut std::collections::HashSet<String>) {
    let mut cursor = 0usize;
    while names.len() < MAX_CJS_NAMED_EXPORTS {
        let Some(start) = find_code_pattern(source, "module.exports", cursor) else {
            break;
        };
        let mut index = skip_ascii_whitespace(source, start + "module.exports".len());
        if source.as_bytes().get(index) != Some(&b'=') {
            cursor = start + "module.exports".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        cursor = if source.as_bytes().get(index) == Some(&b'{') {
            collect_object_literal_keys(source, index, names)
        } else {
            index.saturating_add(1)
        };
    }
}

fn collect_object_assign_module_exports(
    source: &str,
    names: &mut std::collections::HashSet<String>,
) {
    let mut cursor = 0usize;
    while names.len() < MAX_CJS_NAMED_EXPORTS {
        let Some(start) = find_code_pattern(source, "Object.assign", cursor) else {
            break;
        };
        let mut index = skip_ascii_whitespace(source, start + "Object.assign".len());
        if source.as_bytes().get(index) != Some(&b'(') {
            cursor = start + "Object.assign".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        if !source.as_bytes()[index..].starts_with(b"module.exports") {
            cursor = start + "Object.assign".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + "module.exports".len());
        if source.as_bytes().get(index) != Some(&b',') {
            cursor = start + "Object.assign".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        cursor = if source.as_bytes().get(index) == Some(&b'{') {
            collect_object_literal_keys(source, index, names)
        } else {
            index.saturating_add(1)
        };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CjsScanState {
    Code,
    LineComment,
    BlockComment,
    SingleQuote,
    DoubleQuote,
    Template,
    Regex,
    RegexClass,
}

fn find_code_pattern(source: &str, pattern: &str, cursor: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut state = CjsScanState::Code;
    let mut index = cursor;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();

        match state {
            CjsScanState::Code => {
                if byte == b'/' && next == Some(b'/') {
                    state = CjsScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && next == Some(b'*') {
                    state = CjsScanState::BlockComment;
                    index += 2;
                    continue;
                }
                if byte == b'\'' {
                    state = CjsScanState::SingleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'"' {
                    state = CjsScanState::DoubleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'`' {
                    state = CjsScanState::Template;
                    index += 1;
                    continue;
                }
                if byte == b'/' && slash_starts_regex_literal(source, index) {
                    state = CjsScanState::Regex;
                    index += 1;
                    continue;
                }
                if bytes[index..].starts_with(pattern.as_bytes())
                    && has_code_pattern_boundary(source, index, pattern)
                {
                    return Some(index);
                }
                index += 1;
            }
            CjsScanState::LineComment => {
                if byte == b'\n' {
                    state = CjsScanState::Code;
                }
                index += 1;
            }
            CjsScanState::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    state = CjsScanState::Code;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            CjsScanState::SingleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'\'' {
                    state = CjsScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            CjsScanState::DoubleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'"' {
                    state = CjsScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            CjsScanState::Template => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'`' {
                    state = CjsScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            CjsScanState::Regex => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'[' {
                    state = CjsScanState::RegexClass;
                    index += 1;
                } else if byte == b'/' {
                    state = CjsScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            CjsScanState::RegexClass => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b']' {
                    state = CjsScanState::Regex;
                    index += 1;
                } else {
                    index += 1;
                }
            }
        }
    }
    None
}

fn slash_starts_regex_literal(source: &str, slash_index: usize) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = slash_index;
    while cursor > 0 {
        cursor -= 1;
        if bytes[cursor].is_ascii_whitespace() {
            continue;
        }
        return match bytes[cursor] {
            b'(' | b')' | b'[' | b'{' | b'}' | b':' | b',' | b';' | b'=' | b'!' | b'?' | b'&'
            | b'|' | b'+' | b'-' | b'*' | b'%' | b'^' | b'~' | b'<' => true,
            b'>' => cursor > 0 && bytes[cursor - 1] == b'=',
            byte if is_js_ident_continue(byte) => {
                let end = cursor + 1;
                let mut start = cursor;
                while start > 0 && is_js_ident_continue(bytes[start - 1]) {
                    start -= 1;
                }
                matches!(
                    &source[start..end],
                    "await"
                        | "case"
                        | "delete"
                        | "do"
                        | "else"
                        | "in"
                        | "instanceof"
                        | "of"
                        | "return"
                        | "throw"
                        | "typeof"
                        | "void"
                        | "yield"
                )
            }
            _ => false,
        };
    }
    true
}

fn has_code_pattern_boundary(source: &str, index: usize, pattern: &str) -> bool {
    let bytes = source.as_bytes();
    let before_ok = index == 0
        || bytes
            .get(index - 1)
            .is_none_or(|byte| !is_js_ident_continue(*byte) && *byte != b'.');
    let end = index + pattern.len();
    let after_ok = pattern.ends_with('.')
        || bytes
            .get(end)
            .is_none_or(|byte| !is_js_ident_continue(*byte));
    before_ok && after_ok
}

fn skip_ascii_whitespace(source: &str, mut index: usize) -> usize {
    while source
        .as_bytes()
        .get(index)
        .is_some_and(u8::is_ascii_whitespace)
    {
        index += 1;
    }
    index
}

fn collect_object_literal_keys(
    source: &str,
    open_brace: usize,
    names: &mut std::collections::HashSet<String>,
) -> usize {
    let mut depth = 0usize;
    let mut state = CjsScanState::Code;
    let mut entry_start = open_brace + 1;
    let bytes = source.as_bytes();
    let mut iter = source[open_brace..].char_indices().peekable();
    while let Some((offset, ch)) = iter.next() {
        let index = open_brace + offset;
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();

        match state {
            CjsScanState::Code => {
                if byte == b'/' && next == Some(b'/') {
                    state = CjsScanState::LineComment;
                    continue;
                }
                if byte == b'/' && next == Some(b'*') {
                    state = CjsScanState::BlockComment;
                    continue;
                }
                if byte == b'\'' {
                    state = CjsScanState::SingleQuote;
                    continue;
                }
                if byte == b'"' {
                    state = CjsScanState::DoubleQuote;
                    continue;
                }
                if byte == b'`' {
                    state = CjsScanState::Template;
                    continue;
                }
                if byte == b'/' && slash_starts_regex_literal(source, index) {
                    state = CjsScanState::Regex;
                    continue;
                }
                match ch {
                    '{' | '[' | '(' => depth += 1,
                    '}' | ']' | ')' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 && ch == '}' {
                            collect_object_literal_entry(&source[entry_start..index], names);
                            return index + ch.len_utf8();
                        }
                    }
                    ',' if depth == 1 => {
                        collect_object_literal_entry(&source[entry_start..index], names);
                        if names.len() >= MAX_CJS_NAMED_EXPORTS {
                            return index + ch.len_utf8();
                        }
                        entry_start = index + ch.len_utf8();
                    }
                    _ => {}
                }
            }
            CjsScanState::LineComment => {
                if byte == b'\n' {
                    state = CjsScanState::Code;
                }
            }
            CjsScanState::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    state = CjsScanState::Code;
                    iter.next();
                }
            }
            CjsScanState::SingleQuote => {
                if byte == b'\\' {
                    iter.next();
                } else if byte == b'\'' {
                    state = CjsScanState::Code;
                }
            }
            CjsScanState::DoubleQuote => {
                if byte == b'\\' {
                    iter.next();
                } else if byte == b'"' {
                    state = CjsScanState::Code;
                }
            }
            CjsScanState::Template => {
                if byte == b'\\' {
                    iter.next();
                } else if byte == b'`' {
                    state = CjsScanState::Code;
                }
            }
            CjsScanState::Regex => {
                if byte == b'\\' {
                    iter.next();
                } else if byte == b'[' {
                    state = CjsScanState::RegexClass;
                } else if byte == b'/' {
                    state = CjsScanState::Code;
                }
            }
            CjsScanState::RegexClass => {
                if byte == b'\\' {
                    iter.next();
                } else if byte == b']' {
                    state = CjsScanState::Regex;
                }
            }
        }
    }
    source.len()
}

fn collect_object_literal_entry(entry: &str, names: &mut std::collections::HashSet<String>) {
    let key = entry_key(entry);
    if is_valid_js_ident(key) && key != "default" && key != "__esModule" {
        names.insert(key.to_string());
    }
}

fn entry_key(entry: &str) -> &str {
    let trimmed = entry.trim();
    if let Some((quoted, end)) = parse_quoted_string_literal(trimmed, 0) {
        let next = skip_ascii_whitespace(trimmed, end);
        if trimmed.as_bytes().get(next) == Some(&b':') {
            return quoted;
        }
        return "";
    }
    trimmed
        .find(':')
        .map(|separator| &trimmed[..separator])
        .unwrap_or(trimmed)
        .trim()
}

fn parse_quoted_string_literal(source: &str, index: usize) -> Option<(&str, usize)> {
    let quote = *source.as_bytes().get(index)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let mut cursor = index + 1;
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        if byte == b'\\' {
            cursor = cursor.saturating_add(2);
            continue;
        }
        if byte == quote {
            let value = &source[index + 1..cursor];
            return Some((value, cursor + 1));
        }
        cursor += 1;
    }
    None
}

/// Whether CJS `source` re-exports names through a runtime pattern that static scanning in
/// [`extract_cjs_export_names`] cannot resolve, so the named-export set is provably incomplete
/// without evaluating the module. Covers tsc/tslib's `__exportStar(require("./sub"), exports)`
/// helper (which copies a submodule's enumerable keys onto `exports` at runtime) and
/// bulk exports whose final enumerable keys can depend on runtime values.
fn source_has_dynamic_cjs_reexports(source: &str) -> bool {
    source.contains("__exportStar")
        || source.contains("Object.assign(exports")
        || source.contains("Object.assign(module.exports")
        || source.contains("Object.defineProperties(exports")
        || source.contains("Object.defineProperties(module.exports")
        || source_has_module_exports_object_spread(source)
}

fn source_has_module_exports_object_spread(source: &str) -> bool {
    let mut cursor = 0usize;
    while let Some(start) = find_code_pattern(source, "module.exports", cursor) {
        let mut index = skip_ascii_whitespace(source, start + "module.exports".len());
        if source.as_bytes().get(index) != Some(&b'=') {
            cursor = start + "module.exports".len();
            continue;
        }
        index = skip_ascii_whitespace(source, index + 1);
        if source.as_bytes().get(index) != Some(&b'{') {
            cursor = index.saturating_add(1);
            continue;
        }
        let end = collect_object_literal_keys(source, index, &mut HashSet::new());
        if source[index..end.min(source.len())].contains("...") {
            return true;
        }
        cursor = end;
    }
    false
}

fn add_esm_runtime_prelude(source: &str) -> String {
    let mut prelude = String::new();

    if source.contains("require(")
        && !source.contains("createRequire(import.meta.url)")
        && !source.contains("createRequire(")
        && !source.contains("const require =")
        && !source.contains("let require =")
        && !source.contains("var require =")
        && !source.contains("function require(")
    {
        prelude
            .push_str("const require = globalThis._moduleModule.createRequire(import.meta.url);\n");
    }

    if prelude.is_empty() {
        source.to_owned()
    } else {
        format!("{prelude}{source}")
    }
}

#[cfg(test)]
fn needs_esm_global_alias(source: &str, name: &str, triggers: &[&str]) -> bool {
    if !triggers.iter().any(|trigger| source.contains(trigger)) {
        return false;
    }

    if has_named_import_binding(source, name) {
        return false;
    }

    for pattern in [
        format!("const {name}"),
        format!("let {name}"),
        format!("var {name}"),
        format!("function {name}"),
        format!("class {name}"),
        format!("import {name} from"),
        format!("import * as {name}"),
    ] {
        if source.contains(&pattern) {
            return false;
        }
    }

    true
}

#[cfg(test)]
fn has_named_import_binding(source: &str, name: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ScanState {
        Code,
        LineComment,
        BlockComment,
        SingleQuote,
        DoubleQuote,
        Template,
    }

    let bytes = source.as_bytes();
    let mut state = ScanState::Code;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();

        match state {
            ScanState::Code => {
                if byte == b'/' && next == Some(b'/') {
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                if byte == b'/' && next == Some(b'*') {
                    state = ScanState::BlockComment;
                    index += 2;
                    continue;
                }
                if byte == b'\'' {
                    state = ScanState::SingleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'"' {
                    state = ScanState::DoubleQuote;
                    index += 1;
                    continue;
                }
                if byte == b'`' {
                    state = ScanState::Template;
                    index += 1;
                    continue;
                }
                if !is_js_ident_start(byte) {
                    index += 1;
                    continue;
                }

                let start = index;
                index += 1;
                while index < bytes.len() && is_js_ident_continue(bytes[index]) {
                    index += 1;
                }
                if &source[start..index] != "import" {
                    continue;
                }

                let mut cursor = index;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if bytes.get(cursor).copied() != Some(b'{') {
                    continue;
                }
                cursor += 1;
                let imports_start = cursor;
                while cursor < bytes.len() && bytes[cursor] != b'}' {
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    return false;
                }
                if named_imports_bind_name(&source[imports_start..cursor], name) {
                    return true;
                }
                index = cursor + 1;
            }
            ScanState::LineComment => {
                if byte == b'\n' {
                    state = ScanState::Code;
                }
                index += 1;
            }
            ScanState::BlockComment => {
                if byte == b'*' && next == Some(b'/') {
                    state = ScanState::Code;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            ScanState::SingleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'\'' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            ScanState::DoubleQuote => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'"' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            ScanState::Template => {
                if byte == b'\\' {
                    index += 2;
                } else if byte == b'`' {
                    state = ScanState::Code;
                    index += 1;
                } else {
                    index += 1;
                }
            }
        }
    }
    false
}

#[cfg(test)]
fn named_imports_bind_name(imports: &str, name: &str) -> bool {
    imports.split(',').any(|part| {
        let local = part
            .split_once(" as ")
            .map(|(_, alias)| alias)
            .unwrap_or(part);
        local.trim() == name
    })
}

fn is_valid_js_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if is_js_reserved_word(s) {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_alphabetic() && first != '_' && first != '$' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn is_js_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "arguments"
            | "as"
            | "async"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "eval"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "from"
            | "function"
            | "get"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "of"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "set"
            | "static"
            | "super"
            | "switch"
            | "target"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
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

    #[test]
    fn strip_leading_shebang_matches_node() {
        // Shebang stripped (newline preserved so line numbers hold).
        assert_eq!(
            strip_leading_shebang("#!/usr/bin/env node\nexport const x = 1;\n"),
            "\nexport const x = 1;\n"
        );
        // No shebang -> untouched.
        assert_eq!(
            strip_leading_shebang("export const x = 1;\n"),
            "export const x = 1;\n"
        );
        // `#` not at byte 0 -> untouched (only a leading shebang is special).
        assert_eq!(strip_leading_shebang("  #!nope\n"), "  #!nope\n");
        // Whole file is just a shebang.
        assert_eq!(strip_leading_shebang("#!/usr/bin/env node"), "");
    }

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

    #[test]
    fn esm_global_alias_detection_handles_multiline_named_imports() {
        let source = r#"
import {
  Blob,
  File,
  FormData
} from "fetch-blob/from.js";

export { File };
"#;

        assert!(!needs_esm_global_alias(source, "File", &["File"]));
    }

    #[test]
    fn esm_global_alias_detection_handles_named_import_aliases() {
        let source = r#"
import {
  File as RuntimeFile
} from "fetch-blob/from.js";

export const file = RuntimeFile;
"#;

        assert!(!needs_esm_global_alias(
            source,
            "RuntimeFile",
            &["RuntimeFile"]
        ));
    }

    #[test]
    fn esm_global_alias_detection_ignores_commented_named_imports() {
        let source = r#"
// import { File } from "fetch-blob/from.js";
/*
import {
  Blob,
  File
} from "fetch-blob/from.js";
*/
export function makeFile() {
  return new File([], "empty.txt");
}
"#;

        assert!(needs_esm_global_alias(source, "File", &["new File("]));
    }

    #[test]
    fn esm_global_alias_detection_ignores_string_named_imports() {
        let source = r#"
const example = "import { File } from 'fetch-blob/from.js'";
const singleQuoteExample = 'import { File } from "fetch-blob/from.js"';
const template = `import {
  File
} from "fetch-blob/from.js"`;

export const file = new File([], "empty.txt");
"#;

        assert!(needs_esm_global_alias(source, "File", &["new File("]));
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
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .expect("test process runtime")
                .context();

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
        // Unhandled rejection tracking is bounded within a microtask checkpoint.
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                execute_script(
                    scope,
                    "",
                    "for (let i = 0; i < 1100; i++) Promise.reject(new Error('boom ' + i));",
                    &mut None,
                )
            };
            assert_eq!(code, 1);
            let error = error.expect("unhandled rejection limit error");
            assert_eq!(
                error.code.as_deref(),
                Some("ERR_AGENTOS_UNHANDLED_REJECTION_LIMIT")
            );
            assert!(error
                .message
                .contains("unhandled promise rejection registry exceeded limit"));
        }
        // Over-cap rejections that are handled before the drain should not fail.
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let (code, error) = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                execute_script(
                    scope,
                    "",
                    r#"
                    const promises = [];
                    for (let i = 0; i < 1100; i++) promises.push(Promise.reject(new Error('boom ' + i)));
                    for (const promise of promises) promise.catch(() => {});
                    "#,
                    &mut None,
                )
            };
            assert_eq!(code, 0);
            assert!(
                error.is_none(),
                "handled over-cap rejections should not surface a limit error"
            );
        }

        // --- Part 1: InjectGlobals sets _processConfig and _osConfig ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let mut env = HashMap::new();
            env.insert("HOME".into(), "/home/agentos".into());
            env.insert("PATH".into(), "/usr/bin".into());

            let process_config = ProcessConfig {
                cwd: "/app".into(),
                env,
                timing_mitigation: "none".into(),
                frozen_time_ms: Some(1700000000000.0),
                high_resolution_time: true,
            };
            let os_config = OsConfig {
                homedir: "/home/agentos".into(),
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
                eval(
                    &mut isolate,
                    &context,
                    "_processConfig.high_resolution_time"
                ),
                "true"
            );
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.env.HOME"),
                "/home/agentos"
            );
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.env.PATH"),
                "/usr/bin"
            );

            // Verify _osConfig values
            assert_eq!(
                eval(&mut isolate, &context, "_osConfig.homedir"),
                "/home/agentos"
            );
            assert_eq!(eval(&mut isolate, &context, "_osConfig.tmpdir"), "/tmp");
            assert_eq!(eval(&mut isolate, &context, "_osConfig.platform"), "linux");
            assert_eq!(eval(&mut isolate, &context, "_osConfig.arch"), "x64");
        }

        // --- Part 1a: InjectGlobals payload injection fails closed on invalid payload ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: {
                        cwd: "/app",
                        env: { HOME: "/home/agentos" },
                        timing_mitigation: "none",
                        frozen_time_ms: null
                    }
                })"#,
            );

            let err = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect_err("missing osConfig")
            };

            assert_eq!(err.code.as_deref(), Some("ERR_INVALID_GLOBALS_PAYLOAD"));
            assert!(
                err.message.contains("missing osConfig"),
                "unexpected error message: {}",
                err.message
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _processConfig"),
                "undefined",
                "invalid payload must not partially inject process config"
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _osConfig"),
                "undefined",
                "invalid payload must not inject os config"
            );
        }

        // --- Part 1b: InjectGlobals payload injection rejects primitive configs ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: "not-an-object",
                    osConfig: {
                        homedir: "/home/agentos",
                        tmpdir: "/tmp",
                        platform: "linux",
                        arch: "x64"
                    }
                })"#,
            );

            let err = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect_err("primitive processConfig")
            };

            assert_eq!(err.code.as_deref(), Some("ERR_INVALID_GLOBALS_PAYLOAD"));
            assert!(
                err.message.contains("processConfig is not an object"),
                "unexpected error message: {}",
                err.message
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _processConfig"),
                "undefined",
                "wrong-type payload must not inject primitive process config"
            );
        }

        // --- Part 1c: InjectGlobals payload injection freezes configs and env ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: {
                        cwd: "/app",
                        env: "not-an-object",
                        timing_mitigation: "none",
                        frozen_time_ms: null
                    },
                    osConfig: {
                        homedir: "/home/agentos",
                        tmpdir: "/tmp",
                        platform: "linux",
                        arch: "x64"
                    }
                })"#,
            );

            let err = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect_err("primitive env")
            };

            assert_eq!(err.code.as_deref(), Some("ERR_INVALID_GLOBALS_PAYLOAD"));
            assert!(
                err.message.contains("processConfig.env is not an object"),
                "unexpected error message: {}",
                err.message
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _processConfig"),
                "undefined",
                "wrong-type env payload must not partially inject process config"
            );
        }

        // --- Part 1d: InjectGlobals payload injection rejects missing env ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: {
                        cwd: "/app",
                        timing_mitigation: "none",
                        frozen_time_ms: null
                    },
                    osConfig: {
                        homedir: "/home/agentos",
                        tmpdir: "/tmp",
                        platform: "linux",
                        arch: "x64"
                    }
                })"#,
            );

            let err = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect_err("missing env")
            };

            assert_eq!(err.code.as_deref(), Some("ERR_INVALID_GLOBALS_PAYLOAD"));
            assert!(
                err.message.contains("missing processConfig.env"),
                "unexpected error message: {}",
                err.message
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _processConfig"),
                "undefined",
                "missing env payload must not partially inject process config"
            );
        }

        // --- Part 1e: InjectGlobals payload injection rejects non-plain object env ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: {
                        cwd: "/app",
                        env: new Uint8Array([1]),
                        timing_mitigation: "none",
                        frozen_time_ms: null
                    },
                    osConfig: {
                        homedir: "/home/agentos",
                        tmpdir: "/tmp",
                        platform: "linux",
                        arch: "x64"
                    }
                })"#,
            );

            let err = {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect_err("typed array env")
            };

            assert_eq!(err.code.as_deref(), Some("ERR_INVALID_GLOBALS_PAYLOAD"));
            assert!(
                err.message
                    .contains("processConfig.env is not a plain object"),
                "unexpected error message: {}",
                err.message
            );
            assert_eq!(
                eval(&mut isolate, &context, "typeof _processConfig"),
                "undefined",
                "typed-array env payload must not partially inject process config"
            );
        }

        // --- Part 1f: InjectGlobals payload injection freezes configs and env ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);
            let payload = v8_serialize_eval(
                &mut isolate,
                &context,
                r#"({
                    processConfig: {
                        cwd: "/app",
                        env: { HOME: "/home/agentos" },
                        timing_mitigation: "none",
                        frozen_time_ms: null
                    },
                    osConfig: {
                        homedir: "/home/agentos",
                        tmpdir: "/tmp",
                        platform: "linux",
                        arch: "x64"
                    }
                })"#,
            );

            {
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let ctx = v8::Local::new(scope, &context);
                let scope = &mut v8::ContextScope::new(scope, ctx);
                inject_globals_from_payload(scope, &payload).expect("valid globals payload");
            }

            assert_eq!(eval(&mut isolate, &context, "_processConfig.cwd"), "/app");
            assert_eq!(
                eval(&mut isolate, &context, "_processConfig.env.HOME"),
                "/home/agentos"
            );
            assert!(eval_bool(
                &mut isolate,
                &context,
                "Object.isFrozen(_processConfig) && Object.isFrozen(_processConfig.env) && Object.isFrozen(_osConfig)"
            ));
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
                high_resolution_time: false,
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
                high_resolution_time: false,
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
                high_resolution_time: false,
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
                high_resolution_time: false,
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

        // --- Part 6: Guest WebAssembly compilation stays enabled by default ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            assert!(!eval_throws(
                &mut isolate,
                &context,
                "new WebAssembly.Module(new Uint8Array([0,97,115,109,1,0,0,0]))"
            ));
        }

        // --- Part 7: Guest WebAssembly modules can instantiate and execute ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let result = eval(
                &mut isolate,
                &context,
                r#"
                (function() {
                    var bytes = new Uint8Array([
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
                        0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
                        0x03, 0x02, 0x01, 0x00,
                        0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
                        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
                    ]);
                    var module = new WebAssembly.Module(bytes);
                    var instance = new WebAssembly.Instance(module, {});
                    return String(instance.exports.add(19, 23));
                })()
                "#,
            );
            assert_eq!(result, "42");
        }

        // --- Part 8: V8 still enforces its own WebAssembly memory limits ---
        {
            let mut isolate = isolate::create_isolate(None);
            let context = isolate::create_context(&mut isolate);

            let limit_report = eval(
                &mut isolate,
                &context,
                r#"
                (function() {
                    function capture(fn) {
                        try {
                            fn();
                            return "ALLOWED";
                        } catch (error) {
                            return error.name + ":" + error.message;
                        }
                    }

                    var moduleLimit = capture(function() {
                        var bytes = new Uint8Array([
                            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
                            0x05, 0x06, 0x01, 0x01, 0x01, 0x81, 0x80, 0x04,
                        ]);
                        new WebAssembly.Module(bytes);
                    });
                    var memoryLimit = capture(function() {
                        new WebAssembly.Memory({ initial: 1, maximum: 65537 });
                    });
                    return JSON.stringify({ moduleLimit: moduleLimit, memoryLimit: memoryLimit });
                })()
                "#,
            );

            assert!(
                limit_report.contains(r#""moduleLimit":"CompileError:"#),
                "unexpected module limit report: {limit_report}"
            );
            assert!(
                limit_report.contains(r#""memoryLimit":"RangeError:"#),
                "unexpected memory limit report: {limit_report}"
            );
            assert!(
                limit_report.contains("65536"),
                "unexpected limit report: {limit_report}"
            );
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_sync_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                bridge::resolve_pending_promise(scope, &pending, 1, 0, Some(result_v8), None)
                    .unwrap();
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                    0,
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                bridge::resolve_pending_promise(scope, &pending, 2, 0, Some(r2), None).unwrap();
            }
            assert_eq!(pending.len(), 1);

            let r1 = v8_serialize_str(&mut iso, &ctx, "fetch-result");
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, 0, Some(r1), None).unwrap();
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "var _promise = _asyncFn()");

            // Resolve with None (null result)
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                bridge::resolve_pending_promise(scope, &pending, 1, 0, None, None).unwrap();
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                bridge::resolve_pending_promise(scope, &pending, 1, 0, None, None).unwrap();
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

        // --- Part 25a: ESM completion honors process.exitCode ---
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
                    "globalThis.process = { exitCode: 0 };",
                    "process.exitCode = 5; export const done = true;",
                    None,
                    &mut None,
                )
            };

            assert_eq!(code, 5);
            assert!(exports.is_some());
            assert!(error.is_none());
        }

        // --- Part 25b: ESM root modules receive fetch globals from the runtime prelude ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(Vec::new())),
                "test-session".into(),
            );

            let bridge_code = r#"
                globalThis.fetch = async function () { return "ok"; };
            "#;
            let user_code = r#"
                const result = await fetch();
                export const fetchType = typeof fetch;
                export default result;
            "#;
            let (code, exports, error) = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                execute_module(scope, &bridge_ctx, bridge_code, user_code, None, &mut None)
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

                let fetch_type_key = v8::String::new(scope, "fetchType").unwrap();
                assert_eq!(
                    obj.get(scope, fetch_type_key.into())
                        .unwrap()
                        .to_rust_string_lossy(scope),
                    "function"
                );

                let default_key = v8::String::new(scope, "default").unwrap();
                assert_eq!(
                    obj.get(scope, default_key.into())
                        .unwrap()
                        .to_rust_string_lossy(scope),
                    "ok"
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
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
                },
            )
            .unwrap();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 3,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
                },
            )
            .unwrap();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
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
            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: result_v8,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            // Run event loop
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(
                matches!(completed, crate::session::EventLoopStatus::Completed),
                "event loop should complete normally"
            );
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 2,
                        status: 0,
                        payload: r2,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();
            let r1 = v8_serialize_str(&mut iso, &ctx, "fetch-result");
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r1,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
                    &["_asyncFn"],
                );
            }

            eval(&mut iso, &ctx, "_asyncFn('test')");
            assert_eq!(pending.len(), 1);

            // Send TerminateExecution
            let (tx, rx) = crossbeam_channel::unbounded();
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::TerminateExecution,
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(
                matches!(completed, crate::session::EventLoopStatus::Terminated),
                "event loop should return terminated status on termination"
            );
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(
                matches!(completed, crate::session::EventLoopStatus::Terminated),
                "event loop should return terminated status on shutdown"
            );
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
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "child_stdout".into(),
                        payload: payload_bytes,
                    },
                ),
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None);
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "child_stderr".into(),
                        payload: stderr_payload,
                    },
                ),
            ))
            .unwrap();

            // Send child_exit event with exit code
            let exit_payload = v8_serialize_int(&mut iso, &ctx, 1);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "child_exit".into(),
                        payload: exit_payload,
                    },
                ),
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "http_request".into(),
                        payload: http_payload,
                    },
                ),
            ))
            .unwrap();

            // Resolve the pending promise to exit the event loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "unknown_event".into(),
                        payload,
                    },
                ),
            ))
            .unwrap();

            // Resolve pending promise to exit loop
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "child_stdout".into(),
                        payload,
                    },
                ),
            ))
            .unwrap();

            // Resolve pending promise
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            // Should not crash even without dispatch function registered
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None)
            };

            assert!(matches!(
                completed,
                crate::session::EventLoopStatus::Completed
            ));
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

            let _fn_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _fn_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
                crate::runtime_protocol::SessionMessage::StreamEvent(
                    crate::runtime_protocol::StreamEvent {
                        event_type: "child_stdout".into(),
                        payload,
                    },
                ),
            ))
            .unwrap();

            // Resolve pending promise
            let r = v8_serialize_null(&mut iso, &ctx);
            tx.send(crate::session::SessionCommand::Message(
                crate::runtime_protocol::SessionMessage::BridgeResponse(
                    crate::runtime_protocol::BridgeResponse {
                        call_id: 1,
                        status: 0,
                        payload: r,
                        reservation: None,
                    },
                ),
            ))
            .unwrap();

            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(scope, &rx, &pending, None, None, None);
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
            let ctx = isolate::create_context(&mut iso);

            // Create abort channel for timeout
            let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);

            // Get isolate handle for the timeout guard
            let iso_handle = iso.thread_safe_handle();

            // Start a 50ms timeout
            let mut guard =
                crate::timeout::TimeoutGuard::new(&runtime, None, 50, iso_handle, abort_tx)
                    .expect("timeout guard should start");

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
            let ctx = isolate::create_context(&mut iso);

            let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);
            let iso_handle = iso.thread_safe_handle();

            // 5 second timeout — execution completes well before
            let mut guard =
                crate::timeout::TimeoutGuard::new(&runtime, None, 5000, iso_handle, abort_tx)
                    .expect("timeout guard should start");

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
            let _async_store;
            {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                _async_store = bridge::register_async_bridge_fns(
                    scope,
                    &bridge_ctx as *const BridgeCallContext,
                    &pending as *const bridge::PendingPromises,
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
            let mut guard =
                crate::timeout::TimeoutGuard::new(&runtime, None, 50, iso_handle, abort_tx)
                    .expect("timeout guard should start");

            // Run event loop — it should be terminated by the timeout
            // (no messages on cmd_rx, so it blocks until abort_rx fires)
            let completed = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                crate::session::run_event_loop(
                    scope,
                    &cmd_rx,
                    &pending,
                    Some(&abort_rx),
                    None,
                    None,
                )
            };

            assert!(
                matches!(completed, crate::session::EventLoopStatus::Terminated),
                "event loop should have been terminated"
            );
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

        // --- Part 54: process.exitCode is honored for synchronous completion ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);
            execute_script(
                scope,
                "",
                "globalThis.process = { exitCode: 0 };",
                &mut None,
            );

            let (exit_code, error) = execute_script(scope, "", "process.exitCode = 3;", &mut None);
            assert_eq!(exit_code, 3);
            assert!(error.is_none());
        }

        // --- Part 55: process.exitCode is honored for fulfilled async completion ---
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);
            execute_script(
                scope,
                "",
                "globalThis.process = { exitCode: 0 };",
                &mut None,
            );

            let (exit_code, error) = execute_script(
                scope,
                "",
                "(async () => { process.exitCode = 4; })()",
                &mut None,
            );
            assert_eq!(exit_code, 4);
            assert!(error.is_none());
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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            use crate::bridge::{
                deserialize_v8_wire_value as deserialize_v8_value,
                serialize_v8_wire_value as serialize_v8_value,
            };

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
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
                },
            )
            .unwrap();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 3,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
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
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 4,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
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
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 2,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
                },
            )
            .unwrap();

            // Level 2 batch (call_id=3): ./b.mjs has no further imports
            let batch2 = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/b.mjs', source: 'export const b = 10;'}]",
            );
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 3,
                    status: 0,
                    payload: batch2,
                },
            )
            .unwrap();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 4,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
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

        // Part 68a: Batch prefetch extraction is capped per batch
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let mut source_code = String::new();
            for i in 0..(MAX_MODULE_PREFETCH_BATCH_SIZE + 1) {
                source_code.push_str(&format!("import './dep-{i}.mjs';\n"));
            }
            source_code.push_str("export const ok = true;");

            let resource = v8::String::new(scope, "/app/main.mjs").unwrap();
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
            let source = v8::String::new(scope, &source_code).unwrap();
            let mut compiled = v8::script_compiler::Source::new(source, Some(&origin));
            let module = v8::script_compiler::compile_module(scope, &mut compiled).unwrap();

            MODULE_RESOLVE_STATE.with(|cell| {
                *cell.borrow_mut() = Some(ModuleResolveState {
                    bridge_ctx: std::ptr::null(),
                    module_names: HashMap::new(),
                    module_cache: HashMap::new(),
                    guest_reader: None,
                });
            });
            let imports = extract_uncached_imports(scope, module, "/app/main.mjs");
            assert_eq!(
                imports.len(),
                MAX_MODULE_PREFETCH_BATCH_SIZE,
                "static import extraction should stop at the prefetch batch cap"
            );
            clear_module_state();
        }

        // Part 68b: Module cache insertion refuses to exceed the cache cap
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let resource = v8::String::new(scope, "/overflow.mjs").unwrap();
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
            let source = v8::String::new(scope, "export const value = 1;").unwrap();
            let mut compiled = v8::script_compiler::Source::new(source, Some(&origin));
            let module = v8::script_compiler::compile_module(scope, &mut compiled).unwrap();
            let global = v8::Global::new(scope, module);

            let mut module_cache = HashMap::new();
            for i in 0..(MAX_MODULE_RESOLVE_CACHE_ENTRIES - 1) {
                module_cache.insert(format!("/cached-{i}.mjs"), global.clone());
            }
            MODULE_RESOLVE_STATE.with(|cell| {
                *cell.borrow_mut() = Some(ModuleResolveState {
                    bridge_ctx: std::ptr::null(),
                    module_names: HashMap::new(),
                    module_cache,
                    guest_reader: None,
                });
            });

            assert!(
                !cache_resolved_module(
                    module,
                    global,
                    "/overflow.mjs".into(),
                    Some(module_request_cache_key("./overflow.mjs", "/app/main.mjs")),
                ),
                "cache insert should fail instead of exceeding the cache entry cap"
            );
            let cache_len = MODULE_RESOLVE_STATE.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .expect("module state")
                    .module_cache
                    .len()
            });
            assert_eq!(
                cache_len,
                MAX_MODULE_RESOLVE_CACHE_ENTRIES - 1,
                "failed cache insert must not partially insert entries"
            );
            clear_module_state();
        }

        // Part 68c: Batch resolve response parsing is bounded to request length
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);

            let oversized_response = v8_serialize_eval(
                &mut iso,
                &ctx,
                "[{resolved: '/a.mjs', source: 'export const a = 1;'}, {resolved: '/extra.mjs', source: 'export const extra = 1;'}]",
            );
            let mut response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: oversized_response,
                },
            )
            .unwrap();
            let bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(response_buf)),
                "test-session".into(),
            );

            let results = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                batch_resolve_via_ipc(
                    scope,
                    &bridge_ctx,
                    &[("./a.mjs".to_string(), "/app/main.mjs".to_string())],
                )
                .expect("batch resolve response")
            };
            assert_eq!(
                results.len(),
                1,
                "batch response parser must not retain entries beyond the request length"
            );
            assert_eq!(
                results[0]
                    .as_ref()
                    .map(|(resolved, _source)| resolved.as_str()),
                Some("/a.mjs")
            );

            let mut capped_response_buf = Vec::new();
            crate::ipc_binary::write_frame(
                &mut capped_response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 1,
                    status: 0,
                    payload: vec![0; MAX_MODULE_BATCH_RESOLVE_RESPONSE_BYTES + 1],
                },
            )
            .unwrap();
            let capped_bridge_ctx = BridgeCallContext::new(
                Box::new(Vec::new()),
                Box::new(Cursor::new(capped_response_buf)),
                "test-session".into(),
            );
            let capped_result = {
                let scope = &mut v8::HandleScope::new(&mut iso);
                let local = v8::Local::new(scope, &ctx);
                let scope = &mut v8::ContextScope::new(scope, local);
                batch_resolve_via_ipc(
                    scope,
                    &capped_bridge_ctx,
                    &[("./large.mjs".to_string(), "/app/main.mjs".to_string())],
                )
            };
            assert!(
                capped_result.is_none(),
                "batch response payloads over the byte cap should be rejected before deserialization"
            );
        }

        // Part 68d: CJS named export extraction is capped
        {
            let mut source = String::new();
            for i in 0..(MAX_CJS_NAMED_EXPORTS + 1) {
                source.push_str(&format!("exports.name{i} = {i};\n"));
            }

            let exports = extract_cjs_export_names(&source);
            assert_eq!(
                exports.len(),
                MAX_CJS_NAMED_EXPORTS,
                "static CJS export extraction should stop at the named export cap"
            );
            assert!(
                !exports.contains(&format!("name{}", MAX_CJS_NAMED_EXPORTS)),
                "exports beyond the cap must not be retained"
            );

            let object_literal_exports =
                extract_cjs_export_names("module.exports = { foo: 1, shorthand, default: 2 };");
            assert!(
                object_literal_exports.contains(&"foo".to_string()),
                "module.exports object literal keys should be statically extracted"
            );
            assert!(
                object_literal_exports.contains(&"shorthand".to_string()),
                "module.exports shorthand keys should be statically extracted"
            );
            assert!(
                !object_literal_exports.contains(&"default".to_string()),
                "default should not be emitted as a named CJS export"
            );

            let object_assign_exports =
                extract_cjs_export_names("Object.assign(module.exports, { bar: 1, baz });");
            assert!(
                object_assign_exports.contains(&"bar".to_string())
                    && object_assign_exports.contains(&"baz".to_string()),
                "Object.assign(module.exports, object literal) keys should be extracted"
            );

            let multiline_exports = extract_cjs_export_names(
                r#"
                module.exports = {
                    multiFoo: 1,
                    multiBar,
                };

                Object.assign(module.exports, {
                    multiBaz: 2,
                });
                "#,
            );
            assert!(
                multiline_exports.contains(&"multiFoo".to_string())
                    && multiline_exports.contains(&"multiBar".to_string())
                    && multiline_exports.contains(&"multiBaz".to_string()),
                "multiline CJS object literal export keys should be extracted"
            );

            let false_positive_exports = extract_cjs_export_names(
                r#"
                module.exports.foo = { fakeOne: 1 };
                Object.assign(otherTarget, { fakeTwo: 2 });
                // module.exports = { fakeThree: 3 };
                const text = "Object.assign(module.exports, { fakeFour: 4 })";
                /* exports.fakeFive = 5; */
                const tpl = `Object.defineProperty(exports, "fakeSix", {})`;
                module.exports = { "fake:seven": 7 };
                const re = /module.exports = { fakeEight: 8 }/;
                function f() { return /module.exports = { fakeNine: 9 }/; }
                const g = () => /exports.fakeTen = 10/;
                const h = /[/]module.exports = { fakeEleven: 11 }/;
                if (ok) /exports.fakeTwelve = 12/.test(input);
                if (ok) {} /exports.fakeThirteen = 13/.test(input);
                "#,
            );
            assert!(
                !false_positive_exports.contains(&"fakeOne".to_string())
                    && !false_positive_exports.contains(&"fakeTwo".to_string())
                    && !false_positive_exports.contains(&"fakeThree".to_string())
                    && !false_positive_exports.contains(&"fakeFour".to_string())
                    && !false_positive_exports.contains(&"fakeFive".to_string())
                    && !false_positive_exports.contains(&"fakeSix".to_string())
                    && !false_positive_exports.contains(&"fake".to_string())
                    && !false_positive_exports.contains(&"fakeEight".to_string())
                    && !false_positive_exports.contains(&"fakeNine".to_string())
                    && !false_positive_exports.contains(&"fakeTen".to_string())
                    && !false_positive_exports.contains(&"fakeEleven".to_string())
                    && !false_positive_exports.contains(&"fakeTwelve".to_string())
                    && !false_positive_exports.contains(&"fakeThirteen".to_string()),
                "object literal extraction should not emit keys from unrelated objects"
            );

            let mut malformed_literals = String::new();
            for i in 0..2048 {
                malformed_literals.push_str(&format!("module.exports = {{ fake{i}: "));
            }
            let malformed_exports = extract_cjs_export_names(&malformed_literals);
            assert!(
                malformed_exports.is_empty(),
                "malformed object literals should be skipped without collecting fake keys"
            );

            let regex_value_exports =
                extract_cjs_export_names("module.exports = { real: /}/, alsoReal: /[,]}/ };");
            assert!(
                regex_value_exports.contains(&"real".to_string())
                    && regex_value_exports.contains(&"alsoReal".to_string()),
                "regex values inside CJS object literals should not terminate the object scan"
            );

            let division_exports = extract_cjs_export_names("const n = 4 / 2; exports.after = n;");
            assert!(
                division_exports.contains(&"after".to_string()),
                "ordinary division should not hide later CJS export assignments"
            );

            let reserved_exports = extract_cjs_export_names(
                r#"
                exports.arguments = 1;
                exports.class = 1;
                module.exports = { await: 2 };
                module.exports = { let: 3, static: 4, eval: 5 };
                Object.assign(module.exports, {
                    implements: 6,
                    interface: 7,
                    package: 8,
                    private: 9,
                    protected: 10,
                    public: 11,
                });
                Object.defineProperty(exports, "return", {});
                "#,
            );
            assert!(
                reserved_exports.is_empty(),
                "reserved words should not be emitted as generated ESM bindings"
            );

            let mut huge_literal = String::from("module.exports = {\n");
            for i in 0..(MAX_CJS_NAMED_EXPORTS + 1) {
                huge_literal.push_str(&format!("literalName{i}: {i},\n"));
            }
            huge_literal.push_str("};");
            let huge_literal_exports = extract_cjs_export_names(&huge_literal);
            assert_eq!(
                huge_literal_exports.len(),
                MAX_CJS_NAMED_EXPORTS,
                "object literal export extraction should stop at the named export cap"
            );
            assert!(
                !huge_literal_exports.contains(&format!("literalName{}", MAX_CJS_NAMED_EXPORTS)),
                "object literal exports beyond the cap must not be retained"
            );

            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);
            let shim =
                build_cjs_esm_shim(scope, "module.exports = { foo: 1 };", "/object-literal.cjs");
            assert!(
                shim.contains("export const foo = _cjsModule[\"foo\"];"),
                "CJS shim should preserve statically extractable named exports"
            );
        }

        // Part 68e: CJS shim degrades to default-only when runtime extraction is unavailable
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let shim = build_cjs_esm_shim(
                scope,
                "module.exports = makeExportsDynamically();",
                "/runtime.cjs",
            );

            assert!(
                shim.contains("export default _cjsModule;"),
                "CJS shim should preserve default import support"
            );
            assert!(
                !shim.contains("export const name0"),
                "CJS shim must degrade to default-only when runtime extraction is unavailable"
            );
        }

        // Part 68f: CJS shim runtime fallback enumerates dynamically computed exports
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let setup = v8::String::new(
                scope,
                "globalThis._requireFrom = function (path, referrer) { return { dynamicA: 1, dynamicB: 2, default: 3, __esModule: true }; };",
            )
            .unwrap();
            let script = v8::Script::compile(scope, setup, None).unwrap();
            script.run(scope).unwrap();

            let shim = build_cjs_esm_shim(
                scope,
                "module.exports = makeExportsDynamically();",
                "/dynamic.cjs",
            );

            assert!(
                shim.contains("export const dynamicA = _cjsModule[\"dynamicA\"];"),
                "runtime fallback should surface dynamically computed named exports"
            );
            assert!(
                shim.contains("export const dynamicB = _cjsModule[\"dynamicB\"];"),
                "runtime fallback should surface every dynamically computed named export"
            );
            assert!(
                shim.contains("export default _cjsModule;"),
                "CJS shim should preserve default import support"
            );
            assert!(
                !shim.contains("export const default"),
                "runtime fallback must not emit a named export for default"
            );
            assert!(
                !shim.contains("__esModule"),
                "runtime fallback must not emit a named export for __esModule"
            );
        }

        // Part 68g: CJS shim runtime fallback bounds export count and name length
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let setup = v8::String::new(
                scope,
                "globalThis._requireFrom = function () { const o = {}; for (let i = 0; i < 1025; i++) o[\"k\" + String(i).padStart(4, \"0\")] = i; o[\"x\".repeat(600)] = 1; return o; };",
            )
            .unwrap();
            let script = v8::Script::compile(scope, setup, None).unwrap();
            script.run(scope).unwrap();

            let shim = build_cjs_esm_shim(
                scope,
                "module.exports = makeExportsDynamically();",
                "/bounded.cjs",
            );

            let export_count = shim.matches("export const ").count();
            assert_eq!(
                export_count, MAX_CJS_NAMED_EXPORTS,
                "runtime fallback should stop collecting names at the named export cap"
            );
            assert!(
                !shim.contains("export const k1024"),
                "runtime fallback exports beyond the cap must not be retained"
            );
            let longest_export_name = shim
                .lines()
                .filter_map(|line| line.strip_prefix("export const "))
                .filter_map(|rest| rest.split(' ').next())
                .map(str::len)
                .max()
                .unwrap_or(0);
            assert!(
                longest_export_name <= MAX_CJS_RUNTIME_EXPORT_NAME_LEN,
                "runtime fallback must skip export names longer than the length cap"
            );
        }

        // Part 68h: CJS shim runtime fallback tolerates guest evaluation failure
        {
            let mut iso = isolate::create_isolate(None);
            let ctx = isolate::create_context(&mut iso);
            let scope = &mut v8::HandleScope::new(&mut iso);
            let local = v8::Local::new(scope, &ctx);
            let scope = &mut v8::ContextScope::new(scope, local);

            let setup = v8::String::new(
                scope,
                "globalThis._requireFrom = function () { throw new Error(\"boom\"); };",
            )
            .unwrap();
            let script = v8::Script::compile(scope, setup, None).unwrap();
            script.run(scope).unwrap();

            let shim = build_cjs_esm_shim(
                scope,
                "module.exports = makeExportsDynamically();",
                "/throwing.cjs",
            );

            assert!(
                shim.contains("export default _cjsModule;"),
                "CJS shim should preserve default import support after a guest throw"
            );
            assert!(
                !shim.contains("export const "),
                "runtime fallback should yield no named exports when module evaluation throws"
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
            crate::ipc_binary::write_frame(
                &mut response_buf,
                &crate::ipc_binary::BinaryFrame::BridgeResponse {
                    session_id: String::new(),
                    call_id: 3,
                    status: 0,
                    payload: v8_serialize_str(&mut iso, &ctx, "module"),
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
    }
}
