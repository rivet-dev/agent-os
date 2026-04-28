/// Codex headless agent for Agent OS WasmVM.
///
/// This binary supports two modes:
/// - Legacy prompt mode (`codex-exec "prompt"`) which remains a placeholder.
/// - Session turn mode (`codex-exec --session-turn`) used by the ACP adapter.
///
/// Session turn mode reads a JSON line request on stdin, calls a Responses-style
/// LLM provider via `wasi-http`, optionally executes shell commands through
/// `wasi-spawn`, and emits NDJSON events on stdout for the adapter.
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::os::fd::FromRawFd;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Validate WASI stub crates compile by referencing key types.
use codex_network_proxy::NetworkProxy;
use codex_otel::SessionTelemetry;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InboundMessage {
    Start(TurnRequest),
    PermissionResponse {
        request_id: String,
        option_id: String,
    },
}

#[derive(Debug, Deserialize)]
struct TurnRequest {
    cwd: String,
    mode: Option<String>,
    model: Option<String>,
    thought_level: Option<String>,
    developer_instructions: Option<String>,
    history: Vec<Value>,
    prompt: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutboundMessage<'a> {
    TextDelta {
        text: &'a str,
    },
    ToolCallUpdate {
        tool_call_id: &'a str,
        command: &'a str,
        status: &'a str,
        exit_code: Option<i32>,
        stdout: Option<&'a str>,
        stderr: Option<&'a str>,
    },
    PermissionRequest {
        request_id: &'a str,
        tool_call_id: &'a str,
        command: &'a str,
    },
    Done {
        stop_reason: &'a str,
        assistant_text: &'a str,
        history: &'a [Value],
    },
    Error {
        message: &'a str,
    },
}

#[derive(Debug)]
struct FunctionCall {
    call_id: String,
    name: String,
    arguments: Value,
}

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
        match session_turn_mode() {
            Ok(()) => return,
            Err(error) => {
                emit_line(&OutboundMessage::Error {
                    message: &error.to_string(),
                });
                std::process::exit(1);
            }
        }
    }

    let prompt = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        let mut input = String::new();
        match std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
            Ok(_) => input.trim().to_string(),
            Err(e) => {
                eprintln!("codex-exec: failed to read stdin: {}", e);
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
    eprintln!("prompt: {}", prompt);
    std::process::exit(0);
}

