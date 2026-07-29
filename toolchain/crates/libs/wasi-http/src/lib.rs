//! Minimal HTTP/1.1 client for WASI using host_net TCP/TLS imports.
//!
//! Provides `HttpClient` for making HTTP and HTTPS requests through the
//! host_net import module (socket, connect, send, recv, tls_connect).
//! TLS certificate verification is handled by the host runtime.
//!
//! Supports:
//! - GET, POST, PUT, DELETE, PATCH, HEAD methods
//! - Custom headers
//! - JSON request bodies
//! - Streaming SSE (Server-Sent Events) responses
//! - Chunked transfer encoding
//! - Automatic DNS resolution via host_net

use std::fmt;
use std::io;

// AgentOS's owned wasi-libc p1 ABI values for AF_INET and SOCK_STREAM.
const AF_INET: u32 = 1;
const SOCK_STREAM: u32 = 6;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_HEADER_COUNT: usize = 1_024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_SSE_BUFFER_BYTES: usize = 1024 * 1024;
/// Small, non-zero recv timeout (milliseconds) applied to every HTTP socket so
/// the host polls briefly then returns EAGAIN instead of blocking the single
/// guest thread. Must be non-zero — a zero timeval means "blocking" to the host.
const HTTP_RECV_TIMEOUT_MS: u32 = 2;

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Method::Get => write!(f, "GET"),
            Method::Post => write!(f, "POST"),
            Method::Put => write!(f, "PUT"),
            Method::Delete => write!(f, "DELETE"),
            Method::Patch => write!(f, "PATCH"),
            Method::Head => write!(f, "HEAD"),
        }
    }
}

/// Parsed URL components.
#[derive(Debug, Clone)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    /// Parse a URL string into components.
    ///
    /// Supports http:// and https:// schemes.
    pub fn parse(url: &str) -> Result<Self, HttpError> {
        if url.len() > MAX_URL_BYTES || contains_http_ctl(url) {
            return Err(HttpError::InvalidUrl(
                "invalid URL characters or length".into(),
            ));
        }

        let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
            ("https".to_string(), rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            ("http".to_string(), rest)
        } else {
            return Err(HttpError::InvalidUrl(format!(
                "unsupported scheme in: {}",
                url
            )));
        };

        let default_port: u16 = if scheme == "https" { 443 } else { 80 };

        // Split host+port from request target. Query-only URLs use "/" as
        // the path prefix so the request target remains origin-form.
        let split_at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..split_at];
        let suffix = &rest[split_at..];
        let path = if suffix.is_empty() {
            "/".to_string()
        } else if suffix.starts_with('/') {
            suffix.to_string()
        } else {
            format!("/{suffix}")
        };
        if authority.is_empty()
            || authority.contains('@')
            || contains_authority_separator(authority)
            || !is_valid_request_target(&path)
        {
            return Err(HttpError::InvalidUrl("invalid authority or path".into()));
        }

        // Parse host:port
        let (host, port) = if let Some(bracket_end) = authority.find(']') {
            // IPv6: [::1]:port
            if !authority.starts_with('[') {
                return Err(HttpError::InvalidUrl("bad IPv6 host".into()));
            }
            let host = &authority[..=bracket_end];
            let port = if authority.len() > bracket_end + 1
                && authority.as_bytes()[bracket_end + 1] == b':'
            {
                authority[bracket_end + 2..]
                    .parse::<u16>()
                    .map_err(|_| HttpError::InvalidUrl("bad port".into()))?
            } else if authority.len() > bracket_end + 1 {
                return Err(HttpError::InvalidUrl("bad IPv6 authority".into()));
            } else {
                default_port
            };
            (host.to_string(), port)
        } else if let Some(colon) = authority.rfind(':') {
            let host = &authority[..colon];
            let port = authority[colon + 1..]
                .parse::<u16>()
                .map_err(|_| HttpError::InvalidUrl("bad port".into()))?;
            (host.to_string(), port)
        } else {
            (authority.to_string(), default_port)
        };
        if host.is_empty()
            || contains_http_ctl(&host)
            || contains_authority_separator(&host)
            || contains_http_ctl(&path)
        {
            return Err(HttpError::InvalidUrl("invalid host or path".into()));
        }

        Ok(Url {
            scheme,
            host,
            port,
            path,
        })
    }

    /// Whether this URL uses TLS (https).
    pub fn is_tls(&self) -> bool {
        self.scheme == "https"
    }

    /// The host:port string for the Host header.
    pub fn host_header(&self) -> String {
        let default_port = if self.is_tls() { 443 } else { 80 };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// HTTP request builder.
pub struct Request {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: Method, url: &str) -> Result<Self, HttpError> {
        Ok(Request {
            method,
            url: Url::parse(url)?,
            headers: Vec::new(),
            body: None,
        })
    }

    /// Add a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Set a JSON body (also sets Content-Type header).
    pub fn json_body(mut self, json: &str) -> Self {
        self.headers
            .push(("Content-Type".to_string(), "application/json".to_string()));
        self.body = Some(json.as_bytes().to_vec());
        self
    }

    /// Set a raw body.
    pub fn body(mut self, data: Vec<u8>) -> Self {
        self.body = Some(data);
        self
    }

    /// Format the HTTP/1.1 request bytes.
    fn to_bytes(&self) -> Result<Vec<u8>, HttpError> {
        validate_request_headers(&self.headers)?;
        if let Some(ref body) = self.body {
            if body.len() > MAX_REQUEST_BODY_BYTES {
                return Err(HttpError::Protocol("request body too large".into()));
            }
        }

        let mut buf = Vec::with_capacity(512);
        // Request line
        buf.extend_from_slice(format!("{} {} HTTP/1.1\r\n", self.method, self.url.path).as_bytes());

        // Host header (always first)
        buf.extend_from_slice(format!("Host: {}\r\n", self.url.host_header()).as_bytes());

        // User headers
        for (name, value) in &self.headers {
            buf.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }

        // Content-Length for bodies
        if let Some(ref body) = self.body {
            // Only add Content-Length if not already set
            let has_cl = self
                .headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("content-length"));
            if !has_cl {
                buf.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
            }
        }

        // Connection close for simplicity
        let has_connection = self
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("connection"));
        if !has_connection {
            buf.extend_from_slice(b"Connection: close\r\n");
        }

        buf.extend_from_slice(b"\r\n");

        // Body
        if let Some(ref body) = self.body {
            buf.extend_from_slice(body);
        }

        Ok(buf)
    }
}

