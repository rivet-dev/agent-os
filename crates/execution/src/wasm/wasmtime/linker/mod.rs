//! Generated AgentOS Preview1 and custom host-import linker.

use super::super::WasmPermissionTier;
use super::memory;
use super::store::WasmtimeStoreState;
use crate::abi::{
    binding, core_signature, AbiBinding, CoreValueType, ImportId, PermissionTier, Restartability,
    ABI_BINDINGS, ALIAS_BINDINGS,
};
use crate::backend::{HostCallReply, HostServiceError};
use crate::host::{HostOperation, SignalMaskHow, SignalOperation, SignalSetValue};
use serde_json::Value;
use wasmtime::{Caller, Engine, FuncType, Linker, Module, Val, ValType};

mod filesystem;
mod network;
mod preview1;
mod process;
mod terminal;
mod user;

const WASI_ERRNO_SUCCESS: i32 = 0;
const WASI_ERRNO_FAULT: i32 = 21;
const WASI_ERRNO_NOSYS: i32 = 52;
const WASI_ERRNO_INTR: i32 = 27;
const SA_RESTART: u32 = 0x1000_0000;
const MAX_SIGNALS_PER_SAFE_POINT: usize = 64;

pub fn build_linker(
    engine: &Engine,
    tier: WasmPermissionTier,
) -> wasmtime::Result<Linker<WasmtimeStoreState>> {
    let mut linker = Linker::new(engine);
    for abi in ABI_BINDINGS {
        if permitted(*abi, tier) {
            link_binding(&mut linker, engine, *abi, abi.module)?;
        }
    }
    for alias in ALIAS_BINDINGS {
        let abi = *binding(alias.import);
        if permitted(abi, tier) && alias.permission_tiers.contains(permission_tier(tier)) {
            link_binding(&mut linker, engine, abi, alias.alias_module)?;
        }
    }
    Ok(linker)
}

/// Reject unsupported or permission-filtered imports before Wasmtime linker
/// diagnostics are involved. This keeps the public outcome independent of an
/// engine's error-string format while the generated ABI registry remains the
/// sole import allowlist.
pub fn validate_module_imports(
    module: &Module,
    tier: WasmPermissionTier,
) -> Result<(), HostServiceError> {
    for import in module.imports() {
        if import_permitted(import.module(), import.name(), tier) {
            continue;
        }
        return Err(HostServiceError::new(
            "ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT",
            format!(
                "unsupported WebAssembly host import {}.{}",
                import.module(),
                import.name()
            ),
        )
        .with_details(serde_json::json!({
            "module": import.module(),
            "name": import.name(),
        })));
    }
    Ok(())
}

fn import_permitted(module: &str, name: &str, tier: WasmPermissionTier) -> bool {
    ABI_BINDINGS
        .iter()
        .any(|abi| abi.module == module && abi.name == name && permitted(*abi, tier))
        || ALIAS_BINDINGS.iter().any(|alias| {
            let abi = *binding(alias.import);
            alias.alias_module == module
                && abi.name == name
                && permitted(abi, tier)
                && alias.permission_tiers.contains(permission_tier(tier))
        })
}

fn link_binding(
    linker: &mut Linker<WasmtimeStoreState>,
    engine: &Engine,
    abi: AbiBinding,
    module: &'static str,
) -> wasmtime::Result<()> {
    let signature = core_signature(abi.signature);
    let ty = FuncType::new(
        engine,
        signature.params.iter().copied().map(value_type),
        signature.results.iter().copied().map(value_type),
    );
    linker.func_new_async(module, abi.name, ty, move |mut caller, params, results| {
        Box::new(async move { dispatch(&mut caller, abi, params, results).await })
    })?;
    Ok(())
}

fn value_type(value: CoreValueType) -> ValType {
    match value {
        CoreValueType::I32 => ValType::I32,
        CoreValueType::I64 => ValType::I64,
    }
}

fn permitted(abi: AbiBinding, tier: WasmPermissionTier) -> bool {
    abi.permission_tiers.contains(permission_tier(tier))
}

fn permission_tier(tier: WasmPermissionTier) -> PermissionTier {
    match tier {
        WasmPermissionTier::Isolated => PermissionTier::Isolated,
        WasmPermissionTier::ReadOnly => PermissionTier::ReadOnly,
        WasmPermissionTier::ReadWrite => PermissionTier::ReadWrite,
        WasmPermissionTier::Full => PermissionTier::Full,
    }
}

