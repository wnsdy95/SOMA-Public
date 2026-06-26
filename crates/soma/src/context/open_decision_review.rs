//! Proposal bridge from L2 open decisions to the review queue.
//!
//! Contradictions and anomalies are L2 signals. This pass does not resolve
//! them or promote them into durable memory. It captures each unresolved signal
//! as a local short-term claim and creates a `request_verification` proposal so
//! the existing review/action gates can decide what happens next.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::memory::beliefs::BeliefCandidate;
use crate::memory::salience::IPC_FREE_ENERGY_ANOMALY_KIND;
use crate::storage::{
    ClaimRecordDraft, ClaimSourceType, ContextAnomaly, EpisodeId, LearningCriticAction,
    LearningCriticProposalDraft, LearningCriticProposalStatus, LifecycleState, SensitivityLabel,
    Storage, StorageError, StoredEpisode, StoredEvidenceRef, StoredLearningCriticProposal,
    TaskFrameDocument, TaskFrameDraft, TaskFrameProjectionPolicy, TaskFrameScope,
};

pub const OPEN_DECISION_REVIEW_SOURCE: &str = "soma_open_decision_review";
pub const OPEN_DECISION_REVIEW_RULE: &str = "unresolved_l2_open_decision_requires_review";
const DEFAULT_PROPOSAL_SCAN_LIMIT: usize = 1000;
const OPEN_DECISION_TASK_FRAME_BUILDER: &str = "task-frame-v1-open-decision-review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDecisionProposalInput {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenDecisionProposalReport {
    pub source: String,
    pub rule: String,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub limit: usize,
    pub dry_run: bool,
    pub inspected_signal_count: usize,
    pub proposed_count: usize,
    pub skipped_existing_proposal_count: usize,
    pub items: Vec<OpenDecisionProposalItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenDecisionProposalItem {
    pub signal_type: String,
    pub signal_id: i64,
    pub text: String,
    pub score: f32,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub action: String,
    pub task_frame_id: Option<i64>,
    pub claim_id: Option<i64>,
    pub proposal_id: Option<i64>,
    pub skipped_reason: Option<String>,
    pub evidence_refs: Vec<StoredEvidenceRef>,
}

pub fn propose_open_decision_reviews(
    storage: &mut Storage,
    input: OpenDecisionProposalInput,
) -> Result<OpenDecisionProposalReport, StorageError> {
    let limit = input.limit.max(1);
    let existing_refs = existing_open_decision_proposal_refs(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
    )?;
    let signals = collect_open_decision_signals(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
        limit,
    )?;
    let inspected_signal_count = signals.len();
    let mut proposed_count = 0;
    let mut skipped_existing_proposal_count = 0;
    let mut items = Vec::new();

    for signal in signals {
        let ref_key = signal.primary_ref_key();
        if existing_refs.contains(&ref_key) {
            skipped_existing_proposal_count += 1;
            items.push(signal.into_item(
                "skip",
                None,
                None,
                None,
                Some("open_decision_review_proposal_already_exists"),
            ));
            continue;
        }

        let (task_frame_id, claim_id, proposal_id) = if input.dry_run {
            (None, None, None)
        } else {
            let task_frame_id = storage.insert_task_frame(&signal.task_frame_draft())?;
            let claim_id = storage.insert_claim_record(&signal.claim_draft(task_frame_id))?;
            let proposal_id = storage
                .insert_learning_critic_proposal(&signal.proposal_draft(task_frame_id, claim_id))?;
            (Some(task_frame_id), Some(claim_id), Some(proposal_id))
        };
        proposed_count += 1;
        items.push(signal.into_item(
            if input.dry_run { "would_propose" } else { "proposed" },
            task_frame_id,
            claim_id,
            proposal_id,
            None,
        ));
    }

    Ok(OpenDecisionProposalReport {
        source: OPEN_DECISION_REVIEW_SOURCE.to_string(),
        rule: OPEN_DECISION_REVIEW_RULE.to_string(),
        project: input.project,
        session_id: input.session_id,
        limit,
        dry_run: input.dry_run,
        inspected_signal_count,
        proposed_count,
        skipped_existing_proposal_count,
        items,
    })
}

#[derive(Debug, Clone)]
struct OpenDecisionSignal {
    signal_type: String,
    signal_id: i64,
    text: String,
    score: f32,
    project: Option<String>,
    session_id: Option<String>,
    evidence_refs: Vec<StoredEvidenceRef>,
}

impl OpenDecisionSignal {
    fn primary_ref_key(&self) -> (String, String) {
        (self.primary_ref_kind().to_string(), self.signal_id.to_string())
    }

