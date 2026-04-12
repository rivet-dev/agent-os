use agent_os_v8_runtime::bridge::PendingPromises;
use agent_os_v8_runtime::execution;
use agent_os_v8_runtime::isolate;
use agent_os_v8_runtime::session::{run_event_loop, EventLoopStatus, SessionCommand};

#[test]
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
