/// Codex headless command for agentos VM.
///
/// The prompt mode remains a placeholder command. The ACP session-turn path is
/// disabled until it can delegate to the real Codex agent package instead of a
/// bespoke provider loop.
use std::collections::HashMap;
use std::io::{self, Read};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SESSION_TURN_DISABLED: &str =
    "codex-exec --session-turn is disabled until the real Codex agent package is wired";
const MAX_PROMPT_BYTES: usize = 64 * 1024;

use codex_network_proxy::NetworkProxy;
use codex_otel::SessionTelemetry;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("codex-exec {}", VERSION);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("--http-test") {
        return http_test(&args[2..]);
    }

    if args.get(1).map(|s| s.as_str()) == Some("--stub-test") {
        return stub_test();
    }

    if args.get(1).map(|s| s.as_str()) == Some("--session-turn") {
        emit_session_turn_disabled();
        std::process::exit(1);
    }

    let prompt = if args.len() > 1 {
        let prompt = args[1..].join(" ");
        if prompt.len() > MAX_PROMPT_BYTES {
            eprintln!("codex-exec: prompt exceeds {} byte limit", MAX_PROMPT_BYTES);
            std::process::exit(1);
        }
        prompt
    } else {
        let mut input = Vec::new();
        let mut stdin = io::stdin().take((MAX_PROMPT_BYTES + 1) as u64);
        match stdin.read_to_end(&mut input) {
            Ok(_) if input.len() > MAX_PROMPT_BYTES => {
                eprintln!(
                    "codex-exec: stdin prompt exceeds {} byte limit",
                    MAX_PROMPT_BYTES
                );
                std::process::exit(1);
            }
            Ok(_) => match String::from_utf8(input) {
                Ok(input) => input.trim().to_string(),
                Err(error) => {
                    eprintln!("codex-exec: stdin prompt is not valid UTF-8: {}", error);
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("codex-exec: failed to read stdin: {}", error);
                std::process::exit(1);
            }
        }
    };

    if prompt.is_empty() {
        eprintln!("codex-exec: no prompt provided");
        eprintln!("usage: codex-exec <prompt>  or  echo '<prompt>' | codex-exec");
        std::process::exit(1);
    }

    eprintln!("codex-exec: headless prompt mode is not wired to the provider yet");
    eprintln!("prompt received ({} bytes)", prompt.len());
    std::process::exit(0);
}

fn emit_session_turn_disabled() {
    println!(
        "{{\"type\":\"error\",\"message\":\"{}\"}}",
        SESSION_TURN_DISABLED
    );
}

fn print_help() {
    println!("codex-exec {} - headless Codex command", VERSION);
    println!();
    println!("USAGE:");
    println!("    codex-exec [OPTIONS] [PROMPT]");
    println!("    echo '<prompt>' | codex-exec");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help          Print this help message");
    println!("    -V, --version       Print version information");
    println!("    --http-test URL     Test HTTP client via host_net");
    println!("    --stub-test         Validate WASI stub crates");
    println!("    --session-turn      Fail fast until the real Codex agent package is wired");
}

fn stub_test() {
    let proxy = NetworkProxy;
    let mut env = HashMap::new();
    proxy.apply_to_env(&mut env);
    println!("network-proxy: NetworkProxy is zero-size, apply_to_env is no-op");

    let telemetry = SessionTelemetry::new();
    telemetry.counter("test.counter", 1, &[]);
    telemetry.histogram("test.histogram", 42, &[]);
    println!("otel: SessionTelemetry metrics are no-ops");

    let global = codex_otel::metrics::global();
    assert!(global.is_none(), "global metrics should be None on WASI");
    println!("otel: global() returns None (no exporter on WASI)");

    println!("stub-test: all stubs validated successfully");
}

fn http_test(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: codex-exec --http-test <url>");
        std::process::exit(1);
    }

    let url = &args[0];
    match wasi_http::get(url) {
        Ok(resp) => {
            println!("status: {}", resp.status);
            match resp.text() {
                Ok(body) => println!("body: {}", body),
                Err(error) => eprintln!("body decode error: {}", error),
            }
        }
        Err(error) => {
            eprintln!("http error: {}", error);
            std::process::exit(1);
        }
    }
}