    fn primary_ref_kind(&self) -> &'static str {
        match self.signal_type.as_str() {
            "contradiction" => "belief_candidate",
            "anomaly" => "context_anomaly",
            _ => "open_decision",
        }
    }

    fn task_frame_draft(&self) -> TaskFrameDraft {
        let frame = TaskFrameDocument {
            goal_state: format!("Review unresolved {} #{}", self.signal_type, self.signal_id),
            work_mode: "review".to_string(),
            scope: TaskFrameScope {
                project: self.project.clone(),
                session_id: self.session_id.clone(),
                cwd: None,
                files: Vec::new(),
                tools: vec![
                    "soma_context_why".to_string(),
                    "soma_review_action".to_string(),
                    "soma_record_correction".to_string(),
                ],
                client: Some(OPEN_DECISION_REVIEW_SOURCE.to_string()),
            },
            constraints: vec![
                "This is an L2 open decision, not a resolved L4 fact.".to_string(),
                "Do not promote or correct durable memory without trusted verification evidence."
                    .to_string(),
            ],
            direction: vec![self.text.clone()],
            avoid: vec!["Do not treat unresolved contradictions or anomalies as semantic memory."
                .to_string()],
            uncertainty: vec!["Operator or tool verification is required.".to_string()],
            evidence_refs: self.evidence_refs.clone(),
            privacy_labels: task_frame_privacy_labels(),
        };
        TaskFrameDraft {
            builder_version: OPEN_DECISION_TASK_FRAME_BUILDER.to_string(),
            frame,
            projection_policy: TaskFrameProjectionPolicy::project_internal(),
        }
    }

    fn claim_draft(&self, task_frame_id: i64) -> ClaimRecordDraft {
        let mut evidence_refs = self.evidence_refs.clone();
        evidence_refs.push(StoredEvidenceRef {
            kind: "task_frame".to_string(),
            id: task_frame_id.to_string(),
            source: Some(OPEN_DECISION_REVIEW_SOURCE.to_string()),
        });
        ClaimRecordDraft {
            text: self.text.clone(),
            source_type: ClaimSourceType::LocalObserved,
            task_frame_id: Some(task_frame_id),
            evidence_refs,
            confidence: self.score.clamp(0.0, 1.0),
            lifecycle_state: LifecycleState::ShortTermCandidate,
        }
    }

    fn proposal_draft(&self, task_frame_id: i64, claim_id: i64) -> LearningCriticProposalDraft {
        LearningCriticProposalDraft {
            task_frame_id: Some(task_frame_id),
            action: LearningCriticAction::RequestVerification,
            claim_ids: vec![claim_id],
            target_lifecycle_state: None,
            reason: format!(
                "Open decision review via {OPEN_DECISION_REVIEW_RULE}: unresolved {} #{} requires trusted verification before correction/policy/belief extraction",
                self.signal_type, self.signal_id
            ),
            evidence_refs: self.evidence_refs.clone(),
        }
    }

    fn into_item(
        self,
        action: &str,
        task_frame_id: Option<i64>,
        claim_id: Option<i64>,
        proposal_id: Option<i64>,
        skipped_reason: Option<&str>,
    ) -> OpenDecisionProposalItem {
        OpenDecisionProposalItem {
            signal_type: self.signal_type,
            signal_id: self.signal_id,
            text: self.text,
            score: self.score,
            project: self.project,
            session_id: self.session_id,
            action: action.to_string(),
            task_frame_id,
            claim_id,
            proposal_id,
            skipped_reason: skipped_reason.map(str::to_string),
            evidence_refs: self.evidence_refs,
        }
    }
}

