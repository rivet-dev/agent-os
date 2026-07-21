// Async event dispatch for child process and HTTP server streams

/// Dispatch a stream event into V8 by calling the registered callback function.
///
/// Stream events are sent by the host when async operations (child processes,
/// HTTP servers) produce data. The event_type determines which V8 dispatch
/// function is called:
/// - "child_stdout", "child_stderr", "child_exit" → _childProcessDispatch
/// - "http_request" → _httpServerDispatch
/// - "http2" → _http2Dispatch
/// - "stdin", "stdin_end" → _stdinDispatch
/// - "net_socket" → _netSocketDispatch
/// - "signal" → __agentOsWasmSignalDispatch or _signalDispatch
/// - "timer" → _timerDispatch
pub fn dispatch_stream_event(scope: &mut v8::HandleScope, event_type: &str, payload: &[u8]) {
    // Look up the dispatch function on the global object
    let context = scope.get_current_context();
    let global = context.global(scope);

    let dispatch_names: &[&str] = match event_type {
        "child_stdout" | "child_stderr" | "child_exit" => &["_childProcessDispatch"],
        "http_request" => &["_httpServerDispatch"],
        "http2" => &["_http2Dispatch"],
        "stdin" | "stdin_end" => &["_stdinDispatch"],
        "net_socket" => &["_netSocketDispatch"],
        "signal" => &["__agentOsWasmSignalDispatch", "_signalDispatch"],
        "timer" => &["_timerDispatch"],
        _ => return, // Unknown event type — ignore
    };

    for dispatch_name in dispatch_names {
        let key = v8::String::new(scope, dispatch_name).unwrap();
        let maybe_fn = global.get(scope, key.into());

        if let Some(func_val) = maybe_fn {
            if func_val.is_function() {
                let func = v8::Local::<v8::Function>::try_from(func_val).unwrap();

                // Pass event_type and payload as arguments.
                let event_str = v8::String::new(scope, event_type).unwrap();
                let payload_val = if !payload.is_empty() {
                    let maybe_v8_payload = {
                        let tc = &mut v8::TryCatch::new(scope);
                        crate::bridge::deserialize_v8_value(tc, payload).ok()
                    };
                    match maybe_v8_payload {
                        Some(v) => v,
                        None => match std::str::from_utf8(payload) {
                            Ok(text) => match v8::String::new(scope, text) {
                                Some(json_text) => v8::json::parse(scope, json_text)
                                    .unwrap_or_else(|| json_text.into()),
                                None => v8::null(scope).into(),
                            },
                            Err(_) => v8::null(scope).into(),
                        },
                    }
                } else {
                    v8::null(scope).into()
                };

                let undefined = v8::undefined(scope);
                let args: &[v8::Local<v8::Value>] = &[event_str.into(), payload_val];
                func.call(scope, undefined.into(), args);
                return;
            }
        }
    }
}

pub fn dispatch_signal_event(scope: &mut v8::HandleScope, signal_name: &str, signal: i32) {
    let payload = v8::Object::new(scope);
    let signal_key = v8::String::new(scope, "signal").expect("static V8 string");
    let signal_value = v8::String::new(scope, signal_name).expect("signal V8 string");
    payload.set(scope, signal_key.into(), signal_value.into());
    let number_key = v8::String::new(scope, "number").expect("static V8 string");
    let number_value = v8::Integer::new(scope, signal);
    payload.set(scope, number_key.into(), number_value.into());
    let action_key = v8::String::new(scope, "action").expect("static V8 string");
    let action_value = v8::String::new(scope, "default").expect("static V8 string");
    payload.set(scope, action_key.into(), action_value.into());
    dispatch_stream_value(scope, "signal", payload.into());
}

pub fn dispatch_timer_event(scope: &mut v8::HandleScope, timer_id: u64) {
    let timer_id = v8::Number::new(scope, timer_id as f64);
    dispatch_stream_value(scope, "timer", timer_id.into());
}

fn dispatch_stream_value(
    scope: &mut v8::HandleScope,
    event_type: &str,
    payload: v8::Local<v8::Value>,
) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let dispatch_names: &[&str] = match event_type {
        "signal" => &["__agentOsWasmSignalDispatch", "_signalDispatch"],
        "timer" => &["_timerDispatch"],
        _ => return,
    };
    for dispatch_name in dispatch_names {
        let Some(key) = v8::String::new(scope, dispatch_name) else {
            continue;
        };
        let Some(value) = global.get(scope, key.into()) else {
            continue;
        };
        let Ok(function) = v8::Local::<v8::Function>::try_from(value) else {
            continue;
        };
        let Some(event) = v8::String::new(scope, event_type) else {
            return;
        };
        let undefined = v8::undefined(scope);
        function.call(scope, undefined.into(), &[event.into(), payload]);
        return;
    }
}

/// Notify the guest that one registered sidecar capability has durable work to
/// drain. No socket/signal/timer payload crosses this boundary: the guest uses
/// the identity to issue bounded drain operations against the owning subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessDispatch {
    Delivered,
    TargetMissing,
    BridgeMissing,
}

pub fn dispatch_readiness(
    scope: &mut v8::HandleScope,
    capability_id: u64,
    capability_generation: u64,
    flags: agentos_runtime::readiness::ReadyFlags,
) -> ReadinessDispatch {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let Some(key) = v8::String::new(scope, "_agentOSReadyDispatch") else {
        return ReadinessDispatch::BridgeMissing;
    };
    let Some(value) = global.get(scope, key.into()) else {
        return ReadinessDispatch::BridgeMissing;
    };
    let Ok(function) = v8::Local::<v8::Function>::try_from(value) else {
        return ReadinessDispatch::BridgeMissing;
    };

    let capability_id = v8::BigInt::new_from_u64(scope, capability_id);
    let capability_generation = v8::BigInt::new_from_u64(scope, capability_generation);
    let flags = v8::Integer::new_from_unsigned(scope, u32::from(flags.bits()));
    let undefined = v8::undefined(scope);
    match function.call(
        scope,
        undefined.into(),
        &[
            capability_id.into(),
            capability_generation.into(),
            flags.into(),
        ],
    ) {
        Some(result) if result.is_true() => ReadinessDispatch::Delivered,
        _ => ReadinessDispatch::TargetMissing,
    }
}
