//! Context quality adapters.
//!
//! These functions are the narrow bridge from optional cognitive / quality
//! signals into the cloud-LLM-facing ContextEnvelope. They do not redefine
//! the product surface; they only enrich envelope sections when a signal is
//! already connected to ranking, scoping, compression, conflict detection, or
//! evidence selection.

use std::collections::{BTreeMap, BTreeSet};

use crate::context::correction::CORRECTION_SOURCE;
use crate::context::envelope::{ContextItem, ContextSection, EvidenceRef};
use crate::context::matching::stale_claim_matches_text;
use crate::memory::beliefs::BeliefCandidate;
use crate::memory::policy::Policy;
use crate::memory::salience::IPC_FREE_ENERGY_ANOMALY_KIND;
use crate::storage::LifecycleState;
use crate::storage::{
    ClaimSourceType, ContextAnomaly, EvidenceBackedLatentProxy, SensitivityLabel, Storage,
    StorageError, StoredClaimRecord, StoredEpisode, StoredEvidenceRef,
};

pub const DEFAULT_OPEN_DECISION_LIMIT: usize = 5;
pub const DEFAULT_CORRECTION_LIMIT: usize = 5;
pub const DEFAULT_SHORT_TERM_CANDIDATE_LIMIT: usize = 5;
pub const DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT: usize = 5;
pub const DEFAULT_STABLE_FACT_LIMIT: usize = 5;
pub const DEFAULT_PROJECT_EXPERIENCE_EVIDENCE_LIMIT: usize = 3;
const PROJECT_FILTER_SCAN_MULTIPLIER: usize = 10;
const PROJECT_EXPERIENCE_SCAN_LIMIT: usize = 200;
const CORRECTED_POLICY_CONFIDENCE_MULTIPLIER: f32 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub struct CorrectionSignal {
    pub section: ContextSection,
    pub stale_claim: Option<String>,
}

pub fn short_term_candidates_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    short_term_candidates_from_storage_scoped(storage, project_filter, session_filter, None, limit)
}

pub fn short_term_candidates_from_storage_session_set(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    short_term_candidates_from_storage_scoped(
        storage,
        project_filter,
        None,
        Some(session_filters),
        limit,
    )
}

fn short_term_candidates_from_storage_scoped(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let proxies = storage.short_term_candidate_proxies(read_limit)?;
    let mut sections = Vec::with_capacity(limit);
    for proxy in proxies {
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            continue;
        };
        if !episode_matches_scope(&episode, project_filter, session_filter, session_filters) {
            continue;
        }
        if !short_term_proxy_can_project(&proxy) {
            continue;
        }
        sections.push(section_from_short_term_proxy(&proxy, &episode));
        if sections.len() == limit {
            break;
        }
    }
    Ok(sections)
}

pub fn relevant_memory_proxies_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<ContextItem>, StorageError> {
    relevant_memory_proxies_from_storage_scoped(
        storage,
        project_filter,
        session_filter,
        None,
        limit,
    )
}

pub fn relevant_memory_proxies_from_storage_session_set(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    limit: usize,
) -> Result<Vec<ContextItem>, StorageError> {
    relevant_memory_proxies_from_storage_scoped(
        storage,
        project_filter,
        None,
        Some(session_filters),
        limit,
    )
}

pub fn project_experience_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    evidence_limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    let Some(project) = project_filter.and_then(nonempty_str) else {
        return Ok(Vec::new());
    };

    let mut episode_count = 0_usize;
    let mut source_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut memory_tier_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut sessions: BTreeSet<String> = BTreeSet::new();
    let mut git_branches: BTreeSet<String> = BTreeSet::new();
    let mut cwd_samples: BTreeSet<String> = BTreeSet::new();
    let mut first_seen_ns: Option<i64> = None;
    let mut last_seen_ns: Option<i64> = None;
    let mut evidence = Vec::new();
    let evidence_limit = evidence_limit.min(10);

    for episode in storage.recent_episodes(PROJECT_EXPERIENCE_SCAN_LIMIT)? {
        if !episode_matches_scope(&episode, Some(project), session_filter, None) {
            continue;
        }
        episode_count += 1;
        *source_counts.entry(episode.source.to_string()).or_default() += 1;
        *memory_tier_counts.entry(episode.memory_tier.clone()).or_default() += 1;
        if let Some(session_id) = episode.session_id.as_deref().and_then(nonempty_str) {
            sessions.insert(session_id.to_string());
        }
        if let Some(branch) = episode.git_branch.as_deref().and_then(nonempty_str) {
            git_branches.insert(branch.to_string());
        }
        if let Some(cwd) = episode.cwd.as_deref().and_then(nonempty_str) {
            cwd_samples.insert(cwd.to_string());
        }
        first_seen_ns =
            Some(first_seen_ns.map_or(episode.ts_start_ns, |seen| seen.min(episode.ts_start_ns)));
        last_seen_ns =
            Some(last_seen_ns.map_or(episode.ts_start_ns, |seen| seen.max(episode.ts_start_ns)));
        if evidence.len() < evidence_limit {
            evidence.push(evidence_from_episode(&episode));
        }
    }

    if episode_count == 0 || evidence.is_empty() {
        return Ok(Vec::new());
    }

    let mut text = if let Some(session_id) = session_filter.and_then(nonempty_str) {
        format!(
            "Project experience: project `{project}` session `{session_id}` has {episode_count} recent local episode(s) in the last {PROJECT_EXPERIENCE_SCAN_LIMIT} captured episode(s)"
        )
    } else {
        format!(
            "Project experience: project `{project}` has {episode_count} recent local episode(s) across {} session(s) in the last {PROJECT_EXPERIENCE_SCAN_LIMIT} captured episode(s)",
            sessions.len()
        )
    };
    text.push_str(&format!("; sources: {}", count_map_summary(&source_counts)));
    text.push_str(&format!("; memory tiers: {}", count_map_summary(&memory_tier_counts)));
    if let Some(first_seen) = first_seen_ns {
        text.push_str(&format!("; first_seen_ns: {first_seen}"));
    }
    if let Some(last_seen) = last_seen_ns {
        text.push_str(&format!("; last_seen_ns: {last_seen}"));
    }
    if !sessions.is_empty() {
        text.push_str(&format!("; recent sessions: {}", set_summary(&sessions, 5)));
    }
    if !git_branches.is_empty() {
        text.push_str(&format!("; git branches: {}", set_summary(&git_branches, 5)));
    }
    if !cwd_samples.is_empty() {
        text.push_str(&format!("; cwd samples: {}", set_summary(&cwd_samples, 3)));
    }
    text.push('.');

    Ok(vec![ContextSection::typed(text, evidence, "project_experience", "scoped_recent", None)])
}

