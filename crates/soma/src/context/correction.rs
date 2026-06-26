//! User correction capture for ContextEnvelope quality.
//!
//! Corrections are not a separate product surface or learning stage. They are
//! local evidence rows that let a cloud LLM see when the user's current truth
//! overrides stale local memory.

use thiserror::Error;

use crate::context::matching::stale_claim_matches_text;
use crate::memory::beliefs::BeliefCandidate;
use crate::storage::{
    Episode, EpisodeId, EpisodeSource, Storage, StorageError, StoredEvidenceRef,
    VerificationEventDraft, VerificationResult, VerifierType,
};

pub const CORRECTION_SOURCE: &str = "correction";
const CORRECTION_RESOLVE_SCAN_LIMIT: usize = 5_000;
const CORRECTION_CLAIM_SCAN_LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionInput {
    pub claim: Option<String>,
    pub correction: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionRecordReport {
    pub episode_id: EpisodeId,
    pub corrected_claim_ids: Vec<i64>,
    pub resolved_contradiction_count: usize,
}

#[derive(Debug, Error)]
pub enum CorrectionError {
    #[error("correction text is empty")]
    EmptyCorrection,
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

pub fn record_correction(
    storage: &mut Storage,
    input: CorrectionInput,
) -> Result<EpisodeId, CorrectionError> {
    Ok(record_correction_with_report(storage, input)?.episode_id)
}

pub fn record_correction_with_report(
    storage: &mut Storage,
    input: CorrectionInput,
) -> Result<CorrectionRecordReport, CorrectionError> {
    let correction = input.correction.trim().to_string();
    if correction.is_empty() {
        return Err(CorrectionError::EmptyCorrection);
    }

    let claim = input.claim.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let project = input.project;
    let body = correction_body(claim.as_deref(), &correction);
    let digest = correction_digest(claim.as_deref(), &correction);
    let now = now_ns();
    let episode = Episode {
        ts_start_ns: now,
        ts_end_ns: now,
        duration_ms: 0,
        source: EpisodeSource::Other(CORRECTION_SOURCE.to_string()),
        session_id: input.session_id.clone(),
        prompt_text: Some(body),
        response_text: None,
        command: None,
        stdout: None,
        exit_code: None,
        cwd: None,
        git_branch: None,
        project: project.clone(),
        digest: Some(digest),
    };

    let correction_episode_id = storage.append_episode(&episode)?;
    let mut corrected_claim_ids = Vec::new();
    let mut resolved_contradiction_count = 0;
    if let Some(claim) = claim.as_deref() {
        corrected_claim_ids = correct_matching_claim_records(
            storage,
            correction_episode_id,
            claim,
            project.as_deref(),
        )?;
        resolved_contradiction_count = resolve_matching_contradictions(
            storage,
            correction_episode_id,
            claim,
            project.as_deref(),
        )?;
    }
    Ok(CorrectionRecordReport {
        episode_id: correction_episode_id,
        corrected_claim_ids,
        resolved_contradiction_count,
    })
}

fn correction_body(claim: Option<&str>, correction: &str) -> String {
    match claim {
        Some(claim) => format!("Claim corrected:\n{claim}\n\nCorrection:\n{correction}"),
        None => format!("Correction:\n{correction}"),
    }
}

fn correction_digest(claim: Option<&str>, correction: &str) -> String {
    let mut digest = String::from("Correction");
    if let Some(claim) = claim {
        digest.push_str(" for ");
        digest.push_str(&one_line(claim));
    }
    digest.push_str(": ");
    digest.push_str(&one_line(correction));
    digest
}

fn one_line(s: &str) -> String {
    const MAX_CHARS: usize = 160;
    let compact = s.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(MAX_CHARS).collect()
}

fn correct_matching_claim_records(
    storage: &mut Storage,
    correction_episode_id: EpisodeId,
    stale_claim: &str,
    project_filter: Option<&str>,
) -> Result<Vec<i64>, StorageError> {
    let candidates =
        storage.active_claim_records_scoped(project_filter, None, CORRECTION_CLAIM_SCAN_LIMIT)?;
    let mut corrected_claim_ids = Vec::new();
    for claim in candidates {
        if !stale_claim_matches_text(stale_claim, &claim.text) {
            continue;
        }
        storage.insert_verification_event(&VerificationEventDraft {
            claim_id: claim.id,
            verifier_type: VerifierType::Correction,
            result: VerificationResult::Superseded,
            evidence_ref: StoredEvidenceRef {
                kind: "episode".to_string(),
                id: correction_episode_id.to_string(),
                source: Some(CORRECTION_SOURCE.to_string()),
            },
        })?;
        corrected_claim_ids.push(claim.id);
    }
    corrected_claim_ids.sort_unstable();
    corrected_claim_ids.dedup();
    Ok(corrected_claim_ids)
}

fn resolve_matching_contradictions(
    storage: &mut Storage,
    correction_episode_id: EpisodeId,
    stale_claim: &str,
    project_filter: Option<&str>,
) -> Result<usize, StorageError> {
    let candidates = storage.recent_contradictions(CORRECTION_RESOLVE_SCAN_LIMIT)?;
    let mut resolved = 0;
    for candidate in candidates {
        let Some(episode_a) = storage.get_live_episode(candidate.episode_a_id)? else {
            continue;
        };
        let Some(episode_b) = storage.get_live_episode(candidate.episode_b_id)? else {
            continue;
        };
        if let Some(project) = project_filter {
            let touches_project = episode_a.project.as_deref() == Some(project)
                || episode_b.project.as_deref() == Some(project);
            if !touches_project {
                continue;
            }
        }
        let haystack = contradiction_match_text(&candidate, &episode_a, &episode_b);
        if stale_claim_matches_text(stale_claim, &haystack)
            && storage
                .resolve_belief_candidate_with_correction(candidate.id, correction_episode_id)?
        {
            resolved += 1;
        }
    }
    Ok(resolved)
}

fn contradiction_match_text(
    candidate: &BeliefCandidate,
    episode_a: &crate::storage::StoredEpisode,
    episode_b: &crate::storage::StoredEpisode,
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

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::beliefs::BeliefKind;

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
                session_id: Some("correction-test".to_string()),
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
            .expect("append terminal")
    }

    #[test]
    fn records_correction_as_episode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");

        let id = record_correction(
            &mut storage,
            CorrectionInput {
                claim: Some("SOMA voice is core".to_string()),
                correction: "Voice is optional; ContextEnvelope is core.".to_string(),
                project: Some("SOMA".to_string()),
                session_id: Some("manual-correction".to_string()),
            },
        )
        .expect("record correction");

        let episode = storage.get_live_episode(id).expect("get").expect("episode");
        assert_eq!(episode.source.to_string(), CORRECTION_SOURCE);
        assert_eq!(episode.project.as_deref(), Some("SOMA"));
        assert!(episode.prompt_text.unwrap().contains("Voice is optional"));
        assert!(episode.digest.unwrap().contains("Correction for SOMA voice is core"));
    }

    #[test]
    fn rejects_empty_correction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");

        let err = record_correction(
            &mut storage,
            CorrectionInput {
                claim: None,
                correction: "  ".to_string(),
                project: None,
                session_id: None,
            },
        )
        .expect_err("empty correction rejected");

        assert!(matches!(err, CorrectionError::EmptyCorrection));
    }

    #[test]
    fn correction_resolves_matching_contradiction_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let a = append_terminal(&mut storage, "SOMA", "cargo test", 0, 1);
        let b = append_terminal(&mut storage, "SOMA", "cargo test", 1, 2);
        let belief_id = storage
            .insert_belief_candidate(a, b, BeliefKind::Contradicts, 0.91, Some("command flapping"))
            .expect("insert contradiction")
            .expect("new row");

        let correction_id = record_correction(
            &mut storage,
            CorrectionInput {
                claim: Some("cargo test".to_string()),
                correction: "The cargo test contradiction has been resolved.".to_string(),
                project: Some("SOMA".to_string()),
                session_id: Some("manual-correction".to_string()),
            },
        )
        .expect("record correction");

        assert!(storage.recent_contradictions(10).expect("recent").is_empty());
        let row = storage.get_belief_candidate(belief_id).expect("get").expect("row");
        assert_eq!(row.resolved_by_correction_episode_id, Some(correction_id));
        assert!(row.resolved_at_ns.is_some());
    }

    #[test]
    fn correction_resolves_matching_contradiction_beyond_recent_display_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("soma.db");
        let mut storage = Storage::open(&path).expect("open");
        let target_a = append_terminal(
            &mut storage,
            "unknown-project",
            "huggingface-cli download model",
            1,
            1,
        );
        let target_b = append_terminal(
            &mut storage,
            "unknown-project",
            "huggingface-cli download model",
            0,
            2,
        );
        let target_belief_id = storage
            .insert_belief_candidate(
                target_a,
                target_b,
                BeliefKind::Contradicts,
                0.99,
                Some("command flapping"),
            )
            .expect("insert target contradiction")
            .expect("new target row");

        std::thread::sleep(std::time::Duration::from_millis(1));
        for idx in 0..64 {
            let command = format!("low-value command {idx}");
            let a = append_terminal(&mut storage, "unknown-project", &command, 1, 10 + idx);
            let b = append_terminal(&mut storage, "unknown-project", &command, 0, 100 + idx);
            storage
                .insert_belief_candidate(a, b, BeliefKind::Contradicts, 0.91, Some("noise"))
                .expect("insert filler contradiction")
                .expect("new filler row");
        }
        assert!(
            storage
                .recent_contradictions(50)
                .expect("recent display window")
                .iter()
                .all(|candidate| candidate.id != target_belief_id),
            "fixture should push the target outside the old 50-row display window"
        );

        let correction_id = record_correction(
            &mut storage,
            CorrectionInput {
                claim: Some("huggingface-cli download model".to_string()),
                correction: "The command outcome is run-specific evidence, not a stable fact."
                    .to_string(),
                project: Some("unknown-project".to_string()),
                session_id: Some("manual-correction".to_string()),
            },
        )
        .expect("record correction");

        let row = storage.get_belief_candidate(target_belief_id).expect("get").expect("target row");
        assert_eq!(row.resolved_by_correction_episode_id, Some(correction_id));
        assert!(row.resolved_at_ns.is_some());
    }
}
