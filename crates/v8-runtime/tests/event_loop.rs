use agentos_v8_runtime::bridge::PendingPromises;
use agentos_v8_runtime::execution;
use agentos_v8_runtime::isolate;
use agentos_v8_runtime::runtime_protocol::{SessionMessage, StreamEvent};
use agentos_v8_runtime::session::{run_event_loop, EventLoopStatus, SessionCommand};
use crossbeam_channel::Receiver;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WASM_FORTY_TWO_BYTES: &str = "0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,3,2,1,0,7,12,1,8,102,111,114,116,121,84,119,111,0,0,10,6,1,4,0,65,42,11";
const WASM_JSPI_IMPORT_BYTES: &str = "0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,2,19,1,3,101,110,118,11,97,115,121,110,99,95,118,97,108,117,101,0,0,3,2,1,0,7,7,1,3,114,117,110,0,1,10,6,1,4,0,16,0,11";
const EVENT_LOOP_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(6);

// Timing-sensitive assertions flake under the CPU contention of a parallel test
// run (see CLAUDE.md > Testing). Gated off by default; the nightly timing lane
// sets AGENTOS_RUN_TIMING_TESTS=1 to enforce them.
fn run_timing_sensitive_tests() -> bool {
    std::env::var_os("AGENTOS_RUN_TIMING_TESTS").is_some()
}

struct EventLoopWatchdog {
    cancel_tx: Option<crossbeam_channel::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl EventLoopWatchdog {
    fn start() -> (Self, Receiver<()>) {
        let (abort_tx, abort_rx) = crossbeam_channel::bounded::<()>(0);
        let (cancel_tx, cancel_rx) = crossbeam_channel::bounded::<()>(0);
        let join_handle = thread::Builder::new()
            .name("event-loop-test-watchdog".into())
            .spawn(move || {
                crossbeam_channel::select! {
                    recv(cancel_rx) -> _ => {}
                    default(EVENT_LOOP_WATCHDOG_TIMEOUT) => {
                        drop(abort_tx);
                    }
                }
            })
            .expect("watchdog thread should start");

        (
            Self {
                cancel_tx: Some(cancel_tx),
                join_handle: Some(join_handle),
            },
            abort_rx,
        )
    }

    fn cancel(mut self) {
        self.cancel_tx.take();
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("watchdog thread should join");
        }
    }
}

impl Drop for EventLoopWatchdog {
    fn drop(&mut self) {
        self.cancel_tx.take();
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("watchdog thread should join");
        }
    }
}

fn run_event_loop_with_watchdog(
    scope: &mut v8::HandleScope,
    rx: &Receiver<SessionCommand>,
    pending: &PendingPromises,
) -> EventLoopStatus {
    let (watchdog, abort_rx) = EventLoopWatchdog::start();
    let status = run_event_loop(scope, rx, pending, Some(&abort_rx), None, None);
    watchdog.cancel();
    status
}

fn assert_event_loop_watchdog_did_not_fire(status: &EventLoopStatus) {
    assert!(
        !matches!(status, EventLoopStatus::Terminated),
        "event loop watchdog fired after {:?}",
        EVENT_LOOP_WATCHDOG_TIMEOUT
    );
}

fn fresh_isolate_supports_rgi_emoji_unicode_sets() {
    isolate::init_v8_platform();

    let mut isolate = isolate::create_isolate(None);
    let context = isolate::create_context(&mut isolate);
    let scope = &mut v8::HandleScope::new(&mut isolate);
    let context = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, context);
    let source = v8::String::new(
        scope,
        r#"(() => {
          const rgi = new RegExp("^\\p{RGI_Emoji}$", "v");
          const ascii = new RegExp("^[\\p{ASCII}&&\\p{Letter}]+$", "v");
          return rgi.test("👨🏾‍⚕️") &&
            rgi.test("🇧🇪") &&
            !rgi.test("hello") &&
            ascii.test("AgentOS") &&
            !ascii.test("Agent0S");
        })()"#,
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "fresh V8 isolate must support RGI_Emoji and Unicode set expressions"
    );
}

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
         globalThis.__promiseChainDepth = 0; \
         (async () => { \
           await WebAssembly.compile(new Uint8Array([0,97,115,109,1,0,0,0])); \
           let chain = Promise.resolve(); \
           for (let i = 0; i < 256; i++) { \
             chain = chain.then(() => { globalThis.__promiseChainDepth++; }); \
           } \
           await chain; \
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

    let status = run_event_loop_with_watchdog(scope, &rx, &pending);
    assert_event_loop_watchdog_did_not_fire(&status);
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
        "globalThis.__wasmDone === true && globalThis.__promiseChainDepth === 256",
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected native task and recursively queued Promise continuations to resolve"
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

    let status = run_event_loop_with_watchdog(scope, &rx, &pending);
    assert_event_loop_watchdog_did_not_fire(&status);
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

fn event_loop_resumes_jspi_guest_wasm_imports() {
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
        "globalThis.__jspiResult = null; \
         (async () => {{ \
           const module = new WebAssembly.Module(new Uint8Array([{bytes}])); \
           const asyncValue = new WebAssembly.Suspending(async () => {{ \
             await WebAssembly.compile(new Uint8Array([0,97,115,109,1,0,0,0])); \
             return 42; \
           }}); \
           const instance = new WebAssembly.Instance(module, {{ env: {{ async_value: asyncValue }} }}); \
           globalThis.__jspiResult = await WebAssembly.promising(instance.exports.run)(); \
         }})();",
        bytes = WASM_JSPI_IMPORT_BYTES
    );

    let (code, error) = execution::execute_script(scope, "", &source, &mut bridge_cache);
    assert_eq!(code, 0, "unexpected execute_script exit code");
    assert!(
        error.is_none(),
        "unexpected execute_script error: {error:?}"
    );
    assert!(
        execution::has_pending_script_evaluation(),
        "expected pending script evaluation for a JSPI-suspended WASM import"
    );

    let status = run_event_loop_with_watchdog(scope, &rx, &pending);
    assert_event_loop_watchdog_did_not_fire(&status);
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
        "typeof WebAssembly.Suspending === 'function' && \
         typeof WebAssembly.promising === 'function' && \
         globalThis.__jspiResult === 42",
    )
    .unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let result = script.run(scope).unwrap();
    assert!(
        result.boolean_value(scope),
        "expected JSPI to resume WASM after the asynchronous host import"
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

    let status = run_event_loop_with_watchdog(scope, &rx, &pending);
    assert_event_loop_watchdog_did_not_fire(&status);
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
    let status = run_event_loop_with_watchdog(scope, &rx, &pending);
    let elapsed = started.elapsed();

    timer_thread.join().unwrap();

    assert_event_loop_watchdog_did_not_fire(&status);
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
    if run_timing_sensitive_tests() {
        assert!(
            elapsed < Duration::from_secs(5),
            "event loop did not exit promptly after timers drained: {:?}",
            elapsed
        );
    }

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
    fresh_isolate_supports_rgi_emoji_unicode_sets();
    event_loop_pumps_v8_platform_tasks_for_native_wasm_promises();
    event_loop_completes_native_async_wasm_instantiate_promises();
    event_loop_resumes_jspi_guest_wasm_imports();
    event_loop_surfaces_native_async_wasm_compile_errors_without_hanging();
    event_loop_waits_for_refed_guest_timers_between_interval_ticks();
}
