//! Anthropic API client — legacy cloud-assisted slow-loop extractor.
//!
//! Feature-gated behind `llm-summary`. When the feature is on AND
//! `~/.soma/secrets.toml` carries `[anthropic] api_key`, slow_loop may use
//! Claude Haiku for historical narrative synthesis.
//! ADR 0014/0016 boundary: this is explicit opt-in, off-hot-path,
//! cloud-assisted extraction. It is not SOMA's local compiler bridge; the
//! current cloud/local bridge uses deterministic ContextEnvelopes plus
//! optional Ollama-backed `compiler_notes`.
//!
//! ## Single primitive — `call_claude_haiku`
//!
//! `call_claude_haiku(api_key, system, prompt)` is the only public
//! HTTP entry point. Pure round-trip wrapper, no secret loading and
//! no statistics shaping — both happen one layer up in
//! `crate::memory::narrative::synthesize_with_llm`, which is what
//! `slow_loop::synthesize_narrative` actually calls.
//!
//! Returns `Result<String, LlmError>` with structured variants that
//! callers match on:
//!
//! * `FeatureOff` — `llm-summary` cargo feature off (compile-time
//!   stub; no HTTP attempt).
//! * `Network(_)` — DNS / TLS / connection / 30s timeout.
//! * `ApiError { status, body }` — non-2xx HTTP from Anthropic.
//! * `Decode(_)` — 2xx body wasn't the expected JSON envelope.
//! * `EmptyResponse` — 2xx envelope with no text content.
//!
//! `NoApiKey` is also defined for the `From<SecretError>` impl that
//! `synthesize_with_llm` uses one layer up; `call_claude_haiku`
//! itself never produces it (it takes the key as a parameter).
//!
//! ## Cost / safety
//!
//! * Model = `claude-haiku-4-5-20251001` (cheapest production
//!   Claude per CLAUDE.md cutoff 2026-01).
//! * Max output 800 tokens (Haiku tail-fits well below this for
//!   ≤ 200-word Korean paragraphs).
//! * 1 call per slow_loop cycle (1 hour) → ~$1/month.
//! * Episode prompts wrapped in `<episode>...</episode>` to neutralise
//!   prompt-injection from captured user input. The narrative layer
//!   does the wrapping; this module just ships the prompt verbatim.
//! * **No retries** in v1 — `call_claude_haiku` is a single round-trip.
//!   Network / 5xx failures bubble up via `LlmError` and the
//!   slow_loop's narrative path falls back to the rule paragraph.
//!   Retry policy with exponential backoff is deferred to D90-cand
//!   (registered).
//! * 30s request timeout — the slow_loop fires once per hour, so a
//!   long stall on a single cycle is acceptable, but unbounded waits
//!   would block the cycle's other passes (centroid, compression).

use crate::memory::secret::SecretError;

#[derive(Debug)]
pub enum LlmError {
    /// `llm-summary` cargo feature is off — `call_claude_haiku` is a
    /// stub that returns this variant without attempting any HTTP.
    /// Callers fall back to the rule-based path.
    FeatureOff,
    /// `~/.soma/secrets.toml` is missing or lacks `[anthropic] api_key`.
    /// Surfaced by the higher-level `narrative::synthesize_with_llm`
    /// helper that loads secrets; `call_claude_haiku` itself never
    /// produces this variant because it takes the key as a parameter.
    /// Kept on `LlmError` so the `From<SecretError>` impl has a
    /// dedicated mapping (not a `Decode(_)` collision).
    NoApiKey,
    /// Transport-layer failure (DNS, TLS, connection refused, timeout).
    /// String body is `ureq`'s rendered error; opaque on purpose —
    /// callers should not match on its content.
    Network(String),
    /// Anthropic returned a non-2xx HTTP status. `status` is the raw
    /// code (4xx auth/quota, 5xx upstream); `body` is the response
    /// body (typically a JSON `error` object) for diagnostics.
    ApiError { status: u16, body: String },
    /// Response was 2xx but the JSON shape didn't match the expected
    /// `{ content: [{type, text}] }` envelope. Decode error message
    /// wrapped for tracing; callers should treat as transient.
    Decode(String),
    /// Response was 2xx with a parseable JSON envelope, but every
    /// content block was empty. Distinct from `Decode` so monitoring
    /// can alert on "model returned nothing useful" separately from
    /// "we failed to parse".
    EmptyResponse,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::FeatureOff => write!(f, "llm-summary cargo feature disabled"),
            LlmError::NoApiKey => write!(f, "no Anthropic API key configured"),
            LlmError::Network(m) => write!(f, "network: {m}"),
            LlmError::ApiError { status, body } => {
                // R13 audit (2026-05-01) — Anthropic 401 / 403 responses
                // can echo a fragment of the offending API key back in
                // the JSON body (`{"error":{"message":"Invalid API key:
                // sk-ant-..."}}`). `Display` flows into tracing logs
                // and the structured `error_json` envelope on stderr,
                // so a verbatim `body` would leak that fragment to
                // stop-hook log files. Truncate to the first 100
                // chars + ellipsis so the message stays diagnostic
                // without exfiltrating secret material. The full body
                // is still available for in-memory inspection (it's
                // a struct field, not a getter).
                let safe: String = body.chars().take(100).collect();
                let suffix = if body.chars().count() > 100 { "…(truncated)" } else { "" };
                write!(f, "Anthropic API error {status}: {safe}{suffix}")
            }
            LlmError::Decode(m) => write!(f, "decode: {m}"),
            LlmError::EmptyResponse => write!(f, "Anthropic returned empty content"),
        }
    }
}

impl std::error::Error for LlmError {}

