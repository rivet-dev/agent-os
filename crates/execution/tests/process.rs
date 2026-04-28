use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecutionEngine, PythonExecutionEngine, PythonExecutionEvent,
    StartJavascriptExecutionRequest, StartPythonExecutionRequest, StartWasmExecutionRequest,
    WasmExecutionEngine, WasmPermissionTier,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

fn assert_node_available() {
    let binary = std::env::var("AGENT_OS_NODE_BINARY").unwrap_or_else(|_| String::from("node"));
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(output.status.success(), "node --version failed");
}

fn write_fixture(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fixture");
}

fn write_pyodide_lock_fixture(path: &Path) {
    write_fixture(path, "{\"packages\":[]}\n");
    let pyodide_dir = path.parent().expect("pyodide fixture parent");
    for asset in ["pyodide.asm.js", "pyodide.asm.wasm", "python_stdlib.zip"] {
        let asset_path = pyodide_dir.join(asset);
        if !asset_path.exists() {
            fs::write(&asset_path, []).expect("write pyodide runtime fixture");
        }
    }
}

fn embedded_runtime_process_keeps_host_pid_internal_for_javascript() {
    let temp = tempdir().expect("create temp dir");

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from("globalThis.__agentOsRanInV8 = true;")),
        })
        .expect("start JavaScript execution");

    assert!(execution.uses_shared_v8_runtime());
    assert_eq!(execution.child_pid(), 0);

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
}

fn embedded_runtime_process_keeps_host_pid_internal_for_wasm() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let module_path = temp.path().join("guest.wasm");
    let module = wat::parse_str(
        r#"(module
          (func (export "_start"))
        )"#,
    )
    .expect("compile wasm fixture");
    fs::write(&module_path, module).expect("write wasm fixture");

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(module_path.to_string_lossy().into_owned()),
    });

    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: vec![module_path.to_string_lossy().into_owned()],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::ReadWrite,
        })
        .expect("start wasm execution");

    assert!(execution.uses_shared_v8_runtime());
    assert_eq!(execution.child_pid(), 0);

    let result = execution.wait().expect("wait for wasm execution");
    assert_eq!(
        result.exit_code,
        0,
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn embedded_runtime_process_uses_session_control_for_python_kill() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  options.stdout("ready\n");
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      await new Promise(() => setInterval(() => {}, 1000));
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('hang')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");

    assert!(execution.uses_shared_v8_runtime());
    assert_eq!(execution.child_pid(), 0);

    let mut saw_ready = false;
    while !saw_ready {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
            .expect("poll Python event before kill")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => {
                saw_ready = String::from_utf8(chunk)
                    .expect("stdout utf8")
                    .contains("ready");
            }
            Some(PythonExecutionEvent::Exited(code)) => {
                panic!("execution exited unexpectedly before kill with code {code}");
            }
            Some(PythonExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request during kill test: {request:?}");
            }
            Some(PythonExecutionEvent::JavascriptSyncRpcRequest(request)) => {
                panic!("unexpected JS sync RPC request during kill test: {request:?}");
            }
            None => panic!("timed out waiting for Python execution readiness"),
        }
    }

    execution.kill().expect("kill hanging Python execution");

    let mut exit_code = None;
    while exit_code.is_none() {
        match execution
            .poll_event_blocking(Duration::from_millis(100))
            .expect("poll Python event after kill")
        {
            Some(PythonExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(PythonExecutionEvent::Stdout(_)) | Some(PythonExecutionEvent::Stderr(_)) => {}
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request after kill: {request:?}");
            }
            Some(PythonExecutionEvent::JavascriptSyncRpcRequest(request)) => {
                panic!("unexpected JS sync RPC request after kill: {request:?}");
            }
            None => {}
        }
    }

    assert_eq!(exit_code, Some(1));
}

#[test]
fn embedded_runtime_process_suite() {
    embedded_runtime_process_keeps_host_pid_internal_for_javascript();
    embedded_runtime_process_keeps_host_pid_internal_for_wasm();
    embedded_runtime_process_uses_session_control_for_python_kill();
}