/// HTTP response.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// Get the body as a UTF-8 string.
    pub fn text(&self) -> Result<String, HttpError> {
        String::from_utf8(self.body.clone())
            .map_err(|e| HttpError::Protocol(format!("invalid UTF-8 body: {}", e)))
    }

    /// Get a header value (case-insensitive lookup).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Check if the response indicates a chunked transfer encoding.
    fn is_chunked(headers: &[(String, String)]) -> bool {
        headers
            .iter()
            .any(|(n, v)| n.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
    }

    /// Get content-length from headers.
    fn content_length(headers: &[(String, String)]) -> Option<usize> {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse().ok())
    }
}

/// Result of a single non-blocking read step on a streaming body/SSE reader.
///
/// `WouldBlock` means no data was available this instant (the socket has a small
/// recv timeout and the host returned EAGAIN); the async caller should yield to
/// the runtime and re-poll rather than spinning.
#[derive(Debug)]
pub enum ChunkPoll<T> {
    /// A unit of data is ready.
    Ready(T),
    /// End of body/stream.
    Eof,
    /// No data available yet; yield and retry.
    WouldBlock,
}

/// SSE (Server-Sent Events) event.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Streaming SSE reader over an HTTP connection.
pub struct SseReader {
    socket_fd: u32,
    buf: Vec<u8>,
    offset: usize,
    done: bool,
}

impl SseReader {
    fn new(socket_fd: u32) -> Self {
        SseReader {
            socket_fd,
            buf: Vec::new(),
            offset: 0,
            done: false,
        }
    }

