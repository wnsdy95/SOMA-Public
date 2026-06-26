//! Deterministic TaskFrame builder for SOMA's local control plane.
//!
//! This builder is intentionally local and evidence-first. Optional AI helpers
//! may improve later extraction, but the baseline must always produce an
//! inspectable TaskFrame from stored episodes, policy rows, corrections, and L2
//! candidate proxies.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::context::correction::CORRECTION_SOURCE;
use crate::context::envelope::{ContextSection, EvidenceRef};
use crate::memory::policy;
use crate::storage::{
    ClaimSourceType, EvidenceBackedLatentProxy, SensitivityLabel, Storage, StorageError,
    StoredEpisode, StoredEvidenceRef, TaskFrameDocument, TaskFrameDraft, TaskFrameProjectionPolicy,
    TaskFrameScope,
};

pub const TASK_FRAME_BUILDER_VERSION: &str = "task-frame-v1-deterministic";
const RECENT_SCAN_LIMIT: usize = 80;
const RECENT_EVIDENCE_LIMIT: usize = 5;
const POLICY_LIMIT: usize = 5;
const CORRECTION_LIMIT: usize = 5;
const L2_CANDIDATE_LIMIT: usize = 5;
const TEXT_LIMIT: usize = 240;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFrameBuildInput {
    pub query: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub client: Option<String>,
    pub allow_local_private_projection: bool,
    pub local_private_projection_reason: Option<String>,
}

pub fn build_task_frame(
    storage: &Storage,
    input: TaskFrameBuildInput,
) -> Result<TaskFrameDraft, StorageError> {
    let recent = scoped_recent_episodes(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
        RECENT_EVIDENCE_LIMIT,
    )?;
    let mut evidence = EvidenceAccumulator::default();
    for episode in &recent {
        evidence.push_episode(episode.id);
    }

    let mut constraints = Vec::new();
    append_policy_constraints(storage, input.project.as_deref(), &mut constraints, &mut evidence)?;
    append_correction_constraints(&recent, &mut constraints, &mut evidence);

    let mut direction = Vec::new();
    append_l2_candidate_direction(
        storage,
        input.project.as_deref(),
        input.session_id.as_deref(),
        &mut direction,
        &mut evidence,
    )?;
    if direction.is_empty() {
        direction.push(default_direction(input.query.as_deref(), &recent));
    }

    let mut avoid = Vec::new();
    avoid.push("Do not promote cloud drafts to memory without verification.".to_string());
    if constraints.iter().any(|c| c.starts_with("Correction:")) {
        avoid.push(
            "Do not rely on corrected stale claims unless fresh evidence revives them.".to_string(),
        );
    }

    let mut uncertainty = Vec::new();
    if input.query.as_deref().is_none_or(|q| q.trim().is_empty()) {
        uncertainty.push("No explicit task query was supplied.".to_string());
    }
    if recent.is_empty() {
        uncertainty.push("No scoped recent evidence was found.".to_string());
    }

    let scope = TaskFrameScope {
        project: input.project.clone(),
        session_id: input.session_id.clone(),
        cwd: input.cwd.clone(),
        files: Vec::new(),
        tools: tools_from_recent(&recent),
        client: input.client.clone(),
    };

    let goal_state = goal_state(input.query.as_deref(), &recent);
    let work_mode = infer_work_mode(input.query.as_deref(), &recent);
    let frame = TaskFrameDocument {
        goal_state,
        work_mode,
        scope,
        constraints,
        direction,
        avoid,
        uncertainty,
        evidence_refs: evidence.into_vec(),
        privacy_labels: default_privacy_labels(),
    };

    let projection_policy = projection_policy_from_input(&input)?;

    Ok(TaskFrameDraft {
        builder_version: TASK_FRAME_BUILDER_VERSION.to_string(),
        frame,
        projection_policy,
    })
}

