use agent_os_execution::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, JavascriptExecutionEvent,
    StartJavascriptExecutionRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

const NODE_IMPORT_CACHE_METRICS_PREFIX: &str = "__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:";
const NODE_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_NODE_WARMUP_METRICS__:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeImportCacheMetrics {
    resolve_hits: usize,
    resolve_misses: usize,
    package_type_hits: usize,
    package_type_misses: usize,
    module_format_hits: usize,
    module_format_misses: usize,
    source_hits: usize,
    source_misses: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeWarmupMetrics {
    executed: bool,
    reason: String,
    import_count: usize,
    asset_root: String,
}

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

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if !root.exists() {
        return files;
    }

    for entry in fs::read_dir(root).expect("read cache dir") {
        let entry = entry.expect("cache entry");
        let path = entry.path();
        let metadata = entry.metadata().expect("cache metadata");

        if metadata.is_dir() {
            files.extend(collect_files(&path));
        } else if metadata.is_file() {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn parse_import_cache_metrics(stderr: &str) -> NodeImportCacheMetrics {
    let metrics_line = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(NODE_IMPORT_CACHE_METRICS_PREFIX))
        .last()
        .expect("import cache metrics line");

    NodeImportCacheMetrics {
        resolve_hits: parse_metric_value(metrics_line, "resolveHits"),
        resolve_misses: parse_metric_value(metrics_line, "resolveMisses"),
        package_type_hits: parse_metric_value(metrics_line, "packageTypeHits"),
        package_type_misses: parse_metric_value(metrics_line, "packageTypeMisses"),
        module_format_hits: parse_metric_value(metrics_line, "moduleFormatHits"),
        module_format_misses: parse_metric_value(metrics_line, "moduleFormatMisses"),
        source_hits: parse_metric_value(metrics_line, "sourceHits"),
        source_misses: parse_metric_value(metrics_line, "sourceMisses"),
    }
}

fn parse_warmup_metrics(stderr: &str) -> NodeWarmupMetrics {
    let metrics_line = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(NODE_WARMUP_METRICS_PREFIX))
        .last()
        .expect("warmup metrics line");

    NodeWarmupMetrics {
        executed: parse_boolean_metric(metrics_line, "executed"),
        reason: parse_string_metric(metrics_line, "reason"),
        import_count: parse_metric_value(metrics_line, "importCount"),
        asset_root: parse_string_metric(metrics_line, "assetRoot"),
    }
}

fn parse_metric_value(metrics_line: &str, key: &str) -> usize {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let digits: String = metrics_line[start..]
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();

    digits.parse().expect("metric value")
}

fn parse_boolean_metric(metrics_line: &str, key: &str) -> bool {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let remaining = &metrics_line[start..];

    if remaining.starts_with("true") {
        true
    } else if remaining.starts_with("false") {
        false
    } else {
        panic!("invalid boolean metric for {key}: {metrics_line}");
    }
}

fn parse_string_metric(metrics_line: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let mut value = String::new();
    let mut escaped = false;

    for ch in metrics_line[start..].chars() {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return value,
            other => value.push(other),
        }
    }

    panic!("unterminated string metric for {key}: {metrics_line}");
}

fn run_javascript_execution(
    engine: &mut JavascriptExecutionEngine,
    context_id: String,
    cwd: &Path,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
) -> (String, String, i32) {
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id,
            argv,
            env,
            cwd: cwd.to_path_buf(),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");

    (stdout, stderr, result.exit_code)
}

#[test]
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

#[test]
fn javascript_execution_runs_bootstrap_and_streams_stdio() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("bootstrap.mjs"),
        r#"
globalThis.__agentOsBootstrapLoaded = true;
console.log("bootstrap:ready");
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
if (!globalThis.__agentOsBootstrapLoaded) {
  throw new Error("bootstrap missing");
}

let input = "";
process.stdin.setEncoding("utf8");
for await (const chunk of process.stdin) {
  input += chunk;
}

