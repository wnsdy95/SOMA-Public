//! Read-only thread identity preflight.
//!
//! SOMA can safely expose exact session-scoped context today. Cross-client
//! thread scope needs an explicit proof step: persisted episodes must first
//! show stable joins across session ids without ambiguous merges. This module
//! builds that proof report without creating a durable thread id or changing
//! recall; confirmed thread resources are enabled only by the separate
//! operator-confirmed identity ledger.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::storage::StoredEpisode;

pub const DEFAULT_THREAD_IDENTITY_LIMIT: usize = 200;
pub const DEFAULT_THREAD_JOIN_WINDOW_MINUTES: i64 = 120;

#[derive(Debug, Clone)]
pub struct ThreadIdentityReportInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub join_window_minutes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadIdentityReport {
    pub kind: &'static str,
    pub status: &'static str,
    pub scope: ThreadIdentityScope,
    pub inspected_episode_count: usize,
    pub candidate_threads: Vec<ThreadIdentityCandidate>,
    pub ambiguities: Vec<ThreadIdentityAmbiguity>,
    pub trust_boundary: ThreadIdentityTrustBoundary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadIdentityScope {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub join_window_minutes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadIdentityCandidate {
    pub candidate_thread_id: String,
    pub identity_basis: &'static str,
    pub thread_resource_state: &'static str,
    pub session_id: String,
    pub episode_ids: Vec<i64>,
    pub evidence: Vec<String>,
    pub source_values: Vec<String>,
    pub project_values: Vec<String>,
    pub cwd_values: Vec<String>,
    pub git_branch_values: Vec<String>,
    pub time_range_ns: ThreadIdentityTimeRange,
    pub max_gap_minutes: Option<i64>,
    pub eligibility: ThreadIdentityEligibility,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadIdentityTimeRange {
    pub start_ns: i64,
    pub end_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ThreadIdentityEligibility {
    pub stable_session_id: bool,
    pub single_project: bool,
    pub no_large_time_gap: bool,
    pub single_cwd: bool,
    pub single_git_branch: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadIdentityAmbiguity {
    pub reason: &'static str,
    pub severity: &'static str,
    pub affected_sessions: Vec<String>,
    pub episode_ids: Vec<i64>,
    pub detail: String,
    pub required_resolution: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ThreadIdentityTrustBoundary {
    pub read_only_preflight: bool,
    pub persistent_thread_ids_created: bool,
    pub context_thread_resource_enabled: bool,
    pub automatic_cross_session_merge_allowed: bool,
    pub promotion_or_claim_verification_allowed: bool,
    pub note: &'static str,
}

pub fn build_thread_identity_report(
    episodes: &[StoredEpisode],
    input: ThreadIdentityReportInput,
) -> ThreadIdentityReport {
    let join_window_minutes = input.join_window_minutes.max(1);
    let join_window_ns = join_window_minutes.saturating_mul(60).saturating_mul(1_000_000_000);
    let mut filtered: Vec<&StoredEpisode> = episodes
        .iter()
        .filter(|episode| {
            input
                .project
                .as_deref()
                .is_none_or(|project| episode.project.as_deref() == Some(project))
                && input
                    .session_id
                    .as_deref()
                    .is_none_or(|session_id| episode.session_id.as_deref() == Some(session_id))
        })
        .collect();
    filtered.sort_by_key(|episode| (episode.ts_start_ns, episode.id));

    let mut by_session: BTreeMap<String, Vec<&StoredEpisode>> = BTreeMap::new();
    let mut missing_session_episode_ids = Vec::new();
    for episode in &filtered {
        match &episode.session_id {
            Some(session_id) if !session_id.trim().is_empty() => {
                by_session.entry(session_id.clone()).or_default().push(*episode);
            }
            _ => missing_session_episode_ids.push(episode.id),
        }
    }

    let mut candidates = Vec::new();
    let mut ambiguities = Vec::new();
    if !missing_session_episode_ids.is_empty() {
        ambiguities.push(ThreadIdentityAmbiguity {
            reason: "missing_session_id",
            severity: "blocking",
            affected_sessions: Vec::new(),
            episode_ids: missing_session_episode_ids,
            detail: "Episodes without session_id cannot be mapped into a stable thread identity."
                .to_string(),
            required_resolution: "capture_adapter_must_supply_session_id_or_operator_maps_episode",
        });
    }

    for (session_id, session_episodes) in &by_session {
        let candidate = build_candidate(session_id, session_episodes, join_window_ns);
        append_session_ambiguities(&candidate, &mut ambiguities);
        candidates.push(candidate);
    }
    append_cross_session_join_ambiguities(&candidates, join_window_ns, &mut ambiguities);

    ThreadIdentityReport {
        kind: "context_thread_identity_preflight",
        status: "read_only_preflight",
        scope: ThreadIdentityScope {
            project: input.project,
            session_id: input.session_id,
            limit: input.limit,
            join_window_minutes,
        },
        inspected_episode_count: filtered.len(),
        candidate_threads: candidates,
        ambiguities,
        trust_boundary: ThreadIdentityTrustBoundary {
            read_only_preflight: true,
            persistent_thread_ids_created: false,
            context_thread_resource_enabled: false,
            automatic_cross_session_merge_allowed: false,
            promotion_or_claim_verification_allowed: false,
            note: "This preflight does not enable soma://context/thread/<thread_key>; it only exposes evidence needed before an operator can confirm a thread identity.",
        },
    }
}

fn build_candidate(
    session_id: &str,
    episodes: &[&StoredEpisode],
    join_window_ns: i64,
) -> ThreadIdentityCandidate {
    let first = episodes.first().expect("session candidate requires an episode");
    let last = episodes.last().expect("session candidate requires an episode");
    let project_values =
        sorted_nonempty_values(episodes.iter().filter_map(|e| e.project.as_deref()));
    let cwd_values = sorted_nonempty_values(episodes.iter().filter_map(|e| e.cwd.as_deref()));
    let git_branch_values =
        sorted_nonempty_values(episodes.iter().filter_map(|e| e.git_branch.as_deref()));
    let source_values =
        sorted_nonempty_values(episodes.iter().map(|episode| episode.source.to_string()));
    let max_gap_ns = episodes
        .windows(2)
        .map(|pair| pair[1].ts_start_ns.saturating_sub(pair[0].ts_end_ns))
        .max()
        .filter(|gap| *gap > 0);
    let max_gap_minutes = max_gap_ns.map(|gap| gap / 60 / 1_000_000_000);
    let no_large_time_gap = max_gap_ns.is_none_or(|gap| gap <= join_window_ns);
    let project_key = project_values.first().map(String::as_str).unwrap_or("global");
    let candidate_thread_id = format!(
        "thread-candidate:{}:{}:{}-{}",
        sanitize_key(project_key),
        sanitize_key(session_id),
        first.id,
        last.id
    );

    ThreadIdentityCandidate {
        candidate_thread_id,
        identity_basis: "explicit_session_id",
        thread_resource_state: "requires_operator_confirmation",
        session_id: session_id.to_string(),
        episode_ids: episodes.iter().map(|episode| episode.id).collect(),
        evidence: episodes.iter().map(|episode| format!("episode:{}", episode.id)).collect(),
        source_values,
        project_values: project_values.clone(),
        cwd_values: cwd_values.clone(),
        git_branch_values: git_branch_values.clone(),
        time_range_ns: ThreadIdentityTimeRange {
            start_ns: first.ts_start_ns,
            end_ns: last.ts_end_ns,
        },
        max_gap_minutes,
        eligibility: ThreadIdentityEligibility {
            stable_session_id: true,
            single_project: project_values.len() <= 1,
            no_large_time_gap,
            single_cwd: cwd_values.len() <= 1,
            single_git_branch: git_branch_values.len() <= 1,
        },
    }
}

fn append_session_ambiguities(
    candidate: &ThreadIdentityCandidate,
    ambiguities: &mut Vec<ThreadIdentityAmbiguity>,
) {
    if !candidate.eligibility.single_project {
        ambiguities.push(ThreadIdentityAmbiguity {
            reason: "session_crosses_projects",
            severity: "blocking",
            affected_sessions: vec![candidate.session_id.clone()],
            episode_ids: candidate.episode_ids.clone(),
            detail: format!(
                "Session `{}` contains multiple project values: {}.",
                candidate.session_id,
                candidate.project_values.join(", ")
            ),
            required_resolution: "operator_confirms_project_boundary_or_capture_splits_session",
        });
    }
    if !candidate.eligibility.no_large_time_gap {
        ambiguities.push(ThreadIdentityAmbiguity {
            reason: "session_has_large_time_gap",
            severity: "review",
            affected_sessions: vec![candidate.session_id.clone()],
            episode_ids: candidate.episode_ids.clone(),
            detail: format!(
                "Session `{}` has max gap of {} minutes.",
                candidate.session_id,
                candidate.max_gap_minutes.unwrap_or_default()
            ),
            required_resolution: "operator_confirms_continuity_or_session_is_split",
        });
    }
    if !candidate.eligibility.single_cwd {
        ambiguities.push(ThreadIdentityAmbiguity {
            reason: "session_crosses_cwd",
            severity: "review",
            affected_sessions: vec![candidate.session_id.clone()],
            episode_ids: candidate.episode_ids.clone(),
            detail: format!(
                "Session `{}` contains multiple cwd values: {}.",
                candidate.session_id,
                candidate.cwd_values.join(", ")
            ),
            required_resolution: "operator_confirms_workspace_boundary",
        });
    }
    if !candidate.eligibility.single_git_branch {
        ambiguities.push(ThreadIdentityAmbiguity {
            reason: "session_crosses_git_branch",
            severity: "review",
            affected_sessions: vec![candidate.session_id.clone()],
            episode_ids: candidate.episode_ids.clone(),
            detail: format!(
                "Session `{}` contains multiple git branches: {}.",
                candidate.session_id,
                candidate.git_branch_values.join(", ")
            ),
            required_resolution: "operator_confirms_branch_boundary",
        });
    }
}

fn append_cross_session_join_ambiguities(
    candidates: &[ThreadIdentityCandidate],
    join_window_ns: i64,
    ambiguities: &mut Vec<ThreadIdentityAmbiguity>,
) {
    for (left_index, left) in candidates.iter().enumerate() {
        for right in candidates.iter().skip(left_index + 1) {
            if left.project_values.len() != 1
                || right.project_values.len() != 1
                || left.project_values != right.project_values
            {
                continue;
            }
            let gap = range_gap_ns(&left.time_range_ns, &right.time_range_ns);
            if gap <= join_window_ns {
                let mut episode_ids = left.episode_ids.clone();
                episode_ids.extend(right.episode_ids.iter().copied());
                episode_ids.sort_unstable();
                ambiguities.push(ThreadIdentityAmbiguity {
                    reason: "cross_session_join_requires_operator_verification",
                    severity: "review",
                    affected_sessions: vec![left.session_id.clone(), right.session_id.clone()],
                    episode_ids,
                    detail: format!(
                        "Sessions `{}` and `{}` share project `{}` and are within the join window.",
                        left.session_id, right.session_id, left.project_values[0]
                    ),
                    required_resolution:
                        "operator_confirms_same_thread_before_persistent_thread_id",
                });
            }
        }
    }
}

fn range_gap_ns(left: &ThreadIdentityTimeRange, right: &ThreadIdentityTimeRange) -> i64 {
    if left.end_ns < right.start_ns {
        right.start_ns.saturating_sub(left.end_ns)
    } else if right.end_ns < left.start_ns {
        left.start_ns.saturating_sub(right.end_ns)
    } else {
        0
    }
}

fn sorted_nonempty_values<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let set: BTreeSet<String> = values
        .into_iter()
        .map(|value| value.as_ref().trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    set.into_iter().collect()
}

fn sanitize_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}
