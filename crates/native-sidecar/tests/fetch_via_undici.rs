mod support;

use agentos_native_sidecar::wire::GuestRuntimeKind;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_wire_with_metadata, dispose_vm_and_close_session, execute_wire, new_sidecar,
    open_session_wire, temp_dir, write_fixture,
};

const FETCH_VIA_UNDICI_CASES: &[&str] = &["fetch", "abort", "keepalive_no_listener_leak"];

// Regression guard for the net-bridge socket-listener leak: a long-lived VM that
// makes many keep-alive HTTP requests over one reused bridge socket must not
// accumulate per-request socket/undici listeners. Each leaked listener trips the
// guest EventEmitter's `MaxListenersExceededWarning` via `console.error`, which
// lands on the process stderr -- so an empty stderr after N >> 10 reused requests
// proves listeners are added and removed symmetrically per request.
fn javascript_fetch_keepalive_does_not_leak_socket_listeners() {
    assert_node_available();

    // Concurrency is the trigger: with an UNBOUNDED per-origin pool, overlapping
    // requests dispatched during the connect window each find every existing client
    // still `kNeedDrain` and spawn a fresh Client+socket (undici pool.rs kGetDispatcher),
    // accumulating their listener sets without bound. The fix bounds the agent's
    // `connections`, so excess requests queue on existing clients instead of spawning
    // new ones -- the host-side connection count (each guest socket = one accepted TCP
    // connection) then stays a small multiple of the concurrency instead of growing
    // toward the total request count.
    const CONCURRENCY: usize = 8;
    const ROUNDS: usize = 20;
    const REQUESTS: usize = CONCURRENCY * ROUNDS;

    let mut sidecar = new_sidecar("fetch-keepalive-leak");
    let cwd = temp_dir("fetch-keepalive-leak-cwd");
    let entry = cwd.join("fetch-keepalive-entry.mjs");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind host http listener");
    let port = listener.local_addr().expect("listener addr").port();
    let served = Arc::new(AtomicUsize::new(0));
    let connections = Arc::new(AtomicUsize::new(0));
    let served_server = served.clone();
    let connections_server = connections.clone();
    let server = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("configure nonblocking listener");
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut handlers = Vec::new();
        // Accept every connection the guest opens (one reused keep-alive socket in the
        // common case, but robust to a fresh socket per request) and serve keep-alive
        // responses on each until the guest closes it. Reading to the client's EOF
        // means teardown is a clean FIN, never an ECONNRESET test artifact.
        while served_server.load(Ordering::SeqCst) < REQUESTS && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    connections_server.fetch_add(1, Ordering::SeqCst);
                    let served_handler = served.clone();
                    handlers.push(thread::spawn(move || {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .expect("configure read timeout");
                        let handler_deadline = Instant::now() + Duration::from_secs(20);
                        let mut buffer: Vec<u8> = Vec::new();
                        let mut chunk = [0_u8; 4096];
                        while Instant::now() < handler_deadline {
                            match stream.read(&mut chunk) {
                                Ok(0) => break, // guest closed this connection
                                Ok(n) => {
                                    buffer.extend_from_slice(&chunk[..n]);
                                    // GET requests have no body: each ends at CRLFCRLF.
                                    while let Some(pos) = buffer
                                        .windows(4)
                                        .position(|window| window == b"\r\n\r\n")
                                    {
                                        buffer.drain(..pos + 4);
                                        if stream
                                            .write_all(
                                                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 11\r\nConnection: keep-alive\r\n\r\nhello world",
                                            )
                                            .is_err()
                                        {
                                            return;
                                        }
                                        let _ = stream.flush();
                                        served_handler.fetch_add(1, Ordering::SeqCst);
                                    }
                                }
                                Err(error)
                                    if error.kind() == std::io::ErrorKind::WouldBlock
                                        || error.kind() == std::io::ErrorKind::TimedOut =>
                                {
                                    continue
                                }
                                Err(_) => break,
                            }
                        }
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept http request: {error}"),
            }
        }
        // Let in-flight handlers drain to the guest's clean close.
        for handler in handlers {
            let _ = handler.join();
        }
        (
            served_server.load(Ordering::SeqCst),
            connections_server.load(Ordering::SeqCst),
        )
    });

    write_fixture(
        &entry,
        format!(
            r#"
const ROUNDS = {ROUNDS};
const CONCURRENCY = {CONCURRENCY};
// Surface Node process warnings (MaxListenersExceededWarning et al.) onto stderr so
// the host can assert on them -- there is no default "warning" handler otherwise, so a
// listener-leak warning would be emitted-and-dropped instead of observed.
process.on("warning", (warning) => {{
  console.error(`PROCESS_WARNING ${{warning.name}}: ${{warning.message}}`);
}});
let done = 0;
// Overlapping requests per round (not strictly sequential) so they dispatch while
// the pool's clients are still connecting -- the condition that makes an unbounded
// pool spawn a fresh client+socket per request.
for (let round = 0; round < ROUNDS; round++) {{
  await Promise.all(Array.from({{ length: CONCURRENCY }}, async () => {{
    const response = await fetch("http://127.0.0.1:{port}/health", {{
      headers: {{ accept: "text/plain" }},
    }});
    const body = await response.text();
    if (response.status !== 200 || body !== "hello world") {{
      throw new Error(`request failed: status=${{response.status}} body=${{body}}`);
    }}
    done++;
  }}));
}}
console.log(JSON.stringify({{ ok: true, count: done }}));
"#,
        ),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
        format!("[{port}]"),
    );
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-keepalive-process",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-keepalive-process",
        Duration::from_secs(30),
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    let server_result = server.join();

    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    // The leak guard: a MaxListenersExceededWarning (re-surfaced via the fixture's
    // process.on("warning") handler) lands on stderr. The fixture's only other stderr
    // output would be an uncaught error, so stderr must be empty on the happy path.
    assert!(
        stderr.trim().is_empty(),
        "keep-alive request loop produced unexpected stderr (possible listener leak):\n{stderr}"
    );
    assert!(
        !stderr.contains("MaxListenersExceededWarning"),
        "socket-listener leak detected after {REQUESTS} keep-alive requests:\n{stderr}"
    );
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("stdout json line");
    let payload: serde_json::Value =
        serde_json::from_str(json_line).expect("parse keepalive fetch result");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["count"], REQUESTS);
    let (served_count, connection_count) = server_result
        .unwrap_or_else(|_| panic!("server thread failed\nstdout:\n{stdout}\nstderr:\n{stderr}"));
    assert_eq!(
        served_count, REQUESTS,
        "server should have served every request"
    );
    // The real leak guard: the per-origin pool must be BOUNDED. Each guest socket is one
    // accepted TCP connection here, so an unbounded pool that spawns a fresh client+socket
    // per overlapping request makes `connection_count` grow with the total request count;
    // a bounded pool keeps it to a small multiple of the concurrency regardless of how
    // many requests run. (The MaxListenersExceededWarning guard above is necessary but not
    // sufficient -- the leaked listeners spread across many per-client emitters, so no
    // single one may cross the threshold even while sockets grow unbounded.)
    // With the bounded pool, connections stay at/under the cap (+ a small margin for any
    // mid-run reconnect) regardless of how many requests run. Observed: 6 with the cap vs
    // ~2x the concurrency (16) without it for this 8-way / 160-request load.
    let connection_bound = CONCURRENCY + 2;
    assert!(
        connection_count <= connection_bound,
        "undici client-per-request leak: {served_count} requests over {connection_count} \
         connections (expected <= {connection_bound} for concurrency {CONCURRENCY}); an \
         unbounded pool spawns a fresh client+socket per overlapping request"
    );
    eprintln!(
        "[keepalive-leak] served {served_count} requests over {connection_count} connection(s) \
         (bound {connection_bound})"
    );
}

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
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("configure http request read timeout");
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
        format!(
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
        format!("[{port}]"),
    );
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    execute_wire(
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

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-process",
        Duration::from_secs(10),
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
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

            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("configure abort request read timeout");
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
        format!(
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-abort");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
        format!("[{port}]"),
    );
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    execute_wire(
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

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fetch-abort-process",
        Duration::from_secs(10),
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
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
    assert_eq!(payload["timeoutSignalReasonName"], "TimeoutError");
    assert_eq!(payload["timeoutResult"]["name"], "TimeoutError");
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

fn run_named_case(case_name: &str) {
    match case_name {
        "fetch" => javascript_fetch_uses_guest_undici_over_kernel_tcp_socket(),
        "abort" => javascript_fetch_honors_abortsignal_timeout_and_manual_abort(),
        "keepalive_no_listener_leak" => javascript_fetch_keepalive_does_not_leak_socket_listeners(),
        other => panic!("unknown fetch_via_undici case: {other}"),
    }
}

#[test]
fn fetch_via_undici_cases() {
    let current_exe = std::env::current_exe().expect("current test binary path");

    for case_name in FETCH_VIA_UNDICI_CASES {
        let status = Command::new(&current_exe)
            .arg("--exact")
            .arg("__fetch_via_undici_case_runner")
            .arg("--nocapture")
            .env("AGENTOS_FETCH_VIA_UNDICI_CASE", case_name)
            .status()
            .unwrap_or_else(|error| {
                panic!("spawn fetch_via_undici runner for {case_name}: {error}")
            });

        assert!(
            status.success(),
            "fetch_via_undici case {case_name} failed with status {status}"
        );
    }
}

#[test]
fn __fetch_via_undici_case_runner() {
    let Ok(case_name) = std::env::var("AGENTOS_FETCH_VIA_UNDICI_CASE") else {
        return;
    };

    run_named_case(&case_name);
}