fn relevant_memory_proxies_from_storage_scoped(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
    limit: usize,
) -> Result<Vec<ContextItem>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let proxies = storage.long_term_proxies_for_envelope_section("relevant_memory", read_limit)?;
    let mut items = Vec::with_capacity(limit);
    let mut touched_proxy_ids = Vec::new();
    for proxy in proxies {
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            continue;
        };
        if !episode_matches_scope(&episode, project_filter, session_filter, session_filters) {
            continue;
        }
        if !long_term_proxy_can_project(&proxy) {
            continue;
        }
        touched_proxy_ids.push(proxy.id);
        items.push(item_from_long_term_proxy(&proxy, &episode, items.len() + 1));
        if items.len() == limit {
            break;
        }
    }
    storage.touch_long_term_proxy_accesses(touched_proxy_ids)?;
    Ok(items)
}

/// Surface recent contradiction candidates as open decisions for the cloud LLM.
///
/// This is the first explicit "quality module" adapter: the belief graph's
/// `contradicts` rows become auditable ContextEnvelope claims with both the
/// belief row and source episodes as evidence.
pub fn open_decisions_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    open_decisions_from_storage_with_corrections(storage, project_filter, limit, &[])
}

pub fn open_decisions_from_storage_with_corrections(
    storage: &Storage,
    project_filter: Option<&str>,
    limit: usize,
    correction_stale_claims: &[String],
) -> Result<Vec<ContextSection>, StorageError> {
    open_decisions_from_storage_scoped_with_corrections(
        storage,
        project_filter,
        None,
        limit,
        correction_stale_claims,
    )
}

pub fn open_decisions_from_storage_scoped_with_corrections(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    limit: usize,
    correction_stale_claims: &[String],
) -> Result<Vec<ContextSection>, StorageError> {
    open_decisions_from_storage_scoped_with_corrections_sessions(
        storage,
        project_filter,
        session_filter,
        None,
        limit,
        correction_stale_claims,
    )
}

pub fn open_decisions_from_storage_session_set_with_corrections(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    limit: usize,
    correction_stale_claims: &[String],
) -> Result<Vec<ContextSection>, StorageError> {
    open_decisions_from_storage_scoped_with_corrections_sessions(
        storage,
        project_filter,
        None,
        Some(session_filters),
        limit,
        correction_stale_claims,
    )
}

fn open_decisions_from_storage_scoped_with_corrections_sessions(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
    limit: usize,
    correction_stale_claims: &[String],
) -> Result<Vec<ContextSection>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let candidates = storage.recent_contradictions(read_limit)?;
    let mut sections = Vec::with_capacity(limit);

    for candidate in candidates {
        let Some((episode_a, episode_b)) = live_pair(storage, &candidate)? else {
            continue;
        };
        if !episode_pair_matches_scope(
            &episode_a,
            &episode_b,
            project_filter,
            session_filter,
            session_filters,
        ) {
            continue;
        }
        if contradiction_matches_stale_claim(
            &candidate,
            &episode_a,
            &episode_b,
            correction_stale_claims,
        ) {
            continue;
        }

        sections.push(section_from_contradiction(&candidate, &episode_a, &episode_b));
        if sections.len() == limit {
            break;
        }
    }

    if sections.len() < limit {
        let anomalies =
            storage.recent_context_anomalies(IPC_FREE_ENERGY_ANOMALY_KIND, read_limit)?;
        for anomaly in anomalies {
            let Some(episode) = storage.get_live_episode(anomaly.episode_id)? else {
                continue;
            };
            if !episode_matches_scope(&episode, project_filter, session_filter, session_filters) {
                continue;
            }
            if anomaly_matches_stale_claim(&anomaly, &episode, correction_stale_claims) {
                continue;
            }

            sections.push(section_from_context_anomaly(&anomaly, &episode));
            if sections.len() == limit {
                break;
            }
        }
    }

    if sections.len() < limit {
        append_semantic_proxy_sections(
            storage,
            project_filter,
            session_filter,
            session_filters,
            "open_decisions",
            limit - sections.len(),
            &mut sections,
        )?;
    }

    Ok(sections)
}

pub fn user_policy_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
) -> Result<Vec<ContextSection>, StorageError> {
    user_policy_from_storage_with_corrections(storage, project_filter, &[])
}

pub fn stable_facts_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut sections: Vec<ContextSection> = storage
        .semantic_claim_records_scoped(project_filter, session_filter, limit)?
        .into_iter()
        .map(|claim| section_from_semantic_claim(&claim))
        .collect();
    if sections.len() < limit {
        append_semantic_proxy_sections(
            storage,
            project_filter,
            session_filter,
            None,
            "stable_facts",
            limit - sections.len(),
            &mut sections,
        )?;
    }
    Ok(sections)
}

pub fn stable_facts_from_storage_session_set(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let mut sections = Vec::with_capacity(limit);
    for claim in storage.semantic_claim_records_scoped(project_filter, None, read_limit)? {
        if !claim_matches_session_set(storage, &claim, project_filter, session_filters)? {
            continue;
        }
        sections.push(section_from_semantic_claim(&claim));
        if sections.len() == limit {
            break;
        }
    }
    if sections.len() < limit {
        append_semantic_proxy_sections(
            storage,
            project_filter,
            None,
            Some(session_filters),
            "stable_facts",
            limit - sections.len(),
            &mut sections,
        )?;
    }
    Ok(sections)
}

pub fn user_policy_from_storage_with_corrections(
    storage: &Storage,
    project_filter: Option<&str>,
    correction_signals: &[CorrectionSignal],
) -> Result<Vec<ContextSection>, StorageError> {
    let mut sections = Vec::new();
    append_policy_sections(storage, None, correction_signals, &mut sections)?;
    if let Some(project) = project_filter {
        append_policy_sections(storage, Some(project), correction_signals, &mut sections)?;
    }
    append_semantic_proxy_sections(
        storage,
        project_filter,
        None,
        None,
        "user_policy",
        DEFAULT_STABLE_FACT_LIMIT,
        &mut sections,
    )?;
    Ok(sections)
}

