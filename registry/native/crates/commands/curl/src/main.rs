fn main() {
    std::process::exit(run(std::env::args().skip(1)));
}

fn run(args: impl IntoIterator<Item = String>) -> i32 {
    match parse_args(args) {
        Ok(options) => match execute(options) {
            Ok(exit_code) => exit_code,
            Err(error) => {
                eprintln!("curl: {error}");
                1
            }
        },
        Err(error) => {
            eprintln!("curl: {error}");
            2
        }
    }
}

struct Options {
    method: Option<HttpMethod>,
    data: Option<String>,
    url: String,
}

#[derive(Clone, Copy)]
enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut method = None;
    let mut data = None;
    let mut url = None;
    let mut positional_only = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if !positional_only {
            match arg.as_str() {
                "-s" | "--silent" => continue,
                "-X" | "--request" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "missing value for -X/--request".to_string())?;
                    method = Some(parse_method(&value)?);
                    continue;
                }
                "-d" | "--data" | "--data-raw" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "missing value for -d/--data".to_string())?;
                    data = Some(value);
                    continue;
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                _ => {}
            }
        }

        if arg.starts_with('-') && !positional_only {
            return Err(format!("unsupported option: {arg}"));
        }
        if url.replace(arg).is_some() {
            return Err("multiple URLs are not supported".to_string());
        }
    }

    let url = url.ok_or_else(|| usage().to_string())?;
    Ok(Options { method, data, url })
}

fn parse_method(value: &str) -> Result<HttpMethod, String> {
    match value.to_ascii_uppercase().as_str() {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "DELETE" => Ok(HttpMethod::Delete),
        "PATCH" => Ok(HttpMethod::Patch),
        "HEAD" => Ok(HttpMethod::Head),
        _ => Err(format!("unsupported HTTP method: {value}")),
    }
}

fn execute(options: Options) -> Result<i32, String> {
    let method = options
        .method
        .unwrap_or(if options.data.is_some() { HttpMethod::Post } else { HttpMethod::Get });
    let method_name = method.as_str();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "/".to_string());
    let mut argv = vec!["node", "-e", CURL_NODE_SCRIPT, method_name, options.url.as_str()];
    if let Some(data) = options.data.as_deref() {
        argv.push(data);
    }

    let mut child = wasi_spawn::spawn_child(&argv, &[], &cwd).map_err(|error| error.to_string())?;
    let output = child.consume_output().map_err(|error| error.to_string())?;

    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(output.exit_code)
}

fn usage() -> &'static str {
    "usage: curl [-s] [-X METHOD] [-d DATA] <url>"
}

impl HttpMethod {
    fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Head => "HEAD",
        }
    }
}

const CURL_NODE_SCRIPT: &str = r#"
const http = require("http");
const https = require("https");
const { URL } = require("url");

const [, method, urlText, bodyArg] = process.argv;
if (!method || !urlText) {
  console.error("usage: curl [-s] [-X METHOD] [-d DATA] <url>");
  process.exit(2);
}

let url;
try {
  url = new URL(urlText);
} catch (error) {
  console.error(String(error));
  process.exit(2);
}

const client =
  url.protocol === "http:" ? http :
  url.protocol === "https:" ? https :
  null;
if (!client) {
  console.error(`unsupported protocol: ${url.protocol}`);
  process.exit(2);
}

const headers = {};
const hasBody = bodyArg !== undefined;
if (hasBody) {
  headers["Content-Type"] = "application/x-www-form-urlencoded";
  headers["Content-Length"] = Buffer.byteLength(bodyArg);
}

const req = client.request({
  protocol: url.protocol,
  hostname: url.hostname,
  port: url.port || undefined,
  path: `${url.pathname}${url.search}`,
  method,
  headers,
  agent: false,
}, (res) => {
  res.setEncoding("utf8");
  res.on("data", (chunk) => process.stdout.write(chunk));
  res.on("end", () => process.exit(0));
});

req.on("error", (error) => {
  console.error(String(error?.message ?? error));
  process.exit(1);
});

if (hasBody) {
  req.write(bodyArg);
}
req.end();
"#;