fn collect_open_decision_signals(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<OpenDecisionSignal>, StorageError> {
    let read_limit = limit.saturating_mul(10).max(limit);
    let mut signals = Vec::with_capacity(limit);
    for candidate in storage.recent_contradictions(read_limit)? {
        let Some(episode_a) = storage.get_live_episode(candidate.episode_a_id)? else {
            continue;
        };
        let Some(episode_b) = storage.get_live_episode(candidate.episode_b_id)? else {
            continue;
        };
        if !episode_pair_matches_scope(&episode_a, &episode_b, project, session_id) {
            continue;
        }
        signals.push(signal_from_contradiction(&candidate, &episode_a, &episode_b));
        if signals.len() == limit {
            return Ok(signals);
        }
    }

    for anomaly in storage.recent_context_anomalies(IPC_FREE_ENERGY_ANOMALY_KIND, read_limit)? {
        let Some(episode) = storage.get_live_episode(anomaly.episode_id)? else {
            continue;
        };
        if !episode_matches_scope(&episode, project, session_id) {
            continue;
        }
        signals.push(signal_from_anomaly(&anomaly, &episode));
        if signals.len() == limit {
            return Ok(signals);
        }
    }
    Ok(signals)
}

fn signal_from_contradiction(
    candidate: &BeliefCandidate,
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
) -> OpenDecisionSignal {
    let mut text = format!(
        "Open contradiction between local episodes #{} and #{} (score {:.3})",
        candidate.episode_a_id, candidate.episode_b_id, candidate.score
    );
    if let Some(evidence) = candidate.evidence.as_deref().filter(|s| !s.is_empty()) {
        text.push_str(": ");
        text.push_str(evidence);
    }
    text.push('.');
    OpenDecisionSignal {
        signal_type: "contradiction".to_string(),
        signal_id: candidate.id,
        text,
        score: candidate.score.clamp(0.0, 1.0),
        project: shared_or_first(episode_a.project.as_deref(), episode_b.project.as_deref()),
        session_id: shared_or_first(
            episode_a.session_id.as_deref(),
            episode_b.session_id.as_deref(),
        ),
        evidence_refs: vec![
            StoredEvidenceRef {
                kind: "belief_candidate".to_string(),
                id: candidate.id.to_string(),
                source: Some(candidate.kind.to_string()),
            },
            episode_ref(episode_a.id, "contradiction_episode_a"),
            episode_ref(episode_b.id, "contradiction_episode_b"),
        ],
    }
}

fn signal_from_anomaly(anomaly: &ContextAnomaly, episode: &StoredEpisode) -> OpenDecisionSignal {
    let mut text = format!(
        "Open iPC anomaly for local episode #{} (free-energy {:.3})",
        anomaly.episode_id, anomaly.score
    );
    if let Some(evidence) = anomaly.evidence.as_deref().filter(|s| !s.is_empty()) {
        text.push_str(": ");
        text.push_str(evidence);
    }
    text.push('.');
    OpenDecisionSignal {
        signal_type: "anomaly".to_string(),
        signal_id: anomaly.id,
        text,
        score: anomaly.score.clamp(0.0, 1.0),
        project: episode.project.clone(),
        session_id: episode.session_id.clone(),
        evidence_refs: vec![
            StoredEvidenceRef {
                kind: "context_anomaly".to_string(),
                id: anomaly.id.to_string(),
                source: Some(anomaly.kind.clone()),
            },
            episode_ref(episode.id, "anomaly_episode"),
        ],
    }
}

fn existing_open_decision_proposal_refs(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
) -> Result<HashSet<(String, String)>, StorageError> {
    Ok(storage
        .learning_critic_proposals_scoped(project, session_id, None, DEFAULT_PROPOSAL_SCAN_LIMIT)?
        .into_iter()
        .filter(is_open_decision_review_proposal)
        .flat_map(|proposal| proposal.evidence_refs.into_iter())
        .filter(|ev| ev.kind == "belief_candidate" || ev.kind == "context_anomaly")
        .map(|ev| (ev.kind, ev.id))
        .collect())
}

fn is_open_decision_review_proposal(proposal: &StoredLearningCriticProposal) -> bool {
    proposal.action == LearningCriticAction::RequestVerification
        && proposal.status != LearningCriticProposalStatus::Applied
        && proposal.reason.contains(OPEN_DECISION_REVIEW_RULE)
}

fn episode_pair_matches_scope(
    episode_a: &StoredEpisode,
    episode_b: &StoredEpisode,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
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
    true
}

fn episode_matches_scope(
    episode: &StoredEpisode,
    project_filter: Option<&str>,
    session_filter: Option<&str>,
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
    true
}

fn shared_or_first(a: Option<&str>, b: Option<&str>) -> Option<String> {
    match (a, b) {
        (Some(left), Some(right)) if left == right => Some(left.to_string()),
        (Some(left), _) => Some(left.to_string()),
        (_, Some(right)) => Some(right.to_string()),
        _ => None,
    }
}

fn episode_ref(id: EpisodeId, source: &str) -> StoredEvidenceRef {
    StoredEvidenceRef {
        kind: "episode".to_string(),
        id: id.to_string(),
        source: Some(source.to_string()),
    }
}

fn task_frame_privacy_labels() -> BTreeMap<String, SensitivityLabel> {
    [
        ("goal_state", SensitivityLabel::ProjectInternal),
        ("work_mode", SensitivityLabel::Public),
        ("scope", SensitivityLabel::ProjectInternal),
        ("constraints", SensitivityLabel::ProjectInternal),
        ("direction", SensitivityLabel::ProjectInternal),
        ("avoid", SensitivityLabel::ProjectInternal),
        ("uncertainty", SensitivityLabel::ProjectInternal),
        ("evidence_refs", SensitivityLabel::Public),
    ]
    .into_iter()
    .map(|(field, label)| (field.to_string(), label))
    .collect()
}