console.log(`stdout:${process.env.VISIBLE_TEST_ENV}:${input}`);
console.error(`stderr:${process.argv.slice(2).join(",")}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: Some(String::from("./bootstrap.mjs")),
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![
                String::from("./entry.mjs"),
                String::from("alpha"),
                String::from("beta"),
            ],
            env: BTreeMap::from([(String::from("VISIBLE_TEST_ENV"), String::from("ok"))]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    assert_eq!(execution.execution_id(), "exec-1");

    execution
        .write_stdin(b"hello from stdin")
        .expect("write stdin");
    execution.close_stdin().expect("close stdin");

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
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                panic!("unexpected sync RPC request: {}", request.method);
            }
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");

    assert!(stdout.contains("bootstrap:ready"));
    assert!(stdout.contains("stdout:ok:hello from stdin"));
    assert!(stderr.contains("stderr:alpha,beta"));
}

#[test]
fn javascript_execution_keeps_streaming_stdin_sessions_alive_until_closed() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  input += chunk;
});
process.stdin.on("end", () => {
  console.log(`stdin:${input}`);
});
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
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    assert!(
        execution
            .poll_event(Duration::from_millis(200))
            .expect("poll execution event before stdin write")
            .is_none(),
        "streaming-stdin execution should stay alive until stdin closes"
    );

    execution
        .write_stdin(b"still-open")
        .expect("write stdin after idle period");
    execution.close_stdin().expect("close stdin");

    let mut stdout = Vec::new();
    let mut exit_code = None;
    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(_chunk)) => {}
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                panic!("unexpected sync RPC request: {}", request.method);
            }
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    assert!(String::from_utf8(stdout)
        .expect("stdout utf8")
        .contains("stdin:still-open"));
}

#[test]
fn javascript_execution_surfaces_shared_array_buffer_sync_rpc_requests() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import fs from "node:fs";

const stat = fs.statSync("/workspace/note.txt");
const lstat = fs.lstatSync("/workspace/link.txt");
const contents = fs.readFileSync("/workspace/note.txt", { encoding: "utf8" });
const raw = Buffer.from(fs.readFileSync("/workspace/raw.bin")).toString("hex");
const entries = fs.readdirSync("/workspace");
const missing = fs.existsSync("/workspace/missing.txt");

fs.mkdirSync("/workspace/subdir", { recursive: true });
fs.writeFileSync("/workspace/out.bin", Buffer.from([1, 2, 3, 4]));
fs.symlinkSync("/workspace/note.txt", "/workspace/link.txt");
const linkTarget = fs.readlinkSync("/workspace/link.txt");
fs.linkSync("/workspace/note.txt", "/workspace/hard.txt");
fs.renameSync("/workspace/hard.txt", "/workspace/renamed.txt");
fs.unlinkSync("/workspace/renamed.txt");
fs.rmdirSync("/workspace/subdir");

console.log(JSON.stringify({ stat, lstat, contents, raw, entries, missing, linkTarget }));
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
                String::from("AGENT_OS_NODE_SYNC_RPC_ENABLE"),
                String::from("1"),
            )]),
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
                    "fs.statSync" => execution
                        .respond_sync_rpc_success(
                            request.id,
                            json!({
                                "mode": 0o100644,
                                "size": 14,
                                "isDirectory": false,
                                "isSymbolicLink": false,
                            }),
                        )
                        .expect("respond to stat"),
                    "fs.lstatSync" => execution
                        .respond_sync_rpc_success(
                            request.id,
                            json!({
                                "mode": 0o120777,
                                "size": 19,
                                "isDirectory": false,
                                "isSymbolicLink": true,
                            }),
                        )
                        .expect("respond to lstat"),
                    "fs.readFileSync" => {
                        let path = request.args[0].as_str().expect("read path");
                        let result = match path {
                            "/workspace/note.txt" => json!("hello from rpc"),
                            "/workspace/raw.bin" => json!({
                                "__agentOsType": "bytes",
                                "base64": "q80=",
                            }),
                            other => panic!("unexpected read path: {other}"),
                        };
                        execution
                            .respond_sync_rpc_success(request.id, result)
                            .expect("respond to read");
                    }
                    "fs.existsSync" => execution
                        .respond_sync_rpc_success(request.id, json!(false))
                        .expect("respond to exists"),
                    "fs.readdirSync" => execution
                        .respond_sync_rpc_success(request.id, json!(["note.txt", "raw.bin"]))
                        .expect("respond to readdir"),
                    "fs.mkdirSync" => execution
                        .respond_sync_rpc_success(request.id, json!(null))
                        .expect("respond to mkdir"),
                    "fs.writeFileSync" => {
                        assert_eq!(request.args[0], json!("/workspace/out.bin"));
                        assert_eq!(
                            request.args[1],
                            json!({
                                "__agentOsType": "bytes",
                                "base64": "AQIDBA==",
                            })
                        );
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to write");
                    }
                    "fs.symlinkSync" => {
                        assert_eq!(request.args[0], json!("/workspace/note.txt"));
                        assert_eq!(request.args[1], json!("/workspace/link.txt"));
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to symlink");
                    }
                    "fs.readlinkSync" => execution
                        .respond_sync_rpc_success(request.id, json!("/workspace/note.txt"))
                        .expect("respond to readlink"),
                    "fs.linkSync" => {
                        assert_eq!(request.args[0], json!("/workspace/note.txt"));
                        assert_eq!(request.args[1], json!("/workspace/hard.txt"));
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to link");
                    }
                    "fs.renameSync" => {
                        assert_eq!(request.args[0], json!("/workspace/hard.txt"));
                        assert_eq!(request.args[1], json!("/workspace/renamed.txt"));
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to rename");
                    }
                    "fs.unlinkSync" => execution
                        .respond_sync_rpc_success(request.id, json!(null))
                        .expect("respond to unlink"),
                    "fs.rmdirSync" => execution
                        .respond_sync_rpc_success(request.id, json!(null))
                        .expect("respond to rmdir"),
                    other => panic!("unexpected sync RPC method: {other}"),
                }
            }
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
        vec![
            "fs.statSync",
            "fs.lstatSync",
            "fs.readFileSync",
            "fs.readFileSync",
            "fs.readdirSync",
            "fs.existsSync",
            "fs.mkdirSync",
            "fs.writeFileSync",
            "fs.symlinkSync",
            "fs.readlinkSync",
            "fs.linkSync",
            "fs.renameSync",
            "fs.unlinkSync",
            "fs.rmdirSync",
        ]
    );

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(
        stdout.contains("\"contents\":\"hello from rpc\""),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"raw\":\"abcd\""),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"entries\":[\"note.txt\",\"raw.bin\"]"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"size\":14"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn javascript_execution_routes_fs_promises_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import fs from "node:fs/promises";

await fs.access("./note.txt");
const contents = await fs.readFile("./note.txt", "utf8");
const stat = await fs.stat("./note.txt");
const lstat = await fs.lstat("./note.txt");
const entries = await fs.readdir(".");
await fs.mkdir("./subdir", { recursive: true });
await fs.writeFile("./out.bin", Buffer.from([1, 2, 3, 4]));
await fs.copyFile("./note.txt", "./copied.txt");
await fs.rename("./copied.txt", "./renamed.txt");
await fs.chmod("./renamed.txt", 0o600);
await fs.chown("./renamed.txt", 1000, 1001);
await fs.utimes("./renamed.txt", new Date(1000), new Date(2000));
await fs.unlink("./out.bin");
await fs.rmdir("./subdir");

console.log(
  JSON.stringify({
    contents,
    entries,
    isDir: stat.isDirectory(),
    isSymlink: lstat.isSymbolicLink(),
    size: stat.size,
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

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
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
                    "fs.promises.access" => execution
                        .respond_sync_rpc_success(request.id, json!(null))
                        .expect("respond to access"),
                    "fs.promises.readFile" => execution
                        .respond_sync_rpc_success(request.id, json!("hello from promises rpc"))
                        .expect("respond to readFile"),
                    "fs.promises.stat" => execution
                        .respond_sync_rpc_success(
                            request.id,
                            json!({
                                "mode": 0o100644,
                                "size": 23,
                                "isDirectory": false,
                                "isSymbolicLink": false,
                            }),
                        )
                        .expect("respond to stat"),
                    "fs.promises.lstat" => execution
                        .respond_sync_rpc_success(
                            request.id,
                            json!({
                                "mode": 0o100644,
                                "size": 23,
                                "isDirectory": false,
                                "isSymbolicLink": true,
                            }),
                        )
                        .expect("respond to lstat"),
                    "fs.promises.readdir" => execution
                        .respond_sync_rpc_success(request.id, json!(["note.txt", "raw.bin"]))
                        .expect("respond to readdir"),
                    "fs.promises.mkdir"
                    | "fs.promises.copyFile"
                    | "fs.promises.rename"
                    | "fs.promises.chmod"
                    | "fs.promises.chown"
                    | "fs.promises.utimes"
                    | "fs.promises.unlink"
                    | "fs.promises.rmdir" => execution
                        .respond_sync_rpc_success(request.id, json!(null))
                        .expect("respond to async fs mutation"),
                    "fs.promises.writeFile" => {
                        assert_eq!(request.args[0], json!("/out.bin"));
                        assert_eq!(
                            request.args[1],
                            json!({
                                "__agentOsType": "bytes",
                                "base64": "AQIDBA==",
                            })
                        );
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to writeFile");
                    }
                    other => panic!("unexpected async fs RPC method: {other}"),
                }
            }
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
        vec![
            "fs.promises.access",
            "fs.promises.readFile",
            "fs.promises.stat",
            "fs.promises.lstat",
            "fs.promises.readdir",
            "fs.promises.mkdir",
            "fs.promises.writeFile",
            "fs.promises.copyFile",
            "fs.promises.rename",
            "fs.promises.chmod",
            "fs.promises.chown",
            "fs.promises.utimes",
            "fs.promises.unlink",
            "fs.promises.rmdir",
        ]
    );

    assert_eq!(requests[0].1[0], json!("/note.txt"));
    assert_eq!(
        requests[1].1,
        vec![json!("/note.txt"), json!({ "encoding": "utf8" })]
    );
    assert_eq!(
        requests[5].1,
        vec![json!("/subdir"), json!({ "recursive": true })]
    );
    assert_eq!(
        requests[7].1,
        vec![json!("/note.txt"), json!("/copied.txt"), Value::Null]
    );
    assert_eq!(
        requests[11].1,
        vec![json!("/renamed.txt"), json!(1000), json!(2000)]
    );

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(
        stdout.contains("\"contents\":\"hello from promises rpc\""),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"isDir\":false"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"isSymlink\":true"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"entries\":[\"note.txt\",\"raw.bin\"]"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn javascript_execution_routes_fd_fs_and_streams_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import fs from "node:fs";
import { once } from "node:events";

const fd = fs.openSync("/workspace/data.txt", "r");
const stat = fs.fstatSync(fd);
const buffer = Buffer.alloc(5);
const bytesRead = fs.readSync(fd, buffer, 0, buffer.length, 1);
fs.closeSync(fd);

const fdOut = fs.openSync("/workspace/out.txt", "w");
const written = fs.writeSync(fdOut, Buffer.from("hello"), 0, 5, 0);
fs.closeSync(fdOut);

const asyncSummary = await new Promise((resolve, reject) => {
  fs.open("/workspace/async.txt", "r", (openError, asyncFd) => {
    if (openError) {
      reject(openError);
      return;
    }

    const target = Buffer.alloc(5);
    fs.read(asyncFd, target, 0, 5, 0, (readError, asyncBytesRead) => {
      if (readError) {
        reject(readError);
        return;
      }

      fs.fstat(asyncFd, (statError, asyncStat) => {
        if (statError) {
          reject(statError);
          return;
        }

        fs.close(asyncFd, (closeError) => {
          if (closeError) {
            reject(closeError);
            return;
          }

          resolve({
            asyncBytesRead,
            asyncText: target.toString("utf8"),
            asyncSize: asyncStat.size,
          });
        });
      });
    });
  });
});

const callbackWrite = await new Promise((resolve, reject) => {
  fs.open("/workspace/callback-out.txt", "w", (openError, callbackFd) => {
    if (openError) {
      reject(openError);
      return;
    }

    fs.write(callbackFd, "done", 0, "utf8", (writeError, callbackBytesWritten) => {
      if (writeError) {
        reject(writeError);
        return;
      }

      fs.close(callbackFd, (closeError) => {
        if (closeError) {
          reject(closeError);
          return;
        }

        resolve(callbackBytesWritten);
      });
    });
  });
});

const reader = fs.createReadStream("/workspace/stream.txt", {
  encoding: "utf8",
  start: 0,
  end: 9,
  highWaterMark: 4,
});
const streamChunks = [];
reader.on("data", (chunk) => streamChunks.push(chunk));
await once(reader, "close");

const writer = fs.createWriteStream("/workspace/stream-out.txt", { start: 0 });
writer.write("ab");
writer.end("cd");
await once(writer, "close");

let watchMessage = "";
let watchFileMessage = "";
try {
  fs.watch("/workspace/data.txt");
} catch (error) {
  watchMessage = `${error.code}:${error.message}`;
}
try {
  fs.watchFile("/workspace/data.txt", () => {});
} catch (error) {
  watchFileMessage = `${error.code}:${error.message}`;
}

console.log(
  JSON.stringify({
    text: buffer.toString("utf8"),
    bytesRead,
    size: stat.size,
    written,
    asyncSummary,
    callbackWrite,
    streamChunks,
    watchMessage,
    watchFileMessage,
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

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let files = BTreeMap::from([
        (String::from("/workspace/async.txt"), b"async".to_vec()),
        (String::from("/workspace/data.txt"), b"abcdef".to_vec()),
        (
            String::from("/workspace/stream.txt"),
            b"streamdata".to_vec(),
        ),
    ]);
    let mut fd_paths = BTreeMap::<u64, String>::new();
    let mut next_fd = 40_u64;
    let mut stdout = Vec::new();
    let mut exit_code = None;
    let mut requests = Vec::new();
    let mut writes = Vec::new();

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
                requests.push(request.method.clone());
                match request.method.as_str() {
                    "fs.open" | "fs.openSync" => {
                        let fd = next_fd;
                        next_fd += 1;
                        fd_paths
                            .insert(fd, request.args[0].as_str().expect("open path").to_string());
                        execution
                            .respond_sync_rpc_success(request.id, json!(fd))
                            .expect("respond to open");
                    }
                    "fs.fstat" | "fs.fstatSync" => {
                        let fd = request.args[0].as_u64().expect("fstat fd");
                        let path = fd_paths.get(&fd).expect("tracked fd path");
                        let size = files.get(path).map_or(0, |contents| contents.len());
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "mode": 0o100644,
                                    "size": size,
                                    "isDirectory": false,
                                    "isSymbolicLink": false,
                                }),
                            )
                            .expect("respond to fstat");
                    }
                    "fs.read" | "fs.readSync" => {
                        let fd = request.args[0].as_u64().expect("read fd");
                        let length = request.args[1].as_u64().expect("read length") as usize;
                        let position = request.args[2].as_u64().expect("read position") as usize;
                        let path = fd_paths.get(&fd).expect("tracked read fd");
                        let contents = files.get(path).expect("read file contents");
                        let end = (position + length).min(contents.len());
                        let text = String::from_utf8_lossy(&contents[position..end]).to_string();
                        execution
                            .respond_sync_rpc_success(request.id, json!(text))
                            .expect("respond to read");
                    }
                    "fs.write" | "fs.writeSync" => {
                        let fd = request.args[0].as_u64().expect("write fd");
                        let path = fd_paths.get(&fd).expect("tracked write fd").clone();
                        let payload = if let Some(text) = request.args[1].as_str() {
                            text.to_string()
                        } else {
                            request.args[1]
                                .get("base64")
                                .and_then(Value::as_str)
                                .expect("buffer write payload")
                                .to_string()
                        };
                        let position = request.args.get(2).and_then(Value::as_u64);
                        writes.push((path, payload.clone(), position));
                        let bytes_written = match payload.as_str() {
                            "done" => 4,
                            "aGVsbG8=" => 5,
                            "YWI=" => 2,
                            "Y2Q=" => 2,
                            other => panic!("unexpected write payload: {other}"),
                        };
                        execution
                            .respond_sync_rpc_success(request.id, json!(bytes_written))
                            .expect("respond to write");
                    }
                    "fs.close" | "fs.closeSync" => {
                        let fd = request.args[0].as_u64().expect("close fd");
                        fd_paths.remove(&fd);
                        execution
                            .respond_sync_rpc_success(request.id, json!(null))
                            .expect("respond to close");
                    }
                    other => panic!("unexpected fd RPC method: {other}"),
                }
            }
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    assert_eq!(
        requests,
        vec![
            "fs.openSync",
            "fs.fstatSync",
            "fs.readSync",
            "fs.closeSync",
            "fs.openSync",
            "fs.writeSync",
            "fs.closeSync",
            "fs.open",
            "fs.read",
            "fs.fstat",
            "fs.close",
            "fs.open",
            "fs.write",
            "fs.close",
            "fs.open",
            "fs.read",
            "fs.read",
            "fs.read",
            "fs.close",
            "fs.open",
            "fs.write",
            "fs.write",
            "fs.close",
        ]
    );
    assert_eq!(
        writes,
        vec![
            (
                String::from("/workspace/out.txt"),
                String::from("aGVsbG8="),
                Some(0),
            ),
            (
                String::from("/workspace/callback-out.txt"),
                String::from("done"),
                Some(0),
            ),
            (
                String::from("/workspace/stream-out.txt"),
                String::from("YWI="),
                Some(0),
            ),
            (
                String::from("/workspace/stream-out.txt"),
                String::from("Y2Q="),
                Some(2),
            ),
        ]
    );

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(stdout.contains("\"text\":\"bcdef\""), "stdout: {stdout}");
    assert!(stdout.contains("\"bytesRead\":5"), "stdout: {stdout}");
    assert!(stdout.contains("\"size\":6"), "stdout: {stdout}");
    assert!(stdout.contains("\"written\":5"), "stdout: {stdout}");
    assert!(stdout.contains("\"asyncBytesRead\":5"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"asyncText\":\"async\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("\"asyncSize\":5"), "stdout: {stdout}");
    assert!(stdout.contains("\"callbackWrite\":4"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"streamChunks\":[\"stre\",\"amda\",\"ta\"]"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("ERR_AGENT_OS_FS_WATCH_UNAVAILABLE"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("kernel has no file-watching API"),
        "stdout: {stdout}"
    );
}

#[test]
fn javascript_execution_ignores_guest_overrides_for_internal_node_env() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
console.log(`entrypoint:${process.argv[1]}`);
console.log(`args:${process.argv.slice(2).join(",")}`);
console.log(`node-options:${process.env.NODE_OPTIONS ?? "missing"}`);
console.log(`loader-path:${process.env.AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH ?? "missing"}`);
console.log(`loader-visible:${'AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH' in process.env}`);
console.log(
  `internal-keys:${Object.keys(process.env).filter((key) => key.startsWith("AGENT_OS_")).length}`,
);
"#,
    );
    write_fixture(
        &temp.path().join("evil.mjs"),
        r#"
console.log("evil override executed");
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs"), String::from("safe-arg")],
        BTreeMap::from([
            (
                String::from("AGENT_OS_ENTRYPOINT"),
                String::from("./evil.mjs"),
            ),
            (
                String::from("AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH"),
                String::from("./evil-loader.mjs"),
            ),
            (String::from("NODE_OPTIONS"), String::from("--no-warnings")),
        ]),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("entrypoint:") && line.ends_with("entry.mjs")),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("args:safe-arg"), "stdout: {stdout}");
    assert!(stdout.contains("node-options:missing"), "stdout: {stdout}");
    assert!(stdout.contains("loader-path:missing"), "stdout: {stdout}");
    assert!(stdout.contains("loader-visible:false"), "stdout: {stdout}");
    assert!(stdout.contains("internal-keys:0"), "stdout: {stdout}");
    assert!(
        !stdout.contains("evil override executed"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("loader-path:./evil-loader.mjs"),
        "stdout: {stdout}"
    );
}

#[test]
fn javascript_execution_freezes_guest_time_sources() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const firstDate = Date.now();
const firstConstructed = new Date().getTime();
const firstPerformance = performance.now();

await new Promise((resolve) => setTimeout(resolve, 25));

const secondDate = Date.now();
const secondConstructed = new Date().getTime();
const secondPerformance = performance.now();

console.log(
  JSON.stringify({
    sameDate: firstDate === secondDate,
    sameConstructed: firstConstructed === secondConstructed,
    samePerformance: firstPerformance === secondPerformance,
    performanceZero: firstPerformance === 0 && secondPerformance === 0,
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

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("\"sameDate\":true"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"sameConstructed\":true"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"samePerformance\":true"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"performanceZero\":true"),
        "stdout: {stdout}"
    );
}

#[test]
fn javascript_date_function_without_new_uses_frozen_time() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const expected = new Date(Date.now()).toString();
await new Promise((resolve) => setTimeout(resolve, 1200));
const actual = Date();

console.log(
  JSON.stringify({
    actual,
    expected,
    matches: actual === expected,
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

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("\"matches\":true"), "stdout: {stdout}");
}

#[test]
fn javascript_execution_generates_and_reuses_compile_cache_without_leaking_module_state() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("dep.mjs"),
        r#"
globalThis.__agentOsDepInitCount = (globalThis.__agentOsDepInitCount ?? 0) + 1;
console.log(`dep-init:${globalThis.__agentOsDepInitCount}`);
export const answer = 41;
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { answer } from "./dep.mjs";
console.log(`entry:${answer + 1}:${globalThis.__agentOsDepInitCount}`);
"#,
    );

    let mut first_engine = JavascriptExecutionEngine::default();
    let first_context = first_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let first_cache_dir = first_context
        .compile_cache_dir
        .clone()
        .expect("compile cache dir");

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut first_engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("dep-init:1"));
    assert!(first_stdout.contains("entry:42:1"));
    assert!(first_stderr.contains("was not initialized"));

    let cache_files = collect_files(&first_cache_dir);
    assert!(
        cache_files.len() >= 2,
        "expected cache files in {first_cache_dir:?}, got {cache_files:?}"
    );

    let mut second_engine = JavascriptExecutionEngine::default();
    let second_context = second_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    assert_eq!(second_context.compile_cache_dir, Some(first_cache_dir));

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut second_engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("dep-init:1"));
    assert!(second_stdout.contains("entry:42:1"));
    assert!(second_stderr.contains("was accepted"));
    assert!(second_stderr.contains("skip persisting"));
}

#[test]
fn javascript_execution_invalidates_compile_cache_when_imported_source_changes() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { answer } from "./dep.mjs";
console.log(`entry:${answer}`);
"#,
    );

    let mut first_engine = JavascriptExecutionEngine::default();
    let first_context = first_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut first_engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("entry:41"));
    assert!(first_stderr.contains("was not initialized"));

    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 42;\n");

    let mut second_engine = JavascriptExecutionEngine::default();
    let second_context = second_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut second_engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("entry:42"));
    assert!(second_stderr.contains("code hash mismatch"));
    assert!(second_stderr.contains("was not initialized"));
}

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

    let mut engine = JavascriptExecutionEngine::default();
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
    assert_eq!(first_warmup.import_count, 4);

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

    let mut engine = JavascriptExecutionEngine::default();
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
fn javascript_execution_reuses_resolution_and_metadata_caches_across_contexts() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(&temp.path().join("dep.js"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.js");
console.log(`answer:${dep.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));
    assert_eq!(first_metrics.resolve_hits, 0);
    assert!(first_metrics.resolve_misses >= 1);

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:41"));
    assert!(second_metrics.resolve_hits >= 2);
    assert!(second_metrics.package_type_hits >= 1);
    assert!(second_metrics.module_format_hits >= 1);
}

