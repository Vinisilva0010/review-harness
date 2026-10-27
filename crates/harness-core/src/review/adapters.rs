//! Provider adapters. Each is split into pure request building / response parsing
//! (fixture-tested) and a thin send step over an [`HttpTransport`].
//!
//! Wire formats were checked against the providers' official documentation:
//! Gemini `generateContent` (ai.google.dev), the OpenAI chat-completions format as
//! documented by Groq (console.groq.com), and Anthropic Messages (platform.claude.com).

use std::sync::Arc;

use serde_json::{json, Value};

use super::provider::{
    send_with_retry, HttpRequest, HttpResponse, HttpTransport, LlmError, LlmProvider, LlmRequest,
    LlmResponse, RetryPolicy, Secret,
};

pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

fn valid_model(model: &str) -> Result<(), String> {
    let ok = !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    if ok {
        Ok(())
    } else {
        Err(format!("invalid model id {model:?}"))
    }
}

fn clip(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    t.replace('\n', " ")
}

fn u32_at(v: &Value, ptr: &str) -> Option<u32> {
    v.pointer(ptr)?.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// Shared tail of `complete`: send, map HTTP errors (redacting the key), parse.
fn execute(
    transport: &dyn HttpTransport,
    retry: &RetryPolicy,
    key: &Secret,
    req: &HttpRequest,
    parse: fn(&str) -> Result<LlmResponse, LlmError>,
) -> Result<LlmResponse, LlmError> {
    let resp: HttpResponse = send_with_retry(transport, req, retry)?;
    if (200..300).contains(&resp.status) {
        return parse(&resp.body).map_err(|e| match e {
            LlmError::BadResponse(m) => LlmError::BadResponse(key.redact(&m)),
            other => other,
        });
    }
    Err(LlmError::Http {
        status: resp.status,
        message: clip(&key.redact(&resp.body), 300),
    })
}

// ---------------------------------------------------------------- Gemini

pub struct GeminiProvider {
    model: String,
    key: Secret,
    base_url: String,
    transport: Arc<dyn HttpTransport>,
    retry: RetryPolicy,
}

impl GeminiProvider {
    pub fn new(
        model: &str,
        key: Secret,
        base_url: Option<&str>,
        transport: Arc<dyn HttpTransport>,
        retry: RetryPolicy,
    ) -> Result<Self, String> {
        valid_model(model)?;
        Ok(Self {
            model: model.into(),
            key,
            base_url: base_url
                .unwrap_or(GEMINI_BASE_URL)
                .trim_end_matches('/')
                .into(),
            transport,
            retry,
        })
    }

    pub fn build_request(&self, req: &LlmRequest) -> HttpRequest {
        let body = json!({
            "systemInstruction": {"parts": [{"text": req.system}]},
            "contents": [{"role": "user", "parts": [{"text": req.user}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "maxOutputTokens": req.max_output_tokens,
                "temperature": 0
            }
        });
        HttpRequest {
            url: format!("{}/models/{}:generateContent", self.base_url, self.model),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-goog-api-key".into(), self.key.expose().into()),
            ],
            body: body.to_string(),
        }
    }

    pub fn parse_response(body: &str) -> Result<LlmResponse, LlmError> {
        let v: Value = serde_json::from_str(body)
            .map_err(|e| LlmError::BadResponse(format!("invalid JSON: {e}")))?;
        let parts = v
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                let reason = v
                    .pointer("/promptFeedback/blockReason")
                    .and_then(Value::as_str)
                    .unwrap_or("no candidates");
                LlmError::BadResponse(format!("no content returned ({reason})"))
            })?;
        let text: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        Ok(LlmResponse {
            text,
            input_tokens: u32_at(&v, "/usageMetadata/promptTokenCount"),
            output_tokens: u32_at(&v, "/usageMetadata/candidatesTokenCount"),
        })
    }
}

impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }
    fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        execute(
            self.transport.as_ref(),
            &self.retry,
            &self.key,
            &self.build_request(req),
            Self::parse_response,
        )
    }
}

// ------------------------------------------------- OpenAI-compatible (Groq, ...)