fn session_turn_mode() -> io::Result<()> {
    let stdin_fd = wasi_ext::dup(0).map_err(|errno| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("duplicating control stdin: wasi errno {}", errno),
        )
    })?;
    let stdin = unsafe { std::fs::File::from_raw_fd(stdin_fd as i32) };
    let mut stdin = io::BufReader::new(stdin);

    let start = read_message(&mut stdin)?;
    let InboundMessage::Start(request) = start else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a start message",
        ));
    };

    let TurnRequest {
        cwd,
        mode,
        model,
        thought_level,
        developer_instructions,
        history: initial_history,
        prompt,
    } = request;

    let mut history = initial_history;
    history.push(json!({
        "role": "user",
        "content": prompt,
    }));

    let provider_mode = mode.unwrap_or_else(|| "default".to_string());
    let provider_model = model.unwrap_or_else(|| "gpt-5-codex".to_string());
    let thought_level = thought_level.unwrap_or_else(|| "medium".to_string());

    let mut pending_permission_responses = HashMap::new();

    loop {
        let response = call_responses_api(
            &provider_model,
            &thought_level,
            developer_instructions.as_deref(),
            &history,
            provider_mode != "plan",
        )?;

        let function_calls = extract_function_calls(&response)?;
        append_output_items(&mut history, &response);
        if function_calls.is_empty() {
            let assistant_text = extract_assistant_text(&response)?;
            if assistant_text.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "provider response did not contain text or function calls",
                ));
            }

            emit_line(&OutboundMessage::TextDelta {
                text: &assistant_text,
            });
            emit_line(&OutboundMessage::Done {
                stop_reason: "end_turn",
                assistant_text: &assistant_text,
                history: &history,
            });
            return Ok(());
        }

        let mut permission_requests = Vec::with_capacity(function_calls.len());
        for function_call in &function_calls {
            if function_call.name != "shell" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported tool: {}", function_call.name),
                ));
            }

            let command = function_call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "shell tool missing command")
                })?;

            emit_line(&OutboundMessage::ToolCallUpdate {
                tool_call_id: &function_call.call_id,
                command,
                status: "pending",
                exit_code: None,
                stdout: None,
                stderr: None,
            });

            let permission_request_id = format!("perm-{}", function_call.call_id);
            emit_line(&OutboundMessage::PermissionRequest {
                request_id: &permission_request_id,
                tool_call_id: &function_call.call_id,
                command,
            });
            permission_requests.push((function_call, command, permission_request_id));
        }

        let mut permission_outcomes = HashMap::with_capacity(permission_requests.len());
        for (function_call, _command, permission_request_id) in &permission_requests {
            let permission = wait_for_permission(
                &mut stdin,
                permission_request_id,
                &mut pending_permission_responses,
            )
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "waiting for permission response {}: {}",
                        permission_request_id, error
                    ),
                )
            })?;
            permission_outcomes.insert(function_call.call_id.as_str(), permission);
        }

        for (function_call, command, _permission_request_id) in permission_requests {
            let permission = permission_outcomes
                .get(function_call.call_id.as_str())
                .map(String::as_str)
                .unwrap_or("reject_once");
            if !matches!(permission, "allow_once" | "allow_always") {
                emit_line(&OutboundMessage::Done {
                    stop_reason: "cancelled",
                    assistant_text: "",
                    history: &history,
                });
                return Ok(());
            }

            emit_line(&OutboundMessage::ToolCallUpdate {
                tool_call_id: &function_call.call_id,
                command,
                status: "in_progress",
                exit_code: None,
                stdout: None,
                stderr: None,
            });

            let mut child =
                wasi_spawn::spawn_child_ignore_stdin(&["sh", "-lc", command], &[], &cwd).map_err(
                    |error| {
                        io::Error::new(
                            error.kind(),
                            format!("spawning shell for {}: {}", function_call.call_id, error),
                        )
                    },
                )?;
            let output = child.consume_output().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "consuming shell output for {}: {}",
                        function_call.call_id, error
                    ),
                )
            })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let tool_status = if output.exit_code == 0 {
                "completed"
            } else {
                "failed"
            };

            emit_line(&OutboundMessage::ToolCallUpdate {
                tool_call_id: &function_call.call_id,
                command,
                status: tool_status,
                exit_code: Some(output.exit_code),
                stdout: if stdout.is_empty() {
                    None
                } else {
                    Some(stdout.as_str())
                },
                stderr: if stderr.is_empty() {
                    None
                } else {
                    Some(stderr.as_str())
                },
            });

            let mut tool_result = String::new();
            if !stdout.is_empty() {
                tool_result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !tool_result.is_empty() {
                    tool_result.push('\n');
                }
                tool_result.push_str(&stderr);
            }
            if tool_result.is_empty() {
                tool_result = format!("command exited with status {}", output.exit_code);
            }

            history.push(json!({
                "type": "function_call_output",
                "call_id": function_call.call_id,
                "output": tool_result,
            }));
        }
    }
}

fn append_output_items(history: &mut Vec<Value>, response: &Value) {
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        history.extend(output.iter().cloned());
    }
}

fn wait_for_permission(
    stdin: &mut dyn BufRead,
    request_id: &str,
    pending_responses: &mut HashMap<String, String>,
) -> io::Result<String> {
    if let Some(option_id) = pending_responses.remove(request_id) {
        return Ok(option_id);
    }

    loop {
        match read_message(stdin)? {
            InboundMessage::PermissionResponse {
                request_id: incoming_id,
                option_id,
            } if incoming_id == request_id => return Ok(option_id),
            InboundMessage::PermissionResponse {
                request_id: incoming_id,
                option_id,
            } => {
                pending_responses.insert(incoming_id, option_id);
                continue;
            }
            InboundMessage::Start(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unexpected start message while waiting for permission",
                ));
            }
        }
    }
}