    /// Read the next SSE event from the stream.
    ///
    /// Returns `None` when the connection closes or the stream ends.
    pub fn next_event(&mut self) -> Result<Option<SseEvent>, HttpError> {
        if self.done {
            return Ok(None);
        }

        let mut recv_buf = [0u8; 4096];

        loop {
            // Check if we have a complete event in the buffer
            if let Some(end) = find_double_newline(&self.buf[self.offset..]) {
                // Copy event text out before mutating buffer
                let event_start = self.offset;
                let event_end = self.offset + end;
                let event_text = String::from_utf8(self.buf[event_start..event_end].to_vec())
                    .map_err(|e| HttpError::Protocol(e.to_string()))?;
                self.offset = event_end + 2; // skip the \n\n

                // Compact buffer periodically
                if self.offset > 8192 {
                    self.buf = self.buf[self.offset..].to_vec();
                    self.offset = 0;
                }

                return Ok(Some(parse_sse_event(&event_text)));
            }

            // Read more data. `recv_blocking` transparently retries on the
            // cooperative WouldBlock (this synchronous SSE reader, used only by the
            // `http-test` CLI, cannot yield to a runtime), so EOF still surfaces as
            // `Ok(0)`.
            match recv_blocking(self.socket_fd, &mut recv_buf) {
                Ok(0) => {
                    self.done = true;
                    // Parse any remaining buffered data as a final event
                    let tail = self.buf[self.offset..].to_vec();
                    if !tail.is_empty() {
                        if let Ok(s) = std::str::from_utf8(&tail) {
                            let s = s.trim();
                            if !s.is_empty() {
                                return Ok(Some(parse_sse_event(s)));
                            }
                        }
                    }
                    return Ok(None);
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&recv_buf[..n as usize]);
                    if self.buf.len().saturating_sub(self.offset) > MAX_SSE_BUFFER_BYTES {
                        self.done = true;
                        return Err(HttpError::Protocol("SSE event too large".into()));
                    }
                }
                Err(e) => {
                    self.done = true;
                    return Err(e);
                }
            }
        }
    }

    /// Close the underlying socket connection.
    pub fn close(self) {
        let _ = wasi_ext::net_close_socket(self.socket_fd);
    }
}

/// HTTP errors.
#[derive(Debug)]
pub enum HttpError {
    InvalidUrl(String),
    Dns(String),
    Socket(String),
    Tls(String),
    Protocol(String),
    Io(io::Error),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::InvalidUrl(msg) => write!(f, "invalid URL: {}", msg),
            HttpError::Dns(msg) => write!(f, "DNS error: {}", msg),
            HttpError::Socket(msg) => write!(f, "socket error: {}", msg),
            HttpError::Tls(msg) => write!(f, "TLS error: {}", msg),
            HttpError::Protocol(msg) => write!(f, "protocol error: {}", msg),
            HttpError::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// HTTP client using host_net imports for TCP/TLS.
///
/// TLS certificate verification is delegated to the host runtime
/// (Node.js tls.connect with system CA certificates).
pub struct HttpClient;

impl HttpClient {
    pub fn new() -> Self {
        HttpClient
    }

    /// Send a request and return the full response.
    pub fn send(&self, req: &Request) -> Result<Response, HttpError> {
        let request_bytes = req.to_bytes()?;
        let fd = self.connect(&req.url)?;

        // Send request
        if let Err(error) = send_all(fd, &request_bytes) {
            let _ = wasi_ext::net_close_socket(fd);
            return Err(error);
        }

        // Read response
        let result = read_response(fd);

        // Close socket
        let _ = wasi_ext::net_close_socket(fd);

        result
    }

    /// Send a request and return an SSE reader for streaming.
    ///
    /// The caller must call `close()` on the returned reader when done.
    pub fn send_sse(&self, req: &Request) -> Result<(Response, SseReader), HttpError> {
        let request_bytes = req.to_bytes()?;
        let fd = self.connect(&req.url)?;

        // Send request
        if let Err(error) = send_all(fd, &request_bytes) {
            let _ = wasi_ext::net_close_socket(fd);
            return Err(error);
        }

        // Read headers only
        let (status, status_text, headers, remaining) = match read_headers(fd) {
            Ok(headers) => headers,
            Err(error) => {
                let _ = wasi_ext::net_close_socket(fd);
                return Err(error);
            }
        };

        // Create SSE reader with any remaining body data
        let mut reader = SseReader::new(fd);
        if !remaining.is_empty() {
            reader.buf = remaining;
        }

        let response = Response {
            status,
            status_text,
            headers,
            body: Vec::new(), // Body will be read via SseReader
        };

        Ok((response, reader))
    }

