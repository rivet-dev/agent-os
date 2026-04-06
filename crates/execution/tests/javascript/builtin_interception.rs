use super::*;

#[test]
fn javascript_execution_prewarms_builtin_wrappers_across_contexts() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import pathDefault, {
  basename,
  __agentOsInitCount as pathInit,
} from "agent-os:builtin/path";
import {
  pathToFileURL,
  __agentOsInitCount as urlInit,
} from "agent-os:builtin/url";
import {
  readFile,
  __agentOsInitCount as fsInit,
} from "agent-os:builtin/fs-promises";

console.log(`path:${basename("/tmp/example.txt")}:${pathInit}`);
console.log(`url:${pathToFileURL("/tmp/example.txt").href}:${urlInit}`);
console.log(`fs:${typeof readFile}:${fsInit}`);
console.log(`sep:${pathDefault.sep}`);
"#,
    );

    let mut engine = new_test_engine();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let compile_cache_dir = first_context
        .compile_cache_dir
        .clone()
        .expect("compile cache dir");
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("path:example.txt:1"));
    assert!(first_stdout.contains("url:file:///tmp/example.txt:1"));
    assert!(first_stdout.contains("fs:function:1"));
    assert!(first_stdout.contains("sep:/"));
    assert!(first_warmup.executed);
    assert_eq!(first_warmup.reason, "executed");
    assert_eq!(first_warmup.import_count, 3);

    let cache_files = collect_files(&compile_cache_dir);
    assert!(
        !cache_files.is_empty(),
        "expected compile cache files in {compile_cache_dir:?}"
    );

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("path:example.txt:1"));
    assert!(second_stdout.contains("url:file:///tmp/example.txt:1"));
    assert!(second_stdout.contains("fs:function:1"));
    assert!(second_stdout.contains("sep:/"));
    assert!(!second_warmup.executed);
    assert_eq!(second_warmup.reason, "cached");
}

#[test]
fn javascript_execution_repairs_tampered_polyfill_assets_before_execution() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import pathPolyfill, {
  basename,
  join,
  __agentOsInitCount,
} from "agent-os:polyfill/path";

console.log(
  `polyfill:${basename("/tmp/example.txt")}:${join("/tmp", "example.txt")}:${pathPolyfill.sep}:${__agentOsInitCount}`,
);
"#,
    );

    let mut engine = new_test_engine();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("polyfill:example.txt:/tmp/example.txt:/:1"));
    assert!(first_warmup.executed);

    let tampered_polyfill = PathBuf::from(&first_warmup.asset_root).join("polyfills/path.mjs");
    write_fixture(
        &tampered_polyfill,
        "throw new Error('tampered polyfill');\n",
    );

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("polyfill:example.txt:/tmp/example.txt:/:1"));
    assert!(!second_stderr.contains("tampered polyfill"));
    assert!(!second_warmup.executed);
    assert_eq!(second_warmup.reason, "cached");
}

#[test]
fn javascript_execution_redirects_computed_node_fs_imports_through_builtin_assets() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let guest_mount = temp.path().join("guest-mount");
    fs::create_dir_all(&guest_mount).expect("create guest mount");
    write_fixture(&guest_mount.join("flag.txt"), "mapped\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const fs = await import("node:" + "fs");
const text = fs.readFileSync("/guest/flag.txt", "utf8").trim();
const missing = fs.existsSync("/guest/missing.txt");
console.log(`text:${text}`);
console.log(`missing:${missing}`);
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let guest_mount_host_path = guest_mount.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/guest\",\"hostPath\":\"{guest_mount_host_path}\"}}]"),
    )]);

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env,
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let mut stdout = Vec::new();
    let mut exit_code = None;
    let mut requests = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                requests.push((request.method.clone(), request.args.clone()));
                match request.method.as_str() {
                    "fs.readFileSync" => execution
                        .respond_sync_rpc_success(request.id, json!("mapped\n"))
                        .expect("respond to readFileSync"),
                    "fs.existsSync" => execution
                        .respond_sync_rpc_success(request.id, json!(false))
                        .expect("respond to existsSync"),
                    other => panic!("unexpected sync RPC method: {other}"),
                }
            }
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    assert_eq!(
        requests
            .iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec!["fs.readFileSync", "fs.existsSync"]
    );
    assert_eq!(
        requests[0].1,
        vec![json!("/guest/flag.txt"), json!({"encoding":"utf8"})]
    );
    assert_eq!(requests[1].1, vec![json!("/guest/missing.txt")]);

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(stdout.contains("text:mapped"));
    assert!(stdout.contains("missing:false"));
}

#[test]
fn javascript_execution_imports_tls_builtin_when_allowed() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import tls from "node:tls";

const server = tls.createServer();

console.log(JSON.stringify({
  hasConnect: typeof tls.connect,
  hasCreateServer: typeof tls.createServer,
  serverHasListen: typeof server.listen,
  tlsSocketName: tls.TLSSocket?.name ?? null,
}));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"tls\",\"url\",\"util\",\"zlib\"]",
        ),
    )]);
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env,
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = None;

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                panic!("unexpected tls sync RPC method: {}", request.method)
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse tls JSON");
    assert_eq!(
        parsed["hasConnect"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["hasCreateServer"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["serverHasListen"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["tlsSocketName"],
        Value::String(String::from("TLSSocket"))
    );
}

#[test]
fn javascript_execution_imports_http_builtins_when_allowed() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import http from "node:http";
import http2 from "node:http2";
import https from "node:https";

const builtinHttp = process.getBuiltinModule("node:http");
const builtinHttp2 = process.getBuiltinModule("node:http2");
const builtinHttps = process.getBuiltinModule("node:https");

console.log(JSON.stringify({
  http: {
    request: typeof http.request,
    get: typeof http.get,
    createServer: typeof http.createServer,
    builtinRequest: typeof builtinHttp?.request,
  },
  http2: {
    connect: typeof http2.connect,
    createServer: typeof http2.createServer,
    createSecureServer: typeof http2.createSecureServer,
    builtinConnect: typeof builtinHttp2?.connect,
  },
  https: {
    request: typeof https.request,
    get: typeof https.get,
    createServer: typeof https.createServer,
    builtinRequest: typeof builtinHttps?.request,
  },
}));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"http\",\"http2\",\"https\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
    )]);
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env,
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = None;

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                panic!(
                    "unexpected http builtin sync RPC method: {}",
                    request.method
                )
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse http JSON");
    assert_eq!(
        parsed["http"]["request"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["http"]["get"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["http"]["createServer"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["http2"]["connect"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["http2"]["createSecureServer"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["https"]["request"],
        Value::String(String::from("function"))
    );
    assert_eq!(
        parsed["https"]["createServer"],
        Value::String(String::from("function"))
    );
}
