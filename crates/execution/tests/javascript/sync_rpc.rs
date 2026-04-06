use super::*;

#[test]
fn javascript_contexts_preserve_vm_and_bootstrap_configuration() {
    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
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

    let mut engine = new_test_engine();
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