fn projection_policy_from_input(
    input: &TaskFrameBuildInput,
) -> Result<TaskFrameProjectionPolicy, StorageError> {
    let reason = input
        .local_private_projection_reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty());
    if input.allow_local_private_projection {
        let reason = reason.ok_or_else(|| StorageError::Corrupt {
            detail: "local_private projection requires a non-empty explicit reason".to_string(),
        })?;
        return Ok(TaskFrameProjectionPolicy::local_private_explicit(reason));
    }
    if reason.is_some() {
        return Err(StorageError::Corrupt {
            detail: "local_private projection reason requires explicit allow flag".to_string(),
        });
    }
    Ok(TaskFrameProjectionPolicy::project_internal())
}

pub fn task_frame_thread_state_section(
    task_frame: &crate::storage::StoredTaskFrame,
    previous: Option<&ContextSection>,
) -> ContextSection {
    let cloud = &task_frame.cloud_redacted_json;
    let mut lines = vec![format!("TaskFrame #{} shaped this context.", task_frame.id)];

    if let Some(goal) = projected_string(cloud, "goal_state") {
        lines.push(format!("Goal: {}", truncate(goal.trim(), TEXT_LIMIT)));
    }
    if let Some(work_mode) = projected_string(cloud, "work_mode") {
        lines.push(format!("Work mode: {}", truncate(work_mode.trim(), TEXT_LIMIT)));
    }
    append_projected_list(&mut lines, "Constraints", projected_string_list(cloud, "constraints"));
    append_projected_list(&mut lines, "Direction", projected_string_list(cloud, "direction"));
    append_projected_list(&mut lines, "Avoid", projected_string_list(cloud, "avoid"));
    append_projected_list(&mut lines, "Uncertainty", projected_string_list(cloud, "uncertainty"));

    if let Some(previous) = previous {
        if !previous.text.trim().is_empty() {
            lines.push("Compiled local context:".to_string());
            lines.extend(previous.text.lines().take(4).map(|line| line.to_string()));
        }
    }

    let mut evidence = Vec::new();
    push_unique_evidence(
        &mut evidence,
        EvidenceRef {
            kind: "task_frame".to_string(),
            id: task_frame.id.to_string(),
            source: Some(task_frame.builder_version.clone()),
        },
    );
    for ev in projected_evidence_refs(cloud, &task_frame.evidence_refs) {
        push_unique_evidence(&mut evidence, evidence_from_stored(&ev));
    }
    if let Some(previous) = previous {
        for ev in &previous.evidence {
            push_unique_evidence(&mut evidence, ev.clone());
        }
    }

    let status = if previous.is_some() { "task_frame+compiled" } else { "task_frame" };
    ContextSection::typed(lines.join("\n"), evidence, "thread_state", status, None)
}