#[test]
fn javascript_execution_invalidates_bare_package_resolution_when_package_metadata_changes() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let package_dir = temp.path().join("node_modules/demo-pkg");
    fs::create_dir_all(&package_dir).expect("create package dir");

    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-pkg\",\n  \"type\": \"module\",\n  \"exports\": \"./entry.js\"\n}\n",
    );
    write_fixture(&package_dir.join("entry.js"), "export const answer = 41;\n");
    write_fixture(
        &package_dir.join("replacement.js"),
        "export const answer = 42;\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const pkg = await import("demo-pkg");
console.log(`pkg:${pkg.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("pkg:41"));
    assert!(first_metrics.resolve_misses >= 1);

    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-pkg\",\n  \"type\": \"module\",\n  \"exports\": \"./replacement.js\"\n}\n",
    );

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("pkg:42"));
    assert!(second_metrics.resolve_misses >= 1);
}

#[test]
fn javascript_execution_invalidates_package_type_and_module_format_caches() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(&temp.path().join("dep.js"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.js");
const answer = dep.answer ?? dep.default.answer;
console.log(`answer:${answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, _, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));

    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"commonjs\"\n}\n",
    );
    write_fixture(
        &temp.path().join("dep.js"),
        "module.exports = { answer: 42 };\n",
    );

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:42"));
    assert!(second_metrics.package_type_misses >= 1);
    assert!(second_metrics.module_format_misses >= 1);
}

