use super::super::*;
use agentos_execution::backend::{HostServiceError, PayloadLimit};
use agentos_execution::host::HttpHeader;

pub(in crate::execution) struct BoundedHttpRequest {
    pub(in crate::execution) url: Url,
    pub(in crate::execution) method: String,
    pub(in crate::execution) headers: Vec<HttpHeader>,
    pub(in crate::execution) body: Vec<u8>,
    pub(in crate::execution) pinned_addresses: Vec<IpAddr>,
    pub(in crate::execution) default_ca_bundle: Vec<u8>,
    pub(in crate::execution) max_response_bytes: usize,
    pub(in crate::execution) max_header_bytes: usize,
    pub(in crate::execution) max_body_bytes: usize,
}

fn is_http_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

pub(in crate::execution) fn validate_http_request_metadata(
    method: &str,
    headers: &[HttpHeader],
) -> Result<(), SidecarError> {
    if !is_http_token(method) {
        return Err(SidecarError::host(
            "EINVAL",
            "outbound HTTP method must be a valid HTTP token",
        ));
    }
    for header in headers {
        if !is_http_token(header.name.as_str()) {
            return Err(SidecarError::host(
                "EINVAL",
                format!(
                    "outbound HTTP header name {:?} is not a valid HTTP token",
                    header.name.as_str()
                ),
            ));
        }
        if header.value.as_str().bytes().any(|byte| {
            byte == b'\r' || byte == b'\n' || byte == 0x7f || (byte < 0x20 && byte != b'\t')
        }) {
            return Err(SidecarError::host(
                "EINVAL",
                format!(
                    "outbound HTTP header {:?} contains a forbidden control character",
                    header.name.as_str()
                ),
            ));
        }
    }
    Ok(())
}

fn split_http_netloc(netloc: &str) -> Option<(&str, u16)> {
    let (host, port) = netloc.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    Some((host, port))
}

fn http_limit(limit_name: &'static str, limit: usize, observed: usize) -> SidecarError {
    SidecarError::Host(HostServiceError::limit(
        "ERR_AGENTOS_RESOURCE_LIMIT",
        limit_name,
        limit as u64,
        observed as u64,
    ))
}

