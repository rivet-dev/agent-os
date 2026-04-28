use agent_os_execution::{
    v8_runtime::map_bridge_method, CreateJavascriptContextRequest, JavascriptExecution,
    JavascriptExecutionEngine, JavascriptExecutionEvent, JavascriptExecutionResult,
    JavascriptSyncRpcRequest, StartJavascriptExecutionRequest,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

/*
US-040 execution-test audit

Deleted coverage:
- `tests/javascript.rs`: removed because the file only exercised the old
  `legacy-js-tests` host-Node guest path (`loader.mjs`, `runner.mjs`,
  import-cache mutation, and `Command::new("node")` process behavior). The V8
  isolate path no longer uses that guest execution model.
- `permission_flags::node_permission_flags_do_not_expose_workspace_root_or_entrypoint_parent_writes`:
  removed because its JavaScript assertions depended on host-Node permission
  flags emitted for guest JS launches. V8 guest JS now stays in-process, while
  the remaining permission-flag tests still cover the real host-Node launches
  that remain for Python and WASM.
- `benchmark::javascript_benchmark_harness_covers_required_startup_and_import_scenarios`:
  removed because it depended on pre-V8 benchmark marker behavior from the old
  startup harness instead of validating the current V8 execution path. The
  stable artifact and markdown benchmark tests remain.
*/

fn write_fixture(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent dirs");
    }
    fs::write(path, contents).expect("write fixture");
}

fn run_host_node_json(cwd: &Path, entrypoint: &Path) -> Value {
    let output = Command::new("node")
        .arg(entrypoint)
        .current_dir(cwd)
        .output()
        .expect("run host node");

    assert!(
        output.status.success(),
        "host node failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("parse host JSON")
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf 'guest-node-invoked\\n' >> \"{}\"\nexit 99\n",
        log_path.display()
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TestJavascriptChildProcessSpawnOptions {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    internal_bootstrap_env: BTreeMap<String, String>,
    #[serde(default)]
    shell: bool,
}

#[derive(Debug, Deserialize)]
struct TestJavascriptChildProcessSpawnRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    options: TestJavascriptChildProcessSpawnOptions,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TestLegacyJavascriptChildProcessSpawnOptions {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    shell: bool,
    #[serde(default, rename = "maxBuffer")]
    max_buffer: Option<usize>,
}

enum HostChildOutputEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StreamClosed,
}

struct HostChildRecord {
    child: Child,
    stdin: Option<ChildStdin>,
    output_events: Receiver<HostChildOutputEvent>,
    pending_events: VecDeque<Value>,
    exit_status: Option<i32>,
    open_streams: usize,
}

#[derive(Default)]
struct HostChildProcessHarness {
    next_child_id: usize,
    children: BTreeMap<String, HostChildRecord>,
}

impl HostChildProcessHarness {
    fn handle_request(
        &mut self,
        host_cwd: &Path,
        request: JavascriptSyncRpcRequest,
    ) -> Result<Value, String> {
        match request.method.as_str() {
            "child_process.spawn" => self.spawn(host_cwd, &request.args),
            "child_process.spawn_sync" => self.spawn_sync(host_cwd, &request.args),
            "child_process.poll" => self.poll(&request.args),
            "child_process.write_stdin" => self.write_stdin(&request.args),
            "child_process.close_stdin" => self.close_stdin(&request.args),
            "child_process.kill" => self.kill(&request.args),
            "fs.writeFileSync" => self.write_file(host_cwd, &request.args),
            other => Err(format!("unsupported sync RPC method: {other}")),
        }
    }

    fn spawn(&mut self, host_cwd: &Path, args: &[Value]) -> Result<Value, String> {
        let request = parse_test_child_process_spawn_request(args)?;

        let child_id = {
            self.next_child_id += 1;
            format!("child-{}", self.next_child_id)
        };

        let mut command = if request.options.shell {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(&request.command);
            command.args(&request.args);
            command
        } else {
            let mut command = Command::new(self.map_guest_path(host_cwd, &request.command));
            command.args(
                request
                    .args
                    .iter()
                    .map(|arg| self.map_guest_path(host_cwd, arg)),
            );
            command
        };

        let child_cwd = request
            .options
            .cwd
            .as_deref()
            .map(|cwd| std::path::PathBuf::from(self.map_guest_path(host_cwd, cwd)))
            .unwrap_or_else(|| host_cwd.to_path_buf());

        command
            .current_dir(child_cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .envs(&request.options.env)
            .envs(&request.options.internal_bootstrap_env);

        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn {} failed: {error}", request.command))?;

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| String::from("spawned child stdout pipe missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| String::from("spawned child stderr pipe missing"))?;
        let (output_sender, output_events) = mpsc::channel();
        spawn_output_reader(stdout, output_sender.clone(), true);
        spawn_output_reader(stderr, output_sender, false);

        let pid = child.id();
        self.children.insert(
            child_id.clone(),
            HostChildRecord {
                child,
                stdin,
                output_events,
                pending_events: VecDeque::new(),
                exit_status: None,
                open_streams: 2,
            },
        );

        Ok(json!({
            "childId": child_id,
            "pid": pid,
            "command": request.command,
            "args": request.args,
        }))
    }

    fn poll(&mut self, args: &[Value]) -> Result<Value, String> {
        let child_id = args
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("child_process.poll missing child id"))?;
        let wait_ms = args.get(1).and_then(Value::as_u64).unwrap_or_default();
        let child = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| format!("unknown child process {child_id}"))?;

        let deadline = std::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            drain_child_output(child);
            if let Some(event) = child.pending_events.pop_front() {
                return Ok(event);
            }

            if child.exit_status.is_none() {
                if let Some(status) = child
                    .child
                    .try_wait()
                    .map_err(|error| format!("try_wait {child_id} failed: {error}"))?
                {
                    child.exit_status = Some(status.code().unwrap_or(1));
                }
            }

            if let Some(exit_code) = child.exit_status {
                if child.pending_events.is_empty() && child.open_streams == 0 {
                    self.children.remove(child_id);
                    return Ok(json!({
                        "type": "exit",
                        "exitCode": exit_code,
                    }));
                }
            }

            if std::time::Instant::now() >= deadline {
                return Ok(Value::Null);
            }

            thread::sleep(Duration::from_millis(5));
        }
    }

    fn spawn_sync(&mut self, host_cwd: &Path, args: &[Value]) -> Result<Value, String> {
        let (request, max_buffer, input) = parse_test_child_process_spawn_sync_request(args)?;
        let mut command = if request.options.shell {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(&request.command);
            command.args(&request.args);
            command
        } else {
            let mut command = Command::new(self.map_guest_path(host_cwd, &request.command));
            command.args(
                request
                    .args
                    .iter()
                    .map(|arg| self.map_guest_path(host_cwd, arg)),
            );
            command
        };

        let child_cwd = request
            .options
            .cwd
            .as_deref()
            .map(|cwd| std::path::PathBuf::from(self.map_guest_path(host_cwd, cwd)))
            .unwrap_or_else(|| host_cwd.to_path_buf());

        command
            .current_dir(child_cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .envs(&request.options.env)
            .envs(&request.options.internal_bootstrap_env);

        let mut child = command
            .spawn()
            .map_err(|error| format!("spawnSync {} failed: {error}", request.command))?;
        if let Some(input) = input.as_deref() {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| String::from("spawnSync child stdin pipe missing"))?;
            stdin
                .write_all(input)
                .map_err(|error| format!("write spawnSync stdin failed: {error}"))?;
        }
        child.stdin.take();

        let output = child
            .wait_with_output()
            .map_err(|error| format!("wait_with_output for {} failed: {error}", request.command))?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let max_buffer = max_buffer.unwrap_or(1024 * 1024);
        let max_buffer_exceeded =
            output.stdout.len() > max_buffer || output.stderr.len() > max_buffer;

        Ok(json!({
            "stdout": stdout,
            "stderr": stderr,
            "code": output.status.code().unwrap_or(1),
            "maxBufferExceeded": max_buffer_exceeded,
        }))
    }

    fn write_stdin(&mut self, args: &[Value]) -> Result<Value, String> {
        let child_id = args
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("child_process.write_stdin missing child id"))?;
        let chunk = decode_guest_bytes(
            args.get(1)
                .ok_or_else(|| String::from("child_process.write_stdin missing chunk"))?,
        )?;
        let child = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| format!("unknown child process {child_id}"))?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(&chunk)
                .map_err(|error| format!("write stdin for {child_id} failed: {error}"))?;
        }
        Ok(Value::Null)
    }

    fn close_stdin(&mut self, args: &[Value]) -> Result<Value, String> {
        let child_id = args
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("child_process.close_stdin missing child id"))?;
        let child = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| format!("unknown child process {child_id}"))?;
        child.stdin.take();
        Ok(Value::Null)
    }

    fn kill(&mut self, args: &[Value]) -> Result<Value, String> {
        let child_id = args
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("child_process.kill missing child id"))?;
        let child = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| format!("unknown child process {child_id}"))?;
        child
            .child
            .kill()
            .map_err(|error| format!("kill {child_id} failed: {error}"))?;
        Ok(Value::Null)
    }

    fn write_file(&mut self, host_cwd: &Path, args: &[Value]) -> Result<Value, String> {
        let path = args
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| String::from("fs.writeFileSync missing path"))?;
        let contents = decode_guest_bytes(
            args.get(1)
                .ok_or_else(|| String::from("fs.writeFileSync missing contents"))?,
        )?;
        let mapped_path = std::path::PathBuf::from(self.map_guest_path(host_cwd, path));
        if let Some(parent) = mapped_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create parent dirs for {} failed: {error}", path))?;
        }
        fs::write(&mapped_path, contents)
            .map_err(|error| format!("write guest file {} failed: {error}", path))?;
        Ok(Value::Null)
    }

    fn map_guest_path(&self, host_cwd: &Path, candidate: &str) -> String {
        if !candidate.starts_with('/') {
            return String::from(candidate);
        }

        for prefix in ["/root", "/workspace"] {
            if candidate == prefix {
                return host_cwd.to_string_lossy().into_owned();
            }
            if let Some(relative) = candidate.strip_prefix(&format!("{prefix}/")) {
                return host_cwd.join(relative).to_string_lossy().into_owned();
            }
        }

        String::from(candidate)
    }
}

