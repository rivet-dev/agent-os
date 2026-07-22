//! Guest fetch -> host loopback via `loopback_exempt_ports`, asserting the
//! response body arrives byte-clean. Regression gate for the net-poll timeout
//! sentinel: a guest/sidecar sentinel mismatch base64-decodes the sentinel
//! string into phantom bytes injected into EVERY guest TCP stream (this broke
//! all guest HTTP, including agent SDK -> LLM traffic).
mod common;
use agentos_client::config::{AgentOsConfig, PatternPermissions, PermissionMode, Permissions};
use agentos_client::ExecOptions;
use std::io::Write;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn guest_fetch_reaches_host_loopback() {
    if !common::sidecar_available() {
        eprintln!("skip: sidecar not built");
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::Read;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 12\r\nconnection: close\r\n\r\nPROBE_PONG!\n",
            );
        }
    });
    common::ensure_sidecar_env();
    let os = agentos_client::AgentOs::create(AgentOsConfig {
        loopback_exempt_ports: vec![port],
        permissions: Some(Permissions {
            network: Some(PatternPermissions::Mode(PermissionMode::Allow)),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .expect("create VM");
    let script = format!(
        "fetch('http://127.0.0.1:{port}/').then(r=>r.text()).then(t=>console.log('BODY:'+t)).catch(e=>{{console.error('FETCHERR:'+e.message+' cause:'+(e.cause&&e.cause.message));process.exit(3)}})"
    );
    let args = vec![String::from("-e"), script];
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        os.exec_argv_process("node", &args, ExecOptions::default()),
    )
    .await
    .expect("guest fetch timed out")
    .expect("exec");
    println!(
        "exit={} stdout={:?} stderr={:?}",
        result.exit_code, result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("PROBE_PONG"),
        "stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );
}
