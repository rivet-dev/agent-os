//! `reqwest` 0.12 API shim for wasm32-wasip1, backed by agentos `wasi-http`
//! (host_net TCP/TLS). Drop-in target for `[patch.crates-io] reqwest`.
//!
//! STATUS: scaffold. The buffered request/response path is implemented against
//! `wasi_http::HttpClient::send`. The streaming path (`Response::bytes_stream`)
//! currently yields the fully-buffered body as a single chunk — see the TODO on
//! `bytes_stream`. True incremental streaming needs a RAW chunk reader added to
//! `wasi-http` (its `SseReader` SSE-parses, but codex's `transport.rs` does its
//! own SSE parsing over `bytes_stream`, so the shim must surface raw bytes).
//!
//! Not yet wired into `[patch.crates-io]` — doing so before it compiles against
//! codex-exec's whole subtree would break every command's `make wasm` build.

use std::time::Duration;

use bytes::Bytes;
pub use http::Method;
pub use http::StatusCode;

/// `reqwest::header` — reqwest re-exports the `http` crate's header types.
pub mod header {
    pub use http::header::*;
    pub use http::HeaderMap;
    pub use http::HeaderName;
    pub use http::HeaderValue;
}

pub use header::HeaderMap;

/// `reqwest::redirect` — redirect policy. Redirects are followed (or not) host-side
/// by wasi-http; this surface exists so codex/rmcp's builder calls compile.
pub mod redirect {
    #[derive(Clone, Debug)]
    pub struct Policy;
    impl Policy {
        pub fn none() -> Self {
            Policy
        }
        pub fn limited(_max: usize) -> Self {
            Policy
        }
        pub fn default() -> Self {
            Policy
        }
    }
    impl Default for Policy {
        fn default() -> Self {
            Policy
        }
    }
}

/// `reqwest::Url` IS the `url` crate's `Url` (reqwest re-exports it), so all of
/// `host_str`/`join`/`path`/`scheme`/`set_path`/… come for free.
pub use url::Url;

fn parse_url(input: &str) -> Result<Url, Error> {
    Url::parse(input).map_err(|e| Error::new(e.to_string()))
}

/// Yield control to the runtime exactly once.
///
/// Returns `Pending` on the first poll (after registering an immediate wakeup so
/// the task is re-polled right away) and `Ready` on the second. On the
/// single-threaded VM this is a cooperative yield point: while a body recv would
/// block, the runtime gets to drive other tasks before we retry. Same shape as
/// the non-blocking pipe-I/O fix.
fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

struct YieldNow {
    yielded: bool,
}

