//! Local LLM helper for improving ContextEnvelope sections.
//!
//! This module keeps the local LLM in its intended role: a private
//! compiler helper. It may add an evidence-backed summary to the envelope, but
//! failure never blocks deterministic context rendering.
//! ADR 0016 pins this as local compiler assistance, not a final
//! user-facing reasoning surface.

use crate::config::{Config, LocalCompilerConfig};
use crate::context::envelope::{
    attach_compiler_notes, ContextEnvelope, ContextSection, EvidenceRef,
};
use crate::memory::local_llm::{call_ollama, LocalLlmError, DEFAULT_ENDPOINT, DEFAULT_MODEL};

pub const LOCAL_COMPILER_ENV: &str = "SOMA_CONTEXT_LOCAL_COMPILER";
pub const LOCAL_COMPILER_ENDPOINT_ENV: &str = "SOMA_LOCAL_LLM_ENDPOINT";
pub const LOCAL_COMPILER_MODEL_ENV: &str = "SOMA_LOCAL_LLM_MODEL";
pub const LOCAL_COMPILER_SYSTEM_PROMPT: &str = concat!(
    "You are SOMA's private local context compiler. ",
    "Produce only a helper context note for a cloud LLM; do not answer the user. ",
    "Use only the supplied evidence. ",
    "Every factual claim must cite evidence ids inline, e.g. episode:123. ",
    "Do not invent facts."
);

const LOCAL_COMPILER_TEXT_LIMIT: usize = 900;
const LOCAL_COMPILER_ITEM_LIMIT: usize = 5;

pub fn local_compiler_enabled_from_env() -> bool {
    std::env::var(LOCAL_COMPILER_ENV)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
}

pub fn try_attach_local_compiler_note_from_env(
    envelope: &mut ContextEnvelope,
) -> Result<bool, LocalLlmError> {
    if !local_compiler_enabled_from_env() {
        return Ok(false);
    }
    let local_config = load_local_compiler_config_from_home();
    let endpoint_override = std::env::var(LOCAL_COMPILER_ENDPOINT_ENV).ok();
    let model_override = std::env::var(LOCAL_COMPILER_MODEL_ENV).ok();
    let (endpoint, model) = resolve_local_compiler_runtime(
        endpoint_override.as_deref(),
        model_override.as_deref(),
        &local_config,
    );
    attach_local_compiler_note(envelope, &endpoint, &model)?;
    Ok(true)
}

pub fn load_local_compiler_config_from_home() -> LocalCompilerConfig {
    dirs::home_dir()
        .map(|home| Config::load_or_default(&home.join(".soma")).effective_local_compiler().clone())
        .unwrap_or_default()
}

pub fn resolve_local_compiler_runtime(
    endpoint_override: Option<&str>,
    model_override: Option<&str>,
    config: &LocalCompilerConfig,
) -> (String, String) {
    let endpoint = non_empty_override(endpoint_override)
        .or_else(|| non_empty_override(Some(config.local_endpoint.as_str())))
        .unwrap_or(DEFAULT_ENDPOINT)
        .to_string();
    let model = non_empty_override(model_override)
        .or_else(|| non_empty_override(Some(config.local_model.as_str())))
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    (endpoint, model)
}