#[test]
fn javascript_execution_keeps_cjs_fs_requires_extensible_when_loaded_via_esm() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.cjs"),
        r#"
const fs = require("fs");
const marker = Symbol.for("agent-os.fs-marker");
let extensible = Object.isExtensible(fs);
let canDefine = false;

try {
  Object.defineProperty(fs, marker, {
    configurable: true,
    value: true,
  });
  canDefine = fs[marker] === true;
} catch {
  canDefine = false;
}

module.exports = {
  extensible,
  canDefine,
  existsSyncType: typeof fs.existsSync,
};
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import result from "./dep.cjs";
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, _, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0);
    assert!(stdout.contains(r#""extensible":true"#), "{stdout}");
    assert!(stdout.contains(r#""canDefine":true"#), "{stdout}");
    assert!(
        stdout.contains(r#""existsSyncType":"function""#),
        "{stdout}"
    );
}

#[test]
fn javascript_execution_preserves_source_changes_with_cached_resolution() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.mjs");
console.log(`answer:${dep.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, _, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));

    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 42;\n");

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:42"));
    assert!(second_metrics.resolve_hits >= 2);
}

#[test]
fn javascript_execution_reuses_and_invalidates_projected_package_source_cache() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let projected_root = temp.path().join("projected-node-modules");
    let package_dir = projected_root.join("demo-projected");
    fs::create_dir_all(&package_dir).expect("create projected package dir");
    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-projected\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(
        &package_dir.join("entry.js"),
        "import { readFileSync } from 'node:fs';\nexport const answer = 41;\nexport const fsReady = typeof readFileSync === 'function';\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const mod = await import("/root/node_modules/demo-projected/entry.js");
console.log(`answer:${mod.answer}`);
console.log(`fsReady:${mod.fsReady}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let projected_root_host_path = projected_root.to_string_lossy().replace('\\', "\\\\");
    let extra_fs_read_paths_json = format!(
        "[\"{}\"]",
        projected_root.to_string_lossy().replace('\\', "\\\\")
    );
    let debug_env = BTreeMap::from([
        (
            String::from("AGENT_OS_EXTRA_FS_READ_PATHS"),
            extra_fs_read_paths_json,
        ),
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!(
                "[{{\"guestPath\":\"/root/node_modules\",\"hostPath\":\"{projected_root_host_path}\"}}]"
            ),
        ),
        (
            String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
            String::from("1"),
        ),
    ]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0, "stderr: {first_stderr}");
    assert!(first_stdout.contains("answer:41"), "stdout: {first_stdout}");
    assert!(
        first_stdout.contains("fsReady:true"),
        "stdout: {first_stdout}"
    );
    assert_eq!(first_metrics.source_hits, 0, "stderr: {first_stderr}");
    assert!(first_metrics.source_misses >= 1, "stderr: {first_stderr}");

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0, "stderr: {second_stderr}");
    assert!(
        second_stdout.contains("answer:41"),
        "stdout: {second_stdout}"
    );
    assert!(second_metrics.source_hits >= 1, "stderr: {second_stderr}");

    write_fixture(
        &package_dir.join("entry.js"),
        "import { readFileSync } from 'node:fs';\nexport const answer = 42;\nexport const fsReady = typeof readFileSync === 'function';\n",
    );

    let third_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (third_stdout, third_stderr, third_exit) = run_javascript_execution(
        &mut engine,
        third_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let third_metrics = parse_import_cache_metrics(&third_stderr);

    assert_eq!(third_exit, 0, "stderr: {third_stderr}");
    assert!(third_stdout.contains("answer:42"), "stdout: {third_stdout}");
    assert!(
        third_stdout.contains("fsReady:true"),
        "stdout: {third_stdout}"
    );
    assert!(third_metrics.source_misses >= 1, "stderr: {third_stderr}");
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

    let mut engine = JavascriptExecutionEngine::default();
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
fn javascript_execution_virtualizes_process_cwd_and_denies_chdir() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  cwd: process.cwd(),
};

try {
  process.chdir("/other");
  result.chdir = "unexpected";
} catch (error) {
  result.chdir = {
    code: error.code ?? null,
    message: error.message,
  };
}

result.cwdAfter = process.cwd();
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse cwd JSON");
    assert_eq!(parsed["cwd"], Value::String(String::from("/root")));
    assert_eq!(parsed["cwdAfter"], Value::String(String::from("/root")));
    assert_eq!(
        parsed["chdir"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["chdir"]["message"]
        .as_str()
        .expect("chdir message")
        .contains("process.chdir"));
}

#[test]
fn javascript_execution_virtualizes_process_identity() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  execPath: process.execPath,
  argv0: process.argv[0],
  pid: process.pid,
  ppid: process.ppid,
  uid: typeof process.getuid === "function" ? process.getuid() : null,
  gid: typeof process.getgid === "function" ? process.getgid() : null,
};

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH"),
            String::from("/usr/bin/node"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_PID"),
            String::from("41"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_PPID"),
            String::from("7"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_UID"),
            String::from("0"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_GID"),
            String::from("0"),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse process identity JSON");
    assert_eq!(
        parsed["execPath"],
        Value::String(String::from("/usr/bin/node"))
    );
    assert_eq!(
        parsed["argv0"],
        Value::String(String::from("/usr/bin/node"))
    );
    assert_eq!(parsed["pid"], Value::from(41));
    assert_eq!(parsed["ppid"], Value::from(7));
    assert_eq!(parsed["uid"], Value::from(0));
    assert_eq!(parsed["gid"], Value::from(0));
}

#[test]
fn javascript_execution_blocks_remaining_process_property_leaks() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
function summarize(mod) {
  return {
    platform: mod.platform,
    arch: mod.arch,
    version: mod.version,
    release: mod.release,
    config: mod.config,
    versions: mod.versions,
    memoryUsage: typeof mod.memoryUsage === "function" ? mod.memoryUsage() : null,
    memoryUsageRss:
      typeof mod.memoryUsage === "function" && typeof mod.memoryUsage.rss === "function"
        ? mod.memoryUsage.rss()
        : null,
    uptime: typeof mod.uptime === "function" ? mod.uptime() : null,
  };
}

const result = {
  globalProcess: summarize(process),
  requireProcess: summarize(require("node:process")),
  builtinProcess: summarize(process.getBuiltinModule("node:process")),
};

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_VIRTUAL_OS_ARCH"),
            String::from("arm64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
            String::from("v24.0.0"),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse process leak JSON");
    for key in ["globalProcess", "requireProcess", "builtinProcess"] {
        let summary = &parsed[key];
        assert_eq!(summary["platform"], Value::String(String::from("linux")));
        assert_eq!(summary["arch"], Value::String(String::from("arm64")));
        assert_eq!(summary["version"], Value::String(String::from("v24.0.0")));
        assert_eq!(
            summary["release"]["name"],
            Value::String(String::from("node"))
        );
        assert_eq!(
            summary["release"]["lts"],
            Value::String(String::from("Agent OS"))
        );
        assert!(summary["release"]["sourceUrl"].is_null());
        assert!(summary["release"]["headersUrl"].is_null());
        assert_eq!(
            summary["config"]["variables"]["host_arch"],
            Value::String(String::from("arm64"))
        );
        assert_eq!(
            summary["config"]["variables"]["node_shared"],
            Value::Bool(false)
        );
        assert_eq!(
            summary["config"]["variables"]["node_use_openssl"],
            Value::Bool(false)
        );
        assert_eq!(
            summary["versions"]["node"],
            Value::String(String::from("24.0.0"))
        );
        assert_eq!(
            summary["versions"]["openssl"],
            Value::String(String::from("0.0.0"))
        );
        assert_eq!(
            summary["versions"]["v8"],
            Value::String(String::from("0.0"))
        );
        assert_eq!(
            summary["versions"]["zlib"],
            Value::String(String::from("0.0.0"))
        );

        let memory_usage = summary["memoryUsage"]
            .as_object()
            .expect("memory usage object");
        for field in ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"] {
            assert!(
                memory_usage[field].as_u64().unwrap_or_default() > 0
                    || field == "external"
                    || field == "arrayBuffers"
            );
        }
        assert_eq!(
            summary["memoryUsageRss"], summary["memoryUsage"]["rss"],
            "memoryUsage.rss() should match memoryUsage().rss for {key}"
        );
        let uptime = summary["uptime"].as_f64().expect("uptime number");
        assert!(uptime >= 0.0, "uptime should not be negative for {key}");
        assert!(
            uptime < 5.0,
            "uptime should be VM-scoped for {key}, got {uptime}"
        );
    }
}

#[test]
fn javascript_execution_virtualizes_os_module() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import os from "node:os";

function summarize(mod) {
  return {
    hostname: mod.hostname(),
    cpus: mod.cpus(),
    totalmem: mod.totalmem(),
    freemem: mod.freemem(),
    homedir: mod.homedir(),
    tmpdir: mod.tmpdir(),
    platform: mod.platform(),
    type: mod.type(),
    release: mod.release(),
    version: typeof mod.version === "function" ? mod.version() : null,
    arch: typeof mod.arch === "function" ? mod.arch() : null,
    machine: typeof mod.machine === "function" ? mod.machine() : null,
    availableParallelism:
      typeof mod.availableParallelism === "function"
        ? mod.availableParallelism()
        : null,
    loadavg: typeof mod.loadavg === "function" ? mod.loadavg() : null,
    uptime: typeof mod.uptime === "function" ? mod.uptime() : null,
    networkInterfaces: mod.networkInterfaces(),
    userInfo: mod.userInfo(),
    userInfoBuffer: mod.userInfo({ encoding: "buffer" }),
    getPriority: typeof mod.getPriority === "function" ? mod.getPriority(0) : null,
  };
}

const result = {
  importOs: summarize(os),
  requireOs: summarize(require("node:os")),
  builtinOs: summarize(process.getBuiltinModule("node:os")),
};

try {
  os.setPriority(0, 0);
  result.setPriority = "unexpected";
} catch (error) {
  result.setPriority = {
    code: error.code ?? null,
    message: error.message,
  };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (String::from("HOME"), String::from("/root")),
        (String::from("SHELL"), String::from("/bin/bash")),
        (String::from("AGENT_OS_VIRTUAL_PROCESS_UID"), String::from("0")),
        (String::from("AGENT_OS_VIRTUAL_PROCESS_GID"), String::from("0")),
        (
            String::from("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
            String::from("agent-os-test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_CPU_COUNT"),
            String::from("4"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_CPU_MODEL"),
            String::from("Agent OS Test CPU"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_TOTALMEM"),
            String::from("2147483648"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_FREEMEM"),
            String::from("1073741824"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_RELEASE"),
            String::from("6.8.0-agent-os-test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_VERSION"),
            String::from("#1 SMP PREEMPT_DYNAMIC Agent OS Test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_ARCH"),
            String::from("x64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_MACHINE"),
            String::from("x86_64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_USER"),
            String::from("agent"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_SHELL"),
            String::from("/bin/bash"),
        ),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"os\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            ),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse os JSON");

    for surface in ["importOs", "requireOs", "builtinOs"] {
        assert_eq!(
            parsed[surface]["hostname"],
            Value::String(String::from("agent-os-test"))
        );
        assert_eq!(
            parsed[surface]["homedir"],
            Value::String(String::from("/root"))
        );
        assert_eq!(
            parsed[surface]["tmpdir"],
            Value::String(String::from("/tmp"))
        );
        assert_eq!(
            parsed[surface]["platform"],
            Value::String(String::from("linux"))
        );
        assert_eq!(
            parsed[surface]["type"],
            Value::String(String::from("Linux"))
        );
        assert_eq!(
            parsed[surface]["release"],
            Value::String(String::from("6.8.0-agent-os-test"))
        );
        assert_eq!(
            parsed[surface]["version"],
            Value::String(String::from("#1 SMP PREEMPT_DYNAMIC Agent OS Test"))
        );
        assert_eq!(parsed[surface]["arch"], Value::String(String::from("x64")));
        assert_eq!(
            parsed[surface]["machine"],
            Value::String(String::from("x86_64"))
        );
        assert_eq!(parsed[surface]["availableParallelism"], Value::from(4));
        assert_eq!(parsed[surface]["totalmem"], Value::from(2_147_483_648_u64));
        assert_eq!(parsed[surface]["freemem"], Value::from(1_073_741_824_u64));
        assert_eq!(parsed[surface]["loadavg"], json!([0, 0, 0]));
        assert_eq!(parsed[surface]["uptime"], Value::from(0));
        assert_eq!(parsed[surface]["getPriority"], Value::from(0));
        assert_eq!(parsed[surface]["cpus"].as_array().map(Vec::len), Some(4));
        assert_eq!(
            parsed[surface]["cpus"][0]["model"],
            Value::String(String::from("Agent OS Test CPU"))
        );
        assert_eq!(
            parsed[surface]["userInfo"]["username"],
            Value::String(String::from("agent"))
        );
        assert_eq!(parsed[surface]["userInfo"]["uid"], Value::from(0));
        assert_eq!(parsed[surface]["userInfo"]["gid"], Value::from(0));
        assert_eq!(
            parsed[surface]["userInfo"]["shell"],
            Value::String(String::from("/bin/bash"))
        );
        assert_eq!(
            parsed[surface]["userInfo"]["homedir"],
            Value::String(String::from("/root"))
        );
        assert_eq!(
            parsed[surface]["userInfoBuffer"]["username"]["type"],
            Value::String(String::from("Buffer"))
        );
        assert_eq!(
            parsed[surface]["userInfoBuffer"]["shell"]["type"],
            Value::String(String::from("Buffer"))
        );

        let interfaces = parsed[surface]["networkInterfaces"]
            .as_object()
            .expect("network interfaces object");
        assert_eq!(interfaces.len(), 1);
        assert!(interfaces.contains_key("lo"));
        let loopback = interfaces["lo"].as_array().expect("loopback interfaces");
        assert_eq!(loopback.len(), 2);
        assert_eq!(
            loopback[0]["address"],
            Value::String(String::from("127.0.0.1"))
        );
        assert_eq!(loopback[0]["internal"], Value::Bool(true));
        assert_eq!(loopback[1]["address"], Value::String(String::from("::1")));
    }

    assert_eq!(
        parsed["setPriority"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["setPriority"]["message"]
        .as_str()
        .expect("setPriority message")
        .contains("os.setPriority"));
}

#[test]
fn javascript_execution_os_module_safe_defaults_ignore_host_env() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import os from "node:os";

console.log(JSON.stringify({
  hostname: os.hostname(),
  homedir: os.homedir(),
  tmpdir: os.tmpdir(),
  userInfo: os.userInfo(),
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([
        (
            String::from("HOME"),
            String::from("/Users/host-user/should-not-leak"),
        ),
        (
            String::from("USER"),
            String::from("host-user-should-not-leak"),
        ),
        (
            String::from("LOGNAME"),
            String::from("host-logname-should-not-leak"),
        ),
        (
            String::from("TMPDIR"),
            String::from("/var/folders/host-tmp-should-not-leak"),
        ),
        (
            String::from("TEMP"),
            String::from("/tmp/host-temp-should-not-leak"),
        ),
        (
            String::from("TMP"),
            String::from("/tmp/host-tmp-should-not-leak"),
        ),
        (
            String::from("HOSTNAME"),
            String::from("host-machine-should-not-leak"),
        ),
        (String::from("SHELL"), String::from("/bin/zsh")),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"os\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            ),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse os defaults JSON");

    assert_eq!(parsed["hostname"], Value::String(String::from("agent-os")));
    assert_eq!(parsed["homedir"], Value::String(String::from("/root")));
    assert_eq!(parsed["tmpdir"], Value::String(String::from("/tmp")));
    assert_eq!(
        parsed["userInfo"]["username"],
        Value::String(String::from("root"))
    );
    assert_eq!(
        parsed["userInfo"]["shell"],
        Value::String(String::from("/bin/sh"))
    );
    assert_eq!(
        parsed["userInfo"]["homedir"],
        Value::String(String::from("/root"))
    );
}

#[test]
fn javascript_execution_denies_process_signal_handlers_and_native_addons() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("addon.node"), "not-a-real-native-addon\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { fileURLToPath } from 'node:url';

const addonPath = fileURLToPath(new URL('./addon.node', import.meta.url));
const result = {};

try {
  const returned = process.on('beforeExit', () => {});
  result.nonSignalReturnedSelf = returned === process;
  process.removeAllListeners('beforeExit');
} catch (error) {
  result.nonSignal = { code: error.code ?? null, message: error.message };
}

try {
  process.on('SIGTERM', () => {});
  result.signalOn = 'unexpected';
} catch (error) {
  result.signalOn = { code: error.code ?? null, message: error.message };
}

try {
  process.once('SIGINT', () => {});
  result.signalOnce = 'unexpected';
} catch (error) {
  result.signalOnce = { code: error.code ?? null, message: error.message };
}

try {
  process.dlopen({}, addonPath);
  result.dlopen = 'unexpected';
} catch (error) {
  result.dlopen = { code: error.code ?? null, message: error.message };
}

try {
  require(addonPath);
  result.nativeAddon = 'unexpected';
} catch (error) {
  result.nativeAddon = { code: error.code ?? null, message: error.message };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse hardening JSON");
    assert_eq!(parsed["nonSignalReturnedSelf"], Value::Bool(true));
    assert_eq!(
        parsed["signalOn"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["signalOn"]["message"]
        .as_str()
        .expect("signal on message")
        .contains("process.on(SIGTERM)"));
    assert_eq!(
        parsed["signalOnce"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["signalOnce"]["message"]
        .as_str()
        .expect("signal once message")
        .contains("process.once(SIGINT)"));
    assert_eq!(
        parsed["dlopen"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["dlopen"]["message"]
        .as_str()
        .expect("dlopen message")
        .contains("process.dlopen"));
    assert_eq!(
        parsed["nativeAddon"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["nativeAddon"]["message"]
        .as_str()
        .expect("native addon message")
        .contains("native addon loading"));
}

#[test]
fn javascript_execution_still_starts_with_fail_closed_property_hardening() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
console.log(JSON.stringify({
  envType: typeof process.env,
  cwdType: typeof process.cwd,
  execPathType: typeof process.execPath,
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse hardening JSON");
    assert_eq!(parsed["envType"], Value::String(String::from("object")));
    assert_eq!(parsed["cwdType"], Value::String(String::from("function")));
    assert_eq!(
        parsed["execPathType"],
        Value::String(String::from("string"))
    );
}

#[test]
fn javascript_execution_hardens_exec_and_execsync_child_process_calls() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const { exec, execSync } = require('node:child_process');
const execAsync = (command) =>
  new Promise((resolve, reject) => {
    exec(command, (error, stdout, stderr) => {
      if (error) {
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
        return;
      }

      resolve({ stdout, stderr });
    });
  });

console.log(JSON.stringify({
  execSync: JSON.parse(execSync('node ./child.mjs sync', { encoding: 'utf8' }).trim()),
  exec: JSON.parse((await execAsync('node ./child.mjs async')).stdout.trim()),
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            ),
        ),
    ]);
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut next_child_pid = 40_u64;
    let mut child_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "child_process.spawn" => {
                        let payload = request.args[0].as_object().expect("spawn payload");
                        let command = payload["command"].as_str().expect("spawn command");
                        let args = payload["args"]
                            .as_array()
                            .expect("spawn args")
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        let shell = payload["options"]["shell"].as_bool().unwrap_or(false);
                        let marker = if shell {
                            command
                                .split_whitespace()
                                .last()
                                .expect("shell marker")
                                .to_owned()
                        } else {
                            args.last().expect("spawn marker").clone()
                        };
                        let child_id = format!("child-{next_child_pid}");
                        let stdout_payload = format!("{{\"marker\":\"{marker}\"}}\n");
                        child_events.insert(
                            child_id.clone(),
                            vec![
                                json!({
                                    "type": "stdout",
                                    "data": stdout_payload,
                                }),
                                json!({
                                    "type": "exit",
                                    "exitCode": 0,
                                }),
                            ],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "childId": child_id,
                                    "pid": next_child_pid,
                                    "command": command,
                                    "args": args,
                                }),
                            )
                            .expect("respond to child_process.spawn");
                        next_child_pid += 1;
                    }
                    "child_process.poll" => {
                        let child_id = request.args[0].as_str().expect("poll child id");
                        let next = child_events
                            .get_mut(child_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to child_process.poll");
                    }
                    other => panic!("unexpected child_process sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse child_process JSON");
    assert_eq!(
        parsed["execSync"]["marker"],
        Value::String(String::from("sync"))
    );
    assert_eq!(
        parsed["exec"]["marker"],
        Value::String(String::from("async"))
    );
    assert!(methods.iter().any(|method| method == "child_process.spawn"));
    assert!(methods.iter().any(|method| method == "child_process.poll"));
}

#[test]
fn javascript_execution_routes_net_connect_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import net from "node:net";

const summary = await new Promise((resolve, reject) => {
  const socket = net.createConnection({ host: "127.0.0.1", port: 43199 });
  let data = "";
  let ended = false;
  socket.setEncoding("utf8");
  socket.on("connect", () => {
    socket.write("ping");
  });
  socket.on("data", (chunk) => {
    data += chunk;
  });
  socket.on("end", () => {
    ended = true;
  });
  socket.on("error", reject);
  socket.on("close", (hadError) => {
    resolve({
      data,
      ended,
      hadError,
      localPort: socket.localPort,
      remoteAddress: socket.remoteAddress,
      remotePort: socket.remotePort,
    });
  });
});

console.log(JSON.stringify(summary));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut socket_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "net.connect" => {
                        socket_events.insert(
                            String::from("socket-1"),
                            vec![
                                json!({
                                    "type": "data",
                                    "data": "pong",
                                }),
                                json!({
                                    "type": "end",
                                }),
                                json!({
                                    "type": "close",
                                    "hadError": false,
                                }),
                            ],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "socketId": "socket-1",
                                    "localAddress": "127.0.0.1",
                                    "localPort": 42001,
                                    "remoteAddress": "127.0.0.1",
                                    "remotePort": 43199,
                                    "remoteFamily": "IPv4",
                                }),
                            )
                            .expect("respond to net.connect");
                    }
                    "net.write" => {
                        assert_eq!(
                            request.args[0].as_str(),
                            Some("socket-1"),
                            "unexpected socket id for write",
                        );
                        execution
                            .respond_sync_rpc_success(request.id, json!(4))
                            .expect("respond to net.write");
                    }
                    "net.shutdown" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.shutdown");
                    }
                    "net.destroy" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.destroy");
                    }
                    "net.poll" => {
                        let socket_id = request.args[0].as_str().expect("poll socket id");
                        let next = socket_events
                            .get_mut(socket_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.poll");
                    }
                    other => panic!("unexpected net sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse net JSON");
    assert_eq!(parsed["data"], Value::String(String::from("pong")));
    assert_eq!(parsed["ended"], Value::Bool(true));
    assert_eq!(parsed["hadError"], Value::Bool(false));
    assert_eq!(
        parsed["remoteAddress"],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["remotePort"], Value::from(43199));
    assert!(methods.iter().any(|method| method == "net.connect"));
    assert!(methods.iter().any(|method| method == "net.write"));
    assert!(methods.iter().any(|method| method == "net.shutdown"));
    assert!(methods.iter().any(|method| method == "net.poll"));
}

#[test]
fn javascript_execution_routes_net_create_server_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import net from "node:net";

const summary = await new Promise((resolve, reject) => {
  const server = net.createServer({ allowHalfOpen: false }, (socket) => {
    let data = "";
    let connections = -1;
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      data += chunk;
      server.getConnections((error, count) => {
        if (error) {
          reject(error);
          return;
        }
        connections = count;
        socket.end("pong");
      });
    });
    socket.on("error", reject);
    socket.on("close", () => {
      server.close(() => {
        resolve({
          address: server.address(),
          connections,
          data,
          localPort: socket.localPort,
          remoteAddress: socket.remoteAddress,
          remotePort: socket.remotePort,
        });
      });
    });
  });
  server.on("error", reject);
  server.listen({ port: 43111, host: "127.0.0.1", backlog: 2 });
});

console.log(JSON.stringify(summary));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut listener_events = BTreeMap::<String, Vec<Value>>::new();
    let mut socket_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "net.listen" => {
                        assert_eq!(request.args[0]["backlog"], Value::from(2));
                        listener_events.insert(
                            String::from("listener-1"),
                            vec![json!({
                                "type": "connection",
                                "socketId": "socket-1",
                                "localAddress": "127.0.0.1",
                                "localPort": 43111,
                                "remoteAddress": "127.0.0.1",
                                "remotePort": 54000,
                                "remoteFamily": "IPv4",
                            })],
                        );
                        socket_events.insert(
                            String::from("socket-1"),
                            vec![
                                json!({
                                    "type": "data",
                                    "data": "ping",
                                }),
                                json!({
                                    "type": "end",
                                }),
                                json!({
                                    "type": "close",
                                    "hadError": false,
                                }),
                            ],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "serverId": "listener-1",
                                    "localAddress": "127.0.0.1",
                                    "localPort": 43111,
                                    "family": "IPv4",
                                }),
                            )
                            .expect("respond to net.listen");
                    }
                    "net.server_poll" => {
                        let listener_id = request.args[0].as_str().expect("poll listener id");
                        let next = listener_events
                            .get_mut(listener_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.server_poll");
                    }
                    "net.server_connections" => {
                        execution
                            .respond_sync_rpc_success(request.id, json!(1))
                            .expect("respond to net.server_connections");
                    }
                    "net.poll" => {
                        let socket_id = request.args[0].as_str().expect("poll socket id");
                        let next = socket_events
                            .get_mut(socket_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.poll");
                    }
                    "net.write" => {
                        assert_eq!(request.args[0].as_str(), Some("socket-1"));
                        execution
                            .respond_sync_rpc_success(request.id, json!(4))
                            .expect("respond to net.write");
                    }
                    "net.shutdown" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.shutdown");
                    }
                    "net.server_close" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.server_close");
                    }
                    "net.destroy" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.destroy");
                    }
                    other => panic!("unexpected net sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse net JSON");
    assert_eq!(parsed["connections"], Value::from(1));
    assert_eq!(parsed["data"], Value::String(String::from("ping")));
    assert_eq!(
        parsed["address"]["address"],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["address"]["port"], Value::from(43111));
    assert_eq!(
        parsed["remoteAddress"],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["remotePort"], Value::from(54000));
    assert!(methods.iter().any(|method| method == "net.listen"));
    assert!(methods.iter().any(|method| method == "net.server_poll"));
    assert!(methods
        .iter()
        .any(|method| method == "net.server_connections"));
    assert!(methods.iter().any(|method| method == "net.poll"));
    assert!(methods.iter().any(|method| method == "net.write"));
    assert!(methods.iter().any(|method| method == "net.shutdown"));
    assert!(methods.iter().any(|method| method == "net.server_close"));
}

#[test]
fn javascript_execution_routes_net_connect_path_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import net from "node:net";

const summary = await new Promise((resolve, reject) => {
  const socket = net.createConnection({ path: "/tmp/agent-os.sock" });
  socket.on("connect", () => {
    socket.end();
  });
  socket.on("error", reject);
  socket.on("close", (hadError) => {
    resolve({
      hadError,
      remoteAddress: socket.remoteAddress,
      address: socket.address(),
    });
  });
});

console.log(JSON.stringify(summary));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut socket_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "net.connect" => {
                        assert_eq!(
                            request.args[0]["path"],
                            Value::String(String::from("/tmp/agent-os.sock"))
                        );
                        socket_events.insert(
                            String::from("unix-socket-1"),
                            vec![json!({
                                "type": "close",
                                "hadError": false,
                            })],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "socketId": "unix-socket-1",
                                    "remotePath": "/tmp/agent-os.sock",
                                }),
                            )
                            .expect("respond to net.connect");
                    }
                    "net.shutdown" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.shutdown");
                    }
                    "net.destroy" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.destroy");
                    }
                    "net.poll" => {
                        let socket_id = request.args[0].as_str().expect("poll socket id");
                        let next = socket_events
                            .get_mut(socket_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.poll");
                    }
                    other => panic!("unexpected net sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse net JSON");
    assert_eq!(parsed["hadError"], Value::Bool(false));
    assert_eq!(
        parsed["remoteAddress"],
        Value::String(String::from("/tmp/agent-os.sock"))
    );
    assert_eq!(parsed["address"], Value::Null);
    assert!(methods.iter().any(|method| method == "net.connect"));
    assert!(methods.iter().any(|method| method == "net.shutdown"));
    assert!(methods.iter().any(|method| method == "net.poll"));
}

#[test]
fn javascript_execution_routes_net_listen_path_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import net from "node:net";

const summary = await new Promise((resolve, reject) => {
  const server = net.createServer((socket) => {
    socket.on("error", reject);
    socket.on("close", () => {
      server.close(() => {
        resolve({
          address: server.address(),
          localAddress: socket.localAddress,
        });
      });
    });
    socket.end();
  });
  server.on("error", reject);
  server.listen({ path: "/tmp/agent-os.sock", backlog: 2 });
});

console.log(JSON.stringify(summary));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut listener_events = BTreeMap::<String, Vec<Value>>::new();
    let mut socket_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "net.listen" => {
                        assert_eq!(
                            request.args[0]["path"],
                            Value::String(String::from("/tmp/agent-os.sock"))
                        );
                        assert_eq!(request.args[0]["backlog"], Value::from(2));
                        listener_events.insert(
                            String::from("unix-listener-1"),
                            vec![json!({
                                "type": "connection",
                                "socketId": "unix-socket-1",
                                "localPath": "/tmp/agent-os.sock",
                                "remotePath": Value::Null,
                            })],
                        );
                        socket_events.insert(
                            String::from("unix-socket-1"),
                            vec![json!({
                                "type": "close",
                                "hadError": false,
                            })],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "serverId": "unix-listener-1",
                                    "path": "/tmp/agent-os.sock",
                                }),
                            )
                            .expect("respond to net.listen");
                    }
                    "net.server_poll" => {
                        let listener_id = request.args[0].as_str().expect("poll listener id");
                        let next = listener_events
                            .get_mut(listener_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.server_poll");
                    }
                    "net.poll" => {
                        let socket_id = request.args[0].as_str().expect("poll socket id");
                        let next = socket_events
                            .get_mut(socket_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to net.poll");
                    }
                    "net.shutdown" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.shutdown");
                    }
                    "net.server_close" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.server_close");
                    }
                    "net.destroy" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to net.destroy");
                    }
                    other => panic!("unexpected net sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse net JSON");
    assert_eq!(
        parsed["address"],
        Value::String(String::from("/tmp/agent-os.sock"))
    );
    assert_eq!(
        parsed["localAddress"],
        Value::String(String::from("/tmp/agent-os.sock"))
    );
    assert!(methods.iter().any(|method| method == "net.listen"));
    assert!(methods.iter().any(|method| method == "net.server_poll"));
    assert!(methods.iter().any(|method| method == "net.poll"));
    assert!(methods.iter().any(|method| method == "net.shutdown"));
    assert!(methods.iter().any(|method| method == "net.server_close"));
}