    /// Send a request and return a reader that yields RAW (de-framed) body bytes
    /// incrementally. Unlike `send_sse`, this does NOT parse SSE framing, so callers
    /// that run their own SSE parser over a raw byte stream (e.g. the reqwest shim's
    /// `bytes_stream`, which codex's `transport.rs` SSE-parses) receive the
    /// unmodified body. Transfer-Encoding/Content-Length are de-framed; the reader
    /// owns the socket and closes it on drop.
    pub fn send_raw_stream(
        &self,
        req: &Request,
    ) -> Result<(Response, RawBodyReader), HttpError> {
        let request_bytes = req.to_bytes()?;
        let fd = self.connect(&req.url)?;
        if let Err(error) = send_all(fd, &request_bytes) {
            let _ = wasi_ext::net_close_socket(fd);
            return Err(error);
        }
        let (status, status_text, headers, remaining) = match read_headers(fd) {
            Ok(h) => h,
            Err(error) => {
                let _ = wasi_ext::net_close_socket(fd);
                return Err(error);
            }
        };
        let mode = if Response::is_chunked(&headers) {
            BodyMode::Chunked
        } else if let Some(len) = Response::content_length(&headers) {
            BodyMode::Fixed { remaining: len }
        } else {
            BodyMode::Close
        };
        let reader = RawBodyReader {
            fd,
            buf: remaining,
            mode,
            done: false,
        };
        let response = Response {
            status,
            status_text,
            headers,
            body: Vec::new(),
        };
        Ok((response, reader))
    }

