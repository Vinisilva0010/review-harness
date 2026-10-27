//! LLM provider abstraction: trait, secret handling, HTTP transport and retry.

use std::collections::VecDeque;
use std::fmt;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Cap on a provider response body.
pub const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// Per-request HTTP timeout.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// An API key. Never printed: `Debug` is redacted and there is no `Display`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value; only for building auth headers.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Replace every occurrence of the secret in `text`.
    pub fn redact(&self, text: &str) -> String {
        if self.0.len() < 4 {
            return text.to_string();
        }
        text.replace(&self.0, "[REDACTED]")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// What the review engine asks a model to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequest {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    #[error("request timed out")]
    Timeout,
    #[error("provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("unusable provider response: {0}")]
    BadResponse(String),
}

pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

/// A prepared HTTP POST. `Debug` hides header values (they carry credentials).
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("header_names", &names)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: String,
}

pub trait HttpTransport: Send + Sync {
    fn post(&self, req: &HttpRequest) -> Result<HttpResponse, LlmError>;
}

/// Blocking `ureq` transport with a global timeout and a response size cap.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new(HTTP_TIMEOUT)
    }
}

fn is_local(url: &str) -> bool {
    ["http://localhost", "http://127.0.0.1"]
        .iter()
        .any(|p| url.starts_with(p))
}

impl HttpTransport for UreqTransport {
    fn post(&self, req: &HttpRequest) -> Result<HttpResponse, LlmError> {
        if !req.url.starts_with("https://") && !is_local(&req.url) {
            return Err(LlmError::Transport("refusing non-https URL".into()));
        }
        let mut builder = self.agent.post(&req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        let mut resp = builder.send(req.body.as_str()).map_err(|e| match e {
            ureq::Error::Timeout(_) => LlmError::Timeout,
            // ureq errors can embed URLs; they never embed our headers.
            other => LlmError::Transport(other.to_string()),
        })?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut body = String::new();
        resp.body_mut()
            .as_reader()
            .take(MAX_RESPONSE_BYTES)
            .read_to_string(&mut body)
            .map_err(|e| LlmError::Transport(format!("reading body: {e}")))?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

/// Exponential backoff policy for 429 / 503 responses.
#[derive(Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    sleeper: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl RetryPolicy {
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
            sleeper: Arc::new(std::thread::sleep),
        }
    }

    /// Replace the sleep function (tests record delays instead of waiting).
    pub fn with_sleeper(mut self, f: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        self.sleeper = Arc::new(f);
        self
    }

    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max_delay);
        retry_after
            .map_or(backoff, |r| r.max(backoff))
            .min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(3, Duration::from_secs(1), Duration::from_secs(30))
    }
}

/// Send `req`, retrying on 429 and 503 with exponential backoff (honouring
/// `Retry-After`), and return the first non-retryable response.
pub fn send_with_retry(
    transport: &dyn HttpTransport,
    req: &HttpRequest,
    policy: &RetryPolicy,
) -> Result<HttpResponse, LlmError> {
    let mut attempt = 0;
    loop {
        let resp = transport.post(req)?;
        if resp.status != 429 && resp.status != 503 {
            return Ok(resp);
        }
        if attempt >= policy.max_retries {
            return if resp.status == 429 {
                Err(LlmError::RateLimited {
                    retry_after: resp.retry_after,
                })
            } else {
                Ok(resp)
            };
        }
        (policy.sleeper)(policy.delay(attempt, resp.retry_after));
        attempt += 1;
    }
}

/// Test double: returns scripted responses and records every request.
#[derive(Clone, Default)]
pub struct MockProvider {
    script: Arc<Mutex<VecDeque<Result<LlmResponse, LlmError>>>>,
    seen: Arc<Mutex<Vec<LlmRequest>>>,
}

impl MockProvider {
    pub fn new(script: Vec<Result<LlmResponse, LlmError>>) -> Self {
        Self {
            script: Arc::new(Mutex::new(script.into())),
            seen: Arc::default(),
        }
    }

    /// Script of successful text replies with fixed token usage.
    pub fn with_texts(texts: &[&str]) -> Self {
        Self::new(
            texts
                .iter()
                .map(|t| {
                    Ok(LlmResponse {
                        text: (*t).to_string(),
                        input_tokens: Some(100),
                        output_tokens: Some(20),
                    })
                })
                .collect(),
        )
    }

    pub fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        self.seen.lock().unwrap().push(req.clone());
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(LlmError::BadResponse("mock script exhausted".into())))
    }
}

/// Test transport: scripted HTTP responses, recorded requests.
#[derive(Default)]
pub struct FakeTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, LlmError>>>,
    pub seen: Mutex<Vec<HttpRequest>>,
}

impl FakeTransport {
    pub fn new(responses: Vec<Result<HttpResponse, LlmError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            seen: Mutex::default(),
        }
    }

    pub fn ok(status: u16, body: &str) -> Result<HttpResponse, LlmError> {
        Ok(HttpResponse {
            status,
            retry_after: None,
            body: body.to_string(),
        })
    }
}

