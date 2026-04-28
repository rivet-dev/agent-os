use agent_os_v8_runtime::bridge::PendingPromises;
use agent_os_v8_runtime::execution;
use agent_os_v8_runtime::isolate;
use agent_os_v8_runtime::runtime_protocol::{SessionMessage, StreamEvent};
use agent_os_v8_runtime::session::{run_event_loop, EventLoopStatus, SessionCommand};
use std::thread;
use std::time::{Duration, Instant};

const WASM_FORTY_TWO_BYTES: &str =
    "0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,3,2,1,0,7,12,1,8,102,111,114,116,121,84,119,111,0,0,10,6,1,4,0,65,42,11";

fn event_loop_pumps_v8_platform_tasks_for_native_wasm_promises() {
    isolate::init_v8_platform();

    let mut isolate = isolate::create_isolate(None);
    let context = isolate::create_context(&mut isolate);
    let pending = PendingPromises::new();
    let (_tx, rx) = crossbeam_channel::unbounded::<SessionCommand>();
    let mut bridge_cache = None;

    let scope = &mut v8::HandleScope::new(&mut isolate);
    let ctx = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, ctx);

    let (code, error) = execution::execute_script(
        scope,
        "",
        "globalThis.__wasmDone = false; \
         (async () => { \
           await WebAssembly.compile(new Uint8Array([0,97,115,109,1,0,0,0])); \
           globalThis.__wasmDone = true; \
         })();",
        &mut bridge_cache,
    );
    assert_eq!(code, 0, "unexpected execute_script exit code");
    assert!(
        error.is_none(),
        "unexpected execute_script error: {error:?}"
    );
    assert!(
        execution::has_pending_script_evaluation(),
        "expected pending script evaluation for native wasm promise"
    );

    let status = run_event_loop(scope, &rx, &pending, None, None);
    assert!(
        matches!(status, EventLoopStatus::Completed),
        "unexpected event loop status: {:?}",
        status
    );

    if let Some((next_code, next_error)) = execution::finalize_pending_script_evaluation(scope) {
        assert_eq!(next_code, 0, "unexpected finalize exit code");
        assert!(
            next_error.is_none(),
            "unexpected finalize error: {next_error:?}"
        );
    }

    let source = v8::String::new(scope, "globalThis.__wasmDone === true").unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected wasm promise to resolve"
    );
}

fn event_loop_completes_native_async_wasm_instantiate_promises() {
    isolate::init_v8_platform();

    let mut isolate = isolate::create_isolate(None);
    let context = isolate::create_context(&mut isolate);
    let pending = PendingPromises::new();
    let (_tx, rx) = crossbeam_channel::unbounded::<SessionCommand>();
    let mut bridge_cache = None;

    let scope = &mut v8::HandleScope::new(&mut isolate);
    let ctx = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, ctx);

    let source = format!(
        "globalThis.__wasmInstantiateResult = null; \
         (async () => {{ \
           const bytes = new Uint8Array([{bytes}]); \
           const result = await WebAssembly.instantiate(bytes, {{}}); \
           globalThis.__wasmInstantiateResult = {{ \
             hasModule: !!result?.module, \
             hasInstance: !!result?.instance, \
             value: result.instance.exports.fortyTwo(), \
           }}; \
         }})();",
        bytes = WASM_FORTY_TWO_BYTES
    );

    let (code, error) = execution::execute_script(scope, "", &source, &mut bridge_cache);
    assert_eq!(code, 0, "unexpected execute_script exit code");
    assert!(
        error.is_none(),
        "unexpected execute_script error: {error:?}"
    );
    assert!(
        execution::has_pending_script_evaluation(),
        "expected pending script evaluation for native wasm instantiate promise"
    );

    let status = run_event_loop(scope, &rx, &pending, None, None);
    assert!(
        matches!(status, EventLoopStatus::Completed),
        "unexpected event loop status: {:?}",
        status
    );

    if let Some((next_code, next_error)) = execution::finalize_pending_script_evaluation(scope) {
        assert_eq!(next_code, 0, "unexpected finalize exit code");
        assert!(
            next_error.is_none(),
            "unexpected finalize error: {next_error:?}"
        );
    }

    let source = v8::String::new(
        scope,
        "globalThis.__wasmInstantiateResult?.hasModule === true && \
         globalThis.__wasmInstantiateResult?.hasInstance === true && \
         globalThis.__wasmInstantiateResult?.value === 42",
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected async WebAssembly.instantiate() to resolve with module+instance"
    );
}