fn read_message(stdin: &mut dyn BufRead) -> io::Result<InboundMessage> {
    let mut line = String::new();
    let bytes = stdin.read_line(&mut line)?;
    if bytes == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stdin closed"));
    }
    serde_json::from_str(line.trim()).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON message: {}", error),
        )
    })
}

fn emit_line(message: &OutboundMessage<'_>) {
    let mut stdout = io::stdout();
    let payload = serde_json::to_string(message).expect("serialize outbound message");
    let _ = writeln!(stdout, "{payload}");
    let _ = stdout.flush();
}

fn provider_endpoint() -> String {
    let base =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com".to_string());
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/responses")
    } else {
        format!("{trimmed}/v1/responses")
    }
}

fn call_responses_api(
    model: &str,
    thought_level: &str,
    developer_instructions: Option<&str>,
    history: &[Value],
    allow_tools: bool,
) -> io::Result<Value> {
    let mut body = json!({
        "model": model,
        "input": history,
        "reasoning": {
            "effort": thought_level,
        },
    });

    if let Some(instructions) = developer_instructions {
        body["instructions"] = json!(instructions);
    }

    if allow_tools {
        body["tools"] = json!([
            {
                "type": "function",
                "name": "shell",
                "description": "Execute a shell command inside the workspace and return stdout/stderr.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to run."
                        }
                    },
                    "required": ["command"]
                }
            }
        ]);
    } else {
        body["tools"] = json!([]);
    }

    let payload = serde_json::to_string(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

    let url = provider_endpoint();
    let api_key = std::env::var("OPENAI_API_KEY").ok();

    let mut req = wasi_http::Request::new(wasi_http::Method::Post, &url)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
    req = req.header("Content-Type", "application/json");
    if let Some(api_key) = api_key {
        req = req.header("Authorization", &format!("Bearer {api_key}"));
    }
    req = req.json_body(&payload);

    let client = wasi_http::HttpClient::new();
    let response = client
        .send(&req)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;

    if response.status >= 400 {
        let text = response
            .text()
            .unwrap_or_else(|_| "<non-utf8 body>".to_string());
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("provider returned {}: {}", response.status, text),
        ));
    }

    let text = response
        .text()
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
    serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

fn extract_function_calls(response: &Value) -> io::Result<Vec<FunctionCall>> {
    let mut function_calls = Vec::new();
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return Ok(function_calls);
    };

    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }

        let arguments = match item.get("arguments") {
            Some(Value::String(text)) => serde_json::from_str(text)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?,
            Some(value) => value.clone(),
            None => json!({}),
        };

        function_calls.push(FunctionCall {
            call_id: item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "function_call missing call_id")
                })?
                .to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "function_call missing name")
                })?
                .to_string(),
            arguments,
        });
    }

    Ok(function_calls)
}

fn extract_assistant_text(response: &Value) -> io::Result<String> {
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        return Ok(text.to_string());
    }

    let mut parts = Vec::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parts.push(text.to_string());
                            } else if let Some(text) =
                                part.get("output_text").and_then(Value::as_str)
                            {
                                parts.push(text.to_string());
                            }
                        }
                    }
                }
                Some("output_text") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    Ok(parts.join(""))
}

fn print_help() {
    println!(
        "codex-exec {} — headless Codex agent for Agent OS WasmVM",
        VERSION
    );
    println!();
    println!("USAGE:");
    println!("    codex-exec [OPTIONS] [PROMPT]");
    println!("    codex-exec --session-turn");
    println!("    echo '<prompt>' | codex-exec");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help          Print this help message");
    println!("    -V, --version       Print version information");
    println!("    --http-test URL     Test HTTP client via host_net");
    println!("    --stub-test         Validate WASI stub crates");
    println!("    --session-turn      Run a single ACP-managed turn over NDJSON stdio");
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
                Err(e) => eprintln!("body decode error: {}", e),
            }
        }
        Err(e) => {
            eprintln!("http error: {}", e);
            std::process::exit(1);
        }
    }
}