impl std::future::Future for YieldNow {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.yielded {
            std::task::Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

/// `reqwest::IntoUrl` — codex/rmcp pass &str/String/Url.
pub trait IntoUrl {
    fn into_url(self) -> Result<Url, Error>;
    fn as_str(&self) -> &str;
}

impl IntoUrl for &str {
    fn into_url(self) -> Result<Url, Error> {
        parse_url(self)
    }
    fn as_str(&self) -> &str {
        self
    }
}

impl IntoUrl for String {
    fn into_url(self) -> Result<Url, Error> {
        parse_url(&self)
    }
    fn as_str(&self) -> &str {
        String::as_str(self)
    }
}

impl IntoUrl for &String {
    fn into_url(self) -> Result<Url, Error> {
        parse_url(self)
    }
    fn as_str(&self) -> &str {
        String::as_str(self)
    }
}

impl IntoUrl for Url {
    fn into_url(self) -> Result<Url, Error> {
        Ok(self)
    }
    fn as_str(&self) -> &str {
        Url::as_str(self)
    }
}

impl IntoUrl for &Url {
    fn into_url(self) -> Result<Url, Error> {
        Ok(self.clone())
    }
    fn as_str(&self) -> &str {
        Url::as_str(self)
    }
}

/// `reqwest::Error`.
#[derive(Debug)]
pub struct Error {
    msg: String,
    status: Option<StatusCode>,
    url: Option<Url>,
}

impl Error {
    fn new(msg: impl Into<String>) -> Self {
        Error {
            msg: msg.into(),
            status: None,
            url: None,
        }
    }
    pub fn status(&self) -> Option<StatusCode> {
        self.status
    }
    pub fn is_timeout(&self) -> bool {
        false
    }
    pub fn is_connect(&self) -> bool {
        false
    }
    pub fn is_request(&self) -> bool {
        false
    }
    pub fn is_body(&self) -> bool {
        false
    }
    pub fn is_decode(&self) -> bool {
        false
    }
    pub fn is_status(&self) -> bool {
        self.status.is_some()
    }
    pub fn url(&self) -> Option<&Url> {
        self.url.as_ref()
    }
    pub fn url_mut(&mut self) -> Option<&mut Url> {
        self.url.as_mut()
    }
    pub fn with_url(mut self, url: Url) -> Self {
        self.url = Some(url);
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<wasi_http::HttpError> for Error {
    fn from(e: wasi_http::HttpError) -> Self {
        Error::new(e.to_string())
    }
}

/// `reqwest::Body`.
pub struct Body(Vec<u8>);

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self {
        Body(v)
    }
}
impl From<String> for Body {
    fn from(s: String) -> Self {
        Body(s.into_bytes())
    }
}
impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Body(s.as_bytes().to_vec())
    }
}

/// `reqwest::Certificate` — TLS is delegated to the host runtime in wasi-http, so
/// custom roots are accepted and ignored on this target.
#[derive(Clone, Debug)]
pub struct Certificate;

impl Certificate {
    pub fn from_der(_der: &[u8]) -> Result<Self, Error> {
        Ok(Certificate)
    }
    pub fn from_pem(_pem: &[u8]) -> Result<Self, Error> {
        Ok(Certificate)
    }
}

/// `reqwest::Identity` — client TLS identity. TLS is host-brokered, so this is a
/// compile-only placeholder.
#[derive(Clone)]
pub struct Identity;

impl Identity {
    pub fn from_pem(_pem: &[u8]) -> Result<Self, Error> {
        Ok(Identity)
    }
    pub fn from_pkcs12_der(_der: &[u8], _pass: &str) -> Result<Self, Error> {
        Ok(Identity)
    }
}

/// `reqwest::ClientBuilder`.
#[derive(Default)]
pub struct ClientBuilder {
    default_headers: Option<HeaderMap>,
    timeout: Option<Duration>,
}

impl ClientBuilder {
    pub fn new() -> Self {
        ClientBuilder::default()
    }
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = Some(headers);
        self
    }
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
    pub fn add_root_certificate(self, _cert: Certificate) -> Self {
        self
    }
    pub fn danger_accept_invalid_certs(self, _v: bool) -> Self {
        self
    }
    pub fn user_agent<V>(self, _v: V) -> Self {
        self
    }
    pub fn redirect(self, _policy: redirect::Policy) -> Self {
        self
    }
    pub fn connect_timeout(self, _timeout: Duration) -> Self {
        self
    }
    pub fn pool_idle_timeout<D: Into<Option<Duration>>>(self, _d: D) -> Self {
        self
    }
    pub fn pool_max_idle_per_host(self, _n: usize) -> Self {
        self
    }
    pub fn http1_only(self) -> Self {
        self
    }
    pub fn no_proxy(self) -> Self {
        self
    }
    pub fn tcp_keepalive<D: Into<Option<Duration>>>(self, _d: D) -> Self {
        self
    }
    pub fn use_rustls_tls(self) -> Self {
        self
    }
    pub fn identity(self, _id: Identity) -> Self {
        self
    }
    pub fn build(self) -> Result<Client, Error> {
        Ok(Client {
            default_headers: self.default_headers.unwrap_or_default(),
            _timeout: self.timeout,
        })
    }
}

/// `reqwest::Client`.
#[derive(Clone, Debug)]
pub struct Client {
    default_headers: HeaderMap,
    _timeout: Option<Duration>,
}

impl Default for Client {
    fn default() -> Self {
        Client {
            default_headers: HeaderMap::new(),
            _timeout: None,
        }
    }
}

impl Client {
    pub fn new() -> Self {
        Client::default()
    }
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }
    pub fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::GET, url)
    }
    pub fn post<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::POST, url)
    }
    pub fn put<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::PUT, url)
    }
    pub fn patch<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }
    pub fn delete<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }
    pub fn head<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        let url = url.as_str().to_string();
        RequestBuilder {
            method,
            url,
            headers: self.default_headers.clone(),
            body: None,
            err: None,
        }
    }
    /// `Client::execute(Request)` — send a pre-built request (used by oauth2).
    pub async fn execute(&self, req: Request) -> Result<Response, Error> {
        RequestBuilder {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body: req.body,
            err: None,
        }
        .send()
        .await
    }
}