pub(in crate::execution) fn issue_bounded_http_request(
    request: BoundedHttpRequest,
) -> Result<Value, SidecarError> {
    validate_http_request_metadata(&request.method, &request.headers)?;
    if request.pinned_addresses.is_empty() {
        return Err(SidecarError::host(
            "EACCES",
            "no egress-vetted address available for outbound HTTP request",
        ));
    }
    let pinned_host = request.url.host_str().map(str::to_owned);
    let pinned_port = request.url.port_or_known_default();
    let pinned = request.pinned_addresses;
    let resolver = move |netloc: &str| -> std::io::Result<Vec<SocketAddr>> {
        let (host, port) = split_http_netloc(netloc).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid network location: {netloc}"),
            )
        })?;
        if pinned_host.as_deref() != Some(host) || pinned_port != Some(port) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("EACCES: outbound HTTP resolver is not pinned for {host}:{port}"),
            ));
        }
        Ok(pinned.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
    };
    let mut agent_builder = ureq::AgentBuilder::new()
        .resolver(resolver)
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .timeout_write(Duration::from_secs(15));
    if request.url.scheme() == "https" {
        let tls_options = TlsBridgeOptions {
            is_server: false,
            servername: request.url.host_str().map(str::to_owned),
            alpn_protocols: Some(vec![String::from("http/1.1")]),
            ..TlsBridgeOptions::default()
        };
        agent_builder = agent_builder.tls_config(Arc::new(build_client_tls_config(
            &tls_options,
            &request.default_ca_bundle,
        )?));
    }
    let agent = agent_builder.build();
    let mut outbound = agent.request_url(&request.method, &request.url);
    for header in request.headers {
        if header.name.as_str().eq_ignore_ascii_case("host") {
            continue;
        }
        outbound = outbound.set(header.name.as_str(), header.value.as_str());
    }
    let response = if request.body.is_empty() {
        outbound.call()
    } else {
        outbound.send_bytes(&request.body)
    };
    let response = match response {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(error)) => {
            return Err(SidecarError::host(
                "ERR_HTTP_REQUEST_FAILED",
                error.to_string(),
            ))
        }
    };

    let status = response.status();
    let reason = response.status_text().to_owned();
    let mut header_bytes = 0usize;
    let mut headers = BTreeMap::<String, Vec<String>>::new();
    for raw_name in response.headers_names() {
        for value in response.all(&raw_name) {
            header_bytes = header_bytes
                .saturating_add(raw_name.len())
                .saturating_add(value.len());
            if header_bytes > request.max_header_bytes {
                return Err(http_limit(
                    "runtime.network.maxHttpHeaderBytes",
                    request.max_header_bytes,
                    header_bytes,
                ));
            }
            headers
                .entry(raw_name.to_ascii_lowercase())
                .or_default()
                .push(value.to_owned());
        }
    }
    if let Some(content_length) = response
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if content_length > request.max_body_bytes {
            return Err(http_limit(
                "limits.http.maxFetchResponseBytes",
                request.max_body_bytes,
                content_length,
            ));
        }
    }
    let mut body = Vec::with_capacity(request.max_body_bytes.min(64 * 1024));
    response
        .into_reader()
        .take(request.max_body_bytes.saturating_add(1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| {
            SidecarError::host(
                "ERR_HTTP_REQUEST_FAILED",
                format!("failed to read HTTP response: {error}"),
            )
        })?;
    if body.len() > request.max_body_bytes {
        return Err(http_limit(
            "limits.http.maxFetchResponseBytes",
            request.max_body_bytes,
            body.len(),
        ));
    }
    let result = json!({
        "status": status,
        "reason": reason,
        "url": request.url.as_str(),
        "headers": headers,
        "bodyBase64": base64::engine::general_purpose::STANDARD.encode(body),
    });
    PayloadLimit::new(
        "limits.reactor.maxBridgeResponseBytes",
        request.max_response_bytes,
    )?
    .admit_json(&result)
    .map_err(SidecarError::Host)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    #[test]
    fn split_http_netloc_handles_ipv4_names_and_ipv6() {
        assert_eq!(
            split_http_netloc("example.test:80"),
            Some(("example.test", 80))
        );
        assert_eq!(split_http_netloc("[::1]:443"), Some(("::1", 443)));
        assert_eq!(split_http_netloc("missing-port"), None);
    }

    #[test]
    fn typed_http_limit_names_the_public_configuration_path() {
        let SidecarError::Host(error) = http_limit("limits.http.maxFetchResponseBytes", 8, 9)
        else {
            panic!("expected typed host error");
        };
        assert_eq!(error.code, "ERR_AGENTOS_RESOURCE_LIMIT");
        let details = error.details.expect("limit details");
        assert_eq!(details["configPath"], "limits.http.maxFetchResponseBytes");
        assert_eq!(details["limit"], 8);
        assert_eq!(details["observed"], 9);
    }

    fn header(name: &str, value: &str) -> HttpHeader {
        let limit = PayloadLimit::new("test.http.metadata", 1024).expect("metadata limit");
        HttpHeader {
            name: agentos_execution::host::BoundedString::try_new(name.to_owned(), &limit)
                .expect("header name"),
            value: agentos_execution::host::BoundedString::try_new(value.to_owned(), &limit)
                .expect("header value"),
        }
    }

    #[test]
    fn http_metadata_rejects_invalid_methods_names_and_values() {
        validate_http_request_metadata("GET", &[header("x-test", "ok\tvalue")])
            .expect("valid metadata");
        assert!(validate_http_request_metadata("GET bad", &[]).is_err());
        assert!(validate_http_request_metadata("GET", &[header("bad name", "ok")]).is_err());
        assert!(
            validate_http_request_metadata("GET", &[header("x-test", "bad\r\nvalue")]).is_err()
        );
    }

    #[test]
    fn bounded_http_client_does_not_follow_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept one request");
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect");
        });
        let url =
            Url::parse(&format!("http://127.0.0.1:{}/start", address.port())).expect("test URL");
        let result = issue_bounded_http_request(BoundedHttpRequest {
            url,
            method: String::from("GET"),
            headers: Vec::new(),
            body: Vec::new(),
            pinned_addresses: vec![address.ip()],
            default_ca_bundle: Vec::new(),
            max_response_bytes: 4096,
            max_header_bytes: 1024,
            max_body_bytes: 1024,
        })
        .expect("redirect response");
        server.join().expect("test server");
        assert_eq!(result["status"], 302);
    }
}
