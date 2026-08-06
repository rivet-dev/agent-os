//! `reqwest` 0.12 API shim for wasm32-wasip1, backed by agentos `wasi-http`
//! (host_net TCP/TLS). Drop-in target for `[patch.crates-io] reqwest`.
//!
//! This is the guest half of agentOS's brokered HTTP boundary: the trusted
//! sidecar owns network policy and TLS, while request construction, cookies, and
//! streaming response bodies retain reqwest-compatible guest semantics. This
//! boundary does not replace or restrict agentOS's real guest POSIX sockets.

use std::sync::Arc;
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

/// `reqwest::cookie` — the small cookie-store surface used by Codex.
pub mod cookie {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use http::HeaderValue;

    use crate::Url;

    pub trait CookieStore: Send + Sync {
        fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url);
        fn cookies(&self, url: &Url) -> Option<HeaderValue>;
    }

    #[derive(Debug, Default)]
    pub struct Jar {
        cookies: Mutex<HashMap<String, HashMap<String, String>>>,
    }

    impl CookieStore for Jar {
        fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
            let Some(host) = url.host_str() else {
                return;
            };
            let mut hosts = self
                .cookies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let host_cookies = hosts.entry(host.to_string()).or_default();
            for header in cookie_headers {
                let Some(pair) = header
                    .to_str()
                    .ok()
                    .and_then(|value| value.split(';').next())
                else {
                    continue;
                };
                let Some((name, value)) = pair.split_once('=') else {
                    continue;
                };
                host_cookies.insert(name.trim().to_string(), value.trim().to_string());
            }
        }

        fn cookies(&self, url: &Url) -> Option<HeaderValue> {
            let host = url.host_str()?;
            let hosts = self
                .cookies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let values = hosts.get(host)?;
            let header = values
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            HeaderValue::from_str(&header).ok()
        }
    }
}

/// `reqwest::redirect` — redirect policy. The agentOS broker returns the HTTP
/// response without following redirects; `Policy::none()` therefore maps exactly
/// to the transport behavior. Other policies remain API-shaped values only.
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
    pub fn without_url(mut self) -> Self {
        self.url = None;
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
pub enum Body {
    Buffered(Vec<u8>),
    Stream(
        std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, String>> + Send + 'static>>,
    ),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buffered(bytes) => formatter
                .debug_tuple("Body")
                .field(&format_args!("{} buffered bytes", bytes.len()))
                .finish(),
            Self::Stream(_) => formatter.debug_tuple("Body").field(&"stream").finish(),
        }
    }
}

impl Body {
    pub fn wrap_stream<S>(stream: S) -> Self
    where
        S: futures_core::TryStream + Send + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        Bytes: From<S::Ok>,
    {
        use futures_util::{StreamExt, TryStreamExt};
        Self::Stream(
            stream
                .map_ok(Bytes::from)
                .map_err(|error| error.into().to_string())
                .boxed(),
        )
    }

    fn try_clone(&self) -> Option<Self> {
        match self {
            Self::Buffered(bytes) => Some(Self::Buffered(bytes.clone())),
            Self::Stream(_) => None,
        }
    }

    async fn into_bytes(self) -> Result<Vec<u8>, Error> {
        match self {
            Self::Buffered(bytes) => Ok(bytes),
            Self::Stream(mut stream) => {
                use futures_util::StreamExt;
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk.map_err(Error::new)?);
                }
                Ok(bytes)
            }
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(v: Vec<u8>) -> Self {
        Body::Buffered(v)
    }
}
impl From<String> for Body {
    fn from(s: String) -> Self {
        Body::Buffered(s.into_bytes())
    }
}
impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Body::Buffered(s.as_bytes().to_vec())
    }
}
impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        Body::Buffered(bytes.to_vec())
    }
}

/// `reqwest::Certificate` — TLS policy for brokered HTTP is sidecar-owned.
/// Custom per-client roots must fail explicitly instead of appearing to install.
#[derive(Clone, Debug)]
pub struct Certificate;

impl Certificate {
    pub fn from_der(_der: &[u8]) -> Result<Self, Error> {
        Err(Error::new(
			"custom HTTP CA roots are unavailable inside an agentOS VM; configure the trusted sidecar or the VM CA bundle",
		))
    }
    pub fn from_pem(_pem: &[u8]) -> Result<Self, Error> {
        Err(Error::new(
			"custom HTTP CA roots are unavailable inside an agentOS VM; configure the trusted sidecar or the VM CA bundle",
		))
    }
}

/// `reqwest::Identity` — client TLS identity for brokered HTTP is sidecar-owned.
/// The broker does not accept guest-provided client keys.
#[derive(Clone)]
pub struct Identity;