async fn dispatch(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    if caller.data().canceled() {
        return Err(wasmtime::format_err!(
            "ERR_AGENTOS_WASMTIME_CANCELED: execution canceled"
        ));
    }
    drain_signal_checkpoints(caller).await?;
    loop {
        dispatch_once(caller, abi, params, results).await?;
        let signals = drain_signal_checkpoints(caller).await?;
        let interrupted = matches!(results, [Val::I32(value)] if *value == WASI_ERRNO_INTR);
        if interrupted
            && abi.restartability == Restartability::SignalRestartable
            && signals.delivered
            && signals.all_restart
        {
            continue;
        }
        return Ok(());
    }
}

async fn dispatch_once(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    abi: AbiBinding,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    if preview1::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    if user::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    if terminal::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    if filesystem::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    if network::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    if process::dispatch(caller, abi, params, results).await? {
        return Ok(());
    }
    match abi.id {
        ImportId::WasiSnapshotPreview1ArgsSizesGet => {
            let count = caller.data().argv.len();
            let bytes = table_bytes(&caller.data().argv)?;
            let count =
                u32::try_from(count).map_err(|_| wasmtime::format_err!("argv count overflow"))?;
            let bytes =
                u32::try_from(bytes).map_err(|_| wasmtime::format_err!("argv bytes overflow"))?;
            let status = if memory::validate_range(caller, i32_arg(params, 0)?, 4).is_err()
                || memory::validate_range(caller, i32_arg(params, 1)?, 4).is_err()
            {
                WASI_ERRNO_FAULT
            } else {
                memory::write_u32(caller, i32_arg(params, 0)?, count).expect("prevalidated argc");
                memory::write_u32(caller, i32_arg(params, 1)?, bytes)
                    .expect("prevalidated argv bytes");
                WASI_ERRNO_SUCCESS
            };
            set_i32_result(results, status)?;
        }
        ImportId::WasiSnapshotPreview1ArgsGet => {
            let values = caller.data().argv.clone();
            let status = if memory::write_string_table(
                caller,
                i32_arg(params, 0)?,
                i32_arg(params, 1)?,
                &values,
            )
            .is_ok()
            {
                WASI_ERRNO_SUCCESS
            } else {
                WASI_ERRNO_FAULT
            };
            set_i32_result(results, status)?;
        }
        ImportId::WasiSnapshotPreview1EnvironSizesGet => {
            let count = caller.data().env.len();
            let bytes = table_bytes(&caller.data().env)?;
            let count =
                u32::try_from(count).map_err(|_| wasmtime::format_err!("env count overflow"))?;
            let bytes =
                u32::try_from(bytes).map_err(|_| wasmtime::format_err!("env bytes overflow"))?;
            let status = if memory::validate_range(caller, i32_arg(params, 0)?, 4).is_err()
                || memory::validate_range(caller, i32_arg(params, 1)?, 4).is_err()
            {
                WASI_ERRNO_FAULT
            } else {
                memory::write_u32(caller, i32_arg(params, 0)?, count)
                    .expect("prevalidated env count");
                memory::write_u32(caller, i32_arg(params, 1)?, bytes)
                    .expect("prevalidated env bytes");
                WASI_ERRNO_SUCCESS
            };
            set_i32_result(results, status)?;
        }
        ImportId::WasiSnapshotPreview1EnvironGet => {
            let values = caller.data().env.clone();
            let status = if memory::write_string_table(
                caller,
                i32_arg(params, 0)?,
                i32_arg(params, 1)?,
                &values,
            )
            .is_ok()
            {
                WASI_ERRNO_SUCCESS
            } else {
                WASI_ERRNO_FAULT
            };
            set_i32_result(results, status)?;
        }
        ImportId::WasiSnapshotPreview1SchedYield => set_i32_result(results, WASI_ERRNO_SUCCESS)?,
        ImportId::WasiSnapshotPreview1ProcExit => {
            let code = i32_arg(params, 0)? as i32;
            caller.data_mut().exit_code = Some(code);
            return Err(wasmtime::format_err!("agentos:wasi-exit:{code}"));
        }
        _ => set_default_result(results)?,
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SignalDispatch {
    delivered: bool,
    all_restart: bool,
}

async fn drain_signal_checkpoints(
    caller: &mut Caller<'_, WasmtimeStoreState>,
) -> wasmtime::Result<SignalDispatch> {
    let mut outcome = SignalDispatch {
        delivered: false,
        all_restart: true,
    };
    for _ in 0..MAX_SIGNALS_PER_SAFE_POINT {
        let host = caller.data().host.clone();
        if !host.signal_pending() {
            return Ok(outcome);
        }
        let reply = host
            .submit(
                HostOperation::Signal(SignalOperation::TakePublishedDelivery),
                0,
            )
            .await
            .map_err(wasmtime_host_error)?;
        let value = host_json(reply, "process.take_signal")?;
        if value.is_null() {
            return Ok(outcome);
        }
        let signal = value
            .get("signal")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| wasmtime::format_err!("process.take_signal omitted a valid signal"))?;
        let token = value
            .get("token")
            .and_then(Value::as_u64)
            .ok_or_else(|| wasmtime::format_err!("process.take_signal omitted a delivery token"))?;
        let flags = value
            .get("flags")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| wasmtime::format_err!("process.take_signal omitted valid flags"))?;
        outcome.delivered = true;
        outcome.all_restart &= flags & SA_RESTART != 0;

        let trampoline = caller
            .get_export("__wasi_signal_trampoline")
            .and_then(|export| export.into_func())
            .ok_or_else(|| {
                wasmtime::format_err!(
                    "ERR_AGENTOS_WASMTIME_SIGNAL_TRAMPOLINE: caught signal has no trampoline"
                )
            })?
            .typed::<i32, ()>(&mut *caller)
            .map_err(|error| {
                wasmtime::format_err!(
                    "ERR_AGENTOS_WASMTIME_SIGNAL_TRAMPOLINE: invalid trampoline type: {error}"
                )
            })?;
        let handler_result = trampoline.call_async(&mut *caller, signal).await;
        let end_result = caller
            .data()
            .host
            .clone()
            .submit(
                HostOperation::Signal(SignalOperation::EndDelivery { token }),
                std::mem::size_of::<u64>(),
            )
            .await;
        if let Err(error) = handler_result {
            if let Err(end_error) = end_result {
                eprintln!(
                    "ERR_AGENTOS_WASMTIME_SIGNAL_SETTLEMENT: handler failed with {error}; token settlement also failed with {end_error}"
                );
            }
            return Err(error);
        }
        end_result.map_err(wasmtime_host_error)?;
    }
    Err(wasmtime::format_err!(
        "ERR_AGENTOS_WASMTIME_SIGNAL_DRAIN_LIMIT: more than {MAX_SIGNALS_PER_SAFE_POINT} signals were delivered at one safe point"
    ))
}

