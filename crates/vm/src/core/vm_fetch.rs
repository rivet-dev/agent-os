use crate::core::SidecarCoreError;
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::BTreeMap;

// Keep raw loopback fetch buffers inside the default sidecar wire frame budget.
pub const VM_FETCH_BUFFER_LIMIT_BYTES: usize = 1024 * 1024;

pub fn parse_kernel_http_fetch_response(
    buffer: &[u8],
    peer_closed: bool,
    url: &str,
) -> Result<Option<String>, SidecarCoreError> {
    let Some(header_end) = find_http_header_end(buffer) else {
        return Ok(None);
    };
    let header_bytes = &buffer[..header_end];
    let head = String::from_utf8_lossy(header_bytes);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(SidecarCoreError::new(format!(
            "invalid vm.fetch HTTP response status line: {status_line}"
        )));
    }
    let status = status_parts
        .next()
        .ok_or_else(|| {
            SidecarCoreError::new(format!(
                "invalid vm.fetch HTTP response status line: {status_line}"
            ))
        })?
        .parse::<u16>()
        .map_err(|error| {
            SidecarCoreError::new(format!(
                "invalid vm.fetch HTTP response status code in {status_line:?}: {error}"
            ))
        })?;
    let status_text = status_parts.next().unwrap_or_default();
    let mut headers = Vec::new();
    let mut raw_headers = Vec::new();
    let mut content_length = None;
    let mut transfer_encoding_values = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(SidecarCoreError::new(format!(
                "invalid vm.fetch HTTP response header line: {line}"
            )));
        };
        let value = value.trim().to_owned();
        let normalized = name.to_ascii_lowercase();
        if normalized == "content-length" {
            content_length = Some(value.parse::<usize>().map_err(|error| {
                SidecarCoreError::new(format!(
                    "invalid vm.fetch Content-Length header {value:?}: {error}"
                ))
            })?);
        } else if normalized == "transfer-encoding" {
            transfer_encoding_values.push(value.clone());
        }
        headers.push(json!([normalized, value.clone()]));
        raw_headers.push(Value::String(name.to_owned()));
        raw_headers.push(Value::String(value));
    }

    let body_start = header_end + 4;
    let transfer_encoding = transfer_encoding_tokens(&transfer_encoding_values);
    let is_chunked = transfer_encoding.iter().any(|token| token == "chunked");
    let body = if is_chunked {
        if content_length.is_some() {
            return Err(SidecarCoreError::new(
                "vm.fetch HTTP response cannot include both Transfer-Encoding: chunked and Content-Length",
            ));
        }
        if transfer_encoding.len() != 1 {
            return Err(SidecarCoreError::new(format!(
                "unsupported vm.fetch Transfer-Encoding: {}",
                transfer_encoding.join(", ")
            )));
        }
        let Some(decoded) = decode_kernel_http_chunked_body(&buffer[body_start..])? else {
            return Ok(None);
        };
        decoded
    } else if !transfer_encoding.is_empty() {
        return Err(SidecarCoreError::new(format!(
            "unsupported vm.fetch Transfer-Encoding: {}",
            transfer_encoding.join(", ")
        )));
    } else if let Some(content_length) = content_length {
        let body_end = body_start.saturating_add(content_length);
        if buffer.len() < body_end {
            return Ok(None);
        }
        buffer[body_start..body_end].to_vec()
    } else if peer_closed {
        buffer[body_start..].to_vec()
    } else {
        return Ok(None);
    };

    serde_json::to_string(&json!({
        "status": status,
        "statusText": status_text,
        "headers": headers,
        "rawHeaders": raw_headers,
        "body": base64::engine::general_purpose::STANDARD.encode(&body),
        "bodyEncoding": "base64",
        "url": url,
    }))
    .map(Some)
    .map_err(|error| SidecarCoreError::new(format!("ERR_AGENTOS_NODE_SYNC_RPC: {error}")))
}