#[test]
fn javascript_execution_routes_dgram_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import dgram from "node:dgram";

const socket = dgram.createSocket("udp4");
socket.on("error", (error) => {
  console.error(error.stack ?? error.message);
  process.exit(1);
});

const summary = await new Promise((resolve) => {
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

  socket.bind(43112, "127.0.0.1", () => {
    socket.send("ping", 43199, "127.0.0.1");
  });
});

console.log(JSON.stringify(summary));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dgram\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut socket_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "dgram.createSocket" => {
                        socket_events.insert(
                            String::from("udp-socket-1"),
                            vec![json!({
                                "type": "message",
                                "data": {
                                    "__agentOsType": "bytes",
                                    "base64": "cG9uZw==",
                                },
                                "remoteAddress": "127.0.0.1",
                                "remotePort": 43199,
                                "remoteFamily": "IPv4",
                            })],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "socketId": "udp-socket-1",
                                    "type": "udp4",
                                }),
                            )
                            .expect("respond to dgram.createSocket");
                    }
                    "dgram.bind" => {
                        assert_eq!(request.args[0].as_str(), Some("udp-socket-1"));
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "localAddress": "127.0.0.1",
                                    "localPort": 43112,
                                    "family": "IPv4",
                                }),
                            )
                            .expect("respond to dgram.bind");
                    }
                    "dgram.send" => {
                        assert_eq!(request.args[0].as_str(), Some("udp-socket-1"));
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "bytes": 4,
                                    "localAddress": "127.0.0.1",
                                    "localPort": 43112,
                                    "family": "IPv4",
                                }),
                            )
                            .expect("respond to dgram.send");
                    }
                    "dgram.poll" => {
                        let socket_id = request.args[0].as_str().expect("poll socket id");
                        let next = socket_events
                            .get_mut(socket_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to dgram.poll");
                    }
                    "dgram.close" => {
                        execution
                            .respond_sync_rpc_success(request.id, Value::Null)
                            .expect("respond to dgram.close");
                    }
                    other => panic!("unexpected dgram sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dgram JSON");
    assert_eq!(parsed["message"], Value::String(String::from("pong")));
    assert_eq!(
        parsed["address"]["address"],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["address"]["port"], Value::from(43112));
    assert_eq!(
        parsed["rinfo"]["address"],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["rinfo"]["port"], Value::from(43199));
    assert!(methods.iter().any(|method| method == "dgram.createSocket"));
    assert!(methods.iter().any(|method| method == "dgram.bind"));
    assert!(methods.iter().any(|method| method == "dgram.send"));
    assert!(methods.iter().any(|method| method == "dgram.poll"));
    assert!(methods.iter().any(|method| method == "dgram.close"));
}