impl Identity {
    pub fn from_pem(_pem: &[u8]) -> Result<Self, Error> {
        Err(Error::new(
			"per-client HTTP TLS identities are unavailable inside an agentOS VM; configure the trusted sidecar",
		))
    }
    pub fn from_pkcs12_der(_der: &[u8], _pass: &str) -> Result<Self, Error> {
        Err(Error::new(
			"per-client HTTP TLS identities are unavailable inside an agentOS VM; configure the trusted sidecar",
		))
    }
}

#[derive(Clone, Debug)]
pub struct NoProxy;

impl NoProxy {
    pub fn from_string(value: &str) -> Option<Self> {
        (!value.trim().is_empty()).then_some(Self)
    }
}

#[derive(Clone, Debug)]
pub struct Proxy {
    scheme: String,
    has_no_proxy: bool,
}

impl Proxy {
    pub fn all(proxy_scheme: &str) -> Result<Self, Error> {
        let url = parse_url(proxy_scheme)?;
        Ok(Self {
            scheme: url.scheme().to_string(),
            has_no_proxy: false,
        })
    }

    pub fn no_proxy(mut self, no_proxy: Option<NoProxy>) -> Self {
        self.has_no_proxy = no_proxy.is_some();
        self
    }
}

/// `reqwest::ClientBuilder`.
#[derive(Default)]
pub struct ClientBuilder {
    default_headers: Option<HeaderMap>,
    timeout: Option<Duration>,
    cookie_provider: Option<Arc<dyn cookie::CookieStore>>,
    unsupported_transport: Option<String>,
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
    pub fn add_root_certificate(mut self, _cert: Certificate) -> Self {
        self.unsupported_transport = Some(
			"custom HTTP CA roots are unavailable inside an agentOS VM; configure the trusted sidecar or the VM CA bundle"
				.to_string(),
		);
        self
    }
    pub fn danger_accept_invalid_certs(mut self, value: bool) -> Self {
        if value {
            self.unsupported_transport = Some(
				"disabling HTTP certificate validation is unavailable inside an agentOS VM because the trusted sidecar owns TLS policy"
					.to_string(),
			);
        }
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
    pub fn proxy(mut self, proxy: Proxy) -> Self {
        self.unsupported_transport = Some(format!(
            "per-client {} proxy routing (NO_PROXY configured: {}) is unavailable in an agentOS VM; configure outbound routing on the trusted agentOS sidecar",
            proxy.scheme, proxy.has_no_proxy
        ));
        self
    }
    pub fn cookie_provider<C>(mut self, cookie_provider: Arc<C>) -> Self
    where
        C: cookie::CookieStore + 'static,
    {
        self.cookie_provider = Some(cookie_provider);
        self
    }
    pub fn tcp_keepalive<D: Into<Option<Duration>>>(self, _d: D) -> Self {
        self
    }
    pub fn use_rustls_tls(self) -> Self {
        self
    }
    pub fn identity(mut self, _id: Identity) -> Self {
        self.unsupported_transport = Some(
			"per-client HTTP TLS identities are unavailable inside an agentOS VM; configure the trusted sidecar"
				.to_string(),
		);
        self
    }
    pub fn build(self) -> Result<Client, Error> {
        if let Some(message) = self.unsupported_transport {
            return Err(Error::new(message));
        }
        Ok(Client {
            default_headers: self.default_headers.unwrap_or_default(),
            _timeout: self.timeout,
            cookie_provider: self.cookie_provider,
        })
    }
}

/// `reqwest::Client`.
#[derive(Clone)]
pub struct Client {
    default_headers: HeaderMap,
    _timeout: Option<Duration>,
    cookie_provider: Option<Arc<dyn cookie::CookieStore>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Client")
            .field("default_headers", &self.default_headers)
            .field("timeout", &self._timeout)
            .field("has_cookie_provider", &self.cookie_provider.is_some())
            .finish()
    }
}

impl Default for Client {
    fn default() -> Self {
        Client {
            default_headers: HeaderMap::new(),
            _timeout: None,
            cookie_provider: None,
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
            timeout: self._timeout,
            cookie_provider: self.cookie_provider.clone(),
            err: None,
        }
    }
    /// `Client::execute(Request)` — send a pre-built request (used by oauth2).
    pub async fn execute(&self, req: Request) -> Result<Response, Error> {
        RequestBuilder {
            method: req.method,
            url: req.url.to_string(),
            headers: req.headers,
            body: req.body,
            timeout: req.timeout,
            cookie_provider: self.cookie_provider.clone(),
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
    url: Url,
    headers: HeaderMap,
    body: Option<Body>,
    timeout: Option<Duration>,
    version: http::Version,
}

impl Request {
    pub fn new(method: Method, url: Url) -> Self {
        Request {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            version: http::Version::HTTP_11,
        }
    }
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }
    pub fn body_mut(&mut self) -> &mut Option<Body> {
        &mut self.body
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn url_mut(&mut self) -> &mut Url {
        &mut self.url
    }
    pub fn method(&self) -> &Method {
        &self.method
    }
    pub fn timeout(&self) -> Option<&Duration> {
        self.timeout.as_ref()
    }
    pub fn timeout_mut(&mut self) -> &mut Option<Duration> {
        &mut self.timeout
    }
    pub fn version(&self) -> http::Version {
        self.version
    }
    pub fn version_mut(&mut self) -> &mut http::Version {
        &mut self.version
    }
    pub fn try_clone(&self) -> Option<Self> {
        Some(Self {
            method: self.method.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            body: match &self.body {
                Some(body) => Some(body.try_clone()?),
                None => None,
            },
            timeout: self.timeout,
            version: self.version,
        })
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
            url: parse_url(&parts.uri.to_string())?,
            headers: parts.headers,
            body: if body.is_empty() {
                None
            } else {
                Some(Body::Buffered(body))
            },
            timeout: None,
            version: parts.version,
        })
    }
}

