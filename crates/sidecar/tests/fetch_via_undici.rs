mod support;

use agent_os_sidecar::protocol::GuestRuntimeKind;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, collect_process_output_with_timeout, execute, new_sidecar,
    open_session, temp_dir, write_fixture,
};

#[test]
fn javascript_fetch_uses_guest_undici_over_kernel_tcp_socket() {
    assert_node_available();

    let mut sidecar = new_sidecar("fetch-via-undici");
    let cwd = temp_dir("fetch-via-undici-cwd");
    let entry = cwd.join("fetch-entry.mjs");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind host http listener");
    let port = listener.local_addr().expect("listener addr").port();
    let server = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("configure nonblocking listener");
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for guest fetch connection"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept http request: {error}"),
            }
        };
        let mut request = String::new();
        let mut buffer = [0_u8; 4096];
        let bytes_read = stream.read(&mut buffer).expect("read http request");
        request.push_str(&String::from_utf8_lossy(&buffer[..bytes_read]));
        assert!(
            request.contains("GET /health HTTP/1.1"),
            "unexpected request: {request}"
        );

        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
            )
            .expect("write http response");
        stream.flush().expect("flush http response");
    });

    write_fixture(
        &entry,
        &format!(
            r#"
console.log("before-fetch");
console.log(JSON.stringify({{
  fetchType: typeof fetch,
  globalFetchType: typeof globalThis.fetch,
}}));
const response = await fetch("http://127.0.0.1:{port}/health", {{
  headers: {{ accept: "text/plain" }},
}});
if (response.status !== 200) {{
  throw new Error(`status=${{response.status}}`);
}}
if (!response.body || typeof response.body.getReader !== "function") {{
  throw new Error("expected ReadableStream body");
}}
const reader = response.body.getReader();
const decoder = new TextDecoder();
let body = "";
for (;;) {{
  const {{ value, done }} = await reader.read();
  if (done) break;
  body += decoder.decode(value, {{ stream: true }});
}}
body += decoder.decode();
console.log(JSON.stringify({{
  status: response.status,
  body,
  contentType: response.headers.get("content-type"),
  hasReader: true,
}}));
"#,
        ),
    );

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
        format!("[{port}]"),
    );
    let (vm_id, _) = support::create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-process",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-process",
        Duration::from_secs(10),
    );
    let server_result = server.join();

    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("stdout json line");
    let payload: serde_json::Value = serde_json::from_str(json_line).expect("parse fetch result");
    assert_eq!(payload["status"], 200);
    assert_eq!(payload["body"], "hello world");
    assert_eq!(payload["contentType"], "text/plain");
    assert_eq!(payload["hasReader"], true);
    server_result
        .unwrap_or_else(|_| panic!("server thread failed\nstdout:\n{stdout}\nstderr:\n{stderr}"));
}

#[test]
fn javascript_fetch_honors_abortsignal_timeout_and_manual_abort() {
    assert_node_available();

    let mut sidecar = new_sidecar("fetch-abort-via-undici");
    let cwd = temp_dir("fetch-abort-via-undici-cwd");
    let entry = cwd.join("fetch-abort-entry.mjs");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind host http listener");
    let port = listener.local_addr().expect("listener addr").port();
    let server = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("configure nonblocking listener");
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..2 {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(accepted) => break accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "timed out waiting for guest fetch connection"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept http request: {error}"),
                }
            };

            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer);
            thread::sleep(Duration::from_millis(250));
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\nConnection: close\r\n\r\nignored",
            );
            let _ = stream.flush();
        }
    });

    write_fixture(
        &entry,
        &format!(
            r#"
async function expectAbort(label, promiseFactory, expectedReason) {{
  try {{
    await promiseFactory();
    throw new Error(`${{label}} unexpectedly resolved`);
  }} catch (error) {{
    return {{
      label,
      name: error?.name ?? null,
      code: error?.code ?? null,
      message: String(error?.message ?? ""),
      expectedReason,
    }};
  }}
}}

const timeoutSignal = AbortSignal.timeout(50);
let timeoutSignalEvents = 0;
timeoutSignal.addEventListener("abort", () => {{
  timeoutSignalEvents += 1;
}});
const timeoutResult = await expectAbort(
  "timeout",
  () => fetch("http://127.0.0.1:{port}/timeout", {{ signal: timeoutSignal }}),
  timeoutSignal.reason?.name ?? null,
);

const controller = new AbortController();
let manualSignalEvents = 0;
controller.signal.addEventListener("abort", () => {{
  manualSignalEvents += 1;
}});
setTimeout(() => controller.abort("manual-stop"), 25);
const manualResult = await expectAbort(
  "manual",
  () => fetch("http://127.0.0.1:{port}/manual", {{ signal: controller.signal }}),
  controller.signal.reason ?? null,
);

console.log(JSON.stringify({{
  timeoutResult,
  timeoutSignalAborted: timeoutSignal.aborted,
  timeoutSignalEvents,
  timeoutSignalReasonName: timeoutSignal.reason?.name ?? null,
  manualResult,
  manualSignalAborted: controller.signal.aborted,
  manualSignalEvents,
  manualSignalReason: controller.signal.reason ?? null,
}}));
"#,
        ),
    );

    let connection_id = authenticate(&mut sidecar, "conn-abort");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let mut metadata = BTreeMap::new();
    metadata.insert(
        String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
        format!("[{port}]"),
    );
    let (vm_id, _) = support::create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-abort-process",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-abort-process",
        Duration::from_secs(10),
    );
    let server_result = server.join();

    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("stdout json line");
    let payload: serde_json::Value = serde_json::from_str(json_line).expect("parse fetch result");

    assert_eq!(payload["timeoutSignalAborted"], true);
    assert_eq!(payload["timeoutSignalEvents"], 1);
    assert_eq!(payload["timeoutSignalReasonName"], "AbortError");
    assert_ne!(
        payload["timeoutResult"]["message"],
        "timeout unexpectedly resolved"
    );

    assert_eq!(payload["manualSignalAborted"], true);
    assert_eq!(payload["manualSignalEvents"], 1);
    assert_eq!(payload["manualSignalReason"], "manual-stop");
    assert_ne!(
        payload["manualResult"]["message"],
        "manual unexpectedly resolved"
    );

    server_result
        .unwrap_or_else(|_| panic!("server thread failed\nstdout:\n{stdout}\nstderr:\n{stderr}"));
}