pub async fn initialize_inherited_signal_mask(
    store: &mut wasmtime::Store<WasmtimeStoreState>,
    instance: &wasmtime::Instance,
) -> Result<(), HostServiceError> {
    let reply = store
        .data()
        .host
        .clone()
        .submit(
            HostOperation::Signal(SignalOperation::UpdateMask {
                how: SignalMaskHow::Block,
                set: SignalSetValue::default(),
            }),
            std::mem::size_of::<SignalSetValue>(),
        )
        .await?;
    let value = match reply {
        HostCallReply::Json(value) => value,
        _ => {
            return Err(HostServiceError::new(
                "EIO",
                "process.signal_mask returned a non-JSON reply",
            ));
        }
    };
    let signals = value
        .get("signals")
        .and_then(Value::as_array)
        .ok_or_else(|| HostServiceError::new("EIO", "signal-mask query omitted signals"))?;
    let mut low = 0u32;
    let mut high = 0u32;
    for signal in signals {
        let signal = signal
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| (1..=64).contains(value))
            .ok_or_else(|| {
                HostServiceError::new("EIO", "signal-mask query returned an invalid signal")
            })?;
        if signal <= 32 {
            low |= 1 << (signal - 1);
        } else {
            high |= 1 << (signal - 33);
        }
    }
    if low == 0 && high == 0 {
        return Ok(());
    }
    let setter = instance
        .get_typed_func::<(i32, i32), i32>(&mut *store, "__agentos_set_initial_sigmask")
        .map_err(|error| {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_INITIAL_SIGNAL_MASK: private export diagnostic: {error:#}"
            );
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_INITIAL_SIGNAL_MASK",
                "WebAssembly module cannot initialize its inherited signal mask",
            )
        })?;
    let status = setter
        .call_async(&mut *store, (low as i32, high as i32))
        .await
        .map_err(|error| {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_INITIAL_SIGNAL_MASK: private initialization diagnostic: {error:#}"
            );
            HostServiceError::new(
                "ERR_AGENTOS_WASMTIME_INITIAL_SIGNAL_MASK",
                "inherited signal-mask initialization trapped",
            )
        })?;
    if status != 0 {
        return Err(HostServiceError::new(
            "ERR_AGENTOS_WASMTIME_INITIAL_SIGNAL_MASK",
            format!("inherited signal-mask initialization failed with errno {status}"),
        ));
    }
    Ok(())
}