impl HttpTransport for FakeTransport {
    fn post(&self, req: &HttpRequest) -> Result<HttpResponse, LlmError> {
        self.seen.lock().unwrap().push(req.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(LlmError::Transport("fake transport exhausted".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn secret_debug_is_redacted_and_redact_removes_value() {
        let s = Secret::new("sk-supersecretvalue");
        assert!(!format!("{s:?}").contains("supersecret"));
        assert_eq!(
            s.redact("bad key sk-supersecretvalue!"),
            "bad key [REDACTED]!"
        );
    }

    #[test]
    fn http_request_debug_hides_credentials() {
        let r = HttpRequest {
            url: "https://x".into(),
            headers: vec![("x-api-key".into(), "sk-topsecret".into())],
            body: "{}".into(),
        };
        let d = format!("{r:?}");
        assert!(d.contains("x-api-key") && !d.contains("sk-topsecret"));
    }

    fn req() -> HttpRequest {
        HttpRequest {
            url: "https://x".into(),
            headers: vec![],
            body: "{}".into(),
        }
    }

    fn recording_policy(max: u32) -> (RetryPolicy, Arc<Mutex<Vec<Duration>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let p = RetryPolicy::new(max, Duration::from_secs(1), Duration::from_secs(10))
            .with_sleeper(move |d| l.lock().unwrap().push(d));
        (p, log)
    }

    #[test]
    fn retries_429_with_exponential_backoff_then_succeeds() {
        let t = FakeTransport::new(vec![
            FakeTransport::ok(429, "slow down"),
            FakeTransport::ok(429, "slow down"),
            FakeTransport::ok(429, "slow down"),
            FakeTransport::ok(200, "{}"),
        ]);
        let (p, log) = recording_policy(3);
        let r = send_with_retry(&t, &req(), &p).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
    }

    #[test]
    fn gives_up_with_rate_limited_after_max_retries() {
        let t = FakeTransport::new(vec![
            FakeTransport::ok(429, ""),
            FakeTransport::ok(429, ""),
            FakeTransport::ok(429, ""),
        ]);
        let (p, log) = recording_policy(2);
        let e = send_with_retry(&t, &req(), &p).unwrap_err();
        assert!(matches!(e, LlmError::RateLimited { .. }));
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn retry_after_header_is_honoured_but_capped() {
        let mut slow = HttpResponse {
            status: 429,
            retry_after: Some(Duration::from_secs(7)),
            body: String::new(),
        };
        let t = FakeTransport::new(vec![
            Ok(slow.clone()),
            {
                slow.retry_after = Some(Duration::from_secs(9999));
                Ok(slow)
            },
            FakeTransport::ok(200, "{}"),
        ]);
        let (p, log) = recording_policy(3);
        send_with_retry(&t, &req(), &p).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![Duration::from_secs(7), Duration::from_secs(10)]
        );
    }

    #[test]
    fn non_retryable_statuses_return_immediately() {
        let t = FakeTransport::new(vec![FakeTransport::ok(400, "bad")]);
        let (p, log) = recording_policy(3);
        assert_eq!(send_with_retry(&t, &req(), &p).unwrap().status, 400);
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn transport_errors_are_not_retried() {
        let t = FakeTransport::new(vec![Err(LlmError::Timeout)]);
        let (p, _) = recording_policy(3);
        assert_eq!(
            send_with_retry(&t, &req(), &p).unwrap_err(),
            LlmError::Timeout
        );
    }

    #[test]
    fn mock_records_and_exhausts() {
        let m = MockProvider::with_texts(&["a"]);
        let rq = LlmRequest {
            system: "s".into(),
            user: "u".into(),
            max_output_tokens: 1,
        };
        assert_eq!(m.complete(&rq).unwrap().text, "a");
        assert!(m.complete(&rq).is_err());
        assert_eq!(m.requests().len(), 2);
    }

    #[test]
    fn ureq_refuses_remote_http() {
        let t = UreqTransport::default();
        let mut r = req();
        r.url = "http://example.com/x".into();
        assert!(matches!(t.post(&r), Err(LlmError::Transport(_))));
    }

    fn serve_once(response: &'static str, delay: Duration) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                std::thread::sleep(delay);
                let _ = s.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{}/v1", addr.port())
    }

    #[test]
    fn ureq_reads_status_retry_after_and_body_from_local_server() {
        let url = serve_once(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 5\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow",
            Duration::ZERO,
        );
        let r = UreqTransport::default()
            .post(&HttpRequest { url, ..req() })
            .unwrap();
        assert_eq!(r.status, 429);
        assert_eq!(r.retry_after, Some(Duration::from_secs(5)));
        assert_eq!(r.body, "slow");
    }

    #[test]
    fn ureq_times_out() {
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
            Duration::from_secs(5),
        );
        let t = UreqTransport::new(Duration::from_millis(300));
        let e = t.post(&HttpRequest { url, ..req() }).unwrap_err();
        assert_eq!(e, LlmError::Timeout);
    }
}