    /// Establish a TCP connection (with optional TLS upgrade) to the URL's host.
    fn connect(&self, url: &Url) -> Result<u32, HttpError> {
        // Create TCP socket
        let fd = wasi_ext::socket(AF_INET, SOCK_STREAM, 0)
            .map_err(|e| HttpError::Socket(format!("socket() failed: errno {}", e)))?;

        // Connect using host:port format (host_net does DNS resolution internally)
        let addr = format!("{}:{}", url.host, url.port);
        if let Err(e) = wasi_ext::connect(fd, addr.as_bytes()) {
            let _ = wasi_ext::net_close_socket(fd);
            return Err(HttpError::Socket(format!(
                "connect({}) failed: errno {}",
                addr, e
            )));
        }

        // Upgrade to TLS if HTTPS
        if url.is_tls() {
            if let Err(e) = wasi_ext::tls_connect(fd, url.host.as_bytes()) {
                let _ = wasi_ext::net_close_socket(fd);
                return Err(HttpError::Tls(format!(
                    "TLS handshake failed for {}: errno {}",
                    url.host, e
                )));
            }
        }

        // Opt this socket into cooperative non-blocking recv: with a small,
        // non-zero recv timeout the host polls briefly then returns EAGAIN
        // (surfaced as `RecvOutcome::WouldBlock`) instead of monopolizing the
        // single guest thread while a body is in flight. The async layers
        // (reqwest-shim) yield on WouldBlock so other runtime tasks make
        // progress. Best-effort: if the host rejects the option we fall back to
        // the previous blocking behavior rather than failing the request.
        let _ = wasi_ext::set_recv_timeout_ms(fd, HTTP_RECV_TIMEOUT_MS);

        Ok(fd)
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Body framing for an in-progress streamed response.
enum BodyMode {
    /// Content-Length: `remaining` bytes of body still to deliver.
    Fixed { remaining: usize },
    /// Transfer-Encoding: chunked.
    Chunked,
    /// No framing — deliver bytes until the connection closes.
    Close,
}

/// Incremental raw-body reader returned by [`HttpClient::send_raw_stream`].
///
/// `read_chunk` yields the next piece of de-framed body bytes (one HTTP chunk for
/// chunked encoding, one `recv` worth otherwise), or `None` at end of body. It does
/// NOT parse SSE — callers that need SSE events parse the raw bytes themselves.
/// Owns the socket fd and closes it on drop.
pub struct RawBodyReader {
    fd: u32,
    buf: Vec<u8>,
    mode: BodyMode,
    done: bool,
}

impl RawBodyReader {
    /// Read the next piece of body cooperatively.
    ///
    /// Returns [`ChunkPoll::Ready`] with body bytes, [`ChunkPoll::Eof`] at end of
    /// body, or [`ChunkPoll::WouldBlock`] when no data is available yet (the async
    /// caller should yield and re-poll). Partial framing state is retained across
    /// `WouldBlock` returns, so resuming continues where it left off.
    pub fn read_chunk(&mut self) -> Result<ChunkPoll<Vec<u8>>, HttpError> {
        if self.done {
            return Ok(ChunkPoll::Eof);
        }
        match &mut self.mode {
            BodyMode::Fixed { remaining } => {
                if *remaining == 0 {
                    self.done = true;
                    return Ok(ChunkPoll::Eof);
                }
                if !self.buf.is_empty() {
                    let take = self.buf.len().min(*remaining);
                    let out: Vec<u8> = self.buf.drain(..take).collect();
                    *remaining -= take;
                    if *remaining == 0 {
                        self.done = true;
                    }
                    return Ok(ChunkPoll::Ready(out));
                }
                let mut recv_buf = [0u8; 8192];
                let n = match recv_cooperative_http(self.fd, &mut recv_buf)? {
                    ChunkPoll::Ready(n) => n,
                    ChunkPoll::Eof => {
                        self.done = true;
                        return Ok(ChunkPoll::Eof);
                    }
                    ChunkPoll::WouldBlock => return Ok(ChunkPoll::WouldBlock),
                };
                let take = (n as usize).min(*remaining);
                *remaining -= take;
                if *remaining == 0 {
                    self.done = true;
                }
                Ok(ChunkPoll::Ready(recv_buf[..take].to_vec()))
            }
            BodyMode::Chunked => {
                let mut recv_buf = [0u8; 8192];
                loop {
                    if let Some(pos) = find_crlf(&self.buf) {
                        let size_str = std::str::from_utf8(&self.buf[..pos]).map_err(|e| {
                            HttpError::Protocol(format!("invalid chunk size: {}", e))
                        })?;
                        let chunk_size = usize::from_str_radix(size_str.trim(), 16)
                            .map_err(|e| {
                                HttpError::Protocol(format!("invalid chunk size: {}", e))
                            })?;
                        self.buf.drain(..pos + 2); // skip size line + CRLF
                        if chunk_size == 0 {
                            self.done = true;
                            return Ok(ChunkPoll::Eof);
                        }
                        if chunk_size > MAX_RESPONSE_BODY_BYTES {
                            return Err(HttpError::Protocol("response body too large".into()));
                        }
                        while self.buf.len() < chunk_size + 2 {
                            match recv_cooperative_http(self.fd, &mut recv_buf)? {
                                ChunkPoll::Ready(n) => {
                                    self.buf.extend_from_slice(&recv_buf[..n as usize]);
                                }
                                ChunkPoll::Eof => {
                                    return Err(HttpError::Protocol(
                                        "connection closed in chunk".into(),
                                    ));
                                }
                                ChunkPoll::WouldBlock => return Ok(ChunkPoll::WouldBlock),
                            }
                        }
                        if &self.buf[chunk_size..chunk_size + 2] != b"\r\n" {
                            return Err(HttpError::Protocol("missing chunk terminator".into()));
                        }
                        let out: Vec<u8> = self.buf[..chunk_size].to_vec();
                        self.buf.drain(..chunk_size + 2); // skip chunk + CRLF
                        return Ok(ChunkPoll::Ready(out));
                    }
                    match recv_cooperative_http(self.fd, &mut recv_buf)? {
                        ChunkPoll::Ready(n) => {
                            self.buf.extend_from_slice(&recv_buf[..n as usize]);
                        }
                        ChunkPoll::Eof => {
                            return Err(HttpError::Protocol(
                                "connection closed reading chunk size".into(),
                            ));
                        }
                        ChunkPoll::WouldBlock => return Ok(ChunkPoll::WouldBlock),
                    }
                }
            }
            BodyMode::Close => {
                if !self.buf.is_empty() {
                    return Ok(ChunkPoll::Ready(std::mem::take(&mut self.buf)));
                }
                let mut recv_buf = [0u8; 8192];
                match recv_cooperative_http(self.fd, &mut recv_buf)? {
                    ChunkPoll::Ready(n) => Ok(ChunkPoll::Ready(recv_buf[..n as usize].to_vec())),
                    ChunkPoll::Eof => {
                        self.done = true;
                        Ok(ChunkPoll::Eof)
                    }
                    ChunkPoll::WouldBlock => Ok(ChunkPoll::WouldBlock),
                }
            }
        }
    }
}

impl Drop for RawBodyReader {
    fn drop(&mut self) {
        let _ = wasi_ext::net_close_socket(self.fd);
    }
}

// ============================================================
// Internal helpers
// ============================================================

/// Blocking recv that transparently retries on cooperative `WouldBlock`.
///
/// HTTP sockets carry a small recv timeout so the host returns EAGAIN instead of
/// blocking the thread. Synchronous readers (header parsing, the buffered `send`
/// path) cannot yield to the runtime mid-call, so they simply re-poll: behavior
/// is byte-identical to the previous always-blocking recv, just split across
/// short host polls. The streaming readers below DO surface `WouldBlock` so the
/// async layer can yield.
fn recv_blocking(fd: u32, buf: &mut [u8]) -> Result<u32, HttpError> {
    loop {
        match wasi_ext::recv_cooperative(fd, buf, 0) {
            Ok(wasi_ext::RecvOutcome::Read(n)) => return Ok(n as u32),
            Ok(wasi_ext::RecvOutcome::Eof) => return Ok(0),
            Ok(wasi_ext::RecvOutcome::WouldBlock) => continue,
            Err(e) => return Err(HttpError::Socket(format!("recv failed: errno {}", e))),
        }
    }
}

/// Cooperative recv for the streaming readers: maps the host's EAGAIN to
/// [`ChunkPoll::WouldBlock`] so the async layer can yield instead of blocking.
fn recv_cooperative_http(fd: u32, buf: &mut [u8]) -> Result<ChunkPoll<u32>, HttpError> {
    match wasi_ext::recv_cooperative(fd, buf, 0) {
        Ok(wasi_ext::RecvOutcome::Read(n)) => Ok(ChunkPoll::Ready(n as u32)),
        Ok(wasi_ext::RecvOutcome::Eof) => Ok(ChunkPoll::Eof),
        Ok(wasi_ext::RecvOutcome::WouldBlock) => Ok(ChunkPoll::WouldBlock),
        Err(e) => Err(HttpError::Socket(format!("recv failed: errno {}", e))),
    }
}

/// Send all bytes on a socket, handling partial sends.
fn send_all(fd: u32, data: &[u8]) -> Result<(), HttpError> {
    let mut offset = 0;
    while offset < data.len() {
        let n = wasi_ext::send(fd, &data[offset..], 0)
            .map_err(|e| HttpError::Socket(format!("send failed: errno {}", e)))?;
        if n == 0 {
            return Err(HttpError::Socket("send returned zero bytes".into()));
        }
        offset += n as usize;
    }
    Ok(())
}

/// Read response headers and return (status, status_text, headers, remaining_body_bytes).
fn read_headers(fd: u32) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>), HttpError> {
    let mut buf = Vec::with_capacity(4096);
    let mut recv_buf = [0u8; 4096];

    loop {
        let n = recv_blocking(fd, &mut recv_buf)?;
        if n == 0 {
            return Err(HttpError::Protocol(
                "connection closed before headers complete".into(),
            ));
        }
        buf.extend_from_slice(&recv_buf[..n as usize]);

        // Look for end of headers
        if let Some(header_end) = find_header_end(&buf) {
            let header_bytes = &buf[..header_end];
            let header_str = std::str::from_utf8(header_bytes)
                .map_err(|e| HttpError::Protocol(format!("invalid header encoding: {}", e)))?;

            let (status, status_text, headers) = parse_response_headers(header_str)?;
            let remaining = buf[header_end + 4..].to_vec(); // skip \r\n\r\n

            return Ok((status, status_text, headers, remaining));
        }

        // Safety limit on header size
        if buf.len() > MAX_HEADER_BYTES {
            return Err(HttpError::Protocol("headers too large (>64KB)".into()));
        }
    }
}

/// Read a complete HTTP response (headers + body).
fn read_response(fd: u32) -> Result<Response, HttpError> {
    let (status, status_text, headers, remaining) = read_headers(fd)?;

    let body = if Response::is_chunked(&headers) {
        read_chunked_body(fd, remaining)?
    } else if let Some(len) = Response::content_length(&headers) {
        read_fixed_body(fd, remaining, len)?
    } else {
        // Read until connection close
        read_until_close(fd, remaining)?
    };

    Ok(Response {
        status,
        status_text,
        headers,
        body,
    })
}

/// Read body with known Content-Length.
fn read_fixed_body(fd: u32, initial: Vec<u8>, length: usize) -> Result<Vec<u8>, HttpError> {
    if length > MAX_RESPONSE_BODY_BYTES {
        return Err(HttpError::Protocol("response body too large".into()));
    }
    if initial.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(HttpError::Protocol("response body too large".into()));
    }
    let mut body = initial;
    let mut recv_buf = [0u8; 8192];