#[test]
fn javascript_execution_routes_dns_through_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import dns from "node:dns";

const lookup = await new Promise((resolve, reject) => {
  dns.lookup("example.test", { family: 4 }, (error, address, family) => {
    if (error) {
      reject(error);
      return;
    }
    resolve({ address, family });
  });
});

const lookupAll = await dns.promises.lookup("example.test", { all: true });
const resolved = await new Promise((resolve, reject) => {
  dns.resolve("example.test", "A", (error, records) => {
    if (error) {
      reject(error);
      return;
    }
    resolve(records);
  });
});
const resolved4 = await dns.promises.resolve4("example.test");
const resolved6 = await new Promise((resolve, reject) => {
  dns.resolve6("example.test", (error, records) => {
    if (error) {
      reject(error);
      return;
    }
    resolve(records);
  });
});
const resolvedViaPromises = await dns.promises.resolve("example.test", "AAAA");

console.log(JSON.stringify({
  lookup,
  lookupAll,
  resolved,
  resolved4,
  resolved6,
  resolvedViaPromises,
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([(
        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
        String::from(
            "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
        ),
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
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "dns.lookup" => {
                        let family = request.args[0]["family"].as_u64().expect("lookup family");
                        let result = if family == 4 {
                            json!([{ "address": "203.0.113.10", "family": 4 }])
                        } else {
                            json!([
                                { "address": "203.0.113.10", "family": 4 },
                                { "address": "2001:db8::10", "family": 6 },
                            ])
                        };
                        execution
                            .respond_sync_rpc_success(request.id, result)
                            .expect("respond to dns.lookup");
                    }
                    "dns.resolve" => {
                        let rrtype = request.args[0]["rrtype"].as_str().expect("resolve rrtype");
                        let result = if rrtype == "AAAA" {
                            json!(["2001:db8::10"])
                        } else {
                            json!(["203.0.113.10"])
                        };
                        execution
                            .respond_sync_rpc_success(request.id, result)
                            .expect("respond to dns.resolve");
                    }
                    "dns.resolve4" => {
                        execution
                            .respond_sync_rpc_success(request.id, json!(["203.0.113.10"]))
                            .expect("respond to dns.resolve4");
                    }
                    "dns.resolve6" => {
                        execution
                            .respond_sync_rpc_success(request.id, json!(["2001:db8::10"]))
                            .expect("respond to dns.resolve6");
                    }
                    other => panic!("unexpected dns sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns JSON");
    assert_eq!(
        parsed["lookup"]["address"],
        Value::String(String::from("203.0.113.10"))
    );
    assert_eq!(parsed["lookup"]["family"], Value::from(4));
    assert_eq!(
        parsed["lookupAll"][1]["address"],
        Value::String(String::from("2001:db8::10"))
    );
    assert_eq!(
        parsed["resolvedViaPromises"][0],
        Value::String(String::from("2001:db8::10"))
    );
    assert!(methods.iter().any(|method| method == "dns.lookup"));
    assert!(methods.iter().any(|method| method == "dns.resolve"));
    assert!(methods.iter().any(|method| method == "dns.resolve4"));
    assert!(methods.iter().any(|method| method == "dns.resolve6"));
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

    let mut engine = JavascriptExecutionEngine::default();
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

    let mut engine = JavascriptExecutionEngine::default();
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

#[test]
fn javascript_execution_translates_require_resolve_and_cjs_errors_to_guest_paths() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.cjs"),
        "module.exports = { answer: 42 };\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  resolved: require.resolve('./dep.cjs'),
};

try {
  require.resolve('/root/missing.cjs');
  result.resolveMissing = 'unexpected';
} catch (error) {
  result.resolveMissing = {
    code: error.code ?? null,
    message: error.message,
    stack: error.stack ?? null,
    requireStack: error.requireStack ?? [],
  };
}

try {
  require('/root/missing.cjs');
  result.requireMissing = 'unexpected';
} catch (error) {
  result.requireMissing = {
    code: error.code ?? null,
    message: error.message,
    stack: error.stack ?? null,
    requireStack: error.requireStack ?? [],
  };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse require JSON");
    let host_path = temp.path().to_string_lossy();

    assert_eq!(
        parsed["resolved"],
        Value::String(String::from("/root/dep.cjs"))
    );

    for field in ["resolveMissing", "requireMissing"] {
        assert_eq!(
            parsed[field]["code"],
            Value::String(String::from("MODULE_NOT_FOUND"))
        );
        let message = parsed[field]["message"].as_str().expect("missing message");
        let stack = parsed[field]["stack"].as_str().expect("missing stack");
        assert!(message.contains("/root/missing.cjs"), "message: {message}");
        assert!(
            !message.contains(host_path.as_ref()),
            "message leaked host path: {message}"
        );
        assert!(
            !stack.contains(host_path.as_ref()),
            "stack leaked host path: {stack}"
        );

        let require_stack = parsed[field]["requireStack"]
            .as_array()
            .expect("require stack array");
        let mut saw_guest_path = false;
        for entry in require_stack {
            let entry = entry.as_str().expect("require stack entry");
            saw_guest_path |= entry.starts_with("/root/");
            assert!(
                !entry.contains(host_path.as_ref()),
                "requireStack leaked host path: {entry}"
            );
        }
        assert!(
            saw_guest_path,
            "requireStack should contain guest-visible paths"
        );
    }
}

#[test]
fn javascript_execution_blocks_cjs_require_from_hidden_parent_node_modules() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let guest_root = temp.path().join("guest-root");
    let guest_package_dir = guest_root.join("node_modules/visible-pkg");
    let hidden_parent_package_dir = temp.path().join("node_modules/host-only-pkg");
    fs::create_dir_all(&guest_package_dir).expect("create guest package dir");
    fs::create_dir_all(&hidden_parent_package_dir).expect("create hidden parent package dir");

    write_fixture(
        &guest_root.join("dep.cjs"),
        "module.exports = { answer: 41 };\n",
    );
    write_fixture(
        &guest_package_dir.join("package.json"),
        "{\n  \"name\": \"visible-pkg\",\n  \"main\": \"./index.js\"\n}\n",
    );
    write_fixture(
        &guest_package_dir.join("index.js"),
        "module.exports = { answer: 42 };\n",
    );
    write_fixture(
        &hidden_parent_package_dir.join("package.json"),
        "{\n  \"name\": \"host-only-pkg\",\n  \"main\": \"./index.js\"\n}\n",
    );
    write_fixture(
        &hidden_parent_package_dir.join("index.js"),
        "module.exports = { compromised: true };\n",
    );
    write_fixture(
        &guest_root.join("consumer.cjs"),
        r#"
const dep = require("./dep.cjs");
const visible = require("visible-pkg");

let hidden;
try {
  hidden = require("host-only-pkg");
} catch (error) {
  hidden = {
    code: error.code ?? null,
    message: error.message,
  };
}

module.exports = {
  dep: dep.answer,
  visible: visible.answer,
  hidden,
};
"#,
    );
    write_fixture(
        &guest_root.join("entry.mjs"),
        r#"
import result from "./consumer.cjs";
result.cacheKeys = Object.keys(require.cache)
  .filter((key) =>
    key.includes("consumer.cjs") ||
    key.includes("dep.cjs") ||
    key.includes("visible-pkg"),
  )
  .sort();
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let guest_root_host_path = guest_root.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{guest_root_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        &guest_root,
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse CJS JSON");

    assert_eq!(parsed["dep"], Value::from(41));
    assert_eq!(parsed["visible"], Value::from(42));
    assert_eq!(
        parsed["hidden"]["code"],
        Value::String(String::from("MODULE_NOT_FOUND"))
    );
    let hidden_message = parsed["hidden"]["message"]
        .as_str()
        .expect("hidden module missing message");
    assert!(
        hidden_message.contains("host-only-pkg"),
        "message should mention blocked package: {hidden_message}"
    );

    let cache_keys = parsed["cacheKeys"].as_array().expect("cache keys array");
    let cache_key_values: Vec<&str> = cache_keys
        .iter()
        .map(|entry| entry.as_str().expect("cache key"))
        .collect();
    assert!(
        cache_key_values.contains(&"/root/consumer.cjs"),
        "consumer cache key should use guest path: {cache_key_values:?}"
    );
    assert!(
        cache_key_values.contains(&"/root/dep.cjs"),
        "dep cache key should use guest path: {cache_key_values:?}"
    );
    assert!(
        cache_key_values
            .iter()
            .any(|entry| entry.starts_with("/root/node_modules/visible-pkg/")),
        "package cache key should stay in guest path space: {cache_key_values:?}"
    );
}

#[test]
fn javascript_execution_translates_top_level_loader_stacks_to_guest_paths() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
export const broken = ;
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(stdout.trim(), "");
    assert_eq!(exit_code, 1, "stderr: {stderr}");
    let host_path = temp.path().to_string_lossy();
    assert!(
        stderr.contains("/root/entry.mjs"),
        "stderr should use guest path: {stderr}"
    );
    assert!(
        stderr.contains("SyntaxError"),
        "stderr should contain the parse failure: {stderr}"
    );
    assert!(
        !stderr.contains(host_path.as_ref()),
        "stderr leaked host path: {stderr}"
    );
}

#[test]
fn javascript_execution_ignores_forged_import_cache_metrics_written_to_stderr() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("dep.mjs"), "export const value = 1;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import "./dep.mjs";
process.stderr.write('__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:{"resolveHits":999,"resolveMisses":999}\n');
console.log("ready");
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(temp.path().join("compile-cache")),
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
            String::from("1"),
        )]),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stdout.contains("ready"));
    assert!(
        !stderr.contains("\"resolveHits\":999"),
        "forged metrics should not survive stderr filtering: {stderr}"
    );

    let metrics = parse_import_cache_metrics(&stderr);
    assert!(
        metrics.resolve_hits < 999,
        "unexpected metrics: {metrics:?}"
    );
    assert!(
        metrics.resolve_misses > 0,
        "unexpected metrics: {metrics:?}"
    );
}