pub fn user_policy_from_storage_with_corrections_session_set(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    correction_signals: &[CorrectionSignal],
) -> Result<Vec<ContextSection>, StorageError> {
    let mut sections = Vec::new();
    append_policy_sections(storage, None, correction_signals, &mut sections)?;
    if let Some(project) = project_filter {
        append_policy_sections(storage, Some(project), correction_signals, &mut sections)?;
    }
    append_semantic_proxy_sections(
        storage,
        project_filter,
        None,
        Some(session_filters),
        "user_policy",
        DEFAULT_STABLE_FACT_LIMIT,
        &mut sections,
    )?;
    Ok(sections)
}

pub fn corrections_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<ContextSection>, StorageError> {
    Ok(correction_signals_from_storage(storage, project_filter, limit)?
        .into_iter()
        .map(|signal| signal.section)
        .collect())
}

pub fn correction_signals_from_storage(
    storage: &Storage,
    project_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<CorrectionSignal>, StorageError> {
    correction_signals_from_storage_scoped(storage, project_filter, None, limit)
}

pub fn correction_signals_from_storage_scoped(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<CorrectionSignal>, StorageError> {
    correction_signals_from_storage_scoped_sessions(
        storage,
        project_filter,
        session_filter,
        None,
        limit,
    )
}

pub fn correction_signals_from_storage_session_set(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filters: &[String],
    limit: usize,
) -> Result<Vec<CorrectionSignal>, StorageError> {
    correction_signals_from_storage_scoped_sessions(
        storage,
        project_filter,
        None,
        Some(session_filters),
        limit,
    )
}

fn correction_signals_from_storage_scoped_sessions(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
    limit: usize,
) -> Result<Vec<CorrectionSignal>, StorageError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let episodes = storage.recent_episodes(read_limit)?;
    let mut sections = Vec::with_capacity(limit);

    for episode in episodes {
        if episode.source != CORRECTION_SOURCE {
            continue;
        }
        if let Some(project) = project_filter {
            if episode.project.as_deref() != Some(project) {
                continue;
            }
        }
        if let Some(session_id) = session_filter {
            if episode.session_id.as_deref() != Some(session_id) {
                continue;
            }
        }
        if let Some(session_filters) = session_filters {
            if !session_matches_filter_set(episode.session_id.as_deref(), session_filters) {
                continue;
            }
        }

        sections.push(signal_from_correction(&episode));
        if sections.len() == limit {
            break;
        }
    }

    if sections.len() < limit {
        let mut proxy_sections = Vec::new();
        append_semantic_proxy_sections(
            storage,
            project_filter,
            session_filter,
            session_filters,
            "corrections",
            limit - sections.len(),
            &mut proxy_sections,
        )?;
        sections.extend(
            proxy_sections
                .into_iter()
                .map(|section| CorrectionSignal { section, stale_claim: None }),
        );
    }

    Ok(sections)
}

fn append_policy_sections(
    storage: &Storage,
    project: Option<&str>,
    correction_signals: &[CorrectionSignal],
    sections: &mut Vec<ContextSection>,
) -> Result<(), StorageError> {
    for policy in crate::memory::policy::read_policy_set(storage, project)? {
        if let Some(section) = section_from_policy(storage, project, &policy, correction_signals)? {
            sections.push(section);
        }
    }
    Ok(())
}

fn signal_from_correction(episode: &StoredEpisode) -> CorrectionSignal {
    let prompt = episode.prompt_text.as_deref();
    let text = episode
        .digest
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| correction_text_from_prompt(prompt))
        .unwrap_or_else(|| "Correction recorded by user.".to_string());

    CorrectionSignal {
        section: ContextSection::typed(
            text,
            vec![evidence_from_episode(episode)],
            "correction",
            "active",
            None,
        ),
        stale_claim: stale_claim_from_prompt(prompt),
    }
}

fn correction_text_from_prompt(prompt: Option<&str>) -> Option<String> {
    let prompt = prompt?;
    let marker = "Correction:";
    let correction = prompt.split_once(marker).map(|(_, tail)| tail).unwrap_or(prompt).trim();
    if correction.is_empty() {
        None
    } else {
        Some(format!("Correction: {}", correction.lines().next().unwrap_or(correction).trim()))
    }
}

fn stale_claim_from_prompt(prompt: Option<&str>) -> Option<String> {
    let prompt = prompt?;
    let rest = prompt.strip_prefix("Claim corrected:\n")?;
    let claim = rest.split_once("\n\nCorrection:").map(|(claim, _)| claim).unwrap_or(rest).trim();
    if claim.is_empty() {
        None
    } else {
        Some(claim.to_string())
    }
}

fn section_from_policy(
    storage: &Storage,
    project: Option<&str>,
    policy: &Policy,
    correction_signals: &[CorrectionSignal],
) -> Result<Option<ContextSection>, StorageError> {
    let mut evidence = Vec::new();
    for id in &policy.evidence_episode_ids {
        if let Some(episode) = storage.get_live_episode(*id)? {
            evidence.push(evidence_from_episode(&episode));
        }
    }
    if evidence.is_empty() {
        return Ok(None);
    }

    let scope = project.map(|p| format!("project {p}")).unwrap_or_else(|| "global".to_string());
    let matching_corrections = matching_policy_corrections(policy, correction_signals);
    let status = if matching_corrections.is_empty() { "active" } else { "corrected" };
    let confidence = if matching_corrections.is_empty() {
        policy.confidence
    } else {
        (policy.confidence * CORRECTED_POLICY_CONFIDENCE_MULTIPLIER).clamp(0.0, 1.0)
    };
    for signal in matching_corrections {
        for ev in &signal.section.evidence {
            if !evidence.contains(ev) {
                evidence.push(ev.clone());
            }
        }
    }
    let correction_note = if status == "corrected" { ", corrected by user" } else { "" };
    Ok(Some(ContextSection::typed(
        format!(
            "{} (confidence {:.0}%, scope: {scope}{correction_note})",
            policy.rule,
            confidence * 100.0
        ),
        evidence,
        "user_policy",
        status,
        Some(confidence),
    )))
}

fn matching_policy_corrections<'a>(
    policy: &Policy,
    correction_signals: &'a [CorrectionSignal],
) -> Vec<&'a CorrectionSignal> {
    correction_signals
        .iter()
        .filter(|signal| {
            signal
                .stale_claim
                .as_deref()
                .is_some_and(|claim| stale_claim_matches_text(claim, &policy.rule))
        })
        .collect()
}