pub struct OpenAiCompatibleProvider {
    model: String,
    key: Secret,
    base_url: String,
    transport: Arc<dyn HttpTransport>,
    retry: RetryPolicy,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        model: &str,
        key: Secret,
        base_url: &str,
        transport: Arc<dyn HttpTransport>,
        retry: RetryPolicy,
    ) -> Result<Self, String> {
        valid_model(model)?;
        Ok(Self {
            model: model.into(),
            key,
            base_url: base_url.trim_end_matches('/').into(),
            transport,
            retry,
        })
    }

    pub fn build_request(&self, req: &LlmRequest) -> HttpRequest {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": req.system},
                {"role": "user", "content": req.user}
            ],
            "response_format": {"type": "json_object"},
            "max_completion_tokens": req.max_output_tokens,
            "temperature": 0
        });
        HttpRequest {
            url: format!("{}/chat/completions", self.base_url),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                (
                    "authorization".into(),
                    format!("Bearer {}", self.key.expose()),
                ),
            ],
            body: body.to_string(),
        }
    }

    pub fn parse_response(body: &str) -> Result<LlmResponse, LlmError> {
        let v: Value = serde_json::from_str(body)
            .map_err(|e| LlmError::BadResponse(format!("invalid JSON: {e}")))?;
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::BadResponse("missing choices[0].message.content".into()))?;
        Ok(LlmResponse {
            text: text.to_string(),
            input_tokens: u32_at(&v, "/usage/prompt_tokens"),
            output_tokens: u32_at(&v, "/usage/completion_tokens"),
        })
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        "openai_compatible"
    }
    fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        execute(
            self.transport.as_ref(),
            &self.retry,
            &self.key,
            &self.build_request(req),
            Self::parse_response,
        )
    }
}

// ------------------------------------------------------------- Anthropic

pub struct AnthropicProvider {
    model: String,
    key: Secret,
    base_url: String,
    transport: Arc<dyn HttpTransport>,
    retry: RetryPolicy,
}

impl AnthropicProvider {
    pub fn new(
        model: &str,
        key: Secret,
        base_url: Option<&str>,
        transport: Arc<dyn HttpTransport>,
        retry: RetryPolicy,
    ) -> Result<Self, String> {
        valid_model(model)?;
        Ok(Self {
            model: model.into(),
            key,
            base_url: base_url
                .unwrap_or(ANTHROPIC_BASE_URL)
                .trim_end_matches('/')
                .into(),
            transport,
            retry,
        })
    }

