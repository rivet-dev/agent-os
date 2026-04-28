/// HTTP client test binary for validating wasi-http through host_net.
///
/// Usage:
///   http-test get <url>
///   http-test post <url> <json-body>
///   http-test headers <url> <name:value> [<name:value> ...]
///   http-test sse <url>
///
/// Prints status code and body to stdout. Errors go to stderr.
fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("usage: http-test <command> <url> [args...]");
        eprintln!("  get <url>              — GET request, print body");
        eprintln!("  post <url> <json>      — POST with JSON body");
        eprintln!("  headers <url> <h:v>... — GET with custom headers");
        eprintln!("  sse <url>              — Stream SSE events");
        std::process::exit(1);
    }

    let command = &args[1];
    let url = &args[2];

    let result = match command.as_str() {
        "get" => do_get(url),
        "post" => {
            let body = args.get(3).map(|s| s.as_str()).unwrap_or("{}");
            do_post(url, body)
        }
        "headers" => {
            let header_args: Vec<&str> = args[3..].iter().map(|s| s.as_str()).collect();
            do_get_with_headers(url, &header_args)
        }
        "sse" => do_sse(url),
        _ => {
            eprintln!("unknown command: {}", command);
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

fn do_get(url: &str) -> Result<(), wasi_http::HttpError> {
    let resp = wasi_http::get(url)?;
    println!("status: {}", resp.status);
    println!("body: {}", resp.text()?);
    Ok(())
}

fn do_post(url: &str, json: &str) -> Result<(), wasi_http::HttpError> {
    let resp = wasi_http::post_json(url, json)?;
    println!("status: {}", resp.status);
    println!("body: {}", resp.text()?);
    Ok(())
}

fn do_get_with_headers(url: &str, headers: &[&str]) -> Result<(), wasi_http::HttpError> {
    let client = wasi_http::HttpClient::new();
    let mut req = wasi_http::Request::new(wasi_http::Method::Get, url)?;
    for h in headers {
        if let Some(colon) = h.find(':') {
            let name = h[..colon].trim();
            let value = h[colon + 1..].trim();
            req.headers.push((name.to_string(), value.to_string()));
        }
    }
    let resp = client.send(&req)?;
    println!("status: {}", resp.status);
    println!("body: {}", resp.text()?);
    Ok(())
}

fn do_sse(url: &str) -> Result<(), wasi_http::HttpError> {
    let client = wasi_http::HttpClient::new();
    let req =
        wasi_http::Request::new(wasi_http::Method::Get, url)?.header("Accept", "text/event-stream");
    let (resp, mut reader) = client.send_sse(&req)?;
    println!("status: {}", resp.status);

    while let Some(event) = reader.next_event()? {
        if let Some(ref ev_type) = event.event {
            println!("event: {}", ev_type);
        }
        println!("data: {}", event.data);
        if let Some(ref id) = event.id {
            println!("id: {}", id);
        }
        println!();
    }

    reader.close();
    Ok(())
}