impl Drop for HostChildProcessHarness {
    fn drop(&mut self) {
        for child in self.children.values_mut() {
            let _ = child.child.kill();
            let _ = child.child.wait();
        }
    }
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    sender: Sender<HostChildOutputEvent>,
    stdout: bool,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(HostChildOutputEvent::StreamClosed);
                    break;
                }
                Ok(read) => {
                    let event = if stdout {
                        HostChildOutputEvent::Stdout(buffer[..read].to_vec())
                    } else {
                        HostChildOutputEvent::Stderr(buffer[..read].to_vec())
                    };
                    if sender.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = sender.send(HostChildOutputEvent::StreamClosed);
                    break;
                }
            }
        }
    });
}

fn drain_child_output(child: &mut HostChildRecord) {
    loop {
        match child.output_events.try_recv() {
            Ok(HostChildOutputEvent::Stdout(chunk)) => {
                child.pending_events.push_back(json!({
                    "type": "stdout",
                    "data": encode_guest_bytes(&chunk),
                }));
            }
            Ok(HostChildOutputEvent::Stderr(chunk)) => {
                child.pending_events.push_back(json!({
                    "type": "stderr",
                    "data": encode_guest_bytes(&chunk),
                }));
            }
            Ok(HostChildOutputEvent::StreamClosed) => {
                child.open_streams = child.open_streams.saturating_sub(1);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn encode_guest_bytes(bytes: &[u8]) -> Value {
    json!({
        "__agentOsType": "bytes",
        "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn decode_guest_bytes(value: &Value) -> Result<Vec<u8>, String> {
    let encoded = value
        .as_object()
        .ok_or_else(|| String::from("expected bytes payload object"))?;
    let base64 = encoded
        .get("base64")
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("bytes payload missing base64"))?;
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .map_err(|error| format!("invalid base64 bytes payload: {error}"))
}

fn parse_test_child_process_spawn_request(
    args: &[Value],
) -> Result<TestJavascriptChildProcessSpawnRequest, String> {
    if let Some(value) = args.first().cloned() {
        if let Ok(request) = serde_json::from_value::<TestJavascriptChildProcessSpawnRequest>(value)
        {
            return Ok(request);
        }
    }

    let command = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("child_process.spawn missing command"))?;
    let parsed_args = args
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("child_process.spawn missing args payload"))
        .and_then(|value| {
            serde_json::from_str::<Vec<String>>(value)
                .map_err(|error| format!("invalid child_process.spawn args payload: {error}"))
        })?;
    let parsed_options = args
        .get(2)
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("child_process.spawn missing options payload"))
        .and_then(|value| {
            serde_json::from_str::<TestLegacyJavascriptChildProcessSpawnOptions>(value)
                .map_err(|error| format!("invalid child_process.spawn options payload: {error}"))
        })?;

    Ok(TestJavascriptChildProcessSpawnRequest {
        command: String::from(command),
        args: parsed_args,
        options: TestJavascriptChildProcessSpawnOptions {
            cwd: parsed_options.cwd,
            env: parsed_options.env,
            internal_bootstrap_env: BTreeMap::new(),
            shell: parsed_options.shell,
        },
    })
}

fn parse_test_child_process_spawn_sync_request(
    args: &[Value],
) -> Result<
    (
        TestJavascriptChildProcessSpawnRequest,
        Option<usize>,
        Option<Vec<u8>>,
    ),
    String,
> {
    let request = parse_test_child_process_spawn_request(args)?;
    let parsed_options = args
        .get(2)
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("child_process.spawn_sync missing options payload"))
        .and_then(|value| {
            serde_json::from_str::<TestLegacyJavascriptChildProcessSpawnOptions>(value).map_err(
                |error| format!("invalid child_process.spawn_sync options payload: {error}"),
            )
        })?;

    let input = parsed_options
        .input
        .as_ref()
        .map(decode_guest_or_string_bytes)
        .transpose()?;

    Ok((request, parsed_options.max_buffer, input))
}

fn decode_guest_or_string_bytes(value: &Value) -> Result<Vec<u8>, String> {
    match value {
        Value::String(text) => Ok(text.as_bytes().to_vec()),
        other => decode_guest_bytes(other),
    }
}

fn wait_with_host_child_process_bridge(
    mut execution: JavascriptExecution,
    host_cwd: &Path,
) -> JavascriptExecutionResult {
    execution.close_stdin().expect("close JavaScript stdin");
    let mut harness = HostChildProcessHarness::default();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    loop {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
            .expect("poll JavaScript execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                let request_id = request.id;
                match harness.handle_request(host_cwd, request) {
                    Ok(result) => execution
                        .respond_sync_rpc_success(request_id, result)
                        .expect("respond to child_process sync RPC"),
                    Err(message) => execution
                        .respond_sync_rpc_error(request_id, "ERR_TEST_CHILD_PROCESS_RPC", message)
                        .expect("respond to child_process sync RPC error"),
                }
            }
            Some(JavascriptExecutionEvent::Exited(exit_code)) => {
                return JavascriptExecutionResult {
                    execution_id: String::new(),
                    exit_code,
                    stdout,
                    stderr,
                };
            }
            None => panic!("JavaScript execution timed out while awaiting exit"),
        }
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

fn javascript_contexts_preserve_vm_and_bootstrap_configuration() {
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: Some(String::from("./bootstrap.mjs")),
        compile_cache_root: None,
    });

    assert_eq!(context.context_id, "js-ctx-1");
    assert_eq!(context.vm_id, "vm-js");
    assert_eq!(context.bootstrap_module.as_deref(), Some("./bootstrap.mjs"));
    assert_eq!(context.compile_cache_dir, None);
}

fn javascript_execution_uses_v8_runtime_without_spawning_guest_node_binary() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set_path("AGENT_OS_NODE_BINARY", &fake_node_path);

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

    assert!(
        execution.uses_shared_v8_runtime(),
        "guest JS should run inside the shared V8 runtime"
    );
    assert_eq!(
        execution.child_pid(),
        0,
        "shared V8 runtime executions should keep the embedded host pid internal"
    );

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        !log_path.exists(),
        "guest JavaScript execution should not invoke the host node binary"
    );
}