pub fn serialize_kernel_http_fetch_request(
    port: u16,
    path: &str,
    method: &str,
    headers_json: &str,
    body: Option<&str>,
) -> Result<Vec<u8>, SidecarCoreError> {
    let headers = parse_vm_fetch_headers(headers_json)?;
    let method = if method.is_empty() { "GET" } else { method };
    let target_path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    let mut lines = vec![format!("{method} {target_path} HTTP/1.1")];
    let mut has_host = false;
    let mut has_connection = false;
    let mut has_content_length = false;

    for (name, values) in &headers {
        match name.as_str() {
            "host" => has_host = true,
            "connection" => has_connection = true,
            "content-length" => has_content_length = true,
            _ => {}
        }
        lines.push(format!("{name}: {}", values.join(", ")));
    }
    if !has_host {
        lines.push(format!("Host: 127.0.0.1:{port}"));
    }
    if !has_connection {
        lines.push(String::from("Connection: close"));
    }
    let body = body.unwrap_or("").as_bytes();
    if !has_content_length && !body.is_empty() {
        lines.push(format!("Content-Length: {}", body.len()));
    }
    lines.push(String::new());
    lines.push(String::new());

    let mut request = lines.join("\r\n").into_bytes();
    request.extend_from_slice(body);
    Ok(request)
}

pub fn ensure_vm_fetch_response_within_limit(
    response_json: &str,
    operation: &str,
    limit: usize,
) -> Result<(), SidecarCoreError> {
    let size = response_json.len();
    if size > limit {
        return Err(SidecarCoreError::new(format!(
            "{operation} payload is {size} bytes, limit is {limit}"
        )));
    }
    Ok(())
}

pub fn ensure_vm_fetch_raw_response_buffer_within_limit(
    size: usize,
    operation: &str,
) -> Result<(), SidecarCoreError> {
    if size > VM_FETCH_BUFFER_LIMIT_BYTES {
        return Err(SidecarCoreError::new(format!(
            "{operation} raw response buffer is {size} bytes, limit is {VM_FETCH_BUFFER_LIMIT_BYTES}"
        )));
    }
    Ok(())
}