    while body.len() < length {
        let n = recv_blocking(fd, &mut recv_buf)?;
        if n == 0 {
            break;
        }
        if body.len() + n as usize > MAX_RESPONSE_BODY_BYTES {
            return Err(HttpError::Protocol("response body too large".into()));
        }
        body.extend_from_slice(&recv_buf[..n as usize]);
    }

    body.truncate(length);
    Ok(body)
}

/// Read chunked transfer-encoded body.
fn read_chunked_body(fd: u32, initial: Vec<u8>) -> Result<Vec<u8>, HttpError> {
    let mut buf = initial;
    let mut body = Vec::new();
    let mut recv_buf = [0u8; 8192];
    enforce_body_limit(buf.len())?;

    loop {
        // Find chunk size line
        loop {
            if let Some(pos) = find_crlf(&buf) {
                let size_str = std::str::from_utf8(&buf[..pos])
                    .map_err(|e| HttpError::Protocol(format!("invalid chunk size: {}", e)))?;
                let chunk_size = usize::from_str_radix(size_str.trim(), 16)
                    .map_err(|e| HttpError::Protocol(format!("invalid chunk size: {}", e)))?;

                buf = buf[pos + 2..].to_vec(); // skip \r\n

                if chunk_size == 0 {
                    return Ok(body);
                }
                if chunk_size > MAX_RESPONSE_BODY_BYTES
                    || body.len() + chunk_size > MAX_RESPONSE_BODY_BYTES
                {
                    return Err(HttpError::Protocol("response body too large".into()));
                }

                // Read chunk_size bytes + trailing \r\n
                while buf.len() < chunk_size + 2 {
                    let n = recv_blocking(fd, &mut recv_buf)?;
                    if n == 0 {
                        return Err(HttpError::Protocol("connection closed in chunk".into()));
                    }
                    buf.extend_from_slice(&recv_buf[..n as usize]);
                    enforce_body_limit(buf.len() + body.len())?;
                }
                if &buf[chunk_size..chunk_size + 2] != b"\r\n" {
                    return Err(HttpError::Protocol("missing chunk terminator".into()));
                }

                body.extend_from_slice(&buf[..chunk_size]);
                buf = buf[chunk_size + 2..].to_vec(); // skip chunk data + \r\n
                break;
            }

            // Need more data for chunk size line
            let n = recv_blocking(fd, &mut recv_buf)?;
            if n == 0 {
                return Err(HttpError::Protocol(
                    "connection closed reading chunk size".into(),
                ));
            }
            buf.extend_from_slice(&recv_buf[..n as usize]);
            enforce_body_limit(buf.len() + body.len())?;
        }
    }
}

/// Read until connection closes.
fn read_until_close(fd: u32, initial: Vec<u8>) -> Result<Vec<u8>, HttpError> {
    let mut body = initial;
    let mut recv_buf = [0u8; 8192];
    enforce_body_limit(body.len())?;

    loop {
        let n = recv_blocking(fd, &mut recv_buf)?;
        if n == 0 {
            break;
        }
        enforce_body_limit(body.len() + n as usize)?;
        body.extend_from_slice(&recv_buf[..n as usize]);
    }

    Ok(body)
}

/// Parse the status line and headers from the header block.
fn parse_response_headers(
    header_str: &str,
) -> Result<(u16, String, Vec<(String, String)>), HttpError> {
    let mut lines = header_str.split("\r\n");

    // Status line: HTTP/1.1 200 OK
    let status_line = lines
        .next()
        .ok_or_else(|| HttpError::Protocol("empty response".into()))?;
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts
        .next()
        .ok_or_else(|| HttpError::Protocol("missing HTTP version".into()))?;
    let status_str = parts
        .next()
        .ok_or_else(|| HttpError::Protocol("missing status code".into()))?;
    let status: u16 = status_str
        .parse()
        .map_err(|_| HttpError::Protocol(format!("invalid status code: {}", status_str)))?;
    let status_text = parts.next().unwrap_or("").to_string();

    // Headers
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(colon) = line.find(':') {
            if headers.len() >= MAX_HEADER_COUNT {
                return Err(HttpError::Protocol("too many headers".into()));
            }
            let name = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim().to_string();
            validate_header_name(&name)
                .map_err(|msg| HttpError::Protocol(format!("invalid header name: {}", msg)))?;
            if contains_http_ctl(&value) {
                return Err(HttpError::Protocol("invalid header value".into()));
            }
            headers.push((name, value));
        }
    }