fn javascript_execution_virtualizes_process_metadata_for_inline_v8_code() {
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
            argv: vec![String::from("./entry.mjs"), String::from("alpha")],
            env: BTreeMap::from([
                (
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PID"),
                    String::from("4242"),
                ),
                (
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PPID"),
                    String::from("41"),
                ),
            ]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
if (process.argv[1] !== "/root/entry.mjs") throw new Error(`argv=${process.argv[1]}`);
if (process.argv[2] !== "alpha") throw new Error(`arg2=${process.argv[2]}`);
if (process.cwd() !== "/root") throw new Error(`cwd=${process.cwd()}`);
if (process.pid !== 4242) throw new Error(`pid=${process.pid}`);
if (process.ppid !== 41) throw new Error(`ppid=${process.ppid}`);
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_stream_consumers_text_reads_live_stdin() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { text } from "node:stream/consumers";

const body = await text(process.stdin);
console.log(JSON.stringify({ body }));
"#,
            )),
        })
        .expect("start JavaScript execution");

    execution
        .write_stdin(b"alpha\nbeta\n")
        .expect("write JavaScript stdin");
    execution.close_stdin().expect("close JavaScript stdin");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse guest stdout as JSON");
    assert_eq!(output, json!({ "body": "alpha\nbeta\n" }));
}

fn javascript_execution_process_stdin_async_iterator_finishes_with_live_stdin() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
let body = "";
for await (const chunk of process.stdin) {
  body += chunk;
}
console.log(JSON.stringify({ body }));
"#,
            )),
        })
        .expect("start JavaScript execution");

    execution
        .write_stdin(b"{\"request_id\":\"init1\"}\n")
        .expect("write JavaScript stdin");
    execution.close_stdin().expect("close JavaScript stdin");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse guest stdout as JSON");
    assert_eq!(output, json!({ "body": "{\"request_id\":\"init1\"}\n" }));
}

fn javascript_execution_process_exit_from_live_stdin_listener_exits_without_waiting_for_eof() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
process.stdin.setEncoding("utf8");
process.stdin.once("data", (chunk) => {
  process.stdout.write(`stdout:${chunk}`);
  process.stderr.write(`stderr:${chunk}`);
  process.exit(0);
});
"#,
            )),
        })
        .expect("start JavaScript execution");

    execution
        .write_stdin(b"hello-live-stdin\n")
        .expect("write JavaScript stdin");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit_code = loop {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
            .expect("poll JavaScript execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                panic!("unexpected pending sync RPC request: {}", request.id);
            }
            Some(JavascriptExecutionEvent::Exited(code)) => break code,
            None => panic!("JavaScript execution timed out while awaiting exit"),
        }
    };

    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("stdout:hello-live-stdin"),
        "stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("stderr:hello-live-stdin"),
        "stderr:\n{stderr}"
    );
}

fn javascript_execution_live_stdin_replays_end_after_late_listener_registration() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
setTimeout(() => {
  process.stdin.setEncoding("utf8");
  let body = "";
  process.stdin.on("data", (chunk) => {
    body += chunk;
  });
  process.stdin.on("end", () => {
    console.log(JSON.stringify({ body }));
  });
  process.stdin.resume();
}, 50);
"#,
            )),
        })
        .expect("start JavaScript execution");

    execution
        .write_stdin(b"hello-delayed\n")
        .expect("write JavaScript stdin");
    execution.close_stdin().expect("close JavaScript stdin");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse guest stdout as JSON");
    assert_eq!(output, json!({ "body": "hello-delayed\n" }));
}