fn scoped_recent_episodes(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<StoredEpisode>, StorageError> {
    let mut out = Vec::with_capacity(limit);
    for episode in storage.recent_episodes(RECENT_SCAN_LIMIT)? {
        if !episode_matches_scope(&episode, project, session_id) {
            continue;
        }
        out.push(episode);
        if out.len() == limit {
            break;
        }
    }
    Ok(out)
}

fn append_policy_constraints(
    storage: &Storage,
    project: Option<&str>,
    constraints: &mut Vec<String>,
    evidence: &mut EvidenceAccumulator,
) -> Result<(), StorageError> {
    let mut policy_projects = vec![None];
    if let Some(project) = project {
        policy_projects.push(Some(project));
    }
    for policy_project in policy_projects {
        for rule in policy::read_policy_set(storage, policy_project)? {
            constraints.push(format!("Policy: {}", truncate(rule.rule.trim(), TEXT_LIMIT)));
            for episode_id in rule.evidence_episode_ids {
                evidence.push_episode(episode_id);
            }
            if constraints.iter().filter(|c| c.starts_with("Policy:")).count() == POLICY_LIMIT {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn append_correction_constraints(
    recent: &[StoredEpisode],
    constraints: &mut Vec<String>,
    evidence: &mut EvidenceAccumulator,
) {
    for episode in recent.iter().filter(|ep| ep.source == CORRECTION_SOURCE).take(CORRECTION_LIMIT)
    {
        let text = episode
            .digest
            .as_deref()
            .or(episode.prompt_text.as_deref())
            .map(|s| truncate(&first_line(s), TEXT_LIMIT))
            .unwrap_or_else(|| "Correction recorded by user.".to_string());
        constraints.push(format!("Correction: {text}"));
        evidence.push_episode(episode.id);
    }
}

fn append_l2_candidate_direction(
    storage: &Storage,
    project: Option<&str>,
    session_id: Option<&str>,
    direction: &mut Vec<String>,
    evidence: &mut EvidenceAccumulator,
) -> Result<(), StorageError> {
    let proxies = storage.short_term_candidate_proxies(RECENT_SCAN_LIMIT)?;
    for proxy in proxies {
        if !proxy_targets_task_frame(&proxy) || !task_frame_proxy_can_project(&proxy) {
            continue;
        }
        let Some(episode) = storage.get_live_episode(proxy.episode_id)? else {
            continue;
        };
        if !episode_matches_scope(&episode, project, session_id) {
            continue;
        }
        direction
            .push(format!("Consider L2 candidate: {}", truncate(proxy.claim.trim(), TEXT_LIMIT)));
        evidence.push_proxy(proxy.id, &proxy.proxy_type);
        for ev in proxy.evidence_refs {
            evidence.push(ev);
        }
        if direction.len() == L2_CANDIDATE_LIMIT {
            break;
        }
    }
    Ok(())
}

fn proxy_targets_task_frame(proxy: &EvidenceBackedLatentProxy) -> bool {
    proxy
        .target
        .as_deref()
        .is_some_and(|target| normalized_projection_target(target) == "taskframe")
}

fn task_frame_proxy_can_project(proxy: &EvidenceBackedLatentProxy) -> bool {
    proxy.source_trust != ClaimSourceType::CloudDraft
        && !proxy.privacy_labels.is_empty()
        && proxy.privacy_labels.iter().all(|label| {
            matches!(label, SensitivityLabel::Public | SensitivityLabel::ProjectInternal)
        })
}

fn normalized_projection_target(target: &str) -> String {
    target.chars().filter(|ch| ch.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn goal_state(query: Option<&str>, recent: &[StoredEpisode]) -> String {
    if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
        return truncate(query, TEXT_LIMIT);
    }
    if let Some(preview) = recent.iter().find_map(episode_preview) {
        return format!("Continue from recent local evidence: {}", truncate(&preview, TEXT_LIMIT));
    }
    "Continue current SOMA context work.".to_string()
}

fn infer_work_mode(query: Option<&str>, recent: &[StoredEpisode]) -> String {
    let mut text = String::new();
    if let Some(query) = query {
        text.push_str(query);
        text.push('\n');
    }
    for episode in recent.iter().take(3) {
        if let Some(preview) = episode_preview(episode) {
            text.push_str(&preview);
            text.push('\n');
        }
    }
    let lower = text.to_ascii_lowercase();
    if contains_any(&lower, &["review", "audit", "critique"]) {
        "review"
    } else if contains_any(&lower, &["debug", "bug", "fix", "error", "failing", "failure"]) {
        "debug"
    } else if contains_any(&lower, &["implement", "code", "coding", "build", "wire"]) {
        "implement"
    } else if contains_any(&lower, &["plan", "roadmap", "design", "architecture"]) {
        "plan"
    } else if contains_any(&lower, &["research", "paper", "literature", "survey"]) {
        "research"
    } else if contains_any(&lower, &["explain", "describe", "teach"]) {
        "explain"
    } else {
        "mixed"
    }
    .to_string()
}

fn default_direction(query: Option<&str>, recent: &[StoredEpisode]) -> String {
    match infer_work_mode(query, recent).as_str() {
        "implement" => "Make the smallest code change that satisfies the current task.".to_string(),
        "debug" => {
            "Identify the failing behavior, patch it, and verify with focused tests.".to_string()
        }
        "review" => "Prioritize correctness risks, regressions, and missing tests.".to_string(),
        "plan" => {
            "Produce a concrete plan with explicit assumptions and next implementation steps."
                .to_string()
        }
        "research" => {
            "Ground claims in cited sources or local evidence before proposing design changes."
                .to_string()
        }
        "explain" => {
            "Explain the current system behavior using cited local context where available."
                .to_string()
        }
        _ => "Use the cited local evidence to choose the next useful action.".to_string(),
    }
}

fn projected_string(cloud: &Value, key: &str) -> Option<String> {
    cloud.get(key).and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty())
}

fn projected_string_list(cloud: &Value, key: &str) -> Vec<String> {
    cloud
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| truncate(item, TEXT_LIMIT))
                .collect()
        })
        .unwrap_or_default()
}

fn projected_evidence_refs(
    cloud: &Value,
    fallback: &[StoredEvidenceRef],
) -> Vec<StoredEvidenceRef> {
    cloud
        .get("evidence_refs")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| fallback.to_vec())
}

fn append_projected_list(lines: &mut Vec<String>, label: &str, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    lines.push(format!("{label}:"));
    for item in items.into_iter().take(5) {
        lines.push(format!("- {item}"));
    }
}

fn evidence_from_stored(evidence: &StoredEvidenceRef) -> EvidenceRef {
    EvidenceRef {
        kind: evidence.kind.clone(),
        id: evidence.id.clone(),
        source: evidence.source.clone(),
    }
}

fn push_unique_evidence(out: &mut Vec<EvidenceRef>, evidence: EvidenceRef) {
    let exists = out.iter().any(|ev| ev.kind == evidence.kind && ev.id == evidence.id);
    if !exists {
        out.push(evidence);
    }
}

fn tools_from_recent(recent: &[StoredEpisode]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for episode in recent {
        let Some(command) = episode.command.as_deref() else {
            continue;
        };
        let Some(tool) = command.split_whitespace().next() else {
            continue;
        };
        if seen.insert(tool.to_string()) {
            out.push(tool.to_string());
        }
        if out.len() == 5 {
            break;
        }
    }
    out
}

fn episode_matches_scope(
    episode: &StoredEpisode,
    project: Option<&str>,
    session_id: Option<&str>,
) -> bool {
    if let Some(project) = project {
        if episode.project.as_deref() != Some(project) {
            return false;
        }
    }
    if let Some(session_id) = session_id {
        if episode.session_id.as_deref() != Some(session_id) {
            return false;
        }
    }
    true
}

fn episode_preview(episode: &StoredEpisode) -> Option<String> {
    episode
        .prompt_text
        .as_deref()
        .or(episode.command.as_deref())
        .or(episode.digest.as_deref())
        .map(first_line)
        .filter(|s| !s.trim().is_empty())
        .map(|s| truncate(&s, TEXT_LIMIT))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn truncate(s: &str, limit: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let take = limit.saturating_sub(3);
    let mut out = trimmed.chars().take(take).collect::<String>();
    out.push_str("...");
    out
}

fn default_privacy_labels() -> BTreeMap<String, SensitivityLabel> {
    [
        ("goal_state", SensitivityLabel::ProjectInternal),
        ("work_mode", SensitivityLabel::Public),
        ("scope", SensitivityLabel::ProjectInternal),
        ("constraints", SensitivityLabel::ProjectInternal),
        ("direction", SensitivityLabel::ProjectInternal),
        ("avoid", SensitivityLabel::ProjectInternal),
        ("uncertainty", SensitivityLabel::ProjectInternal),
        ("evidence_refs", SensitivityLabel::ProjectInternal),
    ]
    .into_iter()
    .map(|(field, label)| (field.to_string(), label))
    .collect()
}

#[derive(Default)]
struct EvidenceAccumulator {
    seen: BTreeSet<String>,
    refs: Vec<StoredEvidenceRef>,
}

impl EvidenceAccumulator {
    fn push_episode(&mut self, id: i64) {
        self.push(StoredEvidenceRef::episode(id));
    }

    fn push_proxy(&mut self, id: i64, proxy_type: &str) {
        self.push(StoredEvidenceRef {
            kind: "evidence_latent_proxy".to_string(),
            id: id.to_string(),
            source: Some(proxy_type.to_string()),
        });
    }

    fn push(&mut self, evidence_ref: StoredEvidenceRef) {
        let key = format!(
            "{}:{}:{}",
            evidence_ref.kind,
            evidence_ref.id,
            evidence_ref.source.as_deref().unwrap_or("")
        );
        if self.seen.insert(key) {
            self.refs.push(evidence_ref);
        }
    }

    fn into_vec(self) -> Vec<StoredEvidenceRef> {
        self.refs
    }
}