    Ok((status, status_text, headers))
}

/// Find \r\n\r\n in a byte slice (end of HTTP headers).
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Find \r\n in a byte slice.
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

/// Find \n\n in a byte slice (SSE event separator).
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

/// Parse an SSE event block.
fn parse_sse_event(block: &str) -> SseEvent {
    let mut event = None;
    let mut data_lines = Vec::new();
    let mut id = None;

    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start_matches(' ').to_string());
        } else if let Some(rest) = line.strip_prefix("id:") {
            id = Some(rest.trim().to_string());
        }
    }

    SseEvent {
        event,
        data: data_lines.join("\n"),
        id,
    }
}

fn validate_request_headers(headers: &[(String, String)]) -> Result<(), HttpError> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(HttpError::Protocol("too many request headers".into()));
    }
    for (name, value) in headers {
        validate_header_name(name)
            .map_err(|msg| HttpError::Protocol(format!("invalid header name: {}", msg)))?;
        if contains_http_ctl(value) {
            return Err(HttpError::Protocol("invalid header value".into()));
        }
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || !name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
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
    {
        return Err("bad token");
    }
    Ok(())
}

fn contains_http_ctl(value: &str) -> bool {
    value.bytes().any(|b| b < 0x20 || b == 0x7f)
}

fn contains_authority_separator(value: &str) -> bool {
    value
        .bytes()
        .any(|b| matches!(b, b' ' | b'\t' | b'/' | b'?' | b'#'))
}

