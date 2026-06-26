//! ContextEnvelope — cloud-LLM-facing context contract.
//!
//! The envelope is built from the internal retrieval substrate and
//! developer/debug MemoryPack shape, then promoted into the cited prompt/tool
//! artifact a cloud LLM consumes.

use std::collections::HashSet;

use serde::Serialize;

use crate::context::correction::CORRECTION_SOURCE;
use crate::context::matching::stale_claim_matches_text;
use crate::context::pack::{MemoryPack, PackItem, ThreadStateSelection};

pub const CONTEXT_ENVELOPE_VERSION: u32 = 1;
const THREAD_STATE_ITEM_LIMIT: usize = 3;
const THREAD_STATE_TEXT_LIMIT: usize = 96;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContextScope {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

impl ContextScope {
    pub fn current(query: Option<String>) -> Self {
        Self {
            kind: "current".to_string(),
            project: None,
            session_id: None,
            thread_key: None,
            query,
        }
    }

    pub fn project(project: String, query: Option<String>) -> Self {
        Self {
            kind: "project".to_string(),
            project: Some(project),
            session_id: None,
            thread_key: None,
            query,
        }
    }

    pub fn session(session_id: String, project: Option<String>, query: Option<String>) -> Self {
        Self {
            kind: "session".to_string(),
            project,
            session_id: Some(session_id),
            thread_key: None,
            query,
        }
    }

    pub fn thread(thread_key: String, project: Option<String>, query: Option<String>) -> Self {
        Self {
            kind: "thread".to_string(),
            project,
            session_id: None,
            thread_key: Some(thread_key),
            query,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
pub struct EvidenceRef {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl EvidenceRef {
    fn tag(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextSection {
    pub text: String,
    pub evidence: Vec<EvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl ContextSection {
    pub fn new(text: String, evidence: Vec<EvidenceRef>) -> Self {
        Self { text, evidence, kind: None, status: None, confidence: None }
    }

    pub fn typed(
        text: String,
        evidence: Vec<EvidenceRef>,
        kind: impl Into<String>,
        status: impl Into<String>,
        confidence: Option<f32>,
    ) -> Self {
        Self { text, evidence, kind: Some(kind.into()), status: Some(status.into()), confidence }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextItem {
    pub text: String,
    pub evidence: Vec<EvidenceRef>,
    pub reason: String,
    pub rank: usize,
    pub layer: String,
    pub layer_rank: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ContextEnvelope {
    pub version: u32,
    pub assembled_at_ns: i64,
    pub scope: ContextScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_state: Option<ContextSection>,
    pub compiler_notes: Vec<ContextSection>,
    pub short_term_candidates: Vec<ContextSection>,
    pub project_experience: Vec<ContextSection>,
    pub relevant_memory: Vec<ContextItem>,
    pub stable_facts: Vec<ContextSection>,
    pub user_policy: Vec<ContextSection>,
    pub open_decisions: Vec<ContextSection>,
    pub corrections: Vec<ContextSection>,
    pub evidence: Vec<EvidenceRef>,
}

pub fn build_context_envelope(pack: &MemoryPack, scope: ContextScope) -> ContextEnvelope {
    let mut relevant_memory: Vec<ContextItem> =
        Vec::with_capacity(pack.semantic.len() + pack.recent.len());
    for (idx, item) in pack.semantic.iter().enumerate() {
        relevant_memory.push(item_from_pack(item, "semantic", idx + 1, relevant_memory.len() + 1));
    }
    for (idx, item) in pack.recent.iter().enumerate() {
        relevant_memory.push(item_from_pack(item, "recent", idx + 1, relevant_memory.len() + 1));
    }
    let evidence = unique_evidence(&relevant_memory);
    let thread_state = build_thread_state_from_items(
        pack.query.as_deref(),
        &relevant_memory,
        pack.thread_state_selection.as_ref(),
    );

    ContextEnvelope {
        version: CONTEXT_ENVELOPE_VERSION,
        assembled_at_ns: pack.assembled_at_ns,
        scope,
        thread_state,
        compiler_notes: Vec::new(),
        short_term_candidates: Vec::new(),
        project_experience: Vec::new(),
        relevant_memory,
        stable_facts: Vec::new(),
        user_policy: Vec::new(),
        open_decisions: Vec::new(),
        corrections: Vec::new(),
        evidence,
    }
}

pub fn attach_corrections(envelope: &mut ContextEnvelope, corrections: Vec<ContextSection>) {
    merge_section_evidence(&mut envelope.evidence, &corrections);
    envelope.corrections = corrections;
}

pub fn attach_thread_state(envelope: &mut ContextEnvelope, thread_state: Option<ContextSection>) {
    if let Some(section) = &thread_state {
        merge_section_evidence(&mut envelope.evidence, std::slice::from_ref(section));
    }
    envelope.thread_state = thread_state;
}

pub fn attach_compiler_notes(envelope: &mut ContextEnvelope, compiler_notes: Vec<ContextSection>) {
    merge_section_evidence(&mut envelope.evidence, &compiler_notes);
    envelope.compiler_notes = compiler_notes;
}

pub fn attach_short_term_candidates(
    envelope: &mut ContextEnvelope,
    short_term_candidates: Vec<ContextSection>,
) {
    merge_section_evidence(&mut envelope.evidence, &short_term_candidates);
    envelope.short_term_candidates = short_term_candidates;
}

pub fn attach_project_experience(
    envelope: &mut ContextEnvelope,
    project_experience: Vec<ContextSection>,
) {
    merge_section_evidence(&mut envelope.evidence, &project_experience);
    envelope.project_experience = project_experience;
}

pub fn append_relevant_memory_items(
    envelope: &mut ContextEnvelope,
    mut items: Vec<ContextItem>,
    thread_state_selection: Option<&ThreadStateSelection>,
) {
    if items.is_empty() {
        return;
    }
    let start_rank = envelope.relevant_memory.len() + 1;
    for (idx, item) in items.iter_mut().enumerate() {
        item.rank = start_rank + idx;
    }
    envelope.relevant_memory.extend(items);
    envelope.thread_state = build_thread_state_from_items(
        envelope.scope.query.as_deref(),
        &envelope.relevant_memory,
        thread_state_selection,
    );
    rebuild_evidence(envelope);
}

pub fn apply_correction_overrides(envelope: &mut ContextEnvelope, stale_claims: &[String]) {
    if stale_claims.is_empty() {
        return;
    }
    envelope.relevant_memory.retain(|item| {
        if item.evidence.iter().any(|ev| ev.source.as_deref() == Some(CORRECTION_SOURCE)) {
            return false;
        }
        !stale_claims.iter().any(|claim| stale_claim_matches_text(claim, &item.text))
    });
    envelope.evidence = unique_evidence(&envelope.relevant_memory);
    envelope.thread_state = build_thread_state_from_items(
        envelope.scope.query.as_deref(),
        &envelope.relevant_memory,
        None,
    );
}

pub fn attach_open_decisions(envelope: &mut ContextEnvelope, open_decisions: Vec<ContextSection>) {
    merge_section_evidence(&mut envelope.evidence, &open_decisions);
    envelope.open_decisions = open_decisions;
}

pub fn attach_stable_facts(envelope: &mut ContextEnvelope, stable_facts: Vec<ContextSection>) {
    merge_section_evidence(&mut envelope.evidence, &stable_facts);
    envelope.stable_facts = stable_facts;
}

pub fn attach_user_policy(envelope: &mut ContextEnvelope, user_policy: Vec<ContextSection>) {
    merge_section_evidence(&mut envelope.evidence, &user_policy);
    envelope.user_policy = user_policy;
}

pub fn render_json(envelope: &ContextEnvelope) -> String {
    serde_json::to_string(envelope).unwrap_or_else(|_| "{}".to_string())
}

pub fn render_xml(envelope: &ContextEnvelope) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<soma-context version=\"{}\" scope=\"{}\"",
        envelope.version,
        xml_escape(&envelope.scope.kind)
    ));
    if let Some(project) = &envelope.scope.project {
        out.push_str(&format!(" project=\"{}\"", xml_escape(project)));
    }
    if let Some(session_id) = &envelope.scope.session_id {
        out.push_str(&format!(" session_id=\"{}\"", xml_escape(session_id)));
    }
    if let Some(thread_key) = &envelope.scope.thread_key {
        out.push_str(&format!(" thread_key=\"{}\"", xml_escape(thread_key)));
    }
    if let Some(query) = &envelope.scope.query {
        out.push_str(&format!(" query=\"{}\"", xml_escape(query)));
    }
    out.push_str(">\n");

    match &envelope.thread_state {
        Some(section) => {
            out.push_str("  <thread-state");
            push_evidence_attr(&mut out, &section.evidence);
            push_section_attrs(&mut out, section);
            out.push_str(">\n");
            push_text_block(&mut out, &section.text, "    ");
            out.push_str("  </thread-state>\n");
        }
        None => out.push_str("  <thread-state />\n"),
    }

    push_section_list(&mut out, "compiler-notes", &envelope.compiler_notes);
    push_section_list(&mut out, "short-term-candidates", &envelope.short_term_candidates);
    push_section_list(&mut out, "project-experience", &envelope.project_experience);

    out.push_str("  <relevant-memory>\n");
    for item in &envelope.relevant_memory {
        out.push_str("    <item");
        push_evidence_attr(&mut out, &item.evidence);
        out.push_str(&format!(" reason=\"{}\"", xml_escape(&item.reason)));
        out.push_str(&format!(
            " rank=\"{}\" layer=\"{}\" layer_rank=\"{}\"",
            item.rank,
            xml_escape(&item.layer),
            item.layer_rank
        ));
        if let Some(similarity) = item.similarity {
            out.push_str(&format!(" similarity=\"{similarity:.3}\""));
        }
        if let Some(project) = &item.project {
            out.push_str(&format!(" project=\"{}\"", xml_escape(project)));
        }
        out.push('>');
        out.push_str(&xml_escape(&item.text));
        out.push_str("</item>\n");
    }
    out.push_str("  </relevant-memory>\n");

    push_section_list(&mut out, "stable-facts", &envelope.stable_facts);
    push_section_list(&mut out, "user-policy", &envelope.user_policy);
    push_section_list(&mut out, "open-decisions", &envelope.open_decisions);
    push_section_list(&mut out, "corrections", &envelope.corrections);

    out.push_str("  <evidence>\n");
    for evidence in &envelope.evidence {
        out.push_str(&format!(
            "    <ref kind=\"{}\" id=\"{}\"",
            xml_escape(&evidence.kind),
            xml_escape(&evidence.id)
        ));
        if let Some(source) = &evidence.source {
            out.push_str(&format!(" source=\"{}\"", xml_escape(source)));
        }
        out.push_str(" />\n");
    }
    out.push_str("  </evidence>\n");
    out.push_str("</soma-context>\n");
    out
}

fn item_from_pack(item: &PackItem, layer: &str, layer_rank: usize, rank: usize) -> ContextItem {
    ContextItem {
        text: item.preview.clone(),
        evidence: vec![evidence_from_item(item)],
        reason: item.why.clone(),
        rank,
        layer: layer.to_string(),
        layer_rank,
        similarity: item.similarity,
        project: item.project.clone(),
        session_id: item.session_id.clone(),
    }
}

fn evidence_from_item(item: &PackItem) -> EvidenceRef {
    EvidenceRef {
        kind: "episode".to_string(),
        id: item.episode_id.to_string(),
        source: Some(item.source.clone()),
    }
}

fn unique_evidence(items: &[ContextItem]) -> Vec<EvidenceRef> {
    let mut seen = HashSet::new();
    let mut evidence = Vec::new();
    for item in items {
        for ev in &item.evidence {
            if seen.insert(ev.tag()) {
                evidence.push(ev.clone());
            }
        }
    }
    evidence
}

fn rebuild_evidence(envelope: &mut ContextEnvelope) {
    let mut evidence = unique_evidence(&envelope.relevant_memory);
    if let Some(section) = &envelope.thread_state {
        merge_section_evidence(&mut evidence, std::slice::from_ref(section));
    }
    merge_section_evidence(&mut evidence, &envelope.compiler_notes);
    merge_section_evidence(&mut evidence, &envelope.short_term_candidates);
    merge_section_evidence(&mut evidence, &envelope.project_experience);
    merge_section_evidence(&mut evidence, &envelope.stable_facts);
    merge_section_evidence(&mut evidence, &envelope.user_policy);
    merge_section_evidence(&mut evidence, &envelope.open_decisions);
    merge_section_evidence(&mut evidence, &envelope.corrections);
    envelope.evidence = evidence;
}

fn merge_section_evidence(evidence: &mut Vec<EvidenceRef>, sections: &[ContextSection]) {
    let mut seen: HashSet<String> = evidence.iter().map(EvidenceRef::tag).collect();
    for section in sections {
        for ev in &section.evidence {
            if seen.insert(ev.tag()) {
                evidence.push(ev.clone());
            }
        }
    }
}

fn build_thread_state_from_items(
    query: Option<&str>,
    items: &[ContextItem],
    selection: Option<&ThreadStateSelection>,
) -> Option<ContextSection> {
    if items.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    match query.map(|q| compact_with_limit(q, THREAD_STATE_TEXT_LIMIT)).filter(|q| !q.is_empty()) {
        Some(query) => {
            lines.push(format!(
                "SOMA compiled {} local episode(s) relevant to query `{query}`.",
                items.len()
            ));
        }
        None => lines.push(format!(
            "SOMA compiled {} recent local episode(s) for this context scope.",
            items.len()
        )),
    }
    let status = if let Some(selection) = selection {
        lines.push(format!(
            "Working memory selector: {} selected {} episode(s) from persisted mLSTM state dim={} saved_at_ns={}.",
            selection.strategy,
            selection.selected_episode_ids.len(),
            selection.dim,
            selection.saved_at_ns
        ));
        "compiled+mlstm"
    } else {
        "compiled"
    };
    lines.push("Top context:".to_string());

    let selected = select_thread_state_items(items, selection);
    for item in &selected {
        lines.push(thread_state_item_line(item));
    }

    let evidence =
        unique_evidence_for_refs(selected.into_iter().flat_map(|item| item.evidence.iter()));
    if evidence.is_empty() {
        return None;
    }

    Some(ContextSection::typed(lines.join("\n"), evidence, "thread_state", status, None))
}

fn select_thread_state_items<'a>(
    items: &'a [ContextItem],
    selection: Option<&ThreadStateSelection>,
) -> Vec<&'a ContextItem> {
    let mut selected: Vec<&ContextItem> = Vec::new();
    let mut seen = HashSet::new();
    if let Some(selection) = selection {
        for id in &selection.selected_episode_ids {
            let id_s = id.to_string();
            if let Some(item) = items
                .iter()
                .find(|item| item.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == id_s))
            {
                if seen.insert(thread_state_item_key(item)) {
                    selected.push(item);
                }
            }
            if selected.len() >= THREAD_STATE_ITEM_LIMIT {
                return selected;
            }
        }
    }
    for item in items {
        let key = thread_state_item_key(item);
        if seen.insert(key) {
            selected.push(item);
        }
        if selected.len() >= THREAD_STATE_ITEM_LIMIT {
            break;
        }
    }
    selected
}

fn thread_state_item_key(item: &ContextItem) -> String {
    item.evidence
        .iter()
        .find(|ev| ev.kind == "episode")
        .map(EvidenceRef::tag)
        .unwrap_or_else(|| format!("rank:{}", item.rank))
}

fn thread_state_item_line(item: &ContextItem) -> String {
    let evidence = item
        .evidence
        .first()
        .map(EvidenceRef::tag)
        .unwrap_or_else(|| "evidence:unknown".to_string());
    let preview = compact_with_limit(&item.text, THREAD_STATE_TEXT_LIMIT);
    format!(
        "- {} #{} (overall #{}, {}, reason: {}): {}",
        item.layer, item.layer_rank, item.rank, evidence, item.reason, preview
    )
}

fn unique_evidence_for_refs<'a>(
    refs: impl IntoIterator<Item = &'a EvidenceRef>,
) -> Vec<EvidenceRef> {
    let mut seen = HashSet::new();
    let mut evidence = Vec::new();
    for ev in refs {
        if seen.insert(ev.tag()) {
            evidence.push(ev.clone());
        }
    }
    evidence
}

fn compact_one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_with_limit(text: &str, limit: usize) -> String {
    let compact = compact_one_line(text);
    if compact.chars().count() <= limit {
        return compact;
    }

    let mut out = compact.chars().take(limit.saturating_sub(3)).collect::<String>();
    out.push_str("...");
    out
}

fn push_text_block(out: &mut String, text: &str, indent: &str) {
    for line in text.lines() {
        out.push_str(indent);
        out.push_str(&xml_escape(line));
        out.push('\n');
    }
}

fn push_evidence_attr(out: &mut String, evidence: &[EvidenceRef]) {
    if evidence.is_empty() {
        return;
    }
    let refs = evidence.iter().map(EvidenceRef::tag).collect::<Vec<_>>().join(" ");
    out.push_str(&format!(" evidence=\"{}\"", xml_escape(&refs)));
}

fn push_section_attrs(out: &mut String, section: &ContextSection) {
    if let Some(kind) = &section.kind {
        out.push_str(&format!(" kind=\"{}\"", xml_escape(kind)));
    }
    if let Some(status) = &section.status {
        out.push_str(&format!(" status=\"{}\"", xml_escape(status)));
    }
    if let Some(confidence) = section.confidence {
        out.push_str(&format!(" confidence=\"{confidence:.3}\""));
    }
}

fn push_section_list(out: &mut String, tag: &str, sections: &[ContextSection]) {
    if sections.is_empty() {
        out.push_str(&format!("  <{tag} />\n"));
        return;
    }
    out.push_str(&format!("  <{tag}>\n"));
    for section in sections {
        out.push_str("    <claim");
        push_evidence_attr(out, &section.evidence);
        push_section_attrs(out, section);
        out.push('>');
        out.push_str(&xml_escape(&section.text));
        out.push_str("</claim>\n");
    }
    out.push_str(&format!("  </{tag}>\n"));
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::pack::{MemoryPack, PackItem, MEMORY_PACK_VERSION};

    fn pack_with_user_content(preview: &str) -> MemoryPack {
        MemoryPack {
            version: MEMORY_PACK_VERSION,
            assembled_at_ns: 42,
            query: Some("auth".to_string()),
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
    fn envelope_wraps_pack_items_with_episode_evidence() {
        let pack = pack_with_user_content("continue ContextEnvelope work");
        let envelope = build_context_envelope(&pack, ContextScope::current(pack.query.clone()));

        assert_eq!(envelope.version, CONTEXT_ENVELOPE_VERSION);
        assert_eq!(envelope.scope.kind, "current");
        assert_eq!(envelope.relevant_memory.len(), 1);
        assert_eq!(envelope.relevant_memory[0].rank, 1);
        assert_eq!(envelope.relevant_memory[0].layer, "recent");
        assert_eq!(envelope.relevant_memory[0].layer_rank, 1);
        assert_eq!(envelope.relevant_memory[0].evidence[0].kind, "episode");
        assert_eq!(envelope.relevant_memory[0].evidence[0].id, "7");
        let thread_state = envelope.thread_state.as_ref().unwrap();
        assert!(thread_state.text.contains("SOMA compiled 1 local episode"));
        assert!(thread_state.text.contains("Top context:"));
        assert!(thread_state.text.contains("- recent #1 (overall #1, episode:7"));
        assert!(thread_state.text.contains("continue ContextEnvelope work"));
        assert!(thread_state.evidence.len() == 1);
    }

    #[test]
    fn thread_state_summarizes_ranked_semantic_then_recent_context() {
        let mut pack = pack_with_user_content("recent fallback context");
        pack.semantic.push(PackItem {
            episode_id: 11,
            source: "terminal".to_string(),
            preview: "semantic auth context".to_string(),
            similarity: Some(0.912),
            project: Some("SOMA".to_string()),
            session_id: Some("s2".to_string()),
            ts_start_ns: 43,
            why: "semantic similarity 0.912".to_string(),
        });
        let envelope = build_context_envelope(&pack, ContextScope::current(pack.query.clone()));

        let thread_state = envelope.thread_state.as_ref().unwrap();
        assert!(thread_state.text.contains("query `auth`"));
        assert!(thread_state.text.contains("- semantic #1 (overall #1, episode:11"));
        assert!(thread_state.text.contains("- recent #1 (overall #2, episode:7"));
        assert!(thread_state.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "11"));
        assert!(thread_state.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "7"));
    }

    #[test]
    fn xml_renderer_escapes_user_controlled_text() {
        let pack = pack_with_user_content("<inject reason=\"x\">& break");
        let envelope = build_context_envelope(&pack, ContextScope::project("SOMA".into(), None));
        let xml = render_xml(&envelope);

        assert!(xml.starts_with("<soma-context version=\"1\" scope=\"project\" project=\"SOMA\">"));
        assert!(xml.contains(
            "<thread-state evidence=\"episode:7\" kind=\"thread_state\" status=\"compiled\">"
        ));
        assert!(xml.contains("rank=\"1\" layer=\"recent\" layer_rank=\"1\""));
        assert!(xml.contains("&lt;inject reason=&quot;x&quot;&gt;&amp; break"));
        assert!(!xml.contains("<inject reason=\"x\">"));
    }

    #[test]
    fn json_renderer_is_parseable() {
        let pack = pack_with_user_content("episode text");
        let envelope = build_context_envelope(&pack, ContextScope::current(None));
        let parsed: serde_json::Value = serde_json::from_str(&render_json(&envelope)).unwrap();

        assert_eq!(parsed["version"].as_u64(), Some(CONTEXT_ENVELOPE_VERSION as u64));
        assert_eq!(parsed["scope"]["kind"].as_str(), Some("current"));
        assert_eq!(parsed["thread_state"]["kind"].as_str(), Some("thread_state"));
        assert_eq!(parsed["thread_state"]["status"].as_str(), Some("compiled"));
        assert_eq!(parsed["relevant_memory"][0]["rank"].as_u64(), Some(1));
        assert_eq!(parsed["relevant_memory"][0]["layer"].as_str(), Some("recent"));
        assert_eq!(parsed["relevant_memory"][0]["layer_rank"].as_u64(), Some(1));
        assert!(parsed["project_experience"].as_array().unwrap().is_empty());
        assert_eq!(parsed["evidence"][0]["id"].as_str(), Some("7"));
    }

    #[test]
    fn open_decisions_extend_top_level_evidence() {
        let pack = pack_with_user_content("episode text");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));
        attach_open_decisions(
            &mut envelope,
            vec![ContextSection::typed(
                "Open contradiction between #7 and #8.".to_string(),
                vec![
                    EvidenceRef {
                        kind: "belief_candidate".to_string(),
                        id: "3".to_string(),
                        source: Some("contradicts".to_string()),
                    },
                    EvidenceRef {
                        kind: "episode".to_string(),
                        id: "8".to_string(),
                        source: Some("terminal".to_string()),
                    },
                ],
                "contradiction",
                "open",
                Some(0.91),
            )],
        );

        assert_eq!(envelope.open_decisions.len(), 1);
        assert_eq!(envelope.open_decisions[0].kind.as_deref(), Some("contradiction"));
        assert_eq!(envelope.open_decisions[0].status.as_deref(), Some("open"));
        assert_eq!(envelope.open_decisions[0].confidence, Some(0.91));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "belief_candidate" && ev.id == "3"));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "7"));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "8"));
    }

    #[test]
    fn short_term_candidates_extend_top_level_evidence() {
        let pack = pack_with_user_content("episode text");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));
        attach_short_term_candidates(
            &mut envelope,
            vec![ContextSection::typed(
                "Candidate signal: user may want a first-class L2 section.".to_string(),
                vec![
                    EvidenceRef {
                        kind: "evidence_latent_proxy".to_string(),
                        id: "5".to_string(),
                        source: Some("task_candidate".to_string()),
                    },
                    EvidenceRef {
                        kind: "episode".to_string(),
                        id: "12".to_string(),
                        source: Some("claude-code".to_string()),
                    },
                ],
                "task_candidate",
                "short_term_candidate",
                Some(0.73),
            )],
        );

        assert_eq!(envelope.short_term_candidates.len(), 1);
        assert_eq!(envelope.short_term_candidates[0].kind.as_deref(), Some("task_candidate"));
        assert_eq!(
            envelope.short_term_candidates[0].status.as_deref(),
            Some("short_term_candidate")
        );
        assert_eq!(envelope.short_term_candidates[0].confidence, Some(0.73));
        assert!(envelope
            .evidence
            .iter()
            .any(|ev| ev.kind == "evidence_latent_proxy" && ev.id == "5"));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "12"));
        let xml = render_xml(&envelope);
        assert!(xml.contains("<short-term-candidates>"));
        assert!(xml.contains("status=\"short_term_candidate\""));
    }

    #[test]
    fn project_experience_extends_top_level_evidence() {
        let pack = pack_with_user_content("episode text");
        let mut envelope =
            build_context_envelope(&pack, ContextScope::project("SOMA".into(), None));
        attach_project_experience(
            &mut envelope,
            vec![ContextSection::typed(
                "Project experience: project `SOMA` has 3 recent local episode(s).".to_string(),
                vec![EvidenceRef {
                    kind: "episode".to_string(),
                    id: "13".to_string(),
                    source: Some("codex-cli".to_string()),
                }],
                "project_experience",
                "scoped_recent",
                None,
            )],
        );

        assert_eq!(envelope.project_experience.len(), 1);
        assert_eq!(envelope.project_experience[0].kind.as_deref(), Some("project_experience"));
        assert_eq!(envelope.project_experience[0].status.as_deref(), Some("scoped_recent"));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "13"));
        let xml = render_xml(&envelope);
        assert!(xml.contains("<project-experience>"));
        assert!(xml.contains("kind=\"project_experience\""));
        assert!(xml.contains("status=\"scoped_recent\""));
    }

    #[test]
    fn user_policy_extends_top_level_evidence() {
        let pack = pack_with_user_content("episode text");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));
        attach_user_policy(
            &mut envelope,
            vec![ContextSection::typed(
                "Prefer concise Korean status updates.".to_string(),
                vec![EvidenceRef {
                    kind: "episode".to_string(),
                    id: "9".to_string(),
                    source: Some("claude-code".to_string()),
                }],
                "user_policy",
                "active",
                Some(0.82),
            )],
        );

        assert_eq!(envelope.user_policy.len(), 1);
        assert_eq!(envelope.user_policy[0].kind.as_deref(), Some("user_policy"));
        assert_eq!(envelope.user_policy[0].status.as_deref(), Some("active"));
        assert_eq!(envelope.user_policy[0].confidence, Some(0.82));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "9"));
    }

    #[test]
    fn corrections_extend_top_level_evidence() {
        let pack = pack_with_user_content("episode text");
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));
        attach_corrections(
            &mut envelope,
            vec![ContextSection::typed(
                "Correction: ContextEnvelope is core.".to_string(),
                vec![EvidenceRef {
                    kind: "episode".to_string(),
                    id: "10".to_string(),
                    source: Some("correction".to_string()),
                }],
                "correction",
                "active",
                None,
            )],
        );

        assert_eq!(envelope.corrections.len(), 1);
        assert_eq!(envelope.corrections[0].kind.as_deref(), Some("correction"));
        assert_eq!(envelope.corrections[0].status.as_deref(), Some("active"));
        assert!(envelope.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "10"));
        let xml = render_xml(&envelope);
        assert!(xml.contains("<corrections>"));
        assert!(xml.contains("kind=\"correction\" status=\"active\""));
    }

    #[test]
    fn correction_overrides_suppress_stale_relevant_memory() {
        let mut pack = pack_with_user_content("voice is the core product");
        pack.recent.push(PackItem {
            episode_id: 8,
            source: "claude-code".to_string(),
            preview: "ContextEnvelope is the core contract".to_string(),
            similarity: None,
            project: Some("SOMA".to_string()),
            session_id: Some("s1".to_string()),
            ts_start_ns: 43,
            why: "included by recency".to_string(),
        });
        pack.recent.push(PackItem {
            episode_id: 9,
            source: "correction".to_string(),
            preview: "Claim corrected:\nvoice is core".to_string(),
            similarity: None,
            project: Some("SOMA".to_string()),
            session_id: Some("s1".to_string()),
            ts_start_ns: 44,
            why: "included by recency".to_string(),
        });
        let mut envelope = build_context_envelope(&pack, ContextScope::current(None));

        apply_correction_overrides(&mut envelope, &["voice is core".to_string()]);

        assert_eq!(envelope.relevant_memory.len(), 1);
        assert_eq!(envelope.relevant_memory[0].text, "ContextEnvelope is the core contract");
        assert!(!envelope.evidence.iter().any(|ev| ev.id == "7"));
        assert!(!envelope.evidence.iter().any(|ev| ev.id == "9"));
        assert!(envelope.evidence.iter().any(|ev| ev.id == "8"));
        let thread_state = envelope.thread_state.as_ref().unwrap();
        assert!(thread_state.text.contains("ContextEnvelope is the core contract"));
        assert!(!thread_state.text.contains("voice is the core product"));
        assert!(thread_state.evidence.iter().any(|ev| ev.kind == "episode" && ev.id == "8"));
        assert!(!thread_state.evidence.iter().any(|ev| ev.id == "7"));
    }
}
