use agentos_executor_node_v8::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, StartJavascriptExecutionRequest,
};
use agentos_runtime_tokio::{RuntimeConfig, SidecarRuntime};
use std::collections::BTreeMap;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn prepared_execution_does_not_enqueue_guest_code_until_started() {
    let runtime = SidecarRuntime::process(&RuntimeConfig::default())
        .expect("construct test process runtime")
        .context();
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::new(runtime);
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-deferred-exec"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .prepare_execution(StartJavascriptExecutionRequest {
            limits: Default::default(),
            argv0: None,
            guest_runtime: Default::default(),
            vm_id: String::from("vm-deferred-exec"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            wasm_module_bytes: None,
            inline_code: Some(String::from("process.stdout.write('started\\n');")),
        })
        .expect("prepare JavaScript execution");

    assert!(execution.is_prepared_for_start());
    assert_eq!(
        execution
            .poll_event_blocking(Duration::ZERO)
            .expect("poll prepared execution"),
        None,
        "preparation must not enqueue any guest code"
    );

    execution
        .start_prepared()
        .expect("start prepared execution");
    assert!(!execution.is_prepared_for_start());
    let result = execution.wait().expect("wait for prepared execution");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, b"started\n");
}
