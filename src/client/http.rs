//! The streamable HTTP transport: each message is a `POST`; the response is JSON or an SSE
//! stream that ends with the matching response.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::{ClientError, Secret, Transport};

const SESSION_HEADER: &str = "mcp-session-id";
const VERSION_HEADER: &str = "mcp-protocol-version";
const MAX_SSE_BYTES: usize = 16 * 1024 * 1024;

/// How to reach an HTTP MCP server.
#[derive(Clone, Debug)]
pub struct HttpConfig {
    /// The MCP endpoint, for example `https://example.com/mcp`.
    pub url: String,
    /// Sent as `Authorization: Bearer <token>`.
    pub bearer: Option<Secret>,
    /// Extra headers (values are secrets).
    pub headers: Vec<(String, Secret)>,
}

impl HttpConfig {
    /// A server with no credentials.
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), bearer: None, headers: Vec::new() }
    }

    /// Sets the bearer token.
    pub fn bearer(mut self, token: Secret) -> Self {
        self.bearer = Some(token);
        self
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: Secret) -> Self {
        self.headers.push((name.into(), value));
        self
    }
}

/// MCP over streamable HTTP.
#[derive(Debug)]
pub struct HttpTransport {
    http: reqwest::Client,
    url: String,
    headers: HeaderMap,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
}

fn sensitive(value: &str) -> Result<HeaderValue, ClientError> {
    let mut v = HeaderValue::from_str(value).map_err(|_| ClientError::Protocol("a header value is not valid".into()))?;
    v.set_sensitive(true);
    Ok(v)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl HttpTransport {
    /// A transport for `config`. Redirects are not followed, so credentials cannot be sent
    /// anywhere else.
    pub fn new(config: &HttpConfig) -> Result<Self, ClientError> {
        let mut headers = HeaderMap::new();
        if let Some(t) = &config.bearer {
            headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {}", t.expose()))?);
        }
        for (k, v) in &config.headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| ClientError::Protocol(format!("bad header name {k:?}")))?;
            headers.insert(name, sensitive(v.expose())?);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ClientError::Io(e.to_string()))?;
        Ok(Self { http, url: config.url.clone(), headers, session: Mutex::new(None), version: Mutex::new(None) })
    }

    fn post(&self, msg: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .post(&self.url)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .headers(self.headers.clone())
            .body(msg.to_string());
        if let Some(s) = lock(&self.session).as_deref() {
            rb = rb.header(SESSION_HEADER, s);
        }
        if let Some(v) = lock(&self.version).as_deref() {
            rb = rb.header(VERSION_HEADER, v);
        }
        rb
    }

    async fn check(&self, resp: reqwest::Response) -> Result<reqwest::Response, ClientError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let had_session = lock(&self.session).is_some();
        if status.as_u16() == 404 && had_session {
            *lock(&self.session) = None;
            return Err(ClientError::Closed);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ClientError::Http { status: status.as_u16(), body: body.chars().take(200).collect() })
    }
}

fn io(e: reqwest::Error) -> ClientError {
    ClientError::Io(e.without_url().to_string())
}

/// The message in `value` (a response, or a batch) answering `id`.
fn answer_in(value: Value, id: u64) -> Option<Value> {
    match value {
        Value::Array(items) => items.into_iter().find_map(|v| answer_in(v, id)),
        v if v.get("id").and_then(Value::as_u64) == Some(id) && v.get("method").is_none() => Some(v),
        _ => None,
    }
}

/// Feeds SSE bytes and yields the data of each complete event.
#[derive(Default)]
struct Sse {
    buf: String,
    pending: Vec<u8>,
}

impl Sse {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, ClientError> {
        self.pending.extend_from_slice(bytes);
        // Keep an incomplete UTF-8 tail for the next chunk.
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(s) => s.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return Err(ClientError::Protocol("event stream is not UTF-8".into())),
        };
        let text = String::from_utf8_lossy(&self.pending[..valid]).replace("\r\n", "\n").replace('\r', "\n");
        self.pending.drain(..valid);
        self.buf.push_str(&text);
        if self.buf.len() > MAX_SSE_BYTES {
            return Err(ClientError::Protocol("event stream is too large".into()));
        }
        let mut events = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..end + 2).collect();
            let data: Vec<&str> = block.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        Ok(events)
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn notify(&self, msg: Value) -> Result<(), ClientError> {
        let resp = self.post(&msg).send().await.map_err(io)?;
        self.check(resp).await.map(|_| ())
    }

    async fn request(&self, id: u64, msg: Value) -> Result<Value, ClientError> {
        let mut resp = self.check(self.post(&msg).send().await.map_err(io)?).await?;
        if let Some(s) = resp.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
            *lock(&self.session) = Some(s.to_string());
        }
        let is_sse = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/event-stream"));
        if !is_sse {
            let bytes = resp.bytes().await.map_err(io)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| ClientError::Protocol(format!("response is not JSON: {e}")))?;
            return answer_in(value, id).ok_or_else(|| ClientError::Protocol("no response for the request".into()));
        }
        let mut sse = Sse::default();
        while let Some(chunk) = resp.chunk().await.map_err(io)? {
            for data in sse.push(&chunk)? {
                if let Ok(value) = serde_json::from_str::<Value>(&data)
                    && let Some(answer) = answer_in(value, id)
                {
                    return Ok(answer);
                }
            }
        }
        Err(ClientError::Closed)
    }

    fn set_protocol_version(&self, version: &str) {
        *lock(&self.version) = Some(version.to_string());
    }

    async fn close(&self) {
        let session = lock(&self.session).take();
        if let Some(s) = session {
            let mut rb = self.http.delete(&self.url).headers(self.headers.clone()).header(SESSION_HEADER, s);
            if let Some(v) = lock(&self.version).as_deref() {
                rb = rb.header(VERSION_HEADER, v);
            }
            let _ = rb.send().await;
        }
    }
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks_crlf_and_multibyte() {
        let mut s = Sse::default();
        assert!(s.push(b"event: message\r\ndata: {\"a\":").unwrap().is_empty());
        let e = s.push("\"é\"}\r\n\r\n: comment\n\ndata: two\ndata: lines\n\n".as_bytes()).unwrap();
        assert_eq!(e, vec!["{\"a\":\"é\"}".to_string(), "two\nlines".to_string()]);
        // A multibyte character split across chunks.
        let bytes = "data: é\n\n".as_bytes();
        assert!(s.push(&bytes[..7]).unwrap().is_empty());
        assert_eq!(s.push(&bytes[7..]).unwrap(), vec!["é".to_string()]);
        assert!(s.push(&[0xff, 0xff]).is_err());
    }

    #[test]
    fn picks_the_matching_response_out_of_batches_and_ignores_requests() {
        use serde_json::json;
        assert_eq!(answer_in(json!([{"id": 1, "result": {}}, {"id": 2, "result": 7}]), 2), Some(json!({"id": 2, "result": 7})));
        assert_eq!(answer_in(json!({"id": 2, "method": "ping"}), 2), None);
        assert_eq!(answer_in(json!({"method": "notifications/progress"}), 2), None);
    }
}
