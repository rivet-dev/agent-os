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
/// - "timer" → _timerDispatch
pub fn dispatch_stream_event(scope: &mut v8::HandleScope, event_type: &str, payload: &[u8]) {
    // Look up the dispatch function on the global object
    let context = scope.get_current_context();
    let global = context.global(scope);

    let dispatch_name = match event_type {
        "child_stdout" | "child_stderr" | "child_exit" => "_childProcessDispatch",
        "http_request" => "_httpServerDispatch",
        "http2" => "_http2Dispatch",
        "stdin" | "stdin_end" => "_stdinDispatch",
        "timer" => "_timerDispatch",
        _ => return, // Unknown event type — ignore
    };

    let key = v8::String::new(scope, dispatch_name).unwrap();
    let maybe_fn = global.get(scope, key.into());

    if let Some(func_val) = maybe_fn {
        if func_val.is_function() {
            let func = v8::Local::<v8::Function>::try_from(func_val).unwrap();

            // Pass event_type and payload as arguments
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
                                .map(|value| value.into())
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
        }
    }
}