fn non_empty_override(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub fn attach_local_compiler_note(
    envelope: &mut ContextEnvelope,
    endpoint: &str,
    model: &str,
) -> Result<(), LocalLlmError> {
    if envelope.relevant_memory.is_empty() {
        return Ok(());
    }
    let prompt = build_local_compiler_prompt(envelope);
    let text = call_ollama(endpoint, model, LOCAL_COMPILER_SYSTEM_PROMPT, &prompt)?;
    attach_local_compiler_note_from_text(envelope, &text);
    Ok(())
}

pub fn attach_local_compiler_note_from_text(envelope: &mut ContextEnvelope, text: &str) {
    let cleaned = compact_with_limit(text, LOCAL_COMPILER_TEXT_LIMIT);
    if cleaned.is_empty() {
        return;
    }
    let evidence = local_compiler_evidence(envelope);
    if evidence.is_empty() {
        return;
    }
    if !all_claims_cite_evidence(text, &evidence) {
        return;
    }
    attach_compiler_notes(
        envelope,
        vec![ContextSection::typed(cleaned, evidence, "local_compiler_note", "compiled", None)],
    );
}

pub fn build_local_compiler_prompt(envelope: &ContextEnvelope) -> String {
    let mut out = String::new();
    out.push_str("Task: compile the current SOMA ContextEnvelope into a short helper summary for a cloud LLM.\n");
    out.push_str("Rules:\n");
    out.push_str("- Use only the evidence below.\n");
    out.push_str("- Do not answer the user's task; produce context only.\n");
    out.push_str("- Mention uncertainty if the evidence is weak.\n");
    out.push_str("- Keep it under 6 bullets or one short paragraph.\n");
    out.push_str("- Include evidence ids like episode:123 in every factual claim.\n\n");

    out.push_str("Scope:\n");
    out.push_str(&format!("- kind: {}\n", envelope.scope.kind));
    if let Some(project) = &envelope.scope.project {
        out.push_str(&format!("- project: {project}\n"));
    }
    if let Some(query) = &envelope.scope.query {
        out.push_str(&format!("- query: {query}\n"));
    }

    if let Some(thread_state) = &envelope.thread_state {
        out.push_str("\nDeterministic thread_state:\n");
        out.push_str(&compact_with_limit(&thread_state.text, LOCAL_COMPILER_TEXT_LIMIT));
        out.push('\n');
    }

    out.push_str("\nRanked evidence:\n");
    for item in envelope.relevant_memory.iter().take(LOCAL_COMPILER_ITEM_LIMIT) {
        let evidence = item
            .evidence
            .first()
            .map(evidence_tag)
            .unwrap_or_else(|| "evidence:unknown".to_string());
        out.push_str(&format!(
            "- {} rank={} reason={} text={}\n",
            evidence,
            item.rank,
            item.reason,
            compact_with_limit(&item.text, 160)
        ));
    }
    out
}

fn local_compiler_evidence(envelope: &ContextEnvelope) -> Vec<EvidenceRef> {
    if let Some(thread_state) = &envelope.thread_state {
        if !thread_state.evidence.is_empty() {
            return thread_state.evidence.clone();
        }
    }
    let mut evidence = Vec::new();
    for item in envelope.relevant_memory.iter().take(LOCAL_COMPILER_ITEM_LIMIT) {
        for ev in &item.evidence {
            if !evidence.iter().any(|seen: &EvidenceRef| seen.kind == ev.kind && seen.id == ev.id) {
                evidence.push(ev.clone());
            }
        }
    }
    evidence
}

fn evidence_tag(evidence: &EvidenceRef) -> String {
    format!("{}:{}", evidence.kind, evidence.id)
}

fn all_claims_cite_evidence(text: &str, evidence: &[EvidenceRef]) -> bool {
    let tags: Vec<String> = evidence.iter().map(evidence_tag).collect();
    let mut saw_claim = false;
    for line in text.lines() {
        let line = strip_claim_prefix(line.trim());
        if line.is_empty() {
            continue;
        }
        for claim in split_claims(line) {
            let claim = strip_claim_prefix(claim.trim());
            if claim.is_empty() {
                continue;
            }
            saw_claim = true;
            if !tags.iter().any(|tag| claim.contains(tag)) {
                return false;
            }
        }
    }
    saw_claim
}

fn strip_claim_prefix(text: &str) -> &str {
    let text = text.trim_start_matches(['-', '*', ' ']);
    let digit_count = text.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return text;
    }
    let rest = &text[digit_count..];
    if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        return rest.trim_start();
    }
    text
}

fn split_claims(line: &str) -> Vec<&str> {
    let mut claims = Vec::new();
    let mut start = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if !matches!(ch, '.' | '!' | '?' | ';') {
            continue;
        }
        let at_sentence_boundary = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if at_sentence_boundary {
            claims.push(&line[start..idx]);
            start = idx + ch.len_utf8();
        }
    }
    if start < line.len() {
        claims.push(&line[start..]);
    }
    claims
}