fn live_pair(
    storage: &Storage,
    candidate: &BeliefCandidate,
) -> Result<Option<(StoredEpisode, StoredEpisode)>, StorageError> {
    let Some(episode_a) = storage.get_live_episode(candidate.episode_a_id)? else {
        return Ok(None);
    };
    let Some(episode_b) = storage.get_live_episode(candidate.episode_b_id)? else {
        return Ok(None);
    };
    Ok(Some((episode_a, episode_b)))
}

fn contradiction_matches_stale_claim(
    candidate: &BeliefCandidate,
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
    stale_claims: &[String],
) -> bool {
    if stale_claims.is_empty() {
        return false;
    }

    let haystack = contradiction_match_text(candidate, episode_a, episode_b);
    stale_claims.iter().any(|claim| stale_claim_matches_text(claim, &haystack))
}

fn episode_pair_matches_scope(
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
) -> bool {
    if let Some(project) = project_filter {
        let touches_project = episode_a.project.as_deref() == Some(project)
            || episode_b.project.as_deref() == Some(project);
        if !touches_project {
            return false;
        }
    }
    if let Some(session_id) = session_filter {
        let touches_session = episode_a.session_id.as_deref() == Some(session_id)
            || episode_b.session_id.as_deref() == Some(session_id);
        if !touches_session {
            return false;
        }
    }
    if let Some(session_filters) = session_filters {
        if !session_filters.is_empty() {
            let both_sessions_match =
                session_matches_filter_set(episode_a.session_id.as_deref(), session_filters)
                    && session_matches_filter_set(episode_b.session_id.as_deref(), session_filters);
            if !both_sessions_match {
                return false;
            }
        }
    }
    true
}

fn episode_matches_scope(
    episode: &StoredEpisode,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
) -> bool {
    if let Some(project) = project_filter {
        if episode.project.as_deref() != Some(project) {
            return false;
        }
    }
    if let Some(session_id) = session_filter {
        if episode.session_id.as_deref() != Some(session_id) {
            return false;
        }
    }
    if let Some(session_filters) = session_filters {
        if !session_matches_filter_set(episode.session_id.as_deref(), session_filters) {
            return false;
        }
    }
    true
}

fn session_matches_filter_set(session_id: Option<&str>, session_filters: &[String]) -> bool {
    if session_filters.is_empty() {
        return true;
    }
    let Some(session_id) = session_id else {
        return false;
    };
    session_filters.iter().any(|expected| expected == session_id)
}

fn claim_matches_session_set(
    storage: &Storage,
    claim: &StoredClaimRecord,
    project_filter: Option<&str>,
    session_filters: &[String],
) -> Result<bool, StorageError> {
    if session_filters.is_empty() {
        return Ok(true);
    }
    let Some(task_frame_id) = claim.task_frame_id else {
        return Ok(false);
    };
    let Some(task_frame) = storage.task_frame(task_frame_id)? else {
        return Ok(false);
    };
    if let Some(project) = project_filter {
        if task_frame.project.as_deref() != Some(project) {
            return Ok(false);
        }
    }
    Ok(session_matches_filter_set(task_frame.session_id.as_deref(), session_filters))
}

