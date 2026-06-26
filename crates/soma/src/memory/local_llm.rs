//! Local LLM helper via Ollama HTTP.
//!
//! The local LLM is not SOMA's product identity. It is a private
//! compiler helper used by opt-in ContextEnvelope compiler notes.
//! Cloud LLMs remain the interactive reasoning layer; this module only
//! drafts bounded, cited compiler-note text on the user's machine when
//! explicitly enabled.
//! ADR 0016 is the bridge contract for this boundary.
//!
//! Design choice — **ollama HTTP API** over candle / mistral.rs /
//! mlx-rs:
//!
//! * **ollama** — de-facto local LLM runner on Mac. User most
//!   likely already has it (`brew install ollama`). We don't
//!   bundle weights; user pulls models via `ollama pull gemma2:9b`.
//!   Subprocess HTTP at `localhost:11434/api/chat`. Cold start ~2s
//!   for first request, sub-second after.
//! * **candle** (already in tree for cognitive-train) — pure Rust,
//!   no external runtime. Rejected for v1: model loading + chat
//!   templating is non-trivial, build time + binary size grows,
//!   weight file management becomes our problem. Kept as v1.x
//!   alternative if we want to ship fully self-contained.
//! * **mistral.rs / mlx-rs** — promising but immature crate-level
//!   ergonomics; better than candle for chat but worse than ollama
//!   for "user already has it on their machine" reach.
//!
//! Failure modes (all surface to caller; never panic):
//!
//! * `Network` — ollama not running (most common). Message
//!   instructs `ollama serve` or `brew services start ollama`.
//! * `ModelNotPulled` — endpoint up but the requested model isn't
//!   downloaded. Message instructs `ollama pull <model>`.
//! * `ApiError { status, body }` — non-2xx from ollama.
//! * `Decode` — unexpected response shape (ollama version drift).
//! * `EmptyResponse` — successful 200 but no message content.

use serde::{Deserialize, Serialize};

/// Default endpoint — ollama's standard local port. Override through
/// `[local_compiler] local_endpoint` in `~/.soma/config.toml`; legacy
/// `[chat] local_endpoint` remains accepted as a fallback for old configs.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:11434";

/// Default model — `gemma2:9b`. Chosen for v1.x because:
///
/// * **No Chinese-token leak in Korean conversation.** v1.0 default
///   was `qwen2.5:7b` (Alibaba); base distribution is Chinese-native
///   and Korean ↔ Chinese mixed sessions surfaced 漢字 glyphs even
///   under explicit Korean-only prompts. gemma2 (Google) has
///   a multilingual-balanced base — Korean prose stays Korean.
/// * **5.4 GB quantized footprint** — fits Mini profile (24 GB RAM)
///   with headroom; slightly larger than qwen2.5:7b (4.7 GB) but the
///   quality jump for SOMA's primary use-case (Korean engineering
///   prose) justifies it.
/// * Gemma license — research + redistribution permitted under the
///   Gemma Terms of Use; v2 release allowed in commercial flows.
/// * Response latency on M-series — first token ≈ 2 s, sustained
///   ~25 tok/s (slightly slower than qwen2.5:7b's ~30 tok/s,
///   acceptable for private compiler-helper calls).
///
/// Override through `[local_compiler] local_model` in config.toml; legacy
/// `[chat] local_model` remains accepted as a fallback for old configs. The same
/// default is used by opt-in local compiler notes unless explicitly overridden.
/// Tested alternatives:
/// * `llama3.1:8b` (Meta, 4.7 GB) — Korean OK, Chinese leak rare,
///   slightly less fluent than gemma2 in Korean engineering prose.
/// * `eeve-korean:10.8b` — Korean fine-tune, highest Korean fluency
///   but 6.5 GB + slower; pick when long-form Korean compiler notes
///   matter more than latency.
/// * `qwen2.5:7b` — REJECTED as default (Chinese leak); still useful
///   when the user wants Chinese ↔ Korean code-switching.
pub const DEFAULT_MODEL: &str = "gemma2:9b";

/// Per-request timeout. Conservative — first-token latency on cold
/// model load can be 5-10s on an M1 Air.
const REQUEST_TIMEOUT_SECS: u64 = 60;

/// Sampling temperature passed to ollama as `options.temperature`.
/// ollama's default is 0.8 (creative); we drop to 0.3 because:
///
/// * **Lower minority-language token leakage.** At 0.8 the second-
///   most-likely token frequently fires, and for multilingual base
///   models that means Chinese characters slip into Korean prose
///   even with a "Korean only" system prompt. 0.3 keeps responses
///   on-distribution.
/// * **Compiler-helper summaries are closer to deterministic Q&A than
///   creative writing.** 사용자 가 "이거 왜 깨졌지" 류의 context note 를
///   요청하면 우리 가 원하는 건 *정확한* 진단 이지 *창의적* 가설 8 개 가
///   아님.
///
/// Override via `[local_compiler] temperature` in config.toml is a future
/// chunk (D143-cand) — current SOMA scope keeps this baked.
const SAMPLING_TEMPERATURE: f32 = 0.3;

