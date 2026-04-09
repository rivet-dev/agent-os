use std::{
    fs,
    io::{self, Write},
};

use wasi_http::{HttpClient, Method, Request};

fn main() {
    match run() {
        Ok(ExitKind::Success) => {}
        Ok(ExitKind::PrintedHelp) => {}
        Err(message) => {
            eprintln!("curl: {message}");
            std::process::exit(1);
        }
    }
}

enum ExitKind {
    Success,
    PrintedHelp,
}

struct Config {
    method: Method,
    url: String,
    body: Option<Vec<u8>>,
    headers: Vec<(String, String)>,
    output_path: Option<String>,
}

fn run() -> Result<ExitKind, String> {
    let Some(config) = parse_args(std::env::args().skip(1))? else {
        print_help();
        return Ok(ExitKind::PrintedHelp);
    };

    let client = HttpClient::new();
    let mut request =
        Request::new(config.method, &config.url).map_err(|error| error.to_string())?;

    for (name, value) in config.headers {
        request = request.header(&name, &value);
    }

    if let Some(body) = config.body {
        request = request.body(body);
    }

    let response = client.send(&request).map_err(|error| error.to_string())?;

    if let Some(path) = config.output_path {
        fs::write(&path, &response.body).map_err(|error| format!("{path}: {error}"))?;
    } else {
        io::stdout()
            .write_all(&response.body)
            .and_then(|()| io::stdout().flush())
            .map_err(|error| error.to_string())?;
    }

    Ok(ExitKind::Success)
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Config>, String> {
    let mut url = None;
    let mut method = None;
    let mut body = None;
    let mut headers = Vec::new();
    let mut output_path = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-s" | "--silent" => {}
            "-X" | "--request" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires an argument"))?;
                method = Some(parse_method(&value)?);
            }
            "-d" | "--data" | "--data-raw" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires an argument"))?;
                body = Some(value.into_bytes());
            }
            "-H" | "--header" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires an argument"))?;
                headers.push(parse_header(&value)?);
            }
            "-o" | "--output" => {
                output_path = Some(
                    args.next()
                        .ok_or_else(|| format!("{arg} requires an argument"))?,
                );
            }
            _ if arg.starts_with("http://") || arg.starts_with("https://") => {
                if url.is_some() {
                    return Err("only one URL is supported".into());
                }
                url = Some(arg);
            }
            _ => return Err(format!("unsupported argument `{arg}`")),
        }
    }

    let url = url.ok_or_else(|| "missing URL".to_string())?;
    let method = method.unwrap_or_else(|| {
        if body.is_some() {
            Method::Post
        } else {
            Method::Get
        }
    });

    Ok(Some(Config {
        method,
        url,
        body,
        headers,
        output_path,
    }))
}

fn parse_method(raw: &str) -> Result<Method, String> {
    match raw.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "DELETE" => Ok(Method::Delete),
        "PATCH" => Ok(Method::Patch),
        "HEAD" => Ok(Method::Head),
        _ => Err(format!("unsupported HTTP method `{raw}`")),
    }
}

fn parse_header(raw: &str) -> Result<(String, String), String> {
    let Some((name, value)) = raw.split_once(':') else {
        return Err(format!("invalid header `{raw}`"));
    };

    let name = name.trim();
    if name.is_empty() {
        return Err(format!("invalid header `{raw}`"));
    }

    Ok((name.to_string(), value.trim().to_string()))
}

fn print_help() {
    println!("Usage: curl [options...] <url>");
    println!();
    println!("Supported options:");
    println!("  -s, --silent           Ignore progress output (no-op)");
    println!("  -X, --request METHOD   Set the HTTP method");
    println!("  -d, --data DATA        Send request body data");
    println!("  -H, --header HEADER    Add a request header");
    println!("  -o, --output PATH      Write the response body to a file");
    println!("  -h, --help             Show this help text");
}