/// `reqwest::RequestBuilder`.
pub struct RequestBuilder {
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Body>,
    timeout: Option<Duration>,
    cookie_provider: Option<Arc<dyn cookie::CookieStore>>,
    err: Option<String>,
}

impl std::fmt::Debug for RequestBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestBuilder")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field("timeout", &self.timeout)
            .field("has_cookie_provider", &self.cookie_provider.is_some())
            .field("err", &self.err)
            .finish()
    }
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
        match (
            http::HeaderName::try_from(key),
            http::HeaderValue::try_from(value),
        ) {
            (Ok(k), Ok(v)) => {
                self.headers.insert(k, v);
            }
            _ => self.err = Some("invalid header".into()),
        }
        self
    }
    pub fn body<B: Into<Body>>(mut self, body: B) -> Self {
        self.body = Some(body.into());
        self
    }
    pub fn json<T: serde::Serialize + ?Sized>(mut self, json: &T) -> Self {
        match serde_json::to_vec(json) {
            Ok(v) => {
                self.headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                );
                self.body = Some(Body::Buffered(v));
            }
            Err(e) => self.err = Some(e.to_string()),
        }
        self
    }
    pub fn timeout(self, _timeout: Duration) -> Self {
        Self {
            timeout: Some(_timeout),
            ..self
        }
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
                self.body = Some(Body::Buffered(body.into_bytes()));
            }
            Err(e) => self.err = Some(e.to_string()),
        }
        self
    }
    pub fn build(self) -> Result<Request, Error> {
        if let Some(e) = self.err {
            return Err(Error::new(e));
        }
        Ok(Request {
            method: self.method,
            url: parse_url(&self.url)?,
            headers: self.headers,
            body: self.body,
            timeout: self.timeout,
            version: http::Version::HTTP_11,
        })
    }

    /// Perform the request via wasi-http (blocking under the hood; resolves on
    /// first poll on the single-threaded VM).
    pub async fn send(mut self) -> Result<Response, Error> {
        if let Some(e) = self.err {
            return Err(Error::new(e));
        }
        let url = parse_url(&self.url)?;
        if !self.headers.contains_key(http::header::COOKIE) {
            if let Some(cookie) = self
                .cookie_provider
                .as_ref()
                .and_then(|provider| provider.cookies(&url))
            {
                self.headers.insert(http::header::COOKIE, cookie);
            }
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
            req = req.body(body.into_bytes().await?);
        }
        // Always stream under the hood: headers arrive immediately and the body is
        // pulled incrementally. Buffered accessors (`json`/`text`/`bytes`) drain the
        // reader; `bytes_stream` yields raw chunks as they arrive.
        let (resp, reader) = wasi_http::HttpClient::new().send_raw_stream(&req)?;
        let response = Response::from_wasi(resp, reader, url.clone());
        if let Some(cookie_provider) = self.cookie_provider {
            let mut set_cookie_headers = response.headers.get_all(http::header::SET_COOKIE).iter();
            cookie_provider.set_cookies(&mut set_cookie_headers, &url);
        }
        Ok(response)
    }
}

/// `reqwest::Response`.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    reader: Option<wasi_http::RawBodyReader>,
    url: Url,
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
    fn from_wasi(resp: wasi_http::Response, reader: wasi_http::RawBodyReader, url: Url) -> Self {
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
            url,
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
    pub fn url(&self) -> &Url {
        &self.url
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
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(None);
        };
        loop {
            match reader.read_chunk()? {
                wasi_http::ChunkPoll::Ready(chunk) => {
                    return Ok(Some(Bytes::from(chunk)));
                }
                wasi_http::ChunkPoll::Eof => {
                    self.reader = None;
                    return Ok(None);
                }
                wasi_http::ChunkPoll::WouldBlock => yield_now().await,
            }
        }
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