#[derive(Debug)]
pub enum LocalLlmError {
    /// Could not reach the ollama HTTP endpoint at all (TCP refused,
    /// DNS fail, etc.). The error message includes a one-line
    /// remediation for the user.
    Network(String),
    /// Endpoint up but the requested model is not available locally.
    ModelNotPulled { model: String },
    /// Non-2xx HTTP response.
    ApiError { status: u16, body: String },
    /// Response shape did not match ollama's expected envelope.
    Decode(String),
    /// 200 OK but the message content was empty.
    EmptyResponse,
}

impl std::fmt::Display for LocalLlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalLlmError::Network(m) => {
                write!(
                    f,
                    "ollama 가 안 돌고 있음 — `brew install ollama` 후 \
                     `ollama serve` (또는 `brew services start ollama`) \
                     으로 띄워 주세요. (network: {m})"
                )
            }
            LocalLlmError::ModelNotPulled { model } => {
                write!(
                    f,
                    "ollama 에 model `{model}` 미설치 — `ollama pull {model}` 로 \
                     다운로드 (≈ 4-5 GB) 후 다시 시도."
                )
            }
            LocalLlmError::ApiError { status, body } => {
                // Truncate body to 100 chars to keep logs clean
                // (mirrors the LlmError::ApiError redact policy).
                let safe: String = body.chars().take(100).collect();
                let suffix = if body.chars().count() > 100 { "…(truncated)" } else { "" };
                write!(f, "ollama API error {status}: {safe}{suffix}")
            }
            LocalLlmError::Decode(m) => write!(f, "ollama response decode: {m}"),
            LocalLlmError::EmptyResponse => write!(f, "ollama returned empty content"),
        }
    }
}

impl std::error::Error for LocalLlmError {}

/// Call ollama's `/api/chat` endpoint with `system` as the system
/// message and `prompt` as the single user message. Returns the
/// concatenated text from the assistant's response.
///
/// `endpoint` is the ollama base URL (no trailing slash); typically
/// the default `DEFAULT_ENDPOINT`. `model` is the name passed to
/// ollama (`qwen2.5:7b` etc.).
///
/// Streaming is **disabled** here (`stream: false` in the request
/// body) so the entire response arrives as one JSON envelope. The
/// ContextEnvelope compiler only needs a bounded helper note.
pub fn call_ollama(
    endpoint: &str,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<String, LocalLlmError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build();

    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));
    let body = ChatRequest {
        model: model.to_string(),
        messages: vec![
            Message { role: "system".into(), content: system.to_string() },
            Message { role: "user".into(), content: prompt.to_string() },
        ],
        stream: false,
        options: ChatOptions { temperature: SAMPLING_TEMPERATURE },
    };

    let response = match agent.post(&url).send_json(&body) {
        Ok(r) => r,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            // ollama returns 404 with `{"error":"model 'foo' not found"}`
            // when the requested model isn't pulled — surface that as
            // a typed variant so callers can give a specific remediation.
            if status == 404 && body.contains("not found") {
                return Err(LocalLlmError::ModelNotPulled { model: model.to_string() });
            }
            return Err(LocalLlmError::ApiError { status, body });
        }
        Err(e) => return Err(LocalLlmError::Network(e.to_string())),
    };

    let parsed: ChatResponse =
        response.into_json().map_err(|e| LocalLlmError::Decode(e.to_string()))?;

    let content = parsed.message.content;
    if content.trim().is_empty() {
        return Err(LocalLlmError::EmptyResponse);
    }
    Ok(content)
}

/// Health probe — list available models for opt-in local compiler setup.
pub fn list_models(endpoint: &str) -> Result<Vec<String>, LocalLlmError> {
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).build();
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    let response = match agent.get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            return Err(LocalLlmError::ApiError { status, body });
        }
        Err(e) => return Err(LocalLlmError::Network(e.to_string())),
    };
    let parsed: TagsResponse =
        response.into_json().map_err(|e| LocalLlmError::Decode(e.to_string()))?;
    Ok(parsed.models.into_iter().map(|m| m.name).collect())
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    options: ChatOptions,
}

#[derive(Debug, Serialize)]
struct ChatOptions {
    temperature: f32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[allow(dead_code)]
    model: Option<String>,
    message: Message,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct TagEntry {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_message_guides_user() {
        let e = LocalLlmError::Network("connection refused".into());
        let msg = format!("{e}");
        assert!(msg.contains("ollama"), "must mention ollama: {msg}");
        assert!(msg.contains("brew"), "must mention brew install: {msg}");
    }

    #[test]
    fn model_not_pulled_message_includes_model_name() {
        let e = LocalLlmError::ModelNotPulled { model: "qwen2.5:7b".into() };
        let msg = format!("{e}");
        assert!(msg.contains("qwen2.5:7b"), "must include model name: {msg}");
        assert!(msg.contains("pull"), "must instruct to pull: {msg}");
    }

    #[test]
    fn api_error_redacts_long_body() {
        let huge = "x".repeat(500);
        let e = LocalLlmError::ApiError { status: 500, body: huge };
        let msg = format!("{e}");
        assert!(msg.contains("truncated"), "long bodies must be truncated: {msg}");
        assert!(msg.chars().count() < 200, "redacted body must keep msg short");
    }
}