fn is_valid_request_target(value: &str) -> bool {
    value.bytes().all(|b| !matches!(b, 0x00..=0x20 | 0x7f))
}

fn enforce_body_limit(len: usize) -> Result<(), HttpError> {
    if len > MAX_RESPONSE_BODY_BYTES {
        return Err(HttpError::Protocol("response body too large".into()));
    }
    Ok(())
}

/// Convenience function: GET request.
pub fn get(url: &str) -> Result<Response, HttpError> {
    let client = HttpClient::new();
    let req = Request::new(Method::Get, url)?;
    client.send(&req)
}

/// Convenience function: POST request with JSON body.
pub fn post_json(url: &str, json: &str) -> Result<Response, HttpError> {
    let client = HttpClient::new();
    let req = Request::new(Method::Post, url)?.json_body(json);
    client.send(&req)
}

#[cfg(test)]
mod tests {
    use super::{Method, Request, Url};

    #[test]
    fn url_parse_preserves_query_and_fragment_in_request_target() {
        let url = Url::parse("http://example.com?x=1#frag").expect("parse url");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.path, "/?x=1#frag");
    }

    #[test]
    fn url_parse_rejects_spaces_in_authority_or_request_target() {
        assert!(Url::parse("http://exa mple.com/").is_err());
        assert!(Url::parse("http://example.com/a b").is_err());
    }

    #[test]
    fn request_rejects_header_injection_before_serializing() {
        let request = Request::new(Method::Get, "http://example.com/")
            .expect("request")
            .header("X-Test", "ok\r\nInjected: value");
        assert!(request.to_bytes().is_err());
    }
}