/// `reqwest::Request` — a built request (produced by `RequestBuilder::build`-style
/// flows and consumed by `Client::execute`).
pub struct Request {
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: Method, url: Url) -> Self {
        Request {
            method,
            url: url.as_str().to_string(),
            headers: HeaderMap::new(),
            body: None,
        }
    }
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }
    pub fn body_mut(&mut self) -> &mut Option<Vec<u8>> {
        &mut self.body
    }
    pub fn url(&self) -> Url {
        parse_url(&self.url).unwrap_or_else(|_| {
            Url::parse("http://invalid.localhost/").expect("static url parses")
        })
    }
    pub fn method(&self) -> &Method {
        &self.method
    }
}

/// oauth2 builds an `http::Request<Vec<u8>>` and converts it into a
/// `reqwest::Request` to feed `Client::execute`.
impl TryFrom<http::Request<Vec<u8>>> for Request {
    type Error = Error;
    fn try_from(req: http::Request<Vec<u8>>) -> Result<Self, Error> {
        let (parts, body) = req.into_parts();
        Ok(Request {
            method: parts.method,
            url: parts.uri.to_string(),
            headers: parts.headers,
            body: if body.is_empty() { None } else { Some(body) },
        })
    }
}

/// `reqwest::RequestBuilder`.
#[derive(Debug)]
pub struct RequestBuilder {
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    err: Option<String>,
}

impl RequestBuilder {
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        http::HeaderName: TryFrom<K>,
        http::HeaderValue: TryFrom<V>,
    {
        match (http::HeaderName::try_from(key), http::HeaderValue::try_from(value)) {
            (Ok(k), Ok(v)) => {
                self.headers.insert(k, v);
            }
            _ => self.err = Some("invalid header".into()),
        }
        self
    }
    pub fn body<B: Into<Body>>(mut self, body: B) -> Self {
        self.body = Some(body.into().0);
        self
    }
    pub fn json<T: serde::Serialize + ?Sized>(mut self, json: &T) -> Self {
        match serde_json::to_vec(json) {
            Ok(v) => {
                self.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                );
                self.body = Some(v);
            }
            Err(e) => self.err = Some(e.to_string()),
        }
        self
    }
    pub fn timeout(self, _timeout: Duration) -> Self {
        self
    }
    pub fn bearer_auth<T: std::fmt::Display>(self, token: T) -> Self {
        self.header(http::header::AUTHORIZATION, format!("Bearer {token}"))
    }
    pub fn basic_auth<U: std::fmt::Display, P: std::fmt::Display>(
        self,
        username: U,
        password: Option<P>,
    ) -> Self {
        use base64::Engine;
        let raw = match password {
            Some(p) => format!("{username}:{p}"),
            None => format!("{username}:"),
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        self.header(http::header::AUTHORIZATION, format!("Basic {encoded}"))
    }
    pub fn query<T: serde::Serialize + ?Sized>(mut self, query: &T) -> Self {
        match serde_urlencoded::to_string(query) {
            Ok(q) if !q.is_empty() => {
                let sep = if self.url.contains('?') { '&' } else { '?' };
                self.url = format!("{}{}{}", self.url, sep, q);
            }
            Ok(_) => {}
            Err(e) => self.err = Some(e.to_string()),
        }
        self
    }
    pub fn form<T: serde::Serialize + ?Sized>(mut self, form: &T) -> Self {
        match serde_urlencoded::to_string(form) {
            Ok(body) => {
                self.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/x-www-form-urlencoded"),
                );
                self.body = Some(body.into_bytes());
            }
            Err(e) => self.err = Some(e.to_string()),
        }
        self
    }
    pub fn build(self) -> Result<RequestBuilder, Error> {
        if let Some(e) = &self.err {
            return Err(Error::new(e.clone()));
        }
        Ok(self)
    }

    /// Perform the request via wasi-http (blocking under the hood; resolves on
    /// first poll on the single-threaded VM).
    pub async fn send(self) -> Result<Response, Error> {
        if let Some(e) = self.err {
            return Err(Error::new(e));
        }
        let method = match self.method {
            Method::GET => wasi_http::Method::Get,
            Method::POST => wasi_http::Method::Post,
            Method::PUT => wasi_http::Method::Put,
            Method::DELETE => wasi_http::Method::Delete,
            Method::PATCH => wasi_http::Method::Patch,
            Method::HEAD => wasi_http::Method::Head,
            _ => wasi_http::Method::Get,
        };
        let mut req = wasi_http::Request::new(method, &self.url)?;
        for (name, value) in self.headers.iter() {
            if let Ok(v) = value.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
        if let Some(body) = self.body {
            req = req.body(body);
        }
        // Always stream under the hood: headers arrive immediately and the body is
        // pulled incrementally. Buffered accessors (`json`/`text`/`bytes`) drain the
        // reader; `bytes_stream` yields raw chunks as they arrive.
        let (resp, reader) = wasi_http::HttpClient::new().send_raw_stream(&req)?;
        Ok(Response::from_wasi(resp, reader))
    }
}