fn event_loop_surfaces_native_async_wasm_compile_errors_without_hanging() {
    isolate::init_v8_platform();

    let mut isolate = isolate::create_isolate(None);
    let context = isolate::create_context(&mut isolate);
    let pending = PendingPromises::new();
    let (_tx, rx) = crossbeam_channel::unbounded::<SessionCommand>();
    let mut bridge_cache = None;

    let scope = &mut v8::HandleScope::new(&mut isolate);
    let ctx = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, ctx);

    let (code, error) = execution::execute_script(
        scope,
        "",
        "globalThis.__wasmCompileErrorName = null; \
         (async () => { \
           try { \
             await WebAssembly.instantiate(new Uint8Array([0,97,115,109,1,0,0]), {}); \
           } catch (error) { \
             globalThis.__wasmCompileErrorName = error?.constructor?.name ?? null; \
             throw error; \
           } \
         })();",
        &mut bridge_cache,
    );
    assert_eq!(code, 0, "unexpected execute_script exit code");
    assert!(
        error.is_none(),
        "unexpected execute_script error: {error:?}"
    );
    assert!(
        execution::has_pending_script_evaluation(),
        "expected pending script evaluation for native wasm instantiate rejection"
    );

    let status = run_event_loop(scope, &rx, &pending, None, None);
    assert!(
        matches!(status, EventLoopStatus::Completed),
        "unexpected event loop status: {:?}",
        status
    );

    let (next_code, next_error) = execution::finalize_pending_script_evaluation(scope)
        .expect("expected rejected async wasm instantiate promise");
    assert_eq!(next_code, 1, "unexpected finalize exit code");
    let next_error = next_error.expect("expected compile error");
    assert_eq!(next_error.error_type, "CompileError");

    let source = v8::String::new(
        scope,
        "globalThis.__wasmCompileErrorName === 'CompileError'",
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected async WebAssembly.instantiate() rejection to surface CompileError"
    );
}

fn event_loop_waits_for_refed_guest_timers_between_interval_ticks() {
    isolate::init_v8_platform();

    let mut isolate = isolate::create_isolate(None);
    let context = isolate::create_context(&mut isolate);
    let pending = PendingPromises::new();
    let (tx, rx) = crossbeam_channel::unbounded::<SessionCommand>();
    let mut bridge_cache = None;

    let scope = &mut v8::HandleScope::new(&mut isolate);
    let ctx = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, ctx);

    let (code, error) = execution::execute_script(
        scope,
        "",
        "globalThis.__intervalTicks = 0; \
         globalThis.__pendingTimers = 1; \
         globalThis._getPendingTimerCount = () => globalThis.__pendingTimers; \
         globalThis._timerDispatch = () => { \
           globalThis.__intervalTicks += 1; \
           if (globalThis.__intervalTicks >= 4) { \
             globalThis.__pendingTimers = 0; \
           } \
         };",
        &mut bridge_cache,
    );
    assert_eq!(code, 0, "unexpected execute_script exit code");
    assert!(
        error.is_none(),
        "unexpected execute_script error: {error:?}"
    );

    let timer_thread = thread::spawn(move || {
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(500));
            tx.send(SessionCommand::Message(SessionMessage::StreamEvent(
                StreamEvent {
                    event_type: "timer".into(),
                    payload: Vec::new(),
                },
            )))
            .unwrap();
        }
    });

    let started = Instant::now();
    let status = run_event_loop(scope, &rx, &pending, None, None);
    let elapsed = started.elapsed();

    timer_thread.join().unwrap();

    assert!(
        matches!(status, EventLoopStatus::Completed),
        "unexpected event loop status: {:?}",
        status
    );
    assert!(
        elapsed >= Duration::from_millis(1900),
        "event loop exited before four 500ms timer ticks elapsed: {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "event loop did not exit promptly after timers drained: {:?}",
        elapsed
    );

    let source = v8::String::new(
        scope,
        "globalThis.__intervalTicks === 4 && globalThis.__pendingTimers === 0",
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected timer-backed event loop to stay alive until the fourth tick"
    );
}

#[test]
fn event_loop_handles_native_async_wasm_paths_without_hanging() {
    // Keep the async WASM event-loop coverage inside one top-level libtest case.
    // Splitting these into separate tests in the same binary still trips the
    // V8 init/teardown SIGSEGV boundary that affects other consolidated suites.
    event_loop_pumps_v8_platform_tasks_for_native_wasm_promises();
    event_loop_completes_native_async_wasm_instantiate_promises();
    event_loop_surfaces_native_async_wasm_compile_errors_without_hanging();
    event_loop_waits_for_refed_guest_timers_between_interval_ticks();
}