fn contradiction_match_text(
    candidate: &BeliefCandidate,
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
) -> String {
    [
        candidate.evidence.as_deref(),
        episode_a.prompt_text.as_deref(),
        episode_a.response_text.as_deref(),
        episode_a.command.as_deref(),
        episode_a.digest.as_deref(),
        episode_b.prompt_text.as_deref(),
        episode_b.response_text.as_deref(),
        episode_b.command.as_deref(),
        episode_b.digest.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn anomaly_matches_stale_claim(
    anomaly: &ContextAnomaly,
    episode: &StoredEpisode,
    stale_claims: &[String],
) -> bool {
    if stale_claims.is_empty() {
        return false;
    }

    let haystack = anomaly_match_text(anomaly, episode);
    stale_claims.iter().any(|claim| stale_claim_matches_text(claim, &haystack))
}

fn anomaly_match_text(anomaly: &ContextAnomaly, episode: &StoredEpisode) -> String {
    [
        anomaly.evidence.as_deref(),
        episode.prompt_text.as_deref(),
        episode.response_text.as_deref(),
        episode.command.as_deref(),
        episode.digest.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn section_from_contradiction(
    candidate: &BeliefCandidate,
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
) -> ContextSection {
    let mut text = format!(
        "Open contradiction between local episodes #{} and #{} (score {:.3})",
        candidate.episode_a_id, candidate.episode_b_id, candidate.score
    );
    if let Some(evidence) = candidate.evidence.as_deref().filter(|s| !s.is_empty()) {
        text.push_str(": ");
        text.push_str(evidence);
    }
    text.push('.');

    ContextSection::typed(
        text,
        vec![
            EvidenceRef {
                kind: "belief_candidate".to_string(),
                id: candidate.id.to_string(),
                source: Some(candidate.kind.to_string()),
            },
            evidence_from_episode(episode_a),
            evidence_from_episode(episode_b),
        ],
        "contradiction",
        "open",
        Some(candidate.score),
    )
}

fn section_from_context_anomaly(
    anomaly: &ContextAnomaly,
    episode: &StoredEpisode,
) -> ContextSection {
    let mut text = format!(
        "Open iPC anomaly for local episode #{} (free-energy {:.3})",
        anomaly.episode_id, anomaly.score
    );
    if let Some(evidence) = anomaly.evidence.as_deref().filter(|s| !s.is_empty()) {
        text.push_str(": ");
        text.push_str(evidence);
    }
    text.push('.');

    ContextSection::typed(
        text,
        vec![
            EvidenceRef {
                kind: "context_anomaly".to_string(),
                id: anomaly.id.to_string(),
                source: Some(anomaly.kind.to_string()),
            },
            evidence_from_episode(episode),
        ],
        "anomaly",
        "open",
        Some(anomaly.score.clamp(0.0, 1.0)),
    )
}

fn section_from_short_term_proxy(
    proxy: &EvidenceBackedLatentProxy,
    episode: &StoredEpisode,
) -> ContextSection {
    let mut evidence = vec![EvidenceRef {
        kind: "evidence_latent_proxy".to_string(),
        id: proxy.id.to_string(),
        source: Some(proxy.proxy_type.clone()),
    }];
    for ev in &proxy.evidence_refs {
        evidence.push(EvidenceRef {
            kind: ev.kind.clone(),
            id: ev.id.clone(),
            source: ev.source.clone(),
        });
    }
    let episode_evidence = evidence_from_episode(episode);
    if !evidence.contains(&episode_evidence) {
        evidence.push(episode_evidence);
    }

    let scope = proxy.scope.as_deref().filter(|s| !s.trim().is_empty());
    let target = proxy.target.as_deref().filter(|s| !s.trim().is_empty());
    let mut text = format!("Short-term candidate: {}", proxy.claim.trim());
    if let Some(target) = target {
        text.push_str(&format!(" (target: {target})"));
    }
    if let Some(scope) = scope {
        text.push_str(&format!(" (scope: {scope})"));
    }
    if proxy.source_trust != ClaimSourceType::LocalObserved {
        text.push_str(&format!(" (source trust: {})", proxy.source_trust));
    }
    text.push('.');

    ContextSection::typed(
        text,
        evidence,
        proxy.proxy_type.clone(),
        "short_term_candidate",
        Some(proxy.confidence.clamp(0.0, 1.0)),
    )
}

fn short_term_proxy_can_project(proxy: &EvidenceBackedLatentProxy) -> bool {
    !proxy.privacy_labels.is_empty()
        && proxy.privacy_labels.iter().all(|label| {
            matches!(label, SensitivityLabel::Public | SensitivityLabel::ProjectInternal)
        })
}

fn long_term_proxy_can_project(proxy: &EvidenceBackedLatentProxy) -> bool {
    proxy.source_trust != ClaimSourceType::CloudDraft && short_term_proxy_can_project(proxy)
}

fn item_from_long_term_proxy(
    proxy: &EvidenceBackedLatentProxy,
    episode: &StoredEpisode,
    layer_rank: usize,
) -> ContextItem {
    let mut evidence = vec![EvidenceRef {
        kind: "evidence_latent_proxy".to_string(),
        id: proxy.id.to_string(),
        source: Some(proxy.proxy_type.clone()),
    }];
    for ev in &proxy.evidence_refs {
        push_unique_evidence(&mut evidence, evidence_from_stored_ref(ev));
    }
    push_unique_evidence(&mut evidence, evidence_from_episode(episode));

    let mut reason = "long-term evidence-backed latent proxy".to_string();
    if let Some(promotion_reason) =
        proxy.promotion_reason.as_deref().filter(|reason| !reason.trim().is_empty())
    {
        reason.push_str(": ");
        reason.push_str(promotion_reason.trim());
    }

    ContextItem {
        text: long_term_proxy_text(proxy),
        evidence,
        reason,
        rank: 0,
        layer: "long_term_proxy".to_string(),
        layer_rank,
        similarity: None,
        project: episode.project.clone(),
        session_id: episode.session_id.clone(),
    }
}

fn long_term_proxy_text(proxy: &EvidenceBackedLatentProxy) -> String {
    let target = proxy.target.as_deref().filter(|s| !s.trim().is_empty());
    let scope = proxy.scope.as_deref().filter(|s| !s.trim().is_empty());
    let mut text = format!("Long-term memory: {}", proxy.claim.trim());
    if let Some(target) = target {
        text.push_str(&format!(" (target: {target})"));
    }
    if let Some(scope) = scope {
        text.push_str(&format!(" (scope: {scope})"));
    }
    text.push('.');
    text
}

fn append_semantic_proxy_sections(
    storage: &Storage,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
    session_filters: Option<&[String]>,
    envelope_section: &str,
    limit: usize,
    sections: &mut Vec<ContextSection>,
) -> Result<(), StorageError> {
    if limit == 0 {
        return Ok(());
    }
    let read_limit = limit.saturating_mul(PROJECT_FILTER_SCAN_MULTIPLIER).max(limit);
    let proxies = storage.semantic_proxies_for_envelope_section(envelope_section, read_limit)?;
    let mut added = 0;
    for proxy in proxies {
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            continue;
        };
        if !episode_matches_scope(&episode, project_filter, session_filter, session_filters) {
            continue;
        }
        if !semantic_proxy_can_project_to_section(storage, &proxy, envelope_section)? {
            continue;
        }
        sections.push(section_from_semantic_proxy(&proxy, &episode, envelope_section));
        added += 1;
        if added == limit {
            break;
        }
    }
    Ok(())
}

fn section_from_semantic_proxy(
    proxy: &EvidenceBackedLatentProxy,
    episode: &StoredEpisode,
    envelope_section: &str,
) -> ContextSection {
    let mut evidence = vec![EvidenceRef {
        kind: "evidence_latent_proxy".to_string(),
        id: proxy.id.to_string(),
        source: Some(proxy.proxy_type.clone()),
    }];
    for ev in &proxy.evidence_refs {
        push_unique_evidence(&mut evidence, evidence_from_stored_ref(ev));
    }
    push_unique_evidence(&mut evidence, evidence_from_episode(episode));

    let target = proxy.target.as_deref().filter(|s| !s.trim().is_empty());
    let scope = proxy.scope.as_deref().filter(|s| !s.trim().is_empty());
    let mut text = semantic_proxy_text(envelope_section, proxy.claim.trim());
    if let Some(target) = target {
        text.push_str(&format!(" (target: {target})"));
    }
    if let Some(scope) = scope {
        text.push_str(&format!(" (scope: {scope})"));
    }

    ContextSection::typed(
        text,
        evidence,
        semantic_proxy_kind(envelope_section, &proxy.proxy_type),
        semantic_proxy_status(envelope_section),
        Some(proxy.confidence.clamp(0.0, 1.0)),
    )
}

fn semantic_proxy_text(envelope_section: &str, claim: &str) -> String {
    match envelope_section {
        "stable_facts" => claim.to_string(),
        "user_policy" => format!("Policy: {claim}"),
        "corrections" => format!("Correction: {claim}"),
        "open_decisions" => format!("Open decision: {claim}"),
        _ => claim.to_string(),
    }
}

fn semantic_proxy_kind(envelope_section: &str, proxy_type: &str) -> String {
    match envelope_section {
        "stable_facts" => "stable_fact".to_string(),
        "user_policy" => "user_policy".to_string(),
        "corrections" => "correction".to_string(),
        "open_decisions" => proxy_type.to_string(),
        _ => proxy_type.to_string(),
    }
}

fn semantic_proxy_status(envelope_section: &str) -> &'static str {
    match envelope_section {
        "open_decisions" => "open",
        _ => "active",
    }
}

fn semantic_proxy_can_project(proxy: &EvidenceBackedLatentProxy) -> bool {
    short_term_proxy_can_project(proxy)
}

fn semantic_proxy_can_project_to_section(
    storage: &Storage,
    proxy: &EvidenceBackedLatentProxy,
    envelope_section: &str,
) -> Result<bool, StorageError> {
    if !semantic_proxy_can_project(proxy) {
        return Ok(false);
    }
    if envelope_section == "stable_facts" {
        return semantic_proxy_has_semantic_claim_evidence(storage, proxy);
    }
    Ok(true)
}

fn semantic_proxy_has_semantic_claim_evidence(
    storage: &Storage,
    proxy: &EvidenceBackedLatentProxy,
) -> Result<bool, StorageError> {
    for ev in proxy.evidence_refs.iter().filter(|ev| ev.kind == "claim") {
        let Ok(claim_id) = ev.id.parse::<i64>() else {
            continue;
        };
        let Some(claim) = storage.claim_record(claim_id)? else {
            continue;
        };
        if claim.lifecycle_state == LifecycleState::SemanticFact
            && storage.claim_has_durable_promotion_trust(claim.id)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn section_from_semantic_claim(claim: &StoredClaimRecord) -> ContextSection {
    let mut evidence = vec![EvidenceRef {
        kind: "claim".to_string(),
        id: claim.id.to_string(),
        source: Some("semantic_fact".to_string()),
    }];
    for ev in &claim.evidence_refs {
        push_unique_evidence(&mut evidence, evidence_from_stored_ref(ev));
    }

    ContextSection::typed(
        claim.text.trim().to_string(),
        evidence,
        "stable_fact",
        "active",
        Some(claim.confidence.clamp(0.0, 1.0)),
    )
}

fn evidence_from_stored_ref(ev: &StoredEvidenceRef) -> EvidenceRef {
    EvidenceRef { kind: ev.kind.clone(), id: ev.id.clone(), source: ev.source.clone() }
}

fn push_unique_evidence(evidence: &mut Vec<EvidenceRef>, ev: EvidenceRef) {
    if !evidence.contains(&ev) {
        evidence.push(ev);
    }
}

fn evidence_from_episode(episode: &StoredEpisode) -> EvidenceRef {
    EvidenceRef {
        kind: "episode".to_string(),
        id: episode.id.to_string(),
        source: Some(episode.source.to_string()),
    }
}

fn nonempty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn count_map_summary(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "none".to_string();
    }
    counts.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(", ")
}

fn set_summary(values: &BTreeSet<String>, limit: usize) -> String {
    values.iter().take(limit).cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::beliefs::BeliefKind;
    use crate::storage::{
        Episode, EpisodeSource, EvidenceBackedLatentProxyDraft, SensitivityLabel,
    };

    fn append_terminal(
        storage: &mut Storage,
        project: &str,
        command: &str,
        exit_code: i32,
        ts: i64,
    ) -> i64 {
        storage
            .append_episode(&Episode {
                ts_start_ns: ts,
                ts_end_ns: ts,
                duration_ms: 0,
                source: EpisodeSource::Terminal,
                session_id: Some("ctx-quality".to_string()),
                prompt_text: None,
                response_text: None,
                command: Some(command.to_string()),
                stdout: None,
                exit_code: Some(exit_code),
                cwd: None,
                git_branch: None,
                project: Some(project.to_string()),
                digest: None,
            })
            .expect("append episode")
    }

    #[test]
    fn contradiction_candidates_become_open_decisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let a = append_terminal(&mut storage, "myapp", "cargo test", 0, 1);
        let b = append_terminal(&mut storage, "myapp", "cargo test", 1, 2);
        let row_id = storage
            .insert_belief_candidate(a, b, BeliefKind::Contradicts, 0.91, Some("command flapping"))
            .expect("insert contradiction")
            .expect("new row");

        let sections =
            open_decisions_from_storage(&storage, Some("myapp"), DEFAULT_OPEN_DECISION_LIMIT)
                .expect("open decisions");

        assert_eq!(sections.len(), 1);
        assert!(sections[0].text.contains("Open contradiction"));
        assert!(sections[0].text.contains("command flapping"));
        assert_eq!(sections[0].kind.as_deref(), Some("contradiction"));
        assert_eq!(sections[0].status.as_deref(), Some("open"));
        assert_eq!(sections[0].confidence, Some(0.91));
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "belief_candidate" && ev.id == row_id.to_string()));
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == a.to_string()));
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == b.to_string()));
    }

    #[test]
    fn context_anomalies_become_open_decisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let episode_id = append_terminal(&mut storage, "myapp", "cargo check", 0, 1);
        let anomaly_id = storage
            .upsert_context_anomaly(
                episode_id,
                IPC_FREE_ENERGY_ANOMALY_KIND,
                0.92,
                Some("iPC pc_free_energy exceeded threshold"),
            )
            .expect("insert anomaly");

        let sections =
            open_decisions_from_storage(&storage, Some("myapp"), DEFAULT_OPEN_DECISION_LIMIT)
                .expect("open decisions");

        assert_eq!(sections.len(), 1);
        assert!(sections[0].text.contains("Open iPC anomaly"));
        assert!(sections[0].text.contains("pc_free_energy"));
        assert_eq!(sections[0].kind.as_deref(), Some("anomaly"));
        assert_eq!(sections[0].status.as_deref(), Some("open"));
        assert_eq!(sections[0].confidence, Some(0.92));
        assert!(sections[0].evidence.iter().any(|ev| {
            ev.kind == "context_anomaly"
                && ev.id == anomaly_id.to_string()
                && ev.source.as_deref() == Some(IPC_FREE_ENERGY_ANOMALY_KIND)
        }));
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == episode_id.to_string()));
    }

    #[test]
    fn evidence_latent_proxies_become_short_term_candidates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let episode_id = append_terminal(&mut storage, "myapp", "cargo check", 0, 1);
        let mut draft = EvidenceBackedLatentProxyDraft::short_term(
            episode_id,
            "task_candidate",
            "TaskFrame should drive ContextEnvelope projection",
        );
        draft.target = Some("TaskFrame".to_string());
        draft.scope = Some("SOMA control plane".to_string());
        draft.confidence = 0.84;
        let proxy_id = storage.insert_evidence_latent_proxy(&draft).expect("insert proxy");

        let sections = short_term_candidates_from_storage(
            &storage,
            Some("myapp"),
            Some("ctx-quality"),
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )
        .expect("short-term candidates");

        assert_eq!(sections.len(), 1);
        assert!(sections[0].text.contains("Short-term candidate"));
        assert!(sections[0].text.contains("TaskFrame should drive"));
        assert!(sections[0].text.contains("target: TaskFrame"));
        assert_eq!(sections[0].kind.as_deref(), Some("task_candidate"));
        assert_eq!(sections[0].status.as_deref(), Some("short_term_candidate"));
        assert_eq!(sections[0].confidence, Some(0.84));
        assert!(sections[0].evidence.iter().any(|ev| {
            ev.kind == "evidence_latent_proxy"
                && ev.id == proxy_id.to_string()
                && ev.source.as_deref() == Some("task_candidate")
        }));
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == episode_id.to_string()));
    }

    #[test]
    fn short_term_candidate_projection_respects_project_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let episode_id = append_terminal(&mut storage, "other", "cargo check", 0, 1);
        let draft = EvidenceBackedLatentProxyDraft::short_term(
            episode_id,
            "task_candidate",
            "off-project candidate",
        );
        storage.insert_evidence_latent_proxy(&draft).expect("insert proxy");

        let sections = short_term_candidates_from_storage(
            &storage,
            Some("myapp"),
            Some("ctx-quality"),
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )
        .expect("short-term candidates");

        assert!(sections.is_empty(), "off-project candidate leaked: {sections:?}");
    }

    #[test]
    fn short_term_candidate_projection_blocks_unsafe_privacy_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let episode_id = append_terminal(&mut storage, "myapp", "cargo check", 0, 1);
        let mut draft = EvidenceBackedLatentProxyDraft::short_term(
            episode_id,
            "task_candidate",
            "private candidate must not project to cloud context",
        );
        draft.privacy_labels = vec![SensitivityLabel::LocalPrivate];
        storage.insert_evidence_latent_proxy(&draft).expect("insert proxy");

        let sections = short_term_candidates_from_storage(
            &storage,
            Some("myapp"),
            Some("ctx-quality"),
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )
        .expect("short-term candidates");

        assert!(sections.is_empty(), "unsafe candidate leaked: {sections:?}");
    }

    #[test]
    fn project_experience_summarizes_scoped_project_provenance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let old_id = append_terminal(&mut storage, "myapp", "cargo check", 0, 1);
        let recent_id = append_terminal(&mut storage, "myapp", "cargo test", 0, 2);
        append_terminal(&mut storage, "other", "npm test", 0, 3);

        let sections =
            project_experience_from_storage(&storage, Some("myapp"), None, 1).expect("project");

        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].kind.as_deref(), Some("project_experience"));
        assert_eq!(sections[0].status.as_deref(), Some("scoped_recent"));
        assert!(sections[0].text.contains("project `myapp` has 2 recent local episode(s)"));
        assert!(sections[0].text.contains("sources: terminal=2"));
        assert!(sections[0].text.contains("memory tiers: short=2"));
        assert!(sections[0].text.contains("recent sessions: ctx-quality"));
        assert_eq!(sections[0].evidence.len(), 1);
        assert!(sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == recent_id.to_string()));
        assert!(!sections[0]
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == old_id.to_string()));
    }

    #[test]
    fn project_experience_requires_project_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        append_terminal(&mut storage, "myapp", "cargo check", 0, 1);

        let sections = project_experience_from_storage(&storage, None, None, 3).expect("project");

        assert!(
            sections.is_empty(),
            "global project list must not leak into cloud-facing context: {sections:?}"
        );
    }

    #[test]
    fn project_filter_excludes_unrelated_contradictions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let a = append_terminal(&mut storage, "other", "cargo test", 0, 1);
        let b = append_terminal(&mut storage, "other", "cargo test", 1, 2);
        storage
            .insert_belief_candidate(a, b, BeliefKind::Contradicts, 0.91, Some("command flapping"))
            .expect("insert contradiction");

        let sections =
            open_decisions_from_storage(&storage, Some("myapp"), DEFAULT_OPEN_DECISION_LIMIT)
                .expect("open decisions");

        assert!(sections.is_empty(), "off-project contradiction leaked: {sections:?}");
    }

    #[test]
    fn project_filter_excludes_unrelated_context_anomalies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let episode_id = append_terminal(&mut storage, "other", "cargo check", 0, 1);
        storage
            .upsert_context_anomaly(
                episode_id,
                IPC_FREE_ENERGY_ANOMALY_KIND,
                0.92,
                Some("off-project iPC anomaly"),
            )
            .expect("insert anomaly");

        let sections =
            open_decisions_from_storage(&storage, Some("myapp"), DEFAULT_OPEN_DECISION_LIMIT)
                .expect("open decisions");

        assert!(sections.is_empty(), "off-project anomaly leaked: {sections:?}");
    }

    #[test]
    fn correction_stale_claim_suppresses_matching_open_decisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let a = append_terminal(&mut storage, "myapp", "cargo test", 0, 1);
        let b = append_terminal(&mut storage, "myapp", "cargo test", 1, 2);
        storage
            .insert_belief_candidate(a, b, BeliefKind::Contradicts, 0.91, Some("command flapping"))
            .expect("insert contradiction");

        let correction_stale_claims = vec!["cargo test".to_string()];
        let sections = open_decisions_from_storage_with_corrections(
            &storage,
            Some("myapp"),
            DEFAULT_OPEN_DECISION_LIMIT,
            &correction_stale_claims,
        )
        .expect("open decisions");

        assert!(sections.is_empty(), "corrected contradiction leaked: {sections:?}");
    }

    #[test]
    fn policy_rows_become_user_policy_sections() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let global_id = append_terminal(&mut storage, "myapp", "cargo test", 0, 1);
        let project_id = append_terminal(&mut storage, "myapp", "cargo fmt", 0, 2);
        crate::memory::policy::upsert_policy_set(
            &mut storage,
            None,
            &[Policy {
                rule: "Prefer concise Korean status updates.".to_string(),
                evidence_episode_ids: vec![global_id],
                confidence: 0.82,
            }],
        )
        .expect("upsert global policy");
        crate::memory::policy::upsert_policy_set(
            &mut storage,
            Some("myapp"),
            &[Policy {
                rule: "Run cargo fmt before review.".to_string(),
                evidence_episode_ids: vec![project_id],
                confidence: 0.91,
            }],
        )
        .expect("upsert project policy");

        let sections = user_policy_from_storage(&storage, Some("myapp")).expect("user policy");

        assert_eq!(sections.len(), 2);
        assert!(sections.iter().any(|section| section.text.contains("concise Korean")));
        assert!(sections.iter().any(|section| section.text.contains("cargo fmt")));
        assert!(sections.iter().all(|section| section.kind.as_deref() == Some("user_policy")));
        assert!(sections.iter().all(|section| section.status.as_deref() == Some("active")));
        assert!(sections.iter().any(|section| section.confidence == Some(0.82)));
        assert!(sections.iter().any(|section| section.confidence == Some(0.91)));
        assert!(sections
            .iter()
            .flat_map(|section| &section.evidence)
            .any(|ev| ev.kind == "episode" && ev.id == global_id.to_string()));
        assert!(sections
            .iter()
            .flat_map(|section| &section.evidence)
            .any(|ev| ev.kind == "episode" && ev.id == project_id.to_string()));
    }

    #[test]
    fn correction_stale_claim_decays_matching_policy_confidence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let policy_id = append_terminal(&mut storage, "myapp", "voice is core", 0, 1);
        let other_policy_id = append_terminal(&mut storage, "myapp", "cargo fmt", 0, 2);
        crate::memory::policy::upsert_policy_set(
            &mut storage,
            Some("myapp"),
            &[
                Policy {
                    rule: "Voice is core product.".to_string(),
                    evidence_episode_ids: vec![policy_id],
                    confidence: 0.88,
                },
                Policy {
                    rule: "Run cargo fmt before review.".to_string(),
                    evidence_episode_ids: vec![other_policy_id],
                    confidence: 0.80,
                },
            ],
        )
        .expect("upsert project policy");
        let correction_id = storage
            .append_episode(&Episode {
                ts_start_ns: 3,
                ts_end_ns: 3,
                duration_ms: 0,
                source: EpisodeSource::Other(CORRECTION_SOURCE.to_string()),
                session_id: Some("ctx-quality".to_string()),
                prompt_text: Some(
                    "Claim corrected:\nvoice is core\n\nCorrection:\nContextEnvelope is core."
                        .to_string(),
                ),
                response_text: None,
                command: None,
                stdout: None,
                exit_code: None,
                cwd: None,
                git_branch: None,
                project: Some("myapp".to_string()),
                digest: Some("Correction for voice is core: ContextEnvelope is core.".to_string()),
            })
            .expect("append correction");
        let signals =
            correction_signals_from_storage(&storage, Some("myapp"), DEFAULT_CORRECTION_LIMIT)
                .expect("corrections");

        let sections = user_policy_from_storage_with_corrections(&storage, Some("myapp"), &signals)
            .expect("user policy");

        let corrected = sections
            .iter()
            .find(|section| section.text.contains("Voice is core"))
            .expect("corrected policy");
        assert_eq!(corrected.status.as_deref(), Some("corrected"));
        let corrected_confidence = corrected.confidence.expect("corrected confidence");
        assert!((corrected_confidence - 0.22).abs() < 0.001, "{corrected_confidence}");
        assert!(corrected.text.contains("corrected by user"));
        assert!(corrected
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == policy_id.to_string()));
        assert!(corrected.evidence.iter().any(|ev| {
            ev.kind == "episode"
                && ev.id == correction_id.to_string()
                && ev.source.as_deref() == Some(CORRECTION_SOURCE)
        }));

        let active = sections
            .iter()
            .find(|section| section.text.contains("cargo fmt"))
            .expect("active policy");
        assert_eq!(active.status.as_deref(), Some("active"));
        let active_confidence = active.confidence.expect("active confidence");
        assert!((active_confidence - 0.80).abs() < 0.001, "{active_confidence}");
    }

    #[test]
    fn correction_episodes_become_correction_sections() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let correction_id = storage
            .append_episode(&Episode {
                ts_start_ns: 3,
                ts_end_ns: 3,
                duration_ms: 0,
                source: EpisodeSource::Other(CORRECTION_SOURCE.to_string()),
                session_id: Some("ctx-quality".to_string()),
                prompt_text: Some(
                    "Claim corrected:\nvoice is core\n\nCorrection:\nContextEnvelope is core."
                        .to_string(),
                ),
                response_text: None,
                command: None,
                stdout: None,
                exit_code: None,
                cwd: None,
                git_branch: None,
                project: Some("myapp".to_string()),
                digest: Some("Correction for voice is core: ContextEnvelope is core.".to_string()),
            })
            .expect("append correction");
        storage
            .append_episode(&Episode {
                ts_start_ns: 4,
                ts_end_ns: 4,
                duration_ms: 0,
                source: EpisodeSource::ClaudeCode,
                session_id: Some("ctx-quality".to_string()),
                prompt_text: Some("ordinary episode".to_string()),
                response_text: None,
                command: None,
                stdout: None,
                exit_code: None,
                cwd: None,
                git_branch: None,
                project: Some("myapp".to_string()),
                digest: None,
            })
            .expect("append ordinary");

        let signals =
            correction_signals_from_storage(&storage, Some("myapp"), DEFAULT_CORRECTION_LIMIT)
                .expect("corrections");

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].stale_claim.as_deref(), Some("voice is core"));
        assert!(signals[0].section.text.contains("ContextEnvelope is core"));
        assert_eq!(signals[0].section.kind.as_deref(), Some("correction"));
        assert_eq!(signals[0].section.status.as_deref(), Some("active"));
        assert_eq!(signals[0].section.confidence, None);
        assert!(signals[0]
            .section
            .evidence
            .iter()
            .any(|ev| ev.kind == "episode" && ev.id == correction_id.to_string()));
    }

    #[test]
    fn policy_without_live_evidence_is_not_surfaced() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        crate::memory::policy::upsert_policy_set(
            &mut storage,
            None,
            &[Policy {
                rule: "No evidence means no envelope claim.".to_string(),
                evidence_episode_ids: vec![999],
                confidence: 0.82,
            }],
        )
        .expect("upsert global policy");

        let sections = user_policy_from_storage(&storage, None).expect("user policy");

        assert!(sections.is_empty(), "uncited policy leaked: {sections:?}");
    }
}