/// `reqwest::Response`.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    reader: Option<wasi_http::RawBodyReader>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish()
    }
}

impl Response {
    fn from_wasi(resp: wasi_http::Response, reader: wasi_http::RawBodyReader) -> Self {
        let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
        let mut headers = HeaderMap::new();
        for (name, value) in &resp.headers {
            if let (Ok(n), Ok(v)) = (
                http::HeaderName::try_from(name.as_str()),
                http::HeaderValue::try_from(value.as_str()),
            ) {
                headers.insert(n, v);
            }
        }
        Response {
            status,
            headers,
            reader: Some(reader),
        }
    }

    /// Drain the raw reader fully into a buffer (for the buffered accessors).
    ///
    /// Cooperative: when the underlying socket has no data ready yet
    /// (`ChunkPoll::WouldBlock`), this yields to the runtime via [`yield_now`]
    /// instead of blocking the single guest thread, so other tasks make progress
    /// while the body streams in.
    async fn drain(&mut self) -> Result<Vec<u8>, Error> {
        let mut body = Vec::new();
        if let Some(reader) = self.reader.as_mut() {
            loop {
                match reader.read_chunk()? {
                    wasi_http::ChunkPoll::Ready(chunk) => body.extend_from_slice(&chunk),
                    wasi_http::ChunkPoll::Eof => break,
                    wasi_http::ChunkPoll::WouldBlock => yield_now().await,
                }
            }
        }
        self.reader = None;
        Ok(body)
    }
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    pub fn version(&self) -> http::Version {
        http::Version::HTTP_11
    }
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    }
    pub fn error_for_status(self) -> Result<Self, Error> {
        if self.status.is_client_error() || self.status.is_server_error() {
            Err(Error {
                msg: format!("HTTP status {}", self.status),
                status: Some(self.status),
                url: None,
            })
        } else {
            Ok(self)
        }
    }
    pub async fn text(mut self) -> Result<String, Error> {
        let body = self.drain().await?;
        String::from_utf8(body).map_err(|e| Error::new(e.to_string()))
    }
    pub async fn bytes(mut self) -> Result<Bytes, Error> {
        Ok(Bytes::from(self.drain().await?))
    }
    pub async fn json<T: serde::de::DeserializeOwned>(mut self) -> Result<T, Error> {
        let body = self.drain().await?;
        serde_json::from_slice(&body).map_err(|e| Error::new(e.to_string()))
    }
    /// Incremental raw byte stream backed by `wasi_http::RawBodyReader`. Yields
    /// de-framed body chunks as they arrive (codex's `transport.rs` runs its own
    /// SSE parser over these raw bytes). Single-threaded VM: each `recv` resolves
    /// the poll immediately.
    pub fn bytes_stream(self) -> BytesStream {
        BytesStream {
            reader: self.reader,
        }
    }
}

/// `Stream<Item = Result<Bytes, Error>>` over a `RawBodyReader`.
pub struct BytesStream {
    reader: Option<wasi_http::RawBodyReader>,
}

impl futures_core::Stream for BytesStream {
    type Item = Result<Bytes, Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let reader = match self.reader.as_mut() {
            Some(r) => r,
            None => return std::task::Poll::Ready(None),
        };
        match reader.read_chunk() {
            Ok(wasi_http::ChunkPoll::Ready(chunk)) => {
                std::task::Poll::Ready(Some(Ok(Bytes::from(chunk))))
            }
            Ok(wasi_http::ChunkPoll::Eof) => {
                self.reader = None;
                std::task::Poll::Ready(None)
            }
            // No data ready yet: yield to the runtime and ask to be re-polled
            // immediately. On the single-threaded VM this is a cooperative spin
            // that lets other tasks (e.g. the turn loop) run between polls.
            Ok(wasi_http::ChunkPoll::WouldBlock) => {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            Err(e) => {
                self.reader = None;
                std::task::Poll::Ready(Some(Err(Error::new(e.to_string()))))
            }
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