fn compact_with_limit(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut out = compact.chars().take(limit.saturating_sub(3)).collect::<String>();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::envelope::{build_context_envelope, ContextScope};
    use crate::context::pack::{MemoryPack, PackItem, MEMORY_PACK_VERSION};

    fn pack_with_user_content(preview: &str) -> MemoryPack {
        MemoryPack {
            version: MEMORY_PACK_VERSION,
            assembled_at_ns: 42,
            query: Some("compiler".to_string()),
            recent: vec![PackItem {
                episode_id: 7,
                source: "claude-code".to_string(),
                preview: preview.to_string(),
                similarity: None,
                project: Some("SOMA".to_string()),
                session_id: Some("s1".to_string()),
                ts_start_ns: 42,
                why: "included by recency".to_string(),
            }],
            semantic: Vec::new(),
            thread_state_selection: None,
            project_state: serde_json::json!({}),
            self_state: serde_json::json!({}),
        }
    }

    #[test]
    fn prompt_contains_scope_ranked_items_and_evidence() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let envelope =
            build_context_envelope(&pack, ContextScope::current(Some("compiler".into())));

        let prompt = build_local_compiler_prompt(&envelope);

        assert!(prompt.contains("kind: current"));
        assert!(prompt.contains("query: compiler"));
        assert!(prompt.contains("episode:7"));
        assert!(prompt.contains("continue ContextEnvelope local compiler work"));
    }

    #[test]
    fn local_compiler_runtime_uses_config_defaults() {
        let config = LocalCompilerConfig {
            local_endpoint: "http://configured:11434".to_string(),
            local_model: "configured-model".to_string(),
            ..LocalCompilerConfig::default()
        };

        let (endpoint, model) = resolve_local_compiler_runtime(None, None, &config);

        assert_eq!(endpoint, "http://configured:11434");
        assert_eq!(model, "configured-model");
    }

    #[test]
    fn local_compiler_runtime_overrides_config_with_explicit_values() {
        let config = LocalCompilerConfig {
            local_endpoint: "http://configured:11434".to_string(),
            local_model: "configured-model".to_string(),
            ..LocalCompilerConfig::default()
        };

        let (endpoint, model) = resolve_local_compiler_runtime(
            Some("http://override:11434"),
            Some("override-model"),
            &config,
        );

        assert_eq!(endpoint, "http://override:11434");
        assert_eq!(model, "override-model");
    }

    #[test]
    fn local_compiler_runtime_allows_partial_overrides() {
        let config = LocalCompilerConfig {
            local_endpoint: "http://configured:11434".to_string(),
            local_model: "configured-model".to_string(),
            ..LocalCompilerConfig::default()
        };

        let (endpoint, model) =
            resolve_local_compiler_runtime(Some("http://override:11434"), None, &config);

        assert_eq!(endpoint, "http://override:11434");
        assert_eq!(model, "configured-model");
    }

    #[test]
    fn local_compiler_runtime_falls_back_from_empty_values() {
        let config = LocalCompilerConfig {
            local_endpoint: "   ".to_string(),
            local_model: String::new(),
            ..LocalCompilerConfig::default()
        };

        let (endpoint, model) = resolve_local_compiler_runtime(Some("   "), Some(""), &config);

        assert_eq!(endpoint, DEFAULT_ENDPOINT);
        assert_eq!(model, DEFAULT_MODEL);
    }

    #[test]
    fn local_compiler_note_is_evidence_backed_section() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        attach_local_compiler_note_from_text(
            &mut envelope,
            "현재 작업은 ContextEnvelope local compiler 연결이다 episode:7.",
        );

        assert_eq!(envelope.compiler_notes.len(), 1);
        assert_eq!(envelope.compiler_notes[0].kind.as_deref(), Some("local_compiler_note"));
        assert_eq!(envelope.compiler_notes[0].status.as_deref(), Some("compiled"));
        assert!(envelope.compiler_notes[0].evidence.iter().any(|ev| ev.id == "7"));
        assert!(envelope.evidence.iter().any(|ev| ev.id == "7"));
    }

    #[test]
    fn local_compiler_note_requires_inline_evidence_id() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        attach_local_compiler_note_from_text(
            &mut envelope,
            "현재 작업은 ContextEnvelope local compiler 연결이다.",
        );

        assert!(
            envelope.compiler_notes.is_empty(),
            "uncited local LLM output must not become a cloud-facing section"
        );
    }

    #[test]
    fn local_compiler_note_requires_every_claim_to_cite_evidence() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        attach_local_compiler_note_from_text(
            &mut envelope,
            "현재 작업은 ContextEnvelope local compiler 연결이다 episode:7.\n\
             추가 작업은 policy extraction이다.",
        );

        assert!(
            envelope.compiler_notes.is_empty(),
            "one cited sentence must not admit a second uncited claim"
        );
    }

    #[test]
    fn local_compiler_note_accepts_multiple_cited_claims() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        attach_local_compiler_note_from_text(
            &mut envelope,
            "- 현재 작업은 ContextEnvelope local compiler 연결이다 episode:7.\n\
             - 다음 입력도 같은 근거에서 온다 episode:7.",
        );

        assert_eq!(envelope.compiler_notes.len(), 1);
        assert!(envelope.compiler_notes[0]
            .text
            .contains("ContextEnvelope local compiler 연결이다 episode:7"));
    }

    #[test]
    fn local_compiler_note_allows_file_names_and_numbered_claims() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        attach_local_compiler_note_from_text(
            &mut envelope,
            "1. Cargo.toml references are context, not sentence boundaries episode:7.\n\
             2) compiler.rs admission guard is cited episode:7.",
        );

        assert_eq!(envelope.compiler_notes.len(), 1);
    }

    #[test]
    fn local_compiler_note_does_not_replace_deterministic_sections() {
        let pack = pack_with_user_content("continue ContextEnvelope local compiler work");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));
        let thread_state = envelope.thread_state.clone();
        let relevant_memory = envelope.relevant_memory.clone();

        attach_local_compiler_note_from_text(
            &mut envelope,
            "현재 작업은 ContextEnvelope local compiler 연결이다 episode:7.",
        );

        assert_eq!(envelope.thread_state, thread_state);
        assert_eq!(envelope.relevant_memory, relevant_memory);
        assert_eq!(envelope.compiler_notes.len(), 1);
    }

    #[test]
    fn local_compiler_system_prompt_is_not_a_final_answer_surface() {
        assert!(LOCAL_COMPILER_SYSTEM_PROMPT.contains("private local context compiler"));
        assert!(LOCAL_COMPILER_SYSTEM_PROMPT.contains("do not answer the user"));
        assert!(LOCAL_COMPILER_SYSTEM_PROMPT.contains("Every factual claim must cite evidence ids"));
        assert!(!LOCAL_COMPILER_SYSTEM_PROMPT.contains("voice"));
        assert!(!LOCAL_COMPILER_SYSTEM_PROMPT.contains("persona"));
    }
}