fn javascript_execution_file_url_to_path_accepts_guest_absolute_paths() {
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
            inline_code: Some(String::from(
                r#"
import { fileURLToPath } from "node:url";

const guestPath = "/root/node_modules/@mariozechner/pi-coding-agent/dist/config.js";
if (fileURLToPath(guestPath) !== guestPath) {
  throw new Error(`plain path mismatch: ${fileURLToPath(guestPath)}`);
}

const href = "file:///root/node_modules/@mariozechner/pi-coding-agent/dist/config.js";
if (fileURLToPath(href) !== guestPath) {
  throw new Error(`file url mismatch: ${fileURLToPath(href)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_imports_node_events_without_hanging() {
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
            inline_code: Some(String::from(
                r#"
import { EventEmitter, once } from "node:events";

const emitter = new EventEmitter();
const pending = once(emitter, "ready");
emitter.emit("ready", "ok");
const [value] = await pending;

if (value !== "ok") {
  throw new Error(`unexpected once payload: ${value}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_imports_node_process_without_hanging() {
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
            inline_code: Some(String::from(
                r#"
import process from "node:process";

if (!process || typeof process.cwd !== "function") {
  throw new Error("node:process did not export the guest process object");
}

if (typeof process.pid !== "number" || process.pid <= 0) {
  throw new Error(`unexpected pid: ${process.pid}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_imports_node_fs_promises_without_hanging() {
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
            inline_code: Some(String::from(
                r#"
import fs from "node:fs/promises";

if (typeof fs.access !== "function") {
  throw new Error("node:fs/promises did not expose access()");
}
if (typeof fs.readFile !== "function") {
  throw new Error("node:fs/promises did not expose readFile()");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_imports_node_perf_hooks_without_hanging() {
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
            inline_code: Some(String::from(
                r#"
import { performance } from "node:perf_hooks";

if (typeof performance?.now !== "function") {
  throw new Error("node:perf_hooks did not expose performance.now()");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_exposes_compatibility_shims_and_denies_escape_builtins() {
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
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const vm = require("node:vm");
if (typeof vm.runInThisContext !== "function") {
  throw new Error("node:vm compatibility shim missing runInThisContext");
}

const v8 = require("node:v8");
if (typeof v8.cachedDataVersionTag !== "function") {
  throw new Error("node:v8 compatibility shim missing cachedDataVersionTag");
}
const heapStats = v8.getHeapStatistics?.();
if (!heapStats || typeof heapStats.heap_size_limit !== "number" || heapStats.heap_size_limit <= 0) {
  throw new Error("node:v8 compatibility shim missing positive heap_size_limit");
}

const workerThreads = require("node:worker_threads");
if (workerThreads.isMainThread !== true) {
  throw new Error("node:worker_threads compatibility shim missing isMainThread");
}

let workerDenied = false;
try {
  new workerThreads.Worker(new URL("data:text/javascript,0"));
} catch (error) {
  workerDenied = error?.code === "ERR_NOT_IMPLEMENTED";
}
if (!workerDenied) {
  throw new Error("node:worker_threads Worker should stay unavailable");
}

for (const builtin of ["inspector", "cluster"]) {
  let denied = false;
  try {
    require(`node:${builtin}`);
  } catch (error) {
    denied =
      error?.code === "ERR_ACCESS_DENIED" &&
      String(error?.message ?? "").includes(`node:${builtin}`);
  }
  if (!denied) {
    throw new Error(`node:${builtin} was not denied`);
  }
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_provides_async_hooks_and_diagnostics_channel_stubs() {
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
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const asyncHooks = require("node:async_hooks");
const diagnosticsChannel = require("node:diagnostics_channel");

const hook = asyncHooks.createHook({});
if (hook.enable() !== hook || hook.disable() !== hook) {
  throw new Error("node:async_hooks createHook() did not return a no-op hook");
}
if (asyncHooks.executionAsyncId() !== 0 || asyncHooks.triggerAsyncId() !== 0) {
  throw new Error("node:async_hooks ids should default to 0");
}

const storage = new asyncHooks.AsyncLocalStorage();
const result = storage.run("token", () => storage.getStore());
if (result !== "token") {
  throw new Error(`node:async_hooks AsyncLocalStorage lost store: ${String(result)}`);
}

const channel = diagnosticsChannel.channel("undici:request:create");
if (channel.name !== "undici:request:create") {
  throw new Error(`unexpected channel name: ${String(channel.name)}`);
}
if (channel.hasSubscribers !== false) {
  throw new Error("diagnostics channel should report no subscribers");
}
if (diagnosticsChannel.hasSubscribers("undici:request:create") !== false) {
  throw new Error("diagnostics_channel.hasSubscribers should be false");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_supports_require_resolve_for_guest_code() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("local-file.js"),
        "module.exports = 'local';\n",
    );
    write_fixture(
        &temp.path().join("nested/check.cjs"),
        r#"
const localResolved = require.resolve("../local-file.js");
if (localResolved !== "/root/local-file.js") {
  throw new Error(`unexpected local resolution: ${String(localResolved)}`);
}

const packageResolved = require.resolve("some-package");
if (packageResolved !== "/root/node_modules/some-package/index.js") {
  throw new Error(`unexpected package resolution: ${String(packageResolved)}`);
}

const searchPaths = require.resolve.paths("some-package");
const expectedPaths = [
  "/root/nested/node_modules",
  "/root/node_modules",
  "/node_modules",
];
if (JSON.stringify(searchPaths) !== JSON.stringify(expectedPaths)) {
  throw new Error(`unexpected search paths: ${JSON.stringify(searchPaths)}`);
}
"#,
    );
    write_fixture(
        &temp.path().join("node_modules/some-package/package.json"),
        r#"{"main":"./index.js"}"#,
    );
    write_fixture(
        &temp.path().join("node_modules/some-package/index.js"),
        "module.exports = 'pkg';\n",
    );

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
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
if (require.resolve("fs") !== "node:fs") {
  throw new Error(`builtin resolution failed: ${String(require.resolve("fs"))}`);
}

if (require.resolve("./local-file.js") !== "/root/local-file.js") {
  throw new Error(`local resolution failed: ${String(require.resolve("./local-file.js"))}`);
}

if (require.resolve("some-package") !== "/root/node_modules/some-package/index.js") {
  throw new Error(`package resolution failed: ${String(require.resolve("some-package"))}`);
}

const builtinPaths = require.resolve.paths("fs");
if (builtinPaths !== null) {
  throw new Error(`builtin paths should be null, got ${JSON.stringify(builtinPaths)}`);
}

const packagePaths = require.resolve.paths("some-package");
const expectedPackagePaths = ["/root/node_modules", "/node_modules"];
if (JSON.stringify(packagePaths) !== JSON.stringify(expectedPackagePaths)) {
  throw new Error(`unexpected top-level search paths: ${JSON.stringify(packagePaths)}`);
}

let missingCode = null;
try {
  require.resolve("nonexistent");
} catch (error) {
  missingCode = error?.code ?? null;
}
if (missingCode !== "MODULE_NOT_FOUND") {
  throw new Error(`unexpected missing-module code: ${String(missingCode)}`);
}

require("./nested/check.cjs");
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

fn javascript_execution_surfaces_sync_rpc_requests_from_v8_modules() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import fs from "node:fs";
fs.statSync("/workspace/note.txt");
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/note.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 11,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
}

fn javascript_execution_v8_dgram_bridge_matches_sidecar_rpc_shapes() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import dgram from "node:dgram";

const summary = await new Promise((resolve, reject) => {
  const socket = dgram.createSocket("udp4");
  socket.on("error", reject);
  socket.on("message", (message, rinfo) => {
    const address = socket.address();
    socket.close(() => {
      resolve({
        address,
        message: message.toString("utf8"),
        rinfo,
      });
    });
  });
  socket.bind(0, "127.0.0.1", () => {
    socket.send("ping", 7, "127.0.0.1");
  });
});

if (summary.message !== "pong") {
  throw new Error(`unexpected udp message: ${summary.message}`);
}
if (summary.address.address !== "127.0.0.1" || summary.address.port !== 45454) {
  throw new Error(`unexpected socket address: ${JSON.stringify(summary.address)}`);
}
if (summary.rinfo.address !== "127.0.0.1" || summary.rinfo.port !== 7) {
  throw new Error(`unexpected remote info: ${JSON.stringify(summary.rinfo)}`);
}
"#,
    );
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                String::from("[\"dgram\"]"),
            )]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.createSocket request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.createSocket request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.createSocket");
    assert_eq!(request.args, vec![json!({ "type": "udp4" })]);
    execution
        .respond_sync_rpc_success(request.id, json!({ "socketId": "udp-1", "type": "udp4" }))
        .expect("respond to dgram.createSocket");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.bind request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.bind request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.bind");
    assert_eq!(
        request.args,
        vec![json!("udp-1"), json!({ "address": "127.0.0.1", "port": 0 })]
    );
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "localAddress": "127.0.0.1",
                "localPort": 45454,
                "family": "IPv4",
            }),
        )
        .expect("respond to dgram.bind");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.poll request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.poll request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.poll");
    assert_eq!(request.args, vec![json!("udp-1"), json!(10)]);
    execution
        .respond_sync_rpc_success(request.id, json!(null))
        .expect("respond to initial dgram.poll");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.send request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.send request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.send");
    assert_eq!(
        request.args,
        vec![
            json!("udp-1"),
            json!({
                "__agentOsType": "bytes",
                "base64": "cGluZw==",
            }),
            json!({
                "address": "127.0.0.1",
                "port": 7,
            }),
        ]
    );
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "bytes": 4,
                "localAddress": "127.0.0.1",
                "localPort": 45454,
                "family": "IPv4",
            }),
        )
        .expect("respond to dgram.send");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll message dgram.poll request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected message dgram.poll request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.poll");
    assert_eq!(request.args, vec![json!("udp-1"), json!(10)]);
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "type": "message",
                "data": {
                    "__agentOsType": "bytes",
                    "base64": "cG9uZw==",
                },
                "remoteAddress": "127.0.0.1",
                "remotePort": 7,
                "remoteFamily": "IPv4",
            }),
        )
        .expect("respond to message dgram.poll");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.address request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.address request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.address");
    assert_eq!(request.args, vec![json!("udp-1")]);
    execution
        .respond_sync_rpc_success(
            request.id,
            json!("{\"address\":\"127.0.0.1\",\"port\":45454,\"family\":\"IPv4\"}"),
        )
        .expect("respond to dgram.address");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.close request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.close request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.close");
    assert_eq!(request.args, vec![json!("udp-1")]);
    execution
        .respond_sync_rpc_success(request.id, json!(null))
        .expect("respond to dgram.close");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_strips_hashbang_from_module_entrypoints() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &temp.path().join("index.js"),
        "#!/usr/bin/env node\nimport fs from \"node:fs\";\nfs.statSync(\"/workspace/hashbang.txt\");\n",
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./index.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/hashbang.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 9,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_resolves_pnpm_store_dependencies_from_symlinked_entrypoints() {
    let temp = tempdir().expect("create temp dir");
    let node_modules = temp.path().join("node_modules");
    let store_root = node_modules.join(".pnpm/pkg@1.0.0/node_modules");
    let pkg_dir = store_root.join("pkg");
    let dep_dir = store_root.join("@scope/dep");

    fs::create_dir_all(pkg_dir.join("dist")).expect("create package dist");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(node_modules.join("@scope")).expect("create scope dir");

    write_fixture(&pkg_dir.join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &pkg_dir.join("dist/index.js"),
        "import dep from \"@scope/dep\";\ndep();\n",
    );
    write_fixture(
        &dep_dir.join("package.json"),
        r#"{"type":"module","exports":"./index.js"}"#,
    );
    write_fixture(
        &dep_dir.join("index.js"),
        "import fs from \"node:fs\";\nexport default function dep() { fs.statSync(\"/workspace/pnpm.txt\"); }\n",
    );

    symlink(".pnpm/pkg@1.0.0/node_modules/pkg", node_modules.join("pkg"))
        .expect("symlink package into node_modules");

    let guest_mappings = serde_json::to_string(&vec![json!({
        "guestPath": "/root/node_modules",
        "hostPath": node_modules.display().to_string(),
    })])
    .expect("serialize guest mappings");

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("/root/node_modules/pkg/dist/index.js")],
            env: BTreeMap::from([(String::from("AGENT_OS_GUEST_PATH_MAPPINGS"), guest_mappings)]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/pnpm.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 8,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_resolves_dependencies_from_package_specific_symlink_mounts() {
    let temp = tempdir().expect("create temp dir");
    let mounts_root = temp.path().join("mounts");
    let node_modules_root = temp.path().join("node_modules");
    let store_root = node_modules_root.join(".pnpm/pkg@1.0.0/node_modules");
    let pkg_dir = store_root.join("pkg");
    let dep_dir = store_root.join("@scope/dep");
    let mounted_pkg = mounts_root.join("pkg");

    fs::create_dir_all(pkg_dir.join("dist")).expect("create package dist");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(&mounts_root).expect("create mounts root");

    write_fixture(&pkg_dir.join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &pkg_dir.join("dist/index.js"),
        "import dep from \"@scope/dep\";\ndep();\n",
    );
    write_fixture(
        &dep_dir.join("package.json"),
        r#"{"type":"module","exports":"./index.js"}"#,
    );
    write_fixture(
        &dep_dir.join("index.js"),
        "import fs from \"node:fs\";\nexport default function dep() { fs.statSync(\"/workspace/pkg-mount.txt\"); }\n",
    );

    symlink(&pkg_dir, &mounted_pkg).expect("symlink mounted package to pnpm store");

    let guest_mappings = serde_json::to_string(&vec![json!({
        "guestPath": "/root/node_modules/pkg",
        "hostPath": mounted_pkg.display().to_string(),
    })])
    .expect("serialize guest mappings");

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("/root/node_modules/pkg/dist/index.js")],
            env: BTreeMap::from([(String::from("AGENT_OS_GUEST_PATH_MAPPINGS"), guest_mappings)]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/pkg-mount.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 13,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_timer_callbacks_fire_and_clear_correctly() {
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
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
(async () => {
  const clearedTimeout = setTimeout(() => {
    throw new Error("cleared timeout fired");
  }, 10);
  clearTimeout(clearedTimeout);

  await new Promise((resolve) => setTimeout(resolve, 25));

  let intervalTicks = 0;
  await new Promise((resolve, reject) => {
    const interval = setInterval(() => {
      intervalTicks += 1;
      if (intervalTicks === 2) {
        clearInterval(interval);
        resolve();
      } else if (intervalTicks > 2) {
        reject(new Error(`interval fired too many times: ${intervalTicks}`));
      }
    }, 10);

    setTimeout(() => reject(new Error(`interval timeout: ${intervalTicks}`)), 250);
  });

  if (intervalTicks !== 2) {
    throw new Error(`interval tick count mismatch: ${intervalTicks}`);
  }
})().catch((error) => {
  process.exitCode = 1;
  throw error;
});
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_readline_polyfill_emits_lines() {
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
            inline_code: Some(String::from(
                r#"
import { EventEmitter } from "node:events";
import { createInterface } from "node:readline";

const input = new EventEmitter();
const seen = [];
const rl = createInterface({ input });
rl.on("line", (line) => seen.push(line));
input.emit("data", "alpha\nbeta\r\ngamma");
input.emit("end");

if (seen.length !== 3) {
  throw new Error(`expected 3 lines, got ${JSON.stringify(seen)}`);
}
if (seen[0] !== "alpha" || seen[1] !== "beta" || seen[2] !== "gamma") {
  throw new Error(`unexpected lines: ${JSON.stringify(seen)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_builtin_wrappers_expose_common_named_exports() {
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
            inline_code: Some(String::from(
                r#"
import { spawn, spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";

if (typeof spawn !== "function" || typeof spawnSync !== "function") throw new Error("child_process exports missing");
if (typeof closeSync !== "function" || typeof existsSync !== "function" || typeof mkdirSync !== "function") throw new Error("fs exports missing");
if (typeof openSync !== "function" || typeof readFileSync !== "function" || typeof readSync !== "function") throw new Error("fs exports missing");
if (typeof readdirSync !== "function" || typeof realpathSync !== "function" || typeof statSync !== "function" || typeof writeFileSync !== "function") throw new Error("fs exports missing");
if (typeof homedir !== "function" || typeof platform !== "function") throw new Error("os exports missing");
if (typeof basename !== "function" || typeof dirname !== "function" || typeof isAbsolute !== "function" || typeof join !== "function" || typeof resolve !== "function") throw new Error("path exports missing");
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_child_process_conformance_matches_host_node() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import childProcess from "node:child_process";
import fs from "node:fs";

fs.writeFileSync("async-out.txt", Buffer.from("async:beta-async\n", "utf8"));

const syncPiped = childProcess.spawnSync("/bin/cat", [], {
  input: Buffer.from("alpha-sync"),
});
const syncError = childProcess.spawnSync("/bin/cat", ["definitely-missing-agentos-file"]);

const asyncResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn("/bin/cat", ["async-out.txt"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const timer = setTimeout(() => {
    reject(new Error("spawn(/bin/cat async-out.txt) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

const asyncErrorResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn("/bin/cat", ["definitely-missing-agentos-file"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const timer = setTimeout(() => {
    reject(new Error("spawn(/bin/cat missing-file) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

console.log(JSON.stringify({
  syncPipedStatus: syncPiped.status,
  syncPipedStdoutBase64: Buffer.from(syncPiped.stdout ?? []).toString("base64"),
  syncPipedStderrBase64: Buffer.from(syncPiped.stderr ?? []).toString("base64"),
  syncErrorStatus: syncError.status,
  syncErrorStdoutBase64: Buffer.from(syncError.stdout ?? []).toString("base64"),
  syncErrorStderrBase64: Buffer.from(syncError.stderr ?? []).toString("base64"),
  asyncCode: asyncResult.code,
  asyncSignal: asyncResult.signal,
  asyncStdoutBase64: asyncResult.stdoutBase64,
  asyncStderrBase64: asyncResult.stderrBase64,
  asyncErrorCode: asyncErrorResult.code,
  asyncErrorSignal: asyncErrorResult.signal,
  asyncErrorStdoutBase64: asyncErrorResult.stdoutBase64,
  asyncErrorStderrBase64: asyncErrorResult.stderrBase64,
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let host = run_host_node_json(temp.path(), &temp.path().join("entry.mjs"));
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = wait_with_host_child_process_bridge(execution, temp.path());
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let guest: Value = serde_json::from_str(stdout.trim()).expect("parse guest JSON");
    assert_eq!(
        guest,
        host,
        "guest child_process result diverged from host Node\nhost: {}\nguest: {}",
        serde_json::to_string_pretty(&host).expect("pretty host JSON"),
        serde_json::to_string_pretty(&guest).expect("pretty guest JSON")
    );
}

fn javascript_execution_v8_web_stream_globals_support_basic_io() {
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
            inline_code: Some(String::from(
                r#"
const writes = [];
const writable = new WritableStream({
  write(chunk) {
    writes.push(new TextDecoder().decode(chunk));
  },
});
const writer = writable.getWriter();
await writer.write(new TextEncoder().encode("hello"));
writer.releaseLock();

const readable = new ReadableStream({
  start(controller) {
    controller.enqueue("alpha");
    controller.close();
  },
});
const reader = readable.getReader();
const first = await reader.read();
const second = await reader.read();
reader.releaseLock();

if (writes.length !== 1 || writes[0] !== "hello") {
  throw new Error(`unexpected writes: ${JSON.stringify(writes)}`);
}
if (first.value !== "alpha" || first.done !== false || second.done !== true) {
  throw new Error(`unexpected reads: ${JSON.stringify({ first, second })}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_text_codec_streams_support_pipe_through() {
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
            inline_code: Some(String::from(
                r#"
const {
  TextEncoderStream: ModuleTextEncoderStream,
  TextDecoderStream: ModuleTextDecoderStream,
} = await import("node:stream/web");

if (ModuleTextEncoderStream !== TextEncoderStream) {
  throw new Error("node:stream/web TextEncoderStream export diverged from global");
}
if (ModuleTextDecoderStream !== TextDecoderStream) {
  throw new Error("node:stream/web TextDecoderStream export diverged from global");
}

if (new TextEncoderStream().encoding !== "utf-8") {
  throw new Error("unexpected TextEncoderStream encoding");
}

const decoder = new ReadableStream({
  start(controller) {
    controller.enqueue(new Uint8Array([0xe2, 0x82]));
    controller.enqueue(new Uint8Array([0xac, 0x21]));
    controller.close();
  },
}).pipeThrough(new TextDecoderStream());

const decoderReader = decoder.getReader();
const decoded = [];
for (;;) {
  const { done, value } = await decoderReader.read();
  if (done) break;
  decoded.push(value);
}
decoderReader.releaseLock();

if (decoded.join("") !== "€!") {
  throw new Error(`unexpected decoded output: ${JSON.stringify(decoded)}`);
}

const encoded = new ReadableStream({
  start(controller) {
    controller.enqueue("hello");
    controller.enqueue(" world");
    controller.close();
  },
}).pipeThrough(new TextEncoderStream());

const encodedReader = encoded.getReader();
const bytes = [];
for (;;) {
  const { done, value } = await encodedReader.read();
  if (done) break;
  bytes.push(...value);
}
encodedReader.releaseLock();

const roundTrip = new TextDecoder().decode(new Uint8Array(bytes));
if (roundTrip !== "hello world") {
  throw new Error(`unexpected encoded output: ${roundTrip}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_abort_controller_dispatches_abort() {
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
            inline_code: Some(String::from(
                r#"
const controller = new AbortController();
let seenAbort = false;
controller.signal.addEventListener("abort", () => {
  seenAbort = true;
});
controller.abort("stop");
if (!controller.signal.aborted || controller.signal.reason !== "stop" || !seenAbort) {
  throw new Error("abort controller did not update signal state");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_request_accepts_abort_signal() {
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
            inline_code: Some(String::from(
                r#"
const controller = new AbortController();
const request = new Request("http://example.com/test", {
  method: "POST",
  body: JSON.stringify({ ok: true }),
  duplex: "half",
  signal: controller.signal,
  headers: { "content-type": "application/json" },
});
if (!(request.signal instanceof AbortSignal)) {
  throw new Error("request signal was not preserved");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_abort_signal_static_helpers_work() {
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
            inline_code: Some(String::from(
                r#"
if (typeof AbortSignal.timeout !== "function") {
  throw new Error("AbortSignal.timeout missing");
}
if (typeof AbortSignal.any !== "function") {
  throw new Error("AbortSignal.any missing");
}

const timeoutSignal = AbortSignal.timeout(25);
let timeoutEventCount = 0;
timeoutSignal.addEventListener("abort", () => {
  timeoutEventCount += 1;
});
await new Promise((resolve) => setTimeout(resolve, 60));
if (!timeoutSignal.aborted) {
  throw new Error("AbortSignal.timeout did not abort");
}
if (timeoutEventCount !== 1) {
  throw new Error(`unexpected timeout event count: ${timeoutEventCount}`);
}
if (!timeoutSignal.reason || timeoutSignal.reason.name !== "AbortError") {
  throw new Error(`unexpected timeout reason: ${String(timeoutSignal.reason?.name ?? timeoutSignal.reason)}`);
}

const controller = new AbortController();
const sibling = new AbortController();
const composite = AbortSignal.any([sibling.signal, controller.signal]);
let compositeReason;
composite.addEventListener("abort", () => {
  compositeReason = composite.reason;
});
controller.abort("manual-stop");
await new Promise((resolve) => setTimeout(resolve, 0));
if (!composite.aborted) {
  throw new Error("AbortSignal.any did not abort");
}
if (compositeReason !== "manual-stop" || composite.reason !== "manual-stop") {
  throw new Error(`unexpected composite reason: ${String(composite.reason)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_schedule_timer_bridge_resolves() {
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
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
(async () => {
  let resolved = false;
  await _scheduleTimer.apply(undefined, [15]).then(() => {
    resolved = true;
  });
  if (!resolved) {
    throw new Error("_scheduleTimer did not resolve");
  }
})().catch((error) => {
  process.exitCode = 1;
  throw error;
});
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);

    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_kernel_poll_bridge_requests_multiple_fds() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const result = globalThis._kernelPollRaw.applySyncPromise(undefined, [[
  { fd: 0, events: 1 },
  { fd: 1, events: 1 },
], 250]);
if (result.readyCount !== 1) {
  throw new Error(`readyCount=${result.readyCount}`);
}
if (result.fds[0]?.revents !== 1 || result.fds[1]?.revents !== 0) {
  throw new Error(`revents=${JSON.stringify(result.fds)}`);
}
console.log(JSON.stringify(result));
"#,
            )),
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "__kernel_poll");
    assert_eq!(
        request.args,
        vec![
            json!([
                { "fd": 0, "events": 1 },
                { "fd": 1, "events": 1 }
            ]),
            json!(250),
        ]
    );

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "readyCount": 1,
                "fds": [
                    { "fd": 0, "events": 1, "revents": 1 },
                    { "fd": 1, "events": 1, "revents": 0 }
                ]
            }),
        )
        .expect("respond to __kernel_poll");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let stdout: Value = serde_json::from_slice(&result.stdout).expect("parse guest stdout JSON");
    assert_eq!(
        stdout,
        json!({
            "readyCount": 1,
            "fds": [
                { "fd": 0, "events": 1, "revents": 1 },
                { "fd": 1, "events": 1, "revents": 0 }
            ]
        })
    );
}

fn javascript_execution_v8_crypto_random_sources_use_local_secure_bridge() {
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
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const first = new Uint8Array(32);
const second = new Uint8Array(32);
globalThis.crypto.getRandomValues(first);
globalThis.crypto.getRandomValues(second);

if (first.every((value) => value === 0)) {
  throw new Error("first random buffer was all zero");
}
if (second.every((value) => value === 0)) {
  throw new Error("second random buffer was all zero");
}
const buffersMatch = first.length === second.length &&
  first.every((value, index) => value === second[index]);
if (buffersMatch) {
  throw new Error("random buffers repeated");
}

const uuid = globalThis.crypto.randomUUID();
if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(uuid)) {
  throw new Error(`invalid uuid: ${uuid}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_crypto_basic_operations_emit_expected_sync_rpcs() {
    assert_eq!(
        map_bridge_method("_cryptoHashDigest"),
        ("crypto.hashDigest", false)
    );
    assert_eq!(
        map_bridge_method("_cryptoHmacDigest"),
        ("crypto.hmacDigest", false)
    );
    assert_eq!(map_bridge_method("_cryptoPbkdf2"), ("crypto.pbkdf2", false));
    assert_eq!(map_bridge_method("_cryptoScrypt"), ("crypto.scrypt", false));
    assert_eq!(
        map_bridge_method("_netSocketConnectRaw"),
        ("net.connect", false)
    );
    assert_eq!(map_bridge_method("_netSocketPollRaw"), ("net.poll", false));
}

fn javascript_execution_v8_load_polyfill_returns_runtime_module_expressions() {
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
            inline_code: Some(String::from(
                r#"
const pathExpr = _loadPolyfill.applySyncPromise(undefined, ["path"]);
if (typeof pathExpr !== "string" || !pathExpr.includes("node:path")) {
  throw new Error(`unexpected path polyfill expression: ${String(pathExpr)}`);
}

const pathModule = Function('"use strict"; return (' + pathExpr + ');')();
if (pathModule.join("alpha", "beta") !== "alpha/beta") {
  throw new Error("path polyfill expression did not resolve the runtime module");
}

const deniedExpr = _loadPolyfill.applySyncPromise(undefined, ["inspector"]);
if (typeof deniedExpr !== "string" || !deniedExpr.includes("ERR_ACCESS_DENIED")) {
  throw new Error(`unexpected denied polyfill expression: ${String(deniedExpr)}`);
}

let denied = false;
try {
  Function('"use strict"; return (' + deniedExpr + ');')();
} catch (error) {
  denied = error?.code === "ERR_ACCESS_DENIED";
}
if (!denied) {
  throw new Error("denied polyfill expression did not raise ERR_ACCESS_DENIED");
}

if (_loadPolyfill.applySyncPromise(undefined, ["not-a-real-builtin"]) !== null) {
  throw new Error("unknown polyfill name should return null");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_stream_wrapper_exports_common_node_classes() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import {
  Duplex,
  PassThrough,
  Readable,
  Transform,
  Writable,
  isReadable,
  isWritable,
} from "node:stream";
import { createRequire } from "node:module";

for (const [name, value] of Object.entries({ Duplex, PassThrough, Readable, Transform, Writable })) {
  if (typeof value !== "function") {
    throw new Error(`${name} was not exported as a constructor`);
  }
}

const require = createRequire(import.meta.url);
const cjsStream = require("stream");
if (typeof cjsStream !== "function") {
  throw new Error("require('stream') should return the legacy Stream constructor");
}
if (cjsStream !== cjsStream.Stream) {
  throw new Error("require('stream').Stream should alias the CommonJS export");
}
if (typeof cjsStream.Readable !== "function") {
  throw new Error("require('stream').Readable should stay available on the constructor export");
}

const pass = new PassThrough();
let output = "";
pass.on("data", (chunk) => {
  output += Buffer.from(chunk).toString("utf8");
});
pass.end("hello");
await new Promise((resolve, reject) => {
  pass.once("close", resolve);
  pass.once("error", reject);
});

if (output !== "hello") {
  throw new Error(`unexpected passthrough output: ${output}`);
}
if (!isReadable(pass) || !isWritable(pass)) {
  throw new Error("stream helpers misreported passthrough readability");
}
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_buffer_wrapper_exposes_commonjs_constants() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const bufferModule = require("buffer");

if (typeof bufferModule.constants !== "object" || bufferModule.constants === null) {
  throw new Error("require('buffer').constants was not exported");
}
if (typeof bufferModule.constants.MAX_STRING_LENGTH !== "number") {
  throw new Error("require('buffer').constants.MAX_STRING_LENGTH was not exported");
}
if (typeof bufferModule.kMaxLength !== "number") {
  throw new Error("require('buffer').kMaxLength was not exported");
}
if (bufferModule.Buffer?.constants?.MAX_STRING_LENGTH !== bufferModule.constants.MAX_STRING_LENGTH) {
  throw new Error("buffer module constants diverged from Buffer.constants");
}
if (typeof bufferModule.Blob !== "function") {
  throw new Error("require('buffer').Blob was not exported");
}
if (typeof bufferModule.File !== "function") {
  throw new Error("require('buffer').File was not exported");
}
const file = new bufferModule.File(["hello"], "hello.txt", { type: "text/plain" });
if (!(file instanceof bufferModule.Blob)) {
  throw new Error("buffer module File did not extend Blob");
}
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_commonjs_stack_frames_preserve_module_paths() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
require("./probe.cjs");
"#,
    );
    write_fixture(
        &temp.path().join("probe.cjs"),
        r#"
const previousPrepare = Error.prepareStackTrace;
try {
  Error.prepareStackTrace = (_error, stack) => stack;
  const stack = new Error("probe").stack ?? [];
  const frame = stack.find((callsite) => {
    const path =
      callsite.getFileName?.() ?? callsite.getScriptNameOrSourceURL?.();
    return typeof path === "string" && path.endsWith("/probe.cjs");
  });
  if (!frame) {
    const summary = stack.map((callsite) => ({
      fileName: callsite.getFileName?.() ?? null,
      scriptName: callsite.getScriptNameOrSourceURL?.() ?? null,
      text: String(callsite),
    }));
    throw new Error(
      "CommonJS stack frames did not preserve the module path: " +
        JSON.stringify(summary),
    );
  }
} finally {
  Error.prepareStackTrace = previousPrepare;
}
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_commonjs_main_entrypoints_preserve_entrypoint_paths() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.cjs"),
        r#"
const EVAL_FRAMES = new Set(["[eval]", "[eval]-wrapper"]);
const INTERNAL_FRAME_NAMES = new Set([
  "readCallsites",
  "resolveCallerFilePath",
  "getCurrentFilePath",
]);

function readCallsites() {
  const previousPrepare = Error.prepareStackTrace;
  try {
    Error.prepareStackTrace = (_error, stack) => stack;
    return new Error("probe").stack ?? [];
  } finally {
    Error.prepareStackTrace = previousPrepare;
  }
}

function readCallsitePath(callsite) {
  const rawPath =
    callsite.getFileName?.() ?? callsite.getScriptNameOrSourceURL?.();
  if (!rawPath || rawPath.startsWith("node:") || EVAL_FRAMES.has(rawPath)) {
    return null;
  }
  return rawPath;
}

function isInternalCallsite(callsite) {
  const functionName = callsite.getFunctionName?.();
  if (functionName && INTERNAL_FRAME_NAMES.has(functionName)) {
    return true;
  }
  const methodName = callsite.getMethodName?.();
  if (methodName && INTERNAL_FRAME_NAMES.has(methodName)) {
    return true;
  }
  const callsiteString = String(callsite);
  for (const frameName of INTERNAL_FRAME_NAMES) {
    if (
      callsiteString.includes(`${frameName} (`) ||
      callsiteString.includes(`.${frameName} (`)
    ) {
      return true;
    }
  }
  return false;
}

function resolveCallerFilePath() {
  for (const callsite of readCallsites()) {
    const filePath = readCallsitePath(callsite);
    if (!filePath || isInternalCallsite(callsite)) {
      continue;
    }
    return filePath;
  }
  throw new Error("Unable to resolve caller file path.");
}

const resolved = resolveCallerFilePath();
if (!resolved.endsWith("/entry.cjs")) {
  throw new Error(`resolved ${resolved} instead of /entry.cjs`);
}
"#,
    );

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
            argv: vec![String::from("./entry.cjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_inline_commonjs_entrypoints_preserve_entrypoint_paths() {
    let temp = tempdir().expect("create temp dir");
    let source = String::from(
        r#"
const EVAL_FRAMES = new Set(["[eval]", "[eval]-wrapper"]);
const INTERNAL_FRAME_NAMES = new Set([
  "readCallsites",
  "resolveCallerFilePath",
  "getCurrentFilePath",
]);

function readCallsites() {
  const previousPrepare = Error.prepareStackTrace;
  try {
    Error.prepareStackTrace = (_error, stack) => stack;
    return new Error("probe").stack ?? [];
  } finally {
    Error.prepareStackTrace = previousPrepare;
  }
}

function readCallsitePath(callsite) {
  const rawPath =
    callsite.getFileName?.() ?? callsite.getScriptNameOrSourceURL?.();
  if (!rawPath || rawPath.startsWith("node:") || EVAL_FRAMES.has(rawPath)) {
    return null;
  }
  return rawPath;
}

function isInternalCallsite(callsite) {
  const functionName = callsite.getFunctionName?.();
  if (functionName && INTERNAL_FRAME_NAMES.has(functionName)) {
    return true;
  }
  const methodName = callsite.getMethodName?.();
  if (methodName && INTERNAL_FRAME_NAMES.has(methodName)) {
    return true;
  }
  const callsiteString = String(callsite);
  for (const frameName of INTERNAL_FRAME_NAMES) {
    if (
      callsiteString.includes(`${frameName} (`) ||
      callsiteString.includes(`.${frameName} (`)
    ) {
      return true;
    }
  }
  return false;
}

function resolveCallerFilePath() {
  for (const callsite of readCallsites()) {
    const filePath = readCallsitePath(callsite);
    if (!filePath || isInternalCallsite(callsite)) {
      continue;
    }
    return filePath;
  }
  throw new Error("Unable to resolve caller file path.");
}

const resolved = resolveCallerFilePath();
if (!resolved.endsWith("/entry.cjs")) {
  throw new Error(`resolved ${resolved} instead of /entry.cjs`);
}
"#,
    );

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
            argv: vec![String::from("./entry.cjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(source),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_inline_commonjs_entrypoints_preserve_commonjs_globals() {
    let temp = tempdir().expect("create temp dir");
    let source = String::from(
        r#"
console.log(
  JSON.stringify({
    filename: __filename,
    dirname: __dirname,
    cwd: process.cwd(),
  }),
);
"#,
    );

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
            argv: vec![String::from("./entry.cjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(source),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse stdout JSON");
    assert_eq!(
        output,
        json!({
            "filename": "/root/entry.cjs",
            "dirname": "/root",
            "cwd": "/root",
        })
    );
}

fn javascript_execution_v8_commonjs_require_exposes_node_metadata() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.cjs"),
        r#"
const hadSelfBeforeDelete = Object.prototype.hasOwnProperty.call(
  require.cache,
  __filename,
);
delete require.cache[__filename];
module.exports = {
  cacheType: typeof require.cache,
  hadSelfBeforeDelete,
  hasSelfAfterDelete: Object.prototype.hasOwnProperty.call(require.cache, __filename),
  extensionsType: typeof require.extensions,
};
"#,
    );
    write_fixture(
        &temp.path().join("entry.cjs"),
        r#"
const dep = require("./dep.cjs");
console.log(JSON.stringify(dep));
"#,
    );

    let mut host = run_host_node_json(temp.path(), &temp.path().join("entry.cjs"));
    host["hadSelfBeforeDelete"] = json!(true);

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
            argv: vec![String::from("./entry.cjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let guest: Value = serde_json::from_slice(&result.stdout).expect("parse stdout JSON");
    assert_eq!(
        guest, host,
        "guest CommonJS require metadata diverged from host"
    );
}

fn javascript_execution_v8_https_agents_expose_options_objects() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const http = require("http");
const https = require("https");

for (const [name, module] of Object.entries({ http, https })) {
  if (!module.globalAgent || typeof module.globalAgent.options !== "object") {
    throw new Error(`${name}.globalAgent.options was not initialized`);
  }
  const agent = new module.Agent({ keepAlive: true });
  if (!agent.options || agent.options.keepAlive !== true) {
    throw new Error(`${name}.Agent did not preserve constructor options`);
  }
}
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_net_socket_readable_state_tracks_ssh2_writable_shape() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import net from "node:net";

const isWritable = (stream) =>
  Boolean(stream?.writable && stream?._readableState?.ended === false);

const socket = new net.Socket();
if (socket._readableState?.ended !== false) {
  throw new Error(`expected open socket ended=false, got ${String(socket._readableState?.ended)}`);
}
if (!isWritable(socket)) {
  throw new Error("ssh2 writable probe should accept an open socket");
}

socket.destroy();

if (socket._readableState?.ended !== true) {
  throw new Error(`expected destroyed socket ended=true, got ${String(socket._readableState?.ended)}`);
}
if (isWritable(socket)) {
  throw new Error("ssh2 writable probe should reject a destroyed socket");
}
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn javascript_execution_v8_dynamic_import_accepts_file_urls() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.mjs"),
        r#"
export default { value: "ok" };
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const href = new URL("./dep.mjs", import.meta.url).href;
const module = await import(href);
console.log(JSON.stringify({ href, value: module.default.value }));
"#,
    );

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
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse stdout JSON");
    assert_eq!(
        output,
        json!({
            "href": "file:///root/dep.mjs",
            "value": "ok",
        })
    );
}

fn javascript_execution_v8_wasm_instantiate_streaming_never_hangs() {
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
            inline_code: Some(String::from(
                r#"
const bytes = new Uint8Array([
  0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,3,2,1,0,7,12,1,8,102,111,114,116,121,84,119,111,0,0,10,6,1,4,0,65,42,11,
]);
const response = new Response(bytes, {
  headers: { "content-type": "application/wasm" },
});

let outcome = "pending";
try {
  const result = await WebAssembly.instantiateStreaming(Promise.resolve(response), {});
  if (typeof result?.instance?.exports?.fortyTwo !== "function") {
    throw new Error("instantiateStreaming() did not return an exported function");
  }
  if (result.instance.exports.fortyTwo() !== 42) {
    throw new Error(`unexpected wasm export value: ${result.instance.exports.fortyTwo()}`);
  }
  outcome = "ok";
} catch (error) {
  if (error?.code !== "ERR_NOT_IMPLEMENTED") {
    throw error;
  }
  outcome = error.code;
}

console.log(outcome);
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    let outcome = stdout.trim();
    assert!(
        outcome == "ok" || outcome == "ERR_NOT_IMPLEMENTED",
        "unexpected instantiateStreaming outcome: {outcome}"
    );
}

fn javascript_execution_v8_structured_clone_rebinds_to_sandbox_realm() {
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
            inline_code: Some(String::from(
                r#"
const source = new Uint8Array([1, 2, 3, 4]);
const typed = structuredClone(source, { transfer: [source.buffer] });
const map = structuredClone(new Map([["a", 1]]));
const date = structuredClone(new Date(0));
const regexSource = /agent/gi;
regexSource.lastIndex = 2;
const regex = structuredClone(regexSource);
const dataView = structuredClone(new DataView(new Uint8Array([9, 8, 7, 6]).buffer, 1, 2));
const circular = { label: "loop" };
circular.self = circular;
const circularClone = structuredClone(circular);
const nested = structuredClone({
  list: [new Uint16Array([5, 6])],
  set: new Set(["x"]),
});
let functionErrorName = null;
try {
  structuredClone(() => {});
} catch (error) {
  functionErrorName = error?.name ?? String(error);
}
console.log(JSON.stringify({
  typed: {
    instanceof: typed instanceof Uint8Array,
    sameConstructor: typed.constructor === Uint8Array,
    constructorName: typed.constructor?.name,
    length: typed.length,
    first: typed[0],
  },
  map: {
    instanceof: map instanceof Map,
    value: map.get("a"),
  },
  date: {
    instanceof: date instanceof Date,
    value: date.valueOf(),
  },
  regex: {
    instanceof: regex instanceof RegExp,
    source: regex.source,
    flags: regex.flags,
    lastIndex: regex.lastIndex,
  },
  dataView: {
    instanceof: dataView instanceof DataView,
    byteLength: dataView.byteLength,
    first: dataView.getUint8(0),
  },
  circular: circularClone !== circular && circularClone.self === circularClone,
  nested: {
    typedArrayInstanceof: nested.list[0] instanceof Uint16Array,
    setInstanceof: nested.set instanceof Set,
    setValue: nested.set.has("x"),
  },
  functionErrorName,
}));
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(result.stderr.is_empty(), "unexpected stderr: {stderr}");

    let output: Value = serde_json::from_slice(&result.stdout).expect("parse stdout JSON");
    assert_eq!(
        output,
        json!({
            "typed": {
                "instanceof": true,
                "sameConstructor": true,
                "constructorName": "Uint8Array",
                "length": 4,
                "first": 1,
            },
            "map": {
                "instanceof": true,
                "value": 1,
            },
            "date": {
                "instanceof": true,
                "value": 0,
            },
            "regex": {
                "instanceof": true,
                "source": "agent",
                "flags": "gi",
                "lastIndex": 2,
            },
            "dataView": {
                "instanceof": true,
                "byteLength": 2,
                "first": 8,
            },
            "circular": true,
            "nested": {
                "typedArrayInstanceof": true,
                "setInstanceof": true,
                "setValue": true,
            },
            "functionErrorName": "DataCloneError",
        })
    );
}

#[test]
fn javascript_v8_suite() {
    // Keep V8-backed integration coverage inside one top-level libtest case.
    // Running these guest-runtime cases as separate tests in the same binary
    // still trips a V8 teardown/init boundary crash between cases.
    javascript_contexts_preserve_vm_and_bootstrap_configuration();
    javascript_execution_uses_v8_runtime_without_spawning_guest_node_binary();
    javascript_execution_virtualizes_process_metadata_for_inline_v8_code();
    javascript_execution_stream_consumers_text_reads_live_stdin();
    javascript_execution_process_stdin_async_iterator_finishes_with_live_stdin();
    javascript_execution_process_exit_from_live_stdin_listener_exits_without_waiting_for_eof();
    javascript_execution_live_stdin_replays_end_after_late_listener_registration();
    javascript_execution_file_url_to_path_accepts_guest_absolute_paths();
    javascript_execution_imports_node_events_without_hanging();
    javascript_execution_imports_node_process_without_hanging();
    javascript_execution_imports_node_fs_promises_without_hanging();
    javascript_execution_imports_node_perf_hooks_without_hanging();
    javascript_execution_exposes_compatibility_shims_and_denies_escape_builtins();
    javascript_execution_provides_async_hooks_and_diagnostics_channel_stubs();
    javascript_execution_supports_require_resolve_for_guest_code();
    javascript_execution_surfaces_sync_rpc_requests_from_v8_modules();
    javascript_execution_v8_dgram_bridge_matches_sidecar_rpc_shapes();
    javascript_execution_strips_hashbang_from_module_entrypoints();
    javascript_execution_resolves_pnpm_store_dependencies_from_symlinked_entrypoints();
    javascript_execution_resolves_dependencies_from_package_specific_symlink_mounts();
    javascript_execution_v8_timer_callbacks_fire_and_clear_correctly();
    javascript_execution_v8_readline_polyfill_emits_lines();
    javascript_execution_v8_builtin_wrappers_expose_common_named_exports();
    javascript_execution_v8_child_process_conformance_matches_host_node();
    javascript_execution_v8_web_stream_globals_support_basic_io();
    javascript_execution_v8_text_codec_streams_support_pipe_through();
    javascript_execution_v8_abort_controller_dispatches_abort();
    javascript_execution_v8_request_accepts_abort_signal();
    javascript_execution_v8_abort_signal_static_helpers_work();
    javascript_execution_v8_schedule_timer_bridge_resolves();
    javascript_execution_v8_kernel_poll_bridge_requests_multiple_fds();
    javascript_execution_v8_crypto_random_sources_use_local_secure_bridge();
    javascript_execution_v8_crypto_basic_operations_emit_expected_sync_rpcs();
    javascript_execution_v8_load_polyfill_returns_runtime_module_expressions();
    javascript_execution_v8_stream_wrapper_exports_common_node_classes();
    javascript_execution_v8_buffer_wrapper_exposes_commonjs_constants();
    javascript_execution_v8_commonjs_stack_frames_preserve_module_paths();
    javascript_execution_v8_commonjs_main_entrypoints_preserve_entrypoint_paths();
    javascript_execution_v8_inline_commonjs_entrypoints_preserve_entrypoint_paths();
    javascript_execution_v8_inline_commonjs_entrypoints_preserve_commonjs_globals();
    javascript_execution_v8_commonjs_require_exposes_node_metadata();
    javascript_execution_v8_https_agents_expose_options_objects();
    javascript_execution_v8_net_socket_readable_state_tracks_ssh2_writable_shape();
    javascript_execution_v8_dynamic_import_accepts_file_urls();
    javascript_execution_v8_wasm_instantiate_streaming_never_hangs();
    javascript_execution_v8_structured_clone_rebinds_to_sandbox_realm();
}