    pub fn build_request(&self, req: &LlmRequest) -> HttpRequest {
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_output_tokens,
            "system": req.system,
            "messages": [{"role": "user", "content": req.user}]
        });
        HttpRequest {
            url: format!("{}/v1/messages", self.base_url),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-api-key".into(), self.key.expose().into()),
                ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
            ],
            body: body.to_string(),
        }
    }

    pub fn parse_response(body: &str) -> Result<LlmResponse, LlmError> {
        let v: Value = serde_json::from_str(body)
            .map_err(|e| LlmError::BadResponse(format!("invalid JSON: {e}")))?;
        let blocks = v
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| LlmError::BadResponse("missing content array".into()))?;
        let text: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        if text.is_empty() {
            return Err(LlmError::BadResponse("no text content block".into()));
        }
        Ok(LlmResponse {
            text,
            input_tokens: u32_at(&v, "/usage/input_tokens"),
            output_tokens: u32_at(&v, "/usage/output_tokens"),
        })
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        execute(
            self.transport.as_ref(),
            &self.retry,
            &self.key,
            &self.build_request(req),
            Self::parse_response,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::provider::FakeTransport;
    use std::time::Duration;

    const KEY: &str = "test-key-1234567890";

    fn rq() -> LlmRequest {
        LlmRequest {
            system: "SYS".into(),
            user: "USER".into(),
            max_output_tokens: 512,
        }
    }

    fn fixture(name: &str) -> String {
        let p = format!(
            "{}/tests/fixtures/providers/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(p).unwrap()
    }

    fn json_of(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn fast() -> RetryPolicy {
        RetryPolicy::new(2, Duration::from_millis(1), Duration::from_millis(2)).with_sleeper(|_| {})
    }

    fn transport(rs: Vec<Result<HttpResponse, LlmError>>) -> Arc<FakeTransport> {
        Arc::new(FakeTransport::new(rs))
    }

    fn gemini(t: Arc<FakeTransport>) -> GeminiProvider {
        GeminiProvider::new("test-model", Secret::new(KEY), None, t, fast()).unwrap()
    }
    fn openai(t: Arc<FakeTransport>) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new(
            "test-model",
            Secret::new(KEY),
            "https://api.groq.com/openai/v1/",
            t,
            fast(),
        )
        .unwrap()
    }
    fn anthropic(t: Arc<FakeTransport>) -> AnthropicProvider {
        AnthropicProvider::new("test-model", Secret::new(KEY), None, t, fast()).unwrap()
    }

    fn header<'a>(r: &'a HttpRequest, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn gemini_request_matches_fixture() {
        let r = gemini(transport(vec![])).build_request(&rq());
        assert_eq!(json_of(&r.body), json_of(&fixture("gemini_request.json")));
        assert_eq!(
            r.url,
            "https://generativelanguage.googleapis.com/v1beta/models/test-model:generateContent"
        );
        assert_eq!(header(&r, "x-goog-api-key"), Some(KEY));
        assert!(!r.url.contains(KEY), "key must not be in the URL");
    }

    #[test]
    fn openai_request_matches_fixture() {
        let r = openai(transport(vec![])).build_request(&rq());
        assert_eq!(json_of(&r.body), json_of(&fixture("openai_request.json")));
        assert_eq!(r.url, "https://api.groq.com/openai/v1/chat/completions");
        assert_eq!(header(&r, "authorization"), Some(&*format!("Bearer {KEY}")));
    }

    #[test]
    fn anthropic_request_matches_fixture() {
        let r = anthropic(transport(vec![])).build_request(&rq());
        assert_eq!(
            json_of(&r.body),
            json_of(&fixture("anthropic_request.json"))
        );
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(header(&r, "x-api-key"), Some(KEY));
        assert_eq!(header(&r, "anthropic-version"), Some("2023-06-01"));
    }

    #[test]
    fn parses_ok_responses_and_usage() {
        let g = GeminiProvider::parse_response(&fixture("gemini_response_ok.json")).unwrap();
        assert_eq!(
            (g.text.as_str(), g.input_tokens, g.output_tokens),
            ("{\"findings\":[]}", Some(120), Some(8))
        );
        let o =
            OpenAiCompatibleProvider::parse_response(&fixture("openai_response_ok.json")).unwrap();
        assert_eq!(
            (o.text.as_str(), o.input_tokens, o.output_tokens),
            ("{\"findings\":[]}", Some(130), Some(7))
        );
        let a = AnthropicProvider::parse_response(&fixture("anthropic_response_ok.json")).unwrap();
        assert_eq!(
            (a.text.as_str(), a.input_tokens, a.output_tokens),
            ("{\"findings\":[]}", Some(140), Some(9))
        );
    }

    #[test]
    fn malformed_and_empty_responses_are_bad_response() {
        for f in [
            GeminiProvider::parse_response as fn(&str) -> Result<LlmResponse, LlmError>,
            OpenAiCompatibleProvider::parse_response,
            AnthropicProvider::parse_response,
        ] {
            assert!(matches!(f("not json"), Err(LlmError::BadResponse(_))));
            assert!(matches!(f("{}"), Err(LlmError::BadResponse(_))));
        }
        let blocked = GeminiProvider::parse_response(&fixture("gemini_response_blocked.json"));
        assert!(matches!(blocked, Err(LlmError::BadResponse(m)) if m.contains("SAFETY")));
    }

    #[test]
    fn complete_end_to_end_through_fake_transport() {
        let t = transport(vec![FakeTransport::ok(
            200,
            &fixture("anthropic_response_ok.json"),
        )]);
        let r = anthropic(t.clone()).complete(&rq()).unwrap();
        assert_eq!(r.text, "{\"findings\":[]}");
        assert_eq!(t.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn rate_limit_fixtures_retry_then_fail_as_rate_limited() {
        let t = transport(vec![
            FakeTransport::ok(429, &fixture("gemini_error_429.json")),
            FakeTransport::ok(429, &fixture("gemini_error_429.json")),
            FakeTransport::ok(429, &fixture("gemini_error_429.json")),
        ]);
        let e = gemini(t.clone()).complete(&rq()).unwrap_err();
        assert!(matches!(e, LlmError::RateLimited { .. }));
        assert_eq!(t.seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn rate_limit_then_success_for_openai() {
        let t = transport(vec![
            FakeTransport::ok(429, &fixture("openai_error_429.json")),
            FakeTransport::ok(200, &fixture("openai_response_ok.json")),
        ]);
        assert!(openai(t).complete(&rq()).is_ok());
    }

    #[test]
    fn overloaded_503_surfaces_as_http_error() {
        let t = transport(vec![
            FakeTransport::ok(503, &fixture("anthropic_error_overloaded.json")),
            FakeTransport::ok(503, &fixture("anthropic_error_overloaded.json")),
            FakeTransport::ok(503, &fixture("anthropic_error_overloaded.json")),
        ]);
        let e = anthropic(t).complete(&rq()).unwrap_err();
        assert!(matches!(e, LlmError::Http { status: 503, .. }));
    }

    #[test]
    fn timeouts_propagate() {
        let t = transport(vec![Err(LlmError::Timeout)]);
        assert_eq!(openai(t).complete(&rq()).unwrap_err(), LlmError::Timeout);
    }

    #[test]
    fn error_messages_never_contain_the_api_key() {
        // A hostile/buggy server echoing the key back must not leak it via our errors.
        let echo = format!("{{\"error\":\"invalid key {KEY}\"}}");
        let t = transport(vec![FakeTransport::ok(401, &echo)]);
        let e = anthropic(t).complete(&rq()).unwrap_err();
        let shown = format!("{e} / {e:?}");
        assert!(!shown.contains(KEY), "leaked: {shown}");
        assert!(shown.contains("[REDACTED]"));
    }

    #[test]
    fn invalid_model_ids_are_rejected() {
        let t = transport(vec![]);
        let k = || Secret::new(KEY);
        assert!(GeminiProvider::new("a/../b", k(), None, t.clone(), fast()).is_err());
        assert!(GeminiProvider::new("", k(), None, t.clone(), fast()).is_err());
        assert!(AnthropicProvider::new("m?x=1", k(), None, t, fast()).is_err());
    }
}