fn host_json(reply: HostCallReply, method: &str) -> wasmtime::Result<Value> {
    match reply {
        HostCallReply::Json(value) => Ok(value),
        _ => Err(wasmtime::format_err!("{method} returned a non-JSON reply")),
    }
}

fn wasmtime_host_error(error: HostServiceError) -> wasmtime::Error {
    wasmtime::format_err!("{}: {}", error.code, error.message)
}

fn table_bytes(values: &[Vec<u8>]) -> wasmtime::Result<usize> {
    values.iter().try_fold(0usize, |total, value| {
        total
            .checked_add(value.len())
            .ok_or_else(|| wasmtime::format_err!("string table byte count overflow"))
    })
}

fn i32_arg(params: &[Val], index: usize) -> wasmtime::Result<u32> {
    match params.get(index) {
        Some(Val::I32(value)) => Ok(*value as u32),
        _ => Err(wasmtime::format_err!(
            "invalid i32 ABI argument at index {index}"
        )),
    }
}

fn set_i32_result(results: &mut [Val], value: i32) -> wasmtime::Result<()> {
    match results {
        [slot] => {
            *slot = Val::I32(value);
            Ok(())
        }
        _ => Err(wasmtime::format_err!("invalid i32 ABI result shape")),
    }
}

fn set_default_result(results: &mut [Val]) -> wasmtime::Result<()> {
    match results {
        [] => Ok(()),
        [slot @ Val::I32(_)] => {
            *slot = Val::I32(WASI_ERRNO_NOSYS);
            Ok(())
        }
        [slot @ Val::I64(_)] => {
            *slot = Val::I64(-1);
            Ok(())
        }
        _ => Err(wasmtime::format_err!(
            "unsupported AgentOS ABI result shape"
        )),
    }
}

async fn check_fixed_request_limit(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    limit_name: &'static str,
    observed: usize,
    maximum: usize,
    errno: i32,
) -> i32 {
    let warning_at = maximum.saturating_sub(maximum / 5).max(1);
    let publish =
        observed >= warning_at && caller.data_mut().warned_fixed_limits.insert(limit_name);
    if publish {
        let warning = format!(
            "[agentos] WASM request is near {limit_name} ({observed}/{maximum}); split the request if needed\n"
        );
        let host = caller.data().host.clone();
        if let Err(error) = host.publish_stderr(warning.into_bytes()).await {
            eprintln!(
                "ERR_AGENTOS_WASMTIME_LIMIT_WARNING: failed to publish {limit_name} warning: {error}"
            );
        }
    }
    if observed > maximum {
        errno
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::PermissionTier;

    #[test]
    fn linker_permission_filter_matches_generated_registry() {
        for tier in [
            WasmPermissionTier::Isolated,
            WasmPermissionTier::ReadOnly,
            WasmPermissionTier::ReadWrite,
            WasmPermissionTier::Full,
        ] {
            assert_eq!(
                ABI_BINDINGS
                    .iter()
                    .filter(|abi| permitted(**abi, tier))
                    .count(),
                ABI_BINDINGS
                    .iter()
                    .filter(|abi| abi.permission_tiers.contains(permission_tier(tier)))
                    .count()
            );
        }
        assert_eq!(
            permission_tier(WasmPermissionTier::Full),
            PermissionTier::Full
        );
    }

    #[test]
    fn module_import_validation_uses_generated_registry_not_engine_strings() {
        let engine = Engine::default();
        let allowed = ABI_BINDINGS
            .iter()
            .find(|abi| permitted(**abi, WasmPermissionTier::Isolated))
            .expect("isolated import");
        let module = Module::new(
            &engine,
            wat::parse_str(format!(
                "(module (import {:?} {:?} (func)))",
                allowed.module, allowed.name
            ))
            .expect("allowed import module"),
        )
        .expect("compile allowed import module");
        validate_module_imports(&module, WasmPermissionTier::Isolated)
            .expect("generated registry permits import");

        let hostile = Module::new(
            &engine,
            wat::parse_str("(module (import \"ambient_host\" \"escape\" (func)))")
                .expect("hostile import module"),
        )
        .expect("compile hostile import module");
        let error = validate_module_imports(&hostile, WasmPermissionTier::Full)
            .expect_err("unknown import must fail before linker diagnostics");
        assert_eq!(error.code, "ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT");
        assert_eq!(
            error.details.expect("typed import details")["module"],
            "ambient_host"
        );
    }
}