impl From<SecretError> for LlmError {
    fn from(e: SecretError) -> Self {
        match e {
            SecretError::NotConfigured => LlmError::NoApiKey,
            other => LlmError::Decode(other.to_string()),
        }
    }
}

/// Low-level Claude Haiku primitive. Single-call wrapper around
/// `POST https://api.anthropic.com/v1/messages` with the user-supplied
/// API key + system + user prompt. Returns the concatenated text
/// from the first content block on success.
///
/// Off-feature builds return `Err(LlmError::FeatureOff)` so callers
/// can fall back without compile-time `#[cfg]` gymnastics at the
/// call site.
///
/// # Errors
///
/// * `FeatureOff` — `llm-summary` cargo feature is off.
/// * `Network(_)` — DNS / TLS / connection / 30s timeout.
/// * `ApiError { status, body }` — Anthropic returned non-2xx.
/// * `Decode(_)` — response body wasn't the expected JSON envelope.
/// * `EmptyResponse` — every content block was empty.
pub fn call_claude_haiku(api_key: &str, system: &str, prompt: &str) -> Result<String, LlmError> {
    #[cfg(feature = "llm-summary")]
    {
        call_claude_haiku_with_endpoint(
            api_key,
            system,
            prompt,
            "https://api.anthropic.com/v1/messages",
        )
    }
    #[cfg(not(feature = "llm-summary"))]
    {
        let _ = (api_key, system, prompt);
        Err(LlmError::FeatureOff)
    }
}

/// Test-injectable variant — caller supplies the endpoint directly so
/// unit tests can hit a mock HTTP server without touching the real
/// Anthropic API.
#[cfg(feature = "llm-summary")]
pub fn call_claude_haiku_with_endpoint(
    api_key: &str,
    system: &str,
    prompt: &str,
    endpoint: &str,
) -> Result<String, LlmError> {
    let body = build_haiku_request_body(system, prompt);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(30))
        .timeout_write(std::time::Duration::from_secs(10))
        .build();
    // ureq 2.x returns `Err(Status)` for non-2xx by default; the
    // `Ok(_)` arm below is reached only on 2xx. No redundant
    // status-range check needed.
    match agent
        .post(endpoint)
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_string(&body)
    {
        Ok(r) => {
            let text = r.into_string().map_err(|e| LlmError::Network(format!("read body: {e}")))?;
            parse_response(&text)
        }
        Err(ureq::Error::Status(status, r)) => {
            let body = r.into_string().unwrap_or_default();
            Err(LlmError::ApiError { status, body })
        }
        Err(ureq::Error::Transport(t)) => Err(LlmError::Network(format!("{t}"))),
    }
}

#[cfg(not(feature = "llm-summary"))]
pub fn call_claude_haiku_with_endpoint(
    _api_key: &str,
    _system: &str,
    _prompt: &str,
    _endpoint: &str,
) -> Result<String, LlmError> {
    Err(LlmError::FeatureOff)
}

/// Build the JSON body for a Claude Haiku messages request. `system`
/// is sent as the top-level `system` field (not a message); `prompt`
/// becomes the single user message. Public so unit tests can verify
/// the request shape without a network round-trip.
pub fn build_haiku_request_body(system: &str, prompt: &str) -> String {
    serde_json::json!({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 800,
        "system": system,
        "messages": [{
            "role": "user",
            "content": prompt,
        }]
    })
    .to_string()
}

/// Parse Anthropic's JSON response into the markdown body. Robust
/// against missing fields — degrades to typed errors rather than
/// panicking.
pub fn parse_response(body: &str) -> Result<String, LlmError> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| LlmError::Decode(format!("JSON parse: {e}")))?;
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LlmError::Decode(format!("response has no `content` array: {body}")))?;
    let mut out = String::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        return Err(LlmError::EmptyResponse);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_haiku_body_contains_expected_fields() {
        let body = build_haiku_request_body("sys", "user");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"].as_str(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(v["max_tokens"].as_u64(), Some(800));
        assert_eq!(v["system"].as_str(), Some("sys"));
        assert_eq!(v["messages"][0]["content"].as_str(), Some("user"));
        assert_eq!(v["messages"][0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn build_haiku_body_preserves_unicode() {
        // Korean prompt content must survive the JSON round-trip
        // intact — `synthesize_with_llm` ships Korean context
        // summary prose through this body.
        let body = build_haiku_request_body("시스템 프롬프트", "사용자 질문");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["system"].as_str(), Some("시스템 프롬프트"));
        assert_eq!(v["messages"][0]["content"].as_str(), Some("사용자 질문"));
    }

    #[test]
    fn parse_response_extracts_text() {
        let body = r#"{"content":[{"type":"text","text":"hello world"}]}"#;
        assert_eq!(parse_response(body).unwrap(), "hello world");
    }

    #[test]
    fn parse_response_handles_multi_part() {
        let body = r#"{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#;
        assert_eq!(parse_response(body).unwrap(), "ab");
    }

    #[test]
    fn parse_response_rejects_empty_content() {
        let body = r#"{"content":[]}"#;
        assert!(matches!(parse_response(body), Err(LlmError::EmptyResponse)));
    }

    #[test]
    fn parse_response_decode_error_on_garbage() {
        let body = "not json at all";
        assert!(matches!(parse_response(body), Err(LlmError::Decode(_))));
    }

    #[cfg(not(feature = "llm-summary"))]
    #[test]
    fn off_feature_call_claude_haiku_returns_feature_off() {
        let r = call_claude_haiku("k", "s", "p");
        assert!(matches!(r, Err(LlmError::FeatureOff)));
    }
}