fn parse_vm_fetch_headers(
    headers_json: &str,
) -> Result<BTreeMap<String, Vec<String>>, SidecarCoreError> {
    let headers: BTreeMap<String, Value> = serde_json::from_str(headers_json).map_err(|error| {
        SidecarCoreError::new(format!("vm.fetch headers_json must be valid JSON: {error}"))
    })?;
    let mut normalized = BTreeMap::<String, Vec<String>>::new();
    for (raw_name, value) in headers {
        let values = match value {
            Value::String(text) => vec![text],
            Value::Array(values) => values
                .into_iter()
                .map(|entry| {
                    entry.as_str().map(str::to_owned).ok_or_else(|| {
                        SidecarCoreError::new(format!(
                            "vm.fetch header {raw_name} must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            other => {
                return Err(SidecarCoreError::new(format!(
                    "vm.fetch header {raw_name} must be a string or string array, received {other}"
                )));
            }
        };
        normalized
            .entry(raw_name.to_ascii_lowercase())
            .or_default()
            .extend(values);
    }
    Ok(normalized)
}

fn find_http_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn find_crlf(buffer: &[u8], start: usize) -> Option<usize> {
    buffer
        .get(start..)?
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|offset| start + offset)
}

fn transfer_encoding_tokens(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn decode_kernel_http_chunked_body(buffer: &[u8]) -> Result<Option<Vec<u8>>, SidecarCoreError> {
    let mut offset = 0;
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find_crlf(buffer, offset) else {
            return Ok(None);
        };
        let size_line = std::str::from_utf8(&buffer[offset..line_end]).map_err(|error| {
            SidecarCoreError::new(format!(
                "invalid vm.fetch chunk size line encoding: {error}"
            ))
        })?;
        let size_part = size_line.split(';').next().unwrap_or_default();
        if size_part.is_empty() || !size_part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SidecarCoreError::new(format!(
                "invalid vm.fetch chunk size line: {size_line:?}"
            )));
        }
        let chunk_size = usize::from_str_radix(size_part, 16).map_err(|error| {
            SidecarCoreError::new(format!(
                "invalid vm.fetch chunk size {size_part:?}: {error}"
            ))
        })?;
        let chunk_start = line_end + 2;
        let chunk_end = chunk_start
            .checked_add(chunk_size)
            .ok_or_else(|| SidecarCoreError::new("vm.fetch chunk size overflow"))?;
        if chunk_size > 0 {
            let chunk_terminator_end = chunk_end
                .checked_add(2)
                .ok_or_else(|| SidecarCoreError::new("vm.fetch chunk terminator overflow"))?;
            if chunk_terminator_end > buffer.len() {
                return Ok(None);
            }
            if buffer.get(chunk_end..chunk_terminator_end) != Some(b"\r\n") {
                return Err(SidecarCoreError::new("invalid vm.fetch chunk terminator"));
            }
            body.extend_from_slice(&buffer[chunk_start..chunk_end]);
            offset = chunk_terminator_end;
            continue;
        }

        if buffer.get(chunk_start..chunk_start + 2) == Some(b"\r\n") {
            return Ok(Some(body));
        }
        let Some(trailer_end) = find_http_header_end(&buffer[chunk_start..]) else {
            return Ok(None);
        };
        let trailer_bytes = &buffer[chunk_start..chunk_start + trailer_end];
        let trailers = String::from_utf8_lossy(trailer_bytes);
        for line in trailers.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
                return Err(SidecarCoreError::new(format!(
                    "invalid vm.fetch chunk trailer line: {line}"
                )));
            }
        }
        return Ok(Some(body));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn parses_content_length_response() {
        let response = parse_kernel_http_fetch_response(
            b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\nX-Test: ok\r\n\r\nhello",
            false,
            "http://127.0.0.1:8080/hello",
        )
        .expect("parse response")
        .expect("complete response");
        let value: Value = serde_json::from_str(&response).expect("response json");

        assert_eq!(value["status"], 201);
        assert_eq!(value["statusText"], "Created");
        assert_eq!(value["body"], "aGVsbG8=");
        assert_eq!(value["url"], "http://127.0.0.1:8080/hello");
    }

    #[test]
    fn serializes_loopback_fetch_request() {
        let request = serialize_kernel_http_fetch_request(
            3000,
            "submit",
            "POST",
            r#"{"x-test":["a","b"]}"#,
            Some("hello"),
        )
        .expect("serialize request");

        assert_eq!(
            String::from_utf8(request).expect("utf8 request"),
            "POST /submit HTTP/1.1\r\nx-test: a, b\r\nHost: 127.0.0.1:3000\r\nConnection: close\r\nContent-Length: 5\r\n\r\nhello"
        );
    }

    #[test]
    fn decodes_chunked_response_body() {
        let response = parse_kernel_http_fetch_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
            false,
            "http://127.0.0.1:8080/chunked",
        )
        .expect("parse response")
        .expect("complete response");
        let value: Value = serde_json::from_str(&response).expect("response json");

        assert_eq!(value["body"], "aGVsbG8=");
    }

    #[test]
    fn waits_for_incomplete_body() {
        let response = parse_kernel_http_fetch_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe",
            false,
            "http://127.0.0.1:8080/partial",
        )
        .expect("parse response");

        assert!(response.is_none());
    }

    #[test]
    fn rejects_invalid_chunked_content_length_combination() {
        let error = parse_kernel_http_fetch_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            false,
            "http://127.0.0.1:8080/bad",
        )
        .expect_err("response should be invalid");

        assert!(error
            .to_string()
            .contains("cannot include both Transfer-Encoding: chunked and Content-Length"));
    }
}
