//! `soma projects` - active-persona project experience provenance.
//!
//! Named personas isolate the local SOMA database. Project provenance stays
//! inside that persona database as episode metadata, so this command gives the
//! operator a read-only way to answer: "what projects has this persona learned
//! from, and which sessions/sources prove it?"

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::cli::ProjectExperienceArgs;
use crate::storage::{Storage, StorageError};

const SESSION_ENV: &str = "SOMA_SESSION_ID";
const CLIENT_ENV: &str = "SOMA_CLIENT";
const PROJECT_ENV: &str = "SOMA_PROJECT";
const THREAD_ENV: &str = "SOMA_THREAD_KEY";
const PERSONA_ENV: &str = "SOMA_PERSONA";
const DB_ENV: &str = "SOMA_DB";
const PERSONA_HOME_ENV: &str = "SOMA_PERSONA_HOME";
const PERSONAS_DIR_ENV: &str = "SOMA_PERSONAS_DIR";
const ADAPTER_SPOOL_JSONL_ENV: &str = "SOMA_ADAPTER_SPOOL_JSONL";
const DEFAULT_PERSONA: &str = "default";
const DB_FILE: &str = "soma.db";
const METADATA_FILE: &str = "persona.json";

#[derive(Debug)]
pub enum ProjectExperienceError {
    Path(String),
    Storage(StorageError),
    Render(serde_json::Error),
    BadFormat(String),
    CurrentTerminalScopeNotReady(String),
}

impl ProjectExperienceError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ProjectExperienceError::BadFormat(_) => 1,
            ProjectExperienceError::Storage(_) | ProjectExperienceError::Render(_) => 2,
            ProjectExperienceError::Path(_) => 3,
            ProjectExperienceError::CurrentTerminalScopeNotReady(_) => 4,
        }
    }
}

impl std::fmt::Display for ProjectExperienceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectExperienceError::Path(message) => write!(f, "path: {message}"),
            ProjectExperienceError::Storage(err) => write!(f, "storage: {err}"),
            ProjectExperienceError::Render(err) => write!(f, "render: {err}"),
            ProjectExperienceError::BadFormat(message) => write!(f, "bad format: {message}"),
            ProjectExperienceError::CurrentTerminalScopeNotReady(message) => {
                write!(f, "current terminal scope not ready: {message}")
            }
        }
    }
}

impl std::error::Error for ProjectExperienceError {}

impl From<StorageError> for ProjectExperienceError {
    fn from(value: StorageError) -> Self {
        ProjectExperienceError::Storage(value)
    }
}

impl From<serde_json::Error> for ProjectExperienceError {
    fn from(value: serde_json::Error) -> Self {
        ProjectExperienceError::Render(value)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectExperienceContext {
    pub db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectExperienceReport {
    pub schema: &'static str,
    pub source: &'static str,
    pub active_persona: String,
    pub report_persona: String,
    pub report_persona_source: &'static str,
    pub db_path: String,
    pub storage_status: &'static str,
    pub storage_error: Option<String>,
    pub status: &'static str,
    pub project_filter: Option<String>,
    pub project_count: usize,
    pub scoped_episode_count: usize,
    pub unscoped_episode_count: usize,
    pub evidence_limit: usize,
    pub current_terminal_scope: CurrentTerminalProjectScope,
    pub scope_contract: ProjectPersonaScopeContract,
    pub scope_integrity: ProjectScopeIntegrity,
    pub scope_review_plan: ProjectScopeReviewPlan,
    pub scope_verification: ProjectScopeVerificationIndex,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dogfood_evidence: Option<ProjectDogfoodEvidence>,
    pub projects: Vec<ProjectExperienceRow>,
    pub operator_next_action_id: &'static str,
    pub operator_next_action_label: &'static str,
    pub operator_card: ProjectExperienceOperatorCard,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub scope_warnings: Vec<String>,
    pub next_commands: Vec<Vec<String>>,
    pub recovery_commands: Vec<Vec<String>>,
    pub trust_boundary: &'static str,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectPersonaScopeContract {
    pub source: &'static str,
    pub persona_store_contract: &'static str,
    pub project_provenance_contract: &'static str,
    pub session_scope_contract: &'static str,
    pub persona_is_storage_boundary: bool,
    pub project_is_metadata_inside_persona: bool,
    pub project_creates_separate_store: bool,
    pub ready_for_project_scoped_capture: bool,
    pub storage_write_required_for_capture: bool,
    pub storage_write_ready: bool,
    pub storage_write_status: &'static str,
    pub required_scope_envs: Vec<&'static str>,
    pub missing_scope_envs: Vec<&'static str>,
    pub active_persona: String,
    pub db_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_id: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDogfoodEvidence {
    pub source: &'static str,
    pub path: String,
    pub status: &'static str,
    pub report_status: Option<String>,
    pub multi_terminal_scope_status: Option<String>,
    pub summary_pass: Option<u64>,
    pub summary_warn: Option<u64>,
    pub summary_fail: Option<u64>,
    pub error: Option<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectExperienceOperatorCard {
    pub source: &'static str,
    pub status: &'static str,
    pub operator_next_action_id: &'static str,
    pub operator_next_action_label: &'static str,
    pub headline: String,
    pub primary_next_step: String,
    pub primary_next_command: Vec<String>,
    pub ready_for_project_scoped_capture: bool,
    pub scope_warning_count: usize,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectScopeReviewPlan {
    pub source: &'static str,
    pub status: &'static str,
    pub headline: String,
    pub current_scope_ready: bool,
    pub historical_warning_count: usize,
    pub cross_project_session_count: usize,
    pub unscoped_episode_count: usize,
    pub dogfood_scope_status: Option<String>,
    pub cross_project_session_review_items: Vec<ProjectScopeSessionReviewItem>,
    pub review_commands: Vec<Vec<String>>,
    pub clean_capture_commands: Vec<Vec<String>>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectScopeVerificationIndex {
    pub source: &'static str,
    pub status: &'static str,
    pub current_scope_ready: bool,
    pub active_persona: String,
    pub current_scope_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_client: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_session_id: Option<String>,
    pub missing_scope_envs: Vec<&'static str>,
    pub storage_write_ready: bool,
    pub storage_write_status: &'static str,
    pub project_provenance_status: &'static str,
    pub session_project_status: &'static str,
    pub cross_project_session_count: usize,
    pub unscoped_episode_count: usize,
    pub scope_review_status: &'static str,
    pub dogfood_scope_status: Option<String>,
    pub scope_activation_commands: Vec<Vec<String>>,
    pub review_commands: Vec<Vec<String>>,
    pub clean_capture_commands: Vec<Vec<String>>,
    pub cross_project_session_review_items: Vec<ProjectScopeSessionReviewItem>,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectScopeSessionReviewItem {
    pub source: &'static str,
    pub session_id: String,
    pub projects: Vec<String>,
    pub episode_count: usize,
    pub evidence_episode_ids: Vec<i64>,
    pub status: &'static str,
    pub next_action: &'static str,
    pub context_render_command: Vec<String>,
    pub recall_command: Vec<String>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrentTerminalProjectScope {
    pub active_persona: String,
    pub persona_activation_status: &'static str,
    pub db_path: String,
    pub db_path_source: &'static str,
    pub db_path_matches_report: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_spool_jsonl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_filter_matches_current: Option<bool>,
    pub capture_scope_status: &'static str,
    pub ready_for_project_scoped_capture: bool,
    pub storage_write_ready: bool,
    pub storage_write_status: &'static str,
    pub missing_scope_envs: Vec<&'static str>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_project: Option<String>,
    pub client_choice_required: bool,
    pub suggested_clients: Vec<String>,
    pub suggested_persona_call_commands: Vec<Vec<String>>,
    pub suggested_session_start_commands: Vec<Vec<String>>,
    pub next_commands: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectScopeIntegrity {
    pub project_provenance_status: &'static str,
    pub session_project_status: &'static str,
    pub cross_project_session_count: usize,
    pub cross_project_sessions: Vec<ProjectSessionScopeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectSessionScopeRow {
    pub session_id: String,
    pub projects: Vec<String>,
    pub episode_count: usize,
    pub evidence_episode_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectExperienceRow {
    pub project: String,
    pub episode_count: usize,
    pub session_count: usize,
    pub source_counts: BTreeMap<String, usize>,
    pub memory_tier_counts: BTreeMap<String, usize>,
    pub git_branches: Vec<String>,
    pub cwd_samples: Vec<String>,
    pub first_seen_ns: i64,
    pub last_seen_ns: i64,
    pub recent_sessions: Vec<String>,
    pub evidence_episode_ids: Vec<i64>,
}

#[derive(Default)]
struct ProjectAccum {
    episode_count: usize,
    source_counts: BTreeMap<String, usize>,
    memory_tier_counts: BTreeMap<String, usize>,
    git_branches: BTreeSet<String>,
    cwd_samples: BTreeSet<String>,
    first_seen_ns: Option<i64>,
    last_seen_ns: Option<i64>,
    session_last_seen: BTreeMap<String, i64>,
    evidence: Vec<(i64, i64)>,
}

#[derive(Default)]
struct SessionProjectAccum {
    projects: BTreeSet<String>,
    episode_count: usize,
    evidence: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportPersonaRef {
    name: String,
    source: &'static str,
}

pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, ProjectExperienceError> {
    crate::capture::ai_cli::resolve_db_path(cli_override).map_err(|err| {
        ProjectExperienceError::Path(match err {
            crate::capture::ai_cli::IngestError::Path(message) => message,
            other => other.to_string(),
        })
    })
}

pub fn run_projects(
    args: &ProjectExperienceArgs,
    ctx: &ProjectExperienceContext,
) -> Result<String, ProjectExperienceError> {
    let requested_format = if args.json {
        "json"
    } else if args.brief {
        "brief"
    } else {
        args.format.as_str()
    };
    let format = match requested_format {
        "markdown" | "md" => OutputFormat::Markdown,
        "brief" => OutputFormat::Brief,
        "json" => OutputFormat::Json,
        other => {
            return Err(ProjectExperienceError::BadFormat(format!(
                "unknown format `{other}`; expected `markdown`, `brief`, or `json`"
            )));
        }
    };
    let report = build_project_experience_report(args, ctx)?;
    if args.require_current_terminal_scope
        && !report.current_terminal_scope.ready_for_project_scoped_capture
    {
        return Err(ProjectExperienceError::CurrentTerminalScopeNotReady(
            current_terminal_scope_gate_message(&report),
        ));
    }
    if args.current_terminal {
        return match format {
            OutputFormat::Json => render_current_terminal_scope_json(&report),
            OutputFormat::Brief | OutputFormat::Markdown => {
                Ok(render_current_terminal_scope_only(&report))
            }
        };
    }
    match format {
        OutputFormat::Json => {
            let mut text = serde_json::to_string_pretty(&report)?;
            text.push('\n');
            Ok(text)
        }
        OutputFormat::Brief => Ok(render_brief(&report)),
        OutputFormat::Markdown => Ok(render_markdown(&report)),
    }
}

enum OutputFormat {
    Json,
    Brief,
    Markdown,
}

pub(crate) fn build_project_experience_report(
    args: &ProjectExperienceArgs,
    ctx: &ProjectExperienceContext,
) -> Result<ProjectExperienceReport, ProjectExperienceError> {
    let store = match Storage::open(&ctx.db_path) {
        Ok(store) => store,
        Err(err) => return Ok(storage_unavailable_report(args, ctx, err.to_string())),
    };
    let episodes = match store.all_episodes() {
        Ok(episodes) => episodes,
        Err(err) => return Ok(storage_unavailable_report(args, ctx, err.to_string())),
    };
    let mut unscoped_episode_count = 0_usize;
    let mut scoped_episode_count = 0_usize;
    let mut by_project: BTreeMap<String, ProjectAccum> = BTreeMap::new();
    let mut by_session: BTreeMap<String, SessionProjectAccum> = BTreeMap::new();
    let project_filter = nonempty_opt(args.project.as_deref());

    for episode in episodes {
        let Some(project) = episode.project.as_deref().and_then(nonempty) else {
            unscoped_episode_count += 1;
            continue;
        };
        if project_filter.as_deref().is_some_and(|filter| filter != project) {
            continue;
        }
        scoped_episode_count += 1;
        let entry = by_project.entry(project.to_string()).or_default();
        entry.episode_count += 1;
        *entry.source_counts.entry(episode.source.to_string()).or_default() += 1;
        *entry.memory_tier_counts.entry(episode.memory_tier).or_default() += 1;
        if let Some(branch) = episode.git_branch.as_deref().and_then(nonempty) {
            entry.git_branches.insert(branch.to_string());
        }
        if let Some(cwd) = episode.cwd.as_deref().and_then(nonempty) {
            entry.cwd_samples.insert(cwd.to_string());
        }
        if let Some(session) = episode.session_id.as_deref().and_then(nonempty) {
            let last_seen = entry.session_last_seen.entry(session.to_string()).or_insert(i64::MIN);
            *last_seen = (*last_seen).max(episode.ts_start_ns);
            let session_entry = by_session.entry(session).or_default();
            session_entry.projects.insert(project.to_string());
            session_entry.episode_count += 1;
            session_entry.evidence.push((episode.ts_start_ns, episode.id));
        }
        entry.first_seen_ns = Some(
            entry.first_seen_ns.map_or(episode.ts_start_ns, |seen| seen.min(episode.ts_start_ns)),
        );
        entry.last_seen_ns = Some(
            entry.last_seen_ns.map_or(episode.ts_start_ns, |seen| seen.max(episode.ts_start_ns)),
        );
        entry.evidence.push((episode.ts_start_ns, episode.id));
    }

    let evidence_limit = args.evidence_limit.min(50);
    let mut projects: Vec<ProjectExperienceRow> = by_project
        .into_iter()
        .map(|(project, mut accum)| {
            accum.evidence.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
            let mut recent_sessions: Vec<(String, i64)> =
                accum.session_last_seen.into_iter().collect();
            recent_sessions.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ProjectExperienceRow {
                project,
                episode_count: accum.episode_count,
                session_count: recent_sessions.len(),
                source_counts: accum.source_counts,
                memory_tier_counts: accum.memory_tier_counts,
                git_branches: accum.git_branches.into_iter().collect(),
                cwd_samples: accum.cwd_samples.into_iter().take(5).collect(),
                first_seen_ns: accum.first_seen_ns.unwrap_or(0),
                last_seen_ns: accum.last_seen_ns.unwrap_or(0),
                recent_sessions: recent_sessions
                    .into_iter()
                    .take(5)
                    .map(|(session, _)| session)
                    .collect(),
                evidence_episode_ids: accum
                    .evidence
                    .into_iter()
                    .take(evidence_limit)
                    .map(|(_, id)| id)
                    .collect(),
            }
        })
        .collect();
    projects.sort_by(|a, b| {
        b.last_seen_ns.cmp(&a.last_seen_ns).then_with(|| a.project.cmp(&b.project))
    });
    let scope_integrity = build_scope_integrity(unscoped_episode_count, evidence_limit, by_session);
    let status = project_experience_status(projects.len(), &scope_integrity);
    let scope_warnings = project_scope_warnings(&scope_integrity, unscoped_episode_count);
    let report_db_path = ctx.db_path.display().to_string();
    let report_persona = report_persona_for_db_path(&ctx.db_path);
    let current_terminal_scope =
        current_terminal_scope(&report_db_path, project_filter.as_deref(), &report_persona.name);
    let scope_contract = project_persona_scope_contract(&current_terminal_scope);
    let operator = project_operator_action(status, args, &scope_integrity, &current_terminal_scope);
    let next_commands =
        project_next_commands(args, status, &current_terminal_scope, &scope_integrity);
    let operator_card = project_operator_card(
        status,
        &operator,
        &current_terminal_scope,
        &scope_warnings,
        projects.len(),
    );
    let dogfood_evidence = project_dogfood_evidence(args);
    let scope_review_plan = project_scope_review_plan(
        args,
        status,
        &current_terminal_scope,
        &scope_integrity,
        unscoped_episode_count,
        &scope_warnings,
        dogfood_evidence.as_ref(),
    );
    let scope_verification = project_scope_verification_index(
        status,
        &current_terminal_scope,
        &scope_integrity,
        &scope_review_plan,
    );

    Ok(normalize_project_report_commands(ProjectExperienceReport {
        schema: "soma.project_experience_report.v1",
        source: "soma_projects",
        active_persona: report_persona.name.clone(),
        report_persona: report_persona.name,
        report_persona_source: report_persona.source,
        db_path: report_db_path,
        storage_status: "available",
        storage_error: None,
        status,
        project_filter,
        project_count: projects.len(),
        scoped_episode_count,
        unscoped_episode_count,
        evidence_limit,
        current_terminal_scope,
        scope_contract,
        scope_integrity,
        scope_review_plan,
        scope_verification,
        dogfood_evidence,
        projects,
        operator_next_action_id: operator.id,
        operator_next_action_label: operator.label,
        operator_card,
        primary_next_step: operator.next_step,
        primary_next_command: operator.command,
        scope_warnings,
        next_commands,
        recovery_commands: Vec::new(),
        trust_boundary:
            "read_only_episode_provenance; records no capture, verification, promotion, or persona mutation",
    }))
}

fn storage_unavailable_report(
    args: &ProjectExperienceArgs,
    ctx: &ProjectExperienceContext,
    error: String,
) -> ProjectExperienceReport {
    let project_filter = nonempty_opt(args.project.as_deref());
    let recovery_commands = projects_storage_recovery_commands(project_filter.as_deref());
    let report_db_path = ctx.db_path.display().to_string();
    let report_persona = report_persona_for_db_path(&ctx.db_path);
    let current_terminal_scope = current_terminal_scope(
        &report_db_path,
        nonempty_opt(args.project.as_deref()).as_deref(),
        &report_persona.name,
    );
    let scope_contract = project_persona_scope_contract(&current_terminal_scope);
    let operator = ProjectOperatorAction {
        id: "restore_project_experience_storage_access",
        label: "Restore project experience storage access",
        next_step: "Grant SOMA read access to the active persona DB, or activate a workspace-local persona store when this client cannot read ~/.soma.".to_string(),
        command: sandbox_persona_activation_command(project_filter.as_deref()),
    };
    let scope_warnings = vec![
        "Project provenance and session/project isolation are unknown while storage is unreadable."
            .to_string(),
    ];
    let operator_card = project_operator_card(
        "storage_unavailable",
        &operator,
        &current_terminal_scope,
        &scope_warnings,
        0,
    );
    let scope_integrity = ProjectScopeIntegrity {
        project_provenance_status: "unknown_storage_unavailable",
        session_project_status: "unknown_storage_unavailable",
        cross_project_session_count: 0,
        cross_project_sessions: Vec::new(),
    };
    let dogfood_evidence = project_dogfood_evidence(args);
    let scope_review_plan = project_scope_review_plan(
        args,
        "storage_unavailable",
        &current_terminal_scope,
        &scope_integrity,
        0,
        &scope_warnings,
        dogfood_evidence.as_ref(),
    );
    let scope_verification = project_scope_verification_index(
        "storage_unavailable",
        &current_terminal_scope,
        &scope_integrity,
        &scope_review_plan,
    );
    normalize_project_report_commands(ProjectExperienceReport {
        schema: "soma.project_experience_report.v1",
        source: "soma_projects",
        active_persona: report_persona.name.clone(),
        report_persona: report_persona.name,
        report_persona_source: report_persona.source,
        db_path: report_db_path.clone(),
        storage_status: "unavailable",
        storage_error: Some(error),
        status: "storage_unavailable",
        project_filter,
        project_count: 0,
        scoped_episode_count: 0,
        unscoped_episode_count: 0,
        evidence_limit: args.evidence_limit.min(50),
        current_terminal_scope,
        scope_contract,
        scope_integrity,
        scope_review_plan,
        scope_verification,
        dogfood_evidence,
        projects: Vec::new(),
        operator_next_action_id: operator.id,
        operator_next_action_label: operator.label,
        operator_card,
        primary_next_step: operator.next_step,
        primary_next_command: operator.command,
        scope_warnings,
        next_commands: recovery_commands.clone(),
        recovery_commands,
        trust_boundary: "soma_projects_storage_unavailable_is_read_only: reports that project provenance storage could not be read; records no capture, verification event, promotion, persona mutation, or claim that project scopes are clean",
    })
}

fn normalize_project_report_commands(
    mut report: ProjectExperienceReport,
) -> ProjectExperienceReport {
    let (binary_identity, _errors) = crate::cli::binary_identity::collect_binary_identity();

    report.current_terminal_scope.suggested_persona_call_commands =
        project_commands_with_current_binary_when_path_soma_differs(
            report.current_terminal_scope.suggested_persona_call_commands,
            &binary_identity,
        );
    report.current_terminal_scope.suggested_session_start_commands =
        project_commands_with_current_binary_when_path_soma_differs(
            report.current_terminal_scope.suggested_session_start_commands,
            &binary_identity,
        );
    report.current_terminal_scope.next_commands =
        project_commands_with_current_binary_when_path_soma_differs(
            report.current_terminal_scope.next_commands,
            &binary_identity,
        );

    for item in &mut report.scope_review_plan.cross_project_session_review_items {
        item.context_render_command = project_command_with_current_binary_when_path_soma_differs(
            item.context_render_command.clone(),
            &binary_identity,
        );
        item.recall_command = project_command_with_current_binary_when_path_soma_differs(
            item.recall_command.clone(),
            &binary_identity,
        );
    }
    report.scope_review_plan.review_commands =
        project_commands_with_current_binary_when_path_soma_differs(
            report.scope_review_plan.review_commands,
            &binary_identity,
        );
    report.scope_review_plan.clean_capture_commands =
        project_commands_with_current_binary_when_path_soma_differs(
            report.scope_review_plan.clean_capture_commands,
            &binary_identity,
        );

    report.scope_verification.scope_activation_commands =
        report.current_terminal_scope.next_commands.clone();
    report.scope_verification.review_commands = report.scope_review_plan.review_commands.clone();
    report.scope_verification.clean_capture_commands =
        report.scope_review_plan.clean_capture_commands.clone();
    report.scope_verification.cross_project_session_review_items =
        report.scope_review_plan.cross_project_session_review_items.clone();

    report.operator_card.primary_next_command =
        project_command_with_current_binary_when_path_soma_differs(
            report.operator_card.primary_next_command.clone(),
            &binary_identity,
        );
    report.primary_next_command = project_command_with_current_binary_when_path_soma_differs(
        report.primary_next_command,
        &binary_identity,
    );
    report.next_commands = project_commands_with_current_binary_when_path_soma_differs(
        report.next_commands,
        &binary_identity,
    );
    report.recovery_commands = project_commands_with_current_binary_when_path_soma_differs(
        report.recovery_commands,
        &binary_identity,
    );

    report
}

fn project_commands_with_current_binary_when_path_soma_differs(
    commands: Vec<Vec<String>>,
    binary_identity: &crate::cli::binary_identity::BinaryIdentity,
) -> Vec<Vec<String>> {
    commands
        .into_iter()
        .map(|command| {
            project_command_with_current_binary_when_path_soma_differs(command, binary_identity)
        })
        .collect()
}

fn project_command_with_current_binary_when_path_soma_differs(
    command: Vec<String>,
    binary_identity: &crate::cli::binary_identity::BinaryIdentity,
) -> Vec<String> {
    let command = crate::cli::binary_identity::command_with_current_binary_when_path_soma_differs(
        command,
        binary_identity,
    );
    let Some(current_exe) = binary_identity.resolved_soma_bin() else {
        return command;
    };
    let shell_current_exe = shell_quote_path(current_exe);
    command
        .into_iter()
        .map(|part| replace_embedded_soma_invocation(&part, &shell_current_exe))
        .collect()
}

fn replace_embedded_soma_invocation(part: &str, current_exe: &str) -> String {
    part.replace("$(soma ", &format!("$({current_exe} "))
        .replace(" soma ", &format!(" {current_exe} "))
}

fn shell_quote_path(path: &str) -> String {
    if path
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | '+'))
    {
        return path.to_string();
    }
    format!("'{}'", path.replace('\'', "'\"'\"'"))
}

fn current_terminal_scope_gate_message(report: &ProjectExperienceReport) -> String {
    let scope = &report.current_terminal_scope;
    let missing = if scope.missing_scope_envs.is_empty() {
        "none".to_string()
    } else {
        scope.missing_scope_envs.join(",")
    };
    let next =
        scope.next_commands.first().map(|command| command.join(" ")).unwrap_or_else(|| {
            "soma call <persona> --client <client> --project <project>".to_string()
        });
    format!(
        "status={} ready=false missing_env={} active_persona={} db={} storage_write_status={} storage_write_ready={} next={}",
        scope.capture_scope_status,
        missing,
        scope.active_persona,
        scope.db_path,
        scope.storage_write_status,
        scope.storage_write_ready,
        next
    )
}

fn project_dogfood_evidence(args: &ProjectExperienceArgs) -> Option<ProjectDogfoodEvidence> {
    let (path, explicit) = default_dogfood_report_path(args)?;
    if !explicit && !path.is_file() {
        return None;
    }
    let path_text = path.to_string_lossy().into_owned();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            return Some(ProjectDogfoodEvidence {
                source: "soma_projects.dogfood_evidence.v1",
                path: path_text,
                status: "unreadable",
                report_status: None,
                multi_terminal_scope_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                error: Some(err.to_string()),
                trust_boundary: "project_dogfood_evidence_is_read_only: cites optional last-run dogfood evidence only; records no capture, creates no verification event, mutates no persona, promotes no cloud draft, and cannot prove live project-scope cleanliness",
            });
        }
    };
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(err) => {
            return Some(ProjectDogfoodEvidence {
                source: "soma_projects.dogfood_evidence.v1",
                path: path_text,
                status: "invalid_json",
                report_status: None,
                multi_terminal_scope_status: None,
                summary_pass: None,
                summary_warn: None,
                summary_fail: None,
                error: Some(err.to_string()),
                trust_boundary: "project_dogfood_evidence_is_read_only: cites optional last-run dogfood evidence only; records no capture, creates no verification event, mutates no persona, promotes no cloud draft, and cannot prove live project-scope cleanliness",
            });
        }
    };
    if value.get("schema").and_then(Value::as_str) != Some("soma.client_dogfood_report.v1") {
        return Some(ProjectDogfoodEvidence {
            source: "soma_projects.dogfood_evidence.v1",
            path: path_text,
            status: "invalid_schema",
            report_status: value.get("status").and_then(Value::as_str).map(ToOwned::to_owned),
            multi_terminal_scope_status: dogfood_objective_status(
                &value,
                "multi_terminal_persona_project_scope",
            ),
            summary_pass: dogfood_summary_count(&value, "pass"),
            summary_warn: dogfood_summary_count(&value, "warn"),
            summary_fail: dogfood_summary_count(&value, "fail"),
            error: Some("expected schema soma.client_dogfood_report.v1".to_string()),
            trust_boundary: "project_dogfood_evidence_is_read_only: cites optional last-run dogfood evidence only; records no capture, creates no verification event, mutates no persona, promotes no cloud draft, and cannot prove live project-scope cleanliness",
        });
    }
    Some(ProjectDogfoodEvidence {
        source: "soma_projects.dogfood_evidence.v1",
        path: path_text,
        status: "valid",
        report_status: value.get("status").and_then(Value::as_str).map(ToOwned::to_owned),
        multi_terminal_scope_status: dogfood_objective_status(
            &value,
            "multi_terminal_persona_project_scope",
        ),
        summary_pass: dogfood_summary_count(&value, "pass"),
        summary_warn: dogfood_summary_count(&value, "warn"),
        summary_fail: dogfood_summary_count(&value, "fail"),
        error: None,
        trust_boundary: "project_dogfood_evidence_is_read_only: cites optional last-run dogfood evidence only; records no capture, creates no verification event, mutates no persona, promotes no cloud draft, and cannot prove live project-scope cleanliness",
    })
}

fn default_dogfood_report_path(args: &ProjectExperienceArgs) -> Option<(PathBuf, bool)> {
    if let Some(path) = args.dogfood_report.as_deref().and_then(nonempty) {
        return Some((PathBuf::from(path), true));
    }
    if let Some(value) = env::var_os("SOMA_CLIENT_DOGFOOD_REPORT").filter(|value| !value.is_empty())
    {
        return Some((PathBuf::from(value), true));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some((PathBuf::from(home).join(".soma/reports/client-dogfood-latest.json"), false))
}

fn dogfood_objective_status(value: &Value, objective_name: &str) -> Option<String> {
    value
        .get("objectives")
        .and_then(Value::as_array)?
        .iter()
        .find(|objective| {
            objective.get("objective").and_then(Value::as_str) == Some(objective_name)
        })
        .and_then(|objective| objective.get("status").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn dogfood_summary_count(value: &Value, key: &str) -> Option<u64> {
    value.get("summary").and_then(|summary| summary.get(key)).and_then(Value::as_u64)
}

struct ProjectOperatorAction {
    id: &'static str,
    label: &'static str,
    next_step: String,
    command: Vec<String>,
}

fn project_experience_status(
    project_count: usize,
    scope_integrity: &ProjectScopeIntegrity,
) -> &'static str {
    if scope_integrity.cross_project_session_count > 0 {
        "scope_review_required"
    } else if scope_integrity.project_provenance_status == "has_unscoped_episodes" {
        "project_provenance_incomplete"
    } else if project_count == 0 {
        "empty"
    } else {
        "ready"
    }
}

fn project_scope_warnings(
    scope_integrity: &ProjectScopeIntegrity,
    unscoped_episode_count: usize,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if scope_integrity.cross_project_session_count > 0 {
        warnings.push(format!(
            "{} session(s) contain episodes from multiple projects; inspect before claiming terminal/project isolation is clean.",
            scope_integrity.cross_project_session_count
        ));
    }
    if unscoped_episode_count > 0 {
        warnings.push(format!(
            "{unscoped_episode_count} episode(s) have no project metadata; they remain useful experience but cannot prove project-scoped learning."
        ));
    }
    warnings
}

fn project_operator_card(
    status: &'static str,
    operator: &ProjectOperatorAction,
    current_terminal_scope: &CurrentTerminalProjectScope,
    scope_warnings: &[String],
    project_count: usize,
) -> ProjectExperienceOperatorCard {
    let headline = match status {
        "ready" => format!(
            "Project provenance is ready for {project_count} project(s) in the active persona."
        ),
        "scope_review_required" => {
            "Cross-project session evidence needs review before claiming clean terminal/project isolation."
                .to_string()
        }
        "project_provenance_incomplete" => {
            "Some memory has no project metadata; future captures should activate a persona/session/project scope."
                .to_string()
        }
        "empty" => {
            "No project-scoped experience has been captured for the active persona yet.".to_string()
        }
        "storage_unavailable" => {
            "Project provenance storage is unavailable; readiness cannot be claimed.".to_string()
        }
        _ => "Inspect project experience provenance for the active persona.".to_string(),
    };

    let mut safe_to_claim = Vec::new();
    let mut blocked_claims = Vec::new();
    if current_terminal_scope.ready_for_project_scoped_capture {
        safe_to_claim.push(
            "The current terminal exports SOMA_SESSION_ID, SOMA_CLIENT, and SOMA_PROJECT for project-scoped capture."
                .to_string(),
        );
        safe_to_claim.push(format!(
            "The active persona DB reports storage_write_status={} for new capture attempts.",
            current_terminal_scope.storage_write_status
        ));
    } else if current_terminal_scope.capture_scope_status
        == "project_scoped_capture_storage_not_writable"
    {
        blocked_claims.push(format!(
            "The current terminal exports SOMA_SESSION_ID, SOMA_CLIENT, and SOMA_PROJECT, but the active persona DB reports storage_write_status={}; new captures cannot prove project scope until storage is writable.",
            current_terminal_scope.storage_write_status
        ));
    } else {
        blocked_claims.push(
            "The current terminal is not ready to prove project-scoped capture until `soma call <persona> --client ... --project ...` exports, or equivalent session exports, are active."
                .to_string(),
        );
    }

    match status {
        "ready" => {
            safe_to_claim.push(
                "Stored episodes have project provenance and no cross-project session warnings are visible for this report scope."
                    .to_string(),
            );
        }
        "scope_review_required" => {
            blocked_claims.push(
                "Cross-project sessions must be inspected before claiming terminal/project isolation is clean."
                    .to_string(),
            );
        }
        "project_provenance_incomplete" => {
            blocked_claims.push(
                "Unscoped episodes cannot prove project-specific learning, even though they remain usable experience."
                    .to_string(),
            );
        }
        "empty" => {
            blocked_claims.push(
                "No project-scoped memory exists yet for this persona and report scope."
                    .to_string(),
            );
        }
        "storage_unavailable" => {
            blocked_claims.push(
                "Storage must be readable before project provenance or scope isolation can be claimed."
                    .to_string(),
            );
        }
        _ => {}
    }

    for warning in scope_warnings {
        blocked_claims.push(warning.clone());
    }

    ProjectExperienceOperatorCard {
        source: "soma_projects.operator_card.v1",
        status,
        operator_next_action_id: operator.id,
        operator_next_action_label: operator.label,
        headline,
        primary_next_step: operator.next_step.clone(),
        primary_next_command: operator.command.clone(),
        ready_for_project_scoped_capture: current_terminal_scope.ready_for_project_scoped_capture,
        scope_warning_count: scope_warnings.len(),
        safe_to_claim,
        blocked_claims,
        trust_boundary:
            "soma_projects_operator_card_is_read_only: summarizes project provenance and terminal scope only; records no capture, verification event, promotion, persona mutation, or claim that project scopes are clean",
    }
}

fn project_operator_action(
    status: &'static str,
    args: &ProjectExperienceArgs,
    scope_integrity: &ProjectScopeIntegrity,
    current_terminal_scope: &CurrentTerminalProjectScope,
) -> ProjectOperatorAction {
    if current_terminal_scope.capture_scope_status == "project_scoped_capture_storage_not_writable"
    {
        return ProjectOperatorAction {
            id: "restore_project_scope_storage_write_access",
            label: "Restore project scope storage write access",
            next_step: format!(
                "The current terminal has SOMA_SESSION_ID, SOMA_CLIENT, and SOMA_PROJECT, but the active persona DB reports storage_write_status={}; fix storage permissions or bind a writable persona store before using this terminal as live project-scope proof.",
                current_terminal_scope.storage_write_status
            ),
            command: vec!["soma".to_string(), "diagnose".to_string()],
        };
    }
    match status {
        "scope_review_required" => ProjectOperatorAction {
            id: "review_cross_project_sessions",
            label: "Review cross-project sessions",
            next_step: "Inspect cross-project session rows and run `soma session status` in active terminals before claiming project/session isolation is clean.".to_string(),
            command: scope_integrity
                .cross_project_sessions
                .first()
                .map(|session| session_context_render_command(args, &session.session_id))
                .unwrap_or_else(|| scoped_projects_command(args, &["soma", "projects", "--json"])),
        },
        "project_provenance_incomplete" => ProjectOperatorAction {
            id: "review_unscoped_project_episodes",
            label: "Review unscoped project episodes",
            next_step: "Future captures should set SOMA_PROJECT or use project-aware adapter/session setup; current unscoped episodes cannot prove project-specific learning.".to_string(),
            command: scoped_projects_command(args, &["soma", "projects", "--json"]),
        },
        "empty" => ProjectOperatorAction {
            id: "capture_project_scoped_work",
            label: "Capture project-scoped work",
            next_step: "Start a SOMA session in a project directory and capture real work so this persona has project-scoped provenance.".to_string(),
            command: vec!["soma".to_string(), "session".to_string(), "status".to_string()],
        },
        _ => ProjectOperatorAction {
            id: "inspect_project_experience_provenance",
            label: "Inspect project experience provenance",
            next_step:
                "Inspect the active persona's project-scoped experience and session isolation evidence."
                    .to_string(),
            command: scoped_projects_command(args, &["soma", "projects", "--json"]),
        },
    }
}

fn project_next_commands(
    args: &ProjectExperienceArgs,
    status: &str,
    current_terminal_scope: &CurrentTerminalProjectScope,
    scope_integrity: &ProjectScopeIntegrity,
) -> Vec<Vec<String>> {
    let mut commands = vec![scoped_projects_command(args, &["soma", "projects", "--json"])];
    if status == "scope_review_required" {
        for session in scope_integrity.cross_project_sessions.iter().take(3) {
            push_project_command_once(
                &mut commands,
                session_context_render_command(args, &session.session_id),
            );
            push_project_command_once(
                &mut commands,
                session_recall_command(args, &session.session_id),
            );
        }
    }
    if matches!(status, "scope_review_required" | "project_provenance_incomplete" | "empty") {
        commands.push(vec!["soma".to_string(), "session".to_string(), "status".to_string()]);
        if !current_terminal_scope.suggested_persona_call_commands.is_empty() {
            commands.extend(current_terminal_scope.suggested_persona_call_commands.clone());
        } else if current_terminal_scope.suggested_session_start_commands.is_empty() {
            commands.push(session_start_command(args.project.as_deref()));
        } else {
            commands.extend(current_terminal_scope.suggested_session_start_commands.clone());
        }
    }
    if current_terminal_scope.capture_scope_status == "project_scoped_capture_storage_not_writable"
    {
        push_project_command_once(&mut commands, vec!["soma".to_string(), "diagnose".to_string()]);
        push_project_command_once(
            &mut commands,
            scoped_projects_command(args, &["soma", "projects", "--current-terminal", "--json"]),
        );
    }
    commands
}

fn project_scope_review_plan(
    args: &ProjectExperienceArgs,
    status: &'static str,
    current_terminal_scope: &CurrentTerminalProjectScope,
    scope_integrity: &ProjectScopeIntegrity,
    unscoped_episode_count: usize,
    scope_warnings: &[String],
    dogfood_evidence: Option<&ProjectDogfoodEvidence>,
) -> ProjectScopeReviewPlan {
    let current_scope_ready = current_terminal_scope.ready_for_project_scoped_capture;
    let dogfood_scope_status =
        dogfood_evidence.and_then(|evidence| evidence.multi_terminal_scope_status.clone());
    let storage_write_blocked = current_terminal_scope.capture_scope_status
        == "project_scoped_capture_storage_not_writable";
    let review_status = if status == "storage_unavailable" {
        "storage_unavailable"
    } else if storage_write_blocked {
        "project_scope_storage_not_writable"
    } else if current_scope_ready && scope_warnings.is_empty() {
        "ready"
    } else if current_scope_ready {
        "historical_scope_review_required"
    } else {
        "activate_project_scope_then_review"
    };
    let headline = match review_status {
        "ready" => {
            "Current terminal scope is active and no historical scope warnings are visible."
                .to_string()
        }
        "historical_scope_review_required" => {
            "Current terminal is scoped, but historical cross-project or unscoped evidence limits clean-isolation claims."
                .to_string()
        }
        "storage_unavailable" => {
            "Project scope review is unavailable until the active persona DB can be read."
                .to_string()
        }
        "project_scope_storage_not_writable" => format!(
            "Current terminal scope exports are active, but the persona DB reports storage_write_status={}; fix storage before using new captures as proof.",
            current_terminal_scope.storage_write_status
        ),
        _ => {
            "Activate a project-scoped SOMA persona/session before using new captures as project-isolation proof."
                .to_string()
        }
    };

    let mut review_commands = vec![
        scoped_projects_command(args, &["soma", "projects", "--json"]),
        vec!["soma".to_string(), "session".to_string(), "status".to_string()],
    ];
    let cross_project_session_review_items = scope_integrity
        .cross_project_sessions
        .iter()
        .map(|session| ProjectScopeSessionReviewItem {
            source: "soma_projects.cross_project_session_review_item.v1",
            session_id: session.session_id.clone(),
            projects: session.projects.clone(),
            episode_count: session.episode_count,
            evidence_episode_ids: session.evidence_episode_ids.clone(),
            status: "cross_project_session_review_required",
            next_action: "inspect_session_context_before_claiming_scope_isolation",
            context_render_command: session_context_render_command(args, &session.session_id),
            recall_command: session_recall_command(args, &session.session_id),
            trust_boundary:
                "cross_project_session_review_item_is_read_only: commands inspect existing session evidence only; they record no capture, mutate no persona/project scope, create no verification event, and cannot relabel mixed-project evidence as clean isolation proof",
        })
        .collect::<Vec<_>>();
    for item in cross_project_session_review_items.iter().take(3) {
        push_project_command_once(&mut review_commands, item.context_render_command.clone());
        push_project_command_once(&mut review_commands, item.recall_command.clone());
    }
    if let Some(project) = nonempty_opt(args.project.as_deref())
        .or_else(|| current_terminal_scope.suggested_project.clone())
    {
        review_commands.push(project_brief_command(Some(project.as_str())));
    }

    let clean_capture_commands = if storage_write_blocked {
        current_terminal_scope.next_commands.clone()
    } else if !current_terminal_scope.suggested_persona_call_commands.is_empty() {
        current_terminal_scope.suggested_persona_call_commands.clone()
    } else if current_terminal_scope.suggested_session_start_commands.is_empty() {
        vec![session_start_command(args.project.as_deref())]
    } else {
        current_terminal_scope.suggested_session_start_commands.clone()
    };

    let mut safe_to_claim = Vec::new();
    let mut blocked_claims = Vec::new();
    if current_scope_ready {
        safe_to_claim.push(
            "New captures from this terminal can carry SOMA_SESSION_ID, SOMA_CLIENT, and SOMA_PROJECT."
                .to_string(),
        );
        safe_to_claim.push(format!(
            "The active persona DB reports storage_write_status={} for capture writes.",
            current_terminal_scope.storage_write_status
        ));
    } else if storage_write_blocked {
        blocked_claims.push(format!(
            "New captures from this terminal have project/session/client exports, but cannot prove project scope until the active persona DB is writable; storage_write_status={}.",
            current_terminal_scope.storage_write_status
        ));
    } else {
        blocked_claims.push(
            "New captures from this terminal cannot prove project scope until persona/session/project exports are active."
                .to_string(),
        );
    }
    if dogfood_scope_status.as_deref() == Some("pass") {
        safe_to_claim.push(
            "The latest dogfood artifact proves the isolated persona/project/session flow in a controlled run."
                .to_string(),
        );
    }
    if scope_integrity.cross_project_session_count > 0 {
        blocked_claims.push(format!(
            "{} historical session(s) span multiple projects; treat them as review evidence, not clean isolation proof.",
            scope_integrity.cross_project_session_count
        ));
    }
    if unscoped_episode_count > 0 {
        blocked_claims.push(format!(
            "{unscoped_episode_count} historical episode(s) are unscoped; they remain usable experience but cannot prove project-scoped learning."
        ));
    }

    ProjectScopeReviewPlan {
        source: "soma_projects.scope_review_plan.v1",
        status: review_status,
        headline,
        current_scope_ready,
        historical_warning_count: scope_warnings.len(),
        cross_project_session_count: scope_integrity.cross_project_session_count,
        unscoped_episode_count,
        dogfood_scope_status,
        cross_project_session_review_items,
        review_commands,
        clean_capture_commands,
        safe_to_claim,
        blocked_claims,
        trust_boundary:
            "project_scope_review_plan_is_read_only: explains current scope readiness and historical provenance warnings only; records no capture, mutates no persona, creates no verification event, and cannot turn unscoped or cross-project evidence into clean isolation proof",
    }
}

fn project_scope_verification_index(
    status: &'static str,
    current_terminal_scope: &CurrentTerminalProjectScope,
    scope_integrity: &ProjectScopeIntegrity,
    scope_review_plan: &ProjectScopeReviewPlan,
) -> ProjectScopeVerificationIndex {
    ProjectScopeVerificationIndex {
        source: "soma_projects.scope_verification_index.v1",
        status,
        current_scope_ready: current_terminal_scope.ready_for_project_scoped_capture,
        active_persona: current_terminal_scope.active_persona.clone(),
        current_scope_status: current_terminal_scope.capture_scope_status,
        current_client: current_terminal_scope.client.clone(),
        current_project: current_terminal_scope.project.clone(),
        current_session_id: current_terminal_scope.session_id.clone(),
        missing_scope_envs: current_terminal_scope.missing_scope_envs.clone(),
        storage_write_ready: current_terminal_scope.storage_write_ready,
        storage_write_status: current_terminal_scope.storage_write_status,
        project_provenance_status: scope_integrity.project_provenance_status,
        session_project_status: scope_integrity.session_project_status,
        cross_project_session_count: scope_integrity.cross_project_session_count,
        unscoped_episode_count: scope_review_plan.unscoped_episode_count,
        scope_review_status: scope_review_plan.status,
        dogfood_scope_status: scope_review_plan.dogfood_scope_status.clone(),
        scope_activation_commands: current_terminal_scope.next_commands.clone(),
        review_commands: scope_review_plan.review_commands.clone(),
        clean_capture_commands: scope_review_plan.clean_capture_commands.clone(),
        cross_project_session_review_items: scope_review_plan
            .cross_project_session_review_items
            .clone(),
        safe_to_claim: scope_review_plan.safe_to_claim.clone(),
        blocked_claims: scope_review_plan.blocked_claims.clone(),
        trust_boundary: "project_scope_verification_index_is_read_only: mirrors current terminal scope, project provenance integrity, and review-plan commands for automation; records no capture, mutates no persona/session/project state, creates no verification event, promotes no memory, and cannot relabel historical mixed-project evidence as clean",
    }
}

fn session_start_command(project_filter: Option<&str>) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "session".to_string(),
        "start".to_string(),
        "--client".to_string(),
        "<client>".to_string(),
    ];
    if let Some(project) = nonempty_opt(project_filter).or_else(crate::project::current_name) {
        command.extend(["--project".to_string(), project]);
    }
    command
}

fn project_brief_command(project: Option<&str>) -> Vec<String> {
    let mut command = vec!["soma".to_string(), "projects".to_string(), "--brief".to_string()];
    if let Some(project) = project.and_then(|value| nonempty_opt(Some(value))) {
        command.extend(["--project".to_string(), project]);
    }
    command
}

fn session_context_render_command(args: &ProjectExperienceArgs, session_id: &str) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "render".to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    append_project_db_path_arg(args, &mut command);
    command
}

fn session_recall_command(args: &ProjectExperienceArgs, session_id: &str) -> Vec<String> {
    let mut command = vec![
        "soma".to_string(),
        "recall".to_string(),
        "--query".to_string(),
        "project scope review".to_string(),
        "--session-id".to_string(),
        session_id.to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--limit".to_string(),
        "10".to_string(),
    ];
    append_project_db_path_arg(args, &mut command);
    command
}

fn append_project_db_path_arg(args: &ProjectExperienceArgs, command: &mut Vec<String>) {
    if let Some(db_path) = nonempty_opt(args.db_path.as_deref()) {
        command.extend(["--db-path".to_string(), db_path]);
    }
}

fn push_project_command_once(commands: &mut Vec<Vec<String>>, command: Vec<String>) {
    if !commands.iter().any(|existing| existing == &command) {
        commands.push(command);
    }
}

fn build_scope_integrity(
    unscoped_episode_count: usize,
    evidence_limit: usize,
    by_session: BTreeMap<String, SessionProjectAccum>,
) -> ProjectScopeIntegrity {
    let mut cross_project_sessions: Vec<ProjectSessionScopeRow> = by_session
        .into_iter()
        .filter_map(|(session_id, mut accum)| {
            if accum.projects.len() <= 1 {
                return None;
            }
            accum.evidence.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
            Some(ProjectSessionScopeRow {
                session_id,
                projects: accum.projects.into_iter().collect(),
                episode_count: accum.episode_count,
                evidence_episode_ids: accum
                    .evidence
                    .into_iter()
                    .take(evidence_limit)
                    .map(|(_, id)| id)
                    .collect(),
            })
        })
        .collect();
    cross_project_sessions.sort_by(|a, b| {
        b.episode_count.cmp(&a.episode_count).then_with(|| a.session_id.cmp(&b.session_id))
    });
    ProjectScopeIntegrity {
        project_provenance_status: if unscoped_episode_count == 0 {
            "complete"
        } else {
            "has_unscoped_episodes"
        },
        session_project_status: if cross_project_sessions.is_empty() {
            "single_project_sessions"
        } else {
            "cross_project_sessions_observed"
        },
        cross_project_session_count: cross_project_sessions.len(),
        cross_project_sessions,
    }
}

fn render_markdown(report: &ProjectExperienceReport) -> String {
    let mut out = String::new();
    out.push_str("# SOMA project experience\n\n");
    out.push_str(&format!("- active_persona: `{}`\n", report.active_persona));
    out.push_str(&format!("- report_persona: `{}`\n", report.report_persona));
    out.push_str(&format!("- report_persona_source: `{}`\n", report.report_persona_source));
    out.push_str(&format!("- db_path: `{}`\n", report.db_path));
    out.push_str(&format!("- storage_status: `{}`\n", report.storage_status));
    if let Some(error) = &report.storage_error {
        out.push_str(&format!("- storage_error: `{}`\n", error));
    }
    out.push_str(&format!("- status: `{}`\n", report.status));
    out.push_str(&format!(
        "- operator_next_action: `{}` ({})\n",
        report.operator_next_action_id, report.operator_next_action_label
    ));
    out.push_str(&format!("- primary_next_step: {}\n", report.primary_next_step));
    if !report.primary_next_command.is_empty() {
        out.push_str(&format!(
            "- primary_next_command: `{}`\n",
            report.primary_next_command.join(" ")
        ));
    }
    out.push_str(&format!("- project_count: `{}`\n", report.project_count));
    out.push_str(&format!("- scoped_episode_count: `{}`\n", report.scoped_episode_count));
    out.push_str(&format!("- unscoped_episode_count: `{}`\n\n", report.unscoped_episode_count));
    if let Some(evidence) = &report.dogfood_evidence {
        out.push_str("## Dogfood evidence\n\n");
        out.push_str(&format!("- status: `{}`\n", evidence.status));
        out.push_str(&format!(
            "- report_status: `{}`\n",
            evidence.report_status.as_deref().unwrap_or("unknown")
        ));
        out.push_str(&format!(
            "- multi_terminal_persona_project_scope: `{}`\n",
            evidence.multi_terminal_scope_status.as_deref().unwrap_or("unknown")
        ));
        out.push_str(&format!(
            "- summary: `pass={} warn={} fail={}`\n",
            evidence.summary_pass.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            evidence.summary_warn.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            evidence.summary_fail.map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ));
        out.push_str(&format!("- path: `{}`\n", evidence.path));
        out.push_str(&format!("- trust_boundary: `{}`\n\n", evidence.trust_boundary));
    }
    render_current_terminal_scope_markdown(&mut out, report);
    if !report.scope_warnings.is_empty() {
        out.push_str("## Scope warnings\n\n");
        for warning in &report.scope_warnings {
            out.push_str(&format!("- {warning}\n"));
        }
        out.push('\n');
    }
    render_scope_contract_markdown(&mut out, &report.scope_contract);
    render_scope_review_plan_markdown(&mut out, &report.scope_review_plan);
    if !report.next_commands.is_empty() {
        out.push_str("## Next commands\n\n");
        for command in &report.next_commands {
            out.push_str(&format!("- `{}`\n", command.join(" ")));
        }
        out.push('\n');
    }
    if !report.recovery_commands.is_empty() {
        out.push_str("## Recovery\n\n");
        for command in &report.recovery_commands {
            out.push_str(&format!("- `{}`\n", command.join(" ")));
        }
        out.push('\n');
    }
    out.push_str("## Scope integrity\n\n");
    out.push_str(&format!(
        "- project_provenance_status: `{}`\n",
        report.scope_integrity.project_provenance_status
    ));
    out.push_str(&format!(
        "- session_project_status: `{}`\n",
        report.scope_integrity.session_project_status
    ));
    out.push_str(&format!(
        "- cross_project_session_count: `{}`\n",
        report.scope_integrity.cross_project_session_count
    ));
    for session in &report.scope_integrity.cross_project_sessions {
        out.push_str(&format!(
            "- cross_project_session: `{}` projects=`{}` evidence_episode_ids=`{}`\n",
            session.session_id,
            session.projects.join(", "),
            session.evidence_episode_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    out.push('\n');
    if report.projects.is_empty() {
        out.push_str("_No project-scoped episodes found in this persona store._\n");
        return out;
    }
    for project in &report.projects {
        out.push_str(&format!("## {}\n\n", project.project));
        out.push_str(&format!(
            "- episodes: `{}` across `{}` session(s)\n",
            project.episode_count, project.session_count
        ));
        out.push_str(&format!("- first_seen_ns: `{}`\n", project.first_seen_ns));
        out.push_str(&format!("- last_seen_ns: `{}`\n", project.last_seen_ns));
        out.push_str(&format!("- sources: `{}`\n", map_summary(&project.source_counts)));
        out.push_str(&format!("- memory_tiers: `{}`\n", map_summary(&project.memory_tier_counts)));
        if !project.git_branches.is_empty() {
            out.push_str(&format!("- git_branches: `{}`\n", project.git_branches.join(", ")));
        }
        if !project.recent_sessions.is_empty() {
            out.push_str(&format!("- recent_sessions: `{}`\n", project.recent_sessions.join(", ")));
        }
        if !project.cwd_samples.is_empty() {
            out.push_str(&format!("- cwd_samples: `{}`\n", project.cwd_samples.join(", ")));
        }
        out.push_str(&format!(
            "- evidence_episode_ids: `{}`\n\n",
            project.evidence_episode_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

fn render_scope_review_plan_markdown(out: &mut String, plan: &ProjectScopeReviewPlan) {
    out.push_str("## Scope review plan\n\n");
    out.push_str(&format!("- status: `{}`\n", plan.status));
    out.push_str(&format!("- headline: {}\n", plan.headline));
    out.push_str(&format!("- current_scope_ready: `{}`\n", plan.current_scope_ready));
    out.push_str(&format!("- historical_warning_count: `{}`\n", plan.historical_warning_count));
    out.push_str(&format!(
        "- dogfood_scope_status: `{}`\n",
        plan.dogfood_scope_status.as_deref().unwrap_or("unknown")
    ));
    if !plan.safe_to_claim.is_empty() {
        out.push_str("- safe_to_claim:\n");
        for claim in &plan.safe_to_claim {
            out.push_str(&format!("  - {claim}\n"));
        }
    }
    if !plan.blocked_claims.is_empty() {
        out.push_str("- blocked_claims:\n");
        for claim in &plan.blocked_claims {
            out.push_str(&format!("  - {claim}\n"));
        }
    }
    if !plan.review_commands.is_empty() {
        out.push_str("- review_commands:\n");
        for command in &plan.review_commands {
            out.push_str(&format!("  - `{}`\n", command.join(" ")));
        }
    }
    if !plan.cross_project_session_review_items.is_empty() {
        out.push_str("- cross_project_session_review_items:\n");
        for item in &plan.cross_project_session_review_items {
            out.push_str(&format!(
                "  - session=`{}` projects=`{}` context=`{}` recall=`{}`\n",
                item.session_id,
                item.projects.join(", "),
                item.context_render_command.join(" "),
                item.recall_command.join(" ")
            ));
        }
    }
    if !plan.clean_capture_commands.is_empty() {
        out.push_str("- clean_capture_commands:\n");
        for command in &plan.clean_capture_commands {
            out.push_str(&format!("  - `{}`\n", command.join(" ")));
        }
    }
    out.push_str(&format!("- trust_boundary: `{}`\n\n", plan.trust_boundary));
}

fn render_brief(report: &ProjectExperienceReport) -> String {
    let mut out = String::new();
    let scope = &report.current_terminal_scope;
    out.push_str("SOMA project experience brief\n");
    out.push_str(&format!(
        "  Status: {} - {}\n",
        report.operator_card.status, report.operator_card.headline
    ));
    out.push_str(&format!(
        "  Next action: {} ({})\n",
        report.operator_next_action_id, report.operator_next_action_label
    ));
    out.push_str(&format!("  Why: {}\n", report.primary_next_step));
    if !report.primary_next_command.is_empty() {
        out.push_str(&format!("  Command: {}\n", report.primary_next_command.join(" ")));
    }
    out.push_str(&format!(
        "  Persona store: report={} source={} db={} storage={}\n",
        report.report_persona, report.report_persona_source, report.db_path, report.storage_status
    ));
    out.push_str(&format!("    current_shell_persona={}\n", scope.active_persona));
    if let Some(error) = report.storage_error.as_deref() {
        out.push_str(&format!("    storage_error: {error}\n"));
    }
    out.push_str(&format!(
        "  Current scope: status={} ready={} client={} project={} session={} thread={}\n",
        scope.capture_scope_status,
        scope.ready_for_project_scoped_capture,
        scope.client.as_deref().unwrap_or("unset"),
        scope.project.as_deref().unwrap_or("unset"),
        scope.session_id.as_deref().unwrap_or("unset"),
        scope.thread_key.as_deref().unwrap_or("unset")
    ));
    out.push_str(&format!(
        "    storage_write_status={} storage_write_ready={}\n",
        scope.storage_write_status, scope.storage_write_ready
    ));
    out.push_str(&format!(
        "    persona_activation={} db_source={} db_matches_report={}\n",
        scope.persona_activation_status, scope.db_path_source, scope.db_path_matches_report
    ));
    if !scope.missing_scope_envs.is_empty() {
        out.push_str(&format!("    missing_env: {}\n", scope.missing_scope_envs.join(",")));
    }
    if let Some(project) = scope.suggested_project.as_deref() {
        out.push_str(&format!("    suggested_project: {project}\n"));
    }
    if !scope.suggested_clients.is_empty() {
        out.push_str(&format!("    suggested_clients: {}\n", scope.suggested_clients.join(",")));
    }
    for command in &scope.suggested_persona_call_commands {
        out.push_str(&format!("    call: {}\n", command.join(" ")));
    }
    for command in &scope.suggested_session_start_commands {
        out.push_str(&format!("    start: {}\n", command.join(" ")));
    }
    let contract = &report.scope_contract;
    out.push_str(&format!(
        "  Scope contract: persona_store={} project_provenance={} per_project_store={} ready_for_project_scoped_capture={} storage_write_required={} storage_write_ready={} storage_write_status={}\n",
        contract.persona_is_storage_boundary,
        contract.project_is_metadata_inside_persona,
        contract.project_creates_separate_store,
        contract.ready_for_project_scoped_capture,
        contract.storage_write_required_for_capture,
        contract.storage_write_ready,
        contract.storage_write_status
    ));
    out.push_str(&format!("    persona_store: {}\n", contract.persona_store_contract));
    out.push_str(&format!("    project_provenance: {}\n", contract.project_provenance_contract));
    if !contract.missing_scope_envs.is_empty() {
        out.push_str(&format!(
            "    missing_scope_env: {}\n",
            contract.missing_scope_envs.join(",")
        ));
    }
    out.push_str(&format!(
        "  Provenance: projects={} scoped_episodes={} unscoped_episodes={}\n",
        report.project_count, report.scoped_episode_count, report.unscoped_episode_count
    ));
    out.push_str(&format!(
        "  Scope integrity: project={} session={} cross_project_sessions={}\n",
        report.scope_integrity.project_provenance_status,
        report.scope_integrity.session_project_status,
        report.scope_integrity.cross_project_session_count
    ));
    for warning in report.scope_warnings.iter().take(3) {
        out.push_str(&format!("  Scope warning: {warning}\n"));
    }
    let plan = &report.scope_review_plan;
    out.push_str(&format!(
        "  Scope review plan: status={} current_scope_ready={} historical_warnings={} dogfood_scope={}\n",
        plan.status,
        plan.current_scope_ready,
        plan.historical_warning_count,
        plan.dogfood_scope_status.as_deref().unwrap_or("unknown")
    ));
    out.push_str(&format!("    plan: {}\n", plan.headline));
    for claim in plan.safe_to_claim.iter().take(2) {
        out.push_str(&format!("    safe: {claim}\n"));
    }
    for claim in plan.blocked_claims.iter().take(3) {
        out.push_str(&format!("    blocked: {claim}\n"));
    }
    for command in plan.review_commands.iter().take(2) {
        out.push_str(&format!("    review: {}\n", command.join(" ")));
    }
    for item in plan.cross_project_session_review_items.iter().take(3) {
        out.push_str(&format!(
            "    review session: {} projects={} context={}\n",
            item.session_id,
            item.projects.join(","),
            item.context_render_command.join(" ")
        ));
    }
    for command in &plan.clean_capture_commands {
        out.push_str(&format!("    clean capture: {}\n", command.join(" ")));
    }
    if let Some(evidence) = &report.dogfood_evidence {
        let report_status = evidence.report_status.as_deref().unwrap_or("unknown");
        let scope_status = evidence.multi_terminal_scope_status.as_deref().unwrap_or("unknown");
        let pass =
            evidence.summary_pass.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        let warn =
            evidence.summary_warn.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        let fail =
            evidence.summary_fail.map_or_else(|| "unknown".to_string(), |value| value.to_string());
        out.push_str(&format!(
            "  Dogfood artifact: status={} report={} multi_terminal_persona_project_scope={} summary=pass={} warn={} fail={} path={}\n",
            evidence.status, report_status, scope_status, pass, warn, fail, evidence.path
        ));
        out.push_str(
            "    note: last-run dogfood evidence only; does not prove live storage, current terminal scope, or clean project/session isolation.\n",
        );
    }
    for session in report.scope_integrity.cross_project_sessions.iter().take(3) {
        out.push_str(&format!(
            "    cross_project_session: {} projects={} evidence={}\n",
            session.session_id,
            session.projects.join(","),
            session.evidence_episode_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        ));
    }
    if !report.projects.is_empty() {
        out.push_str("  Top projects:\n");
        for project in report.projects.iter().take(5) {
            out.push_str(&format!(
                "    - {} episodes={} sessions={} sources={} recent_sessions={}\n",
                project.project,
                project.episode_count,
                project.session_count,
                map_summary(&project.source_counts),
                project.recent_sessions.join(",")
            ));
        }
    }
    if !report.recovery_commands.is_empty() {
        for command in report.recovery_commands.iter().take(3) {
            out.push_str(&format!("  Recovery: {}\n", command.join(" ")));
        }
    } else if !report.next_commands.is_empty() {
        for command in report.next_commands.iter().take(3) {
            out.push_str(&format!("  Next: {}\n", command.join(" ")));
        }
    }
    out.push_str(
        "  Trust boundary: read-only project provenance and terminal scope only; records no capture, persona mutation, verification event, promotion, or cloud draft.\n",
    );
    out
}

fn render_current_terminal_scope_only(report: &ProjectExperienceReport) -> String {
    let scope = &report.current_terminal_scope;
    let contract = &report.scope_contract;
    let mut out = String::new();
    out.push_str("SOMA current terminal scope\n");
    out.push_str(&format!(
        "  Status: {} ready={}\n",
        scope.capture_scope_status, scope.ready_for_project_scoped_capture
    ));
    out.push_str(&format!(
        "  Storage write: status={} ready={}\n",
        scope.storage_write_status, scope.storage_write_ready
    ));
    out.push_str(&format!(
        "  Persona: active={} activation={} db={} source={} matches_report={}\n",
        scope.active_persona,
        scope.persona_activation_status,
        scope.db_path,
        scope.db_path_source,
        scope.db_path_matches_report
    ));
    out.push_str(&format!(
        "  Scope: client={} project={} session={} thread={}\n",
        scope.client.as_deref().unwrap_or("unset"),
        scope.project.as_deref().unwrap_or("unset"),
        scope.session_id.as_deref().unwrap_or("unset"),
        scope.thread_key.as_deref().unwrap_or("unset")
    ));
    if !scope.missing_scope_envs.is_empty() {
        out.push_str(&format!("  Missing env: {}\n", scope.missing_scope_envs.join(",")));
    }
    if !scope.warnings.is_empty() {
        for warning in scope.warnings.iter().take(4) {
            out.push_str(&format!("  Warning: {warning}\n"));
        }
    }
    if !scope.next_commands.is_empty() {
        for command in scope.next_commands.iter().take(4) {
            out.push_str(&format!("  Next: {}\n", command.join(" ")));
        }
    }
    out.push_str(&format!(
        "  Contract: persona_store={} project_metadata_inside_persona={} per_project_store={} storage_write_required={} storage_write_ready={} storage_write_status={} required_env={}\n",
        contract.persona_is_storage_boundary,
        contract.project_is_metadata_inside_persona,
        contract.project_creates_separate_store,
        contract.storage_write_required_for_capture,
        contract.storage_write_ready,
        contract.storage_write_status,
        contract.required_scope_envs.join(",")
    ));
    out.push_str(
        "  Trust boundary: read-only terminal scope view; records no capture, persona mutation, verification event, promotion, or cloud draft.\n",
    );
    out
}

fn render_current_terminal_scope_json(
    report: &ProjectExperienceReport,
) -> Result<String, ProjectExperienceError> {
    let value = serde_json::json!({
        "schema": "soma.project_current_terminal_scope.v1",
        "source": "soma_projects_current_terminal_scope",
        "status": report.current_terminal_scope.capture_scope_status,
        "ready_for_project_scoped_capture": report
            .current_terminal_scope
            .ready_for_project_scoped_capture,
        "storage_write_ready": report.current_terminal_scope.storage_write_ready,
        "storage_write_status": report.current_terminal_scope.storage_write_status,
        "active_persona": report.current_terminal_scope.active_persona,
        "db_path": report.current_terminal_scope.db_path,
        "current_terminal_scope": report.current_terminal_scope,
        "scope_contract": report.scope_contract,
        "operator_next_action_id": report.operator_next_action_id,
        "operator_next_action_label": report.operator_next_action_label,
        "primary_next_step": report.primary_next_step,
        "primary_next_command": report.primary_next_command,
        "next_commands": report.current_terminal_scope.next_commands,
        "trust_boundary": "soma_project_current_terminal_scope_is_read_only: reports current process persona/project/session exports and scope contract only; records no capture, mutates no persona, creates no verification event, promotes no memory, and cannot relabel historical sessions as clean",
    });
    let mut text = serde_json::to_string_pretty(&value)?;
    text.push('\n');
    Ok(text)
}

fn current_terminal_scope(
    report_db_path: &str,
    project_filter: Option<&str>,
    report_persona: &str,
) -> CurrentTerminalProjectScope {
    let active_persona = active_persona();
    let db_env = env_nonempty(DB_ENV);
    let default_db_path = default_db_path();
    let db_path = db_env.clone().or(default_db_path).unwrap_or_else(|| "<unresolved>".to_string());
    let db_path_source = if db_env.is_some() { DB_ENV } else { "default_home" };
    let persona_home = env_nonempty(PERSONA_HOME_ENV).or_else(|| {
        if let Some(db) = db_env.as_deref() {
            return PathBuf::from(db).parent().map(|path| path.display().to_string());
        }
        default_soma_home()
    });
    let adapter_spool_jsonl = env_nonempty(ADAPTER_SPOOL_JSONL_ENV).or_else(|| {
        persona_home.as_ref().map(|home| {
            PathBuf::from(home).join("adapter").join("events.jsonl").display().to_string()
        })
    });
    let session_id = env_nonempty(SESSION_ENV);
    let client = env_nonempty(CLIENT_ENV);
    let project = env_nonempty(PROJECT_ENV);
    let thread_key = env_nonempty(THREAD_ENV);
    let mut missing_scope_envs = Vec::new();
    if session_id.is_none() {
        missing_scope_envs.push(SESSION_ENV);
    }
    if client.is_none() {
        missing_scope_envs.push(CLIENT_ENV);
    }
    if project.is_none() {
        missing_scope_envs.push(PROJECT_ENV);
    }
    let db_path_matches_report = same_path_string(&db_path, report_db_path);
    let project_filter_matches_current =
        project_filter.map(|filter| project.as_deref() == Some(filter));
    let persona_activation_status = match (env_nonempty(PERSONA_ENV).is_some(), db_env.is_some()) {
        (true, true) => "active_persona_isolated_store",
        (true, false) => "active_persona_missing_soma_db_export",
        (false, true) => "db_path_override_without_persona_name",
        (false, false) => "default_persona_store",
    };
    let mut warnings = Vec::new();
    if !db_path_matches_report {
        warnings.push(format!(
            "current terminal DB `{db_path}` does not match this project report DB `{report_db_path}`"
        ));
    }
    if session_id.is_none() {
        warnings
            .push(format!("{SESSION_ENV} is unset; terminal capture cannot prove a session lane"));
    }
    if client.is_none() {
        warnings
            .push(format!("{CLIENT_ENV} is unset; terminal capture cannot prove the client lane"));
    }
    if project.is_none() {
        warnings.push(format!(
            "{PROJECT_ENV} is unset; terminal capture cannot prove project-scoped learning"
        ));
    }
    if project_filter_matches_current == Some(false) {
        warnings.push(format!(
            "current terminal project `{}` does not match requested project filter `{}`",
            project.as_deref().unwrap_or("<unset>"),
            project_filter.unwrap_or("<unset>")
        ));
    }
    if persona_activation_status == "active_persona_missing_soma_db_export" {
        warnings.push(format!(
            "{PERSONA_ENV} is set but {DB_ENV} is missing; run `eval \"$(soma call {active_persona})\"` to bind the persona store"
        ));
    }
    let storage_write_status = project_scope_storage_write_status(&db_path);
    let storage_write_ready = project_scope_storage_write_ready(storage_write_status);
    if !storage_write_ready {
        warnings.push(format!(
            "current terminal DB `{db_path}` reports storage_write_status={storage_write_status}; capture may fail before it can prove project scope"
        ));
    }
    let capture_scope_status = if !db_path_matches_report {
        "persona_db_mismatch"
    } else if project_filter_matches_current == Some(false) {
        "project_filter_mismatch"
    } else if session_id.is_some() && client.is_some() && project.is_some() && !storage_write_ready
    {
        "project_scoped_capture_storage_not_writable"
    } else if session_id.is_some() && client.is_some() && project.is_some() {
        "project_scoped_capture_ready"
    } else {
        "capture_scope_incomplete"
    };
    let ready_for_project_scoped_capture = capture_scope_status == "project_scoped_capture_ready";
    let suggested_project = nonempty_opt(project_filter).or_else(crate::project::current_name);
    let suggested_clients = suggested_session_clients(client.as_deref());
    let client_choice_required = client.is_none();
    let suggested_persona_call_commands = suggested_persona_call_commands(
        capture_scope_status,
        report_persona,
        &suggested_clients,
        suggested_project.as_deref(),
    );
    let suggested_session_start_commands = suggested_session_start_commands(
        capture_scope_status,
        &suggested_clients,
        suggested_project.as_deref(),
    );
    let next_commands = current_terminal_next_commands(
        capture_scope_status,
        project_filter,
        &suggested_persona_call_commands,
        &suggested_session_start_commands,
    );
    CurrentTerminalProjectScope {
        active_persona,
        persona_activation_status,
        db_path,
        db_path_source,
        db_path_matches_report,
        persona_home,
        adapter_spool_jsonl,
        session_id,
        client,
        project,
        thread_key,
        project_filter_matches_current,
        capture_scope_status,
        ready_for_project_scoped_capture,
        storage_write_ready,
        storage_write_status,
        missing_scope_envs,
        warnings,
        suggested_project,
        client_choice_required,
        suggested_clients,
        suggested_persona_call_commands,
        suggested_session_start_commands,
        next_commands,
    }
}

fn project_persona_scope_contract(
    scope: &CurrentTerminalProjectScope,
) -> ProjectPersonaScopeContract {
    ProjectPersonaScopeContract {
        source: "soma_projects.scope_contract.v1",
        persona_store_contract:
            "A persona/profile owns the isolated local SOMA database and adapter spool.",
        project_provenance_contract:
            "A project is provenance metadata inside the active persona store, not a separate memory store.",
        session_scope_contract:
            "A terminal/client session is project-scoped only when SOMA_SESSION_ID, SOMA_CLIENT, and SOMA_PROJECT are exported together.",
        persona_is_storage_boundary: true,
        project_is_metadata_inside_persona: true,
        project_creates_separate_store: false,
        ready_for_project_scoped_capture: scope.ready_for_project_scoped_capture,
        storage_write_required_for_capture: true,
        storage_write_ready: scope.storage_write_ready,
        storage_write_status: scope.storage_write_status,
        required_scope_envs: vec![SESSION_ENV, CLIENT_ENV, PROJECT_ENV],
        missing_scope_envs: scope.missing_scope_envs.clone(),
        active_persona: scope.active_persona.clone(),
        db_path: scope.db_path.clone(),
        current_project: scope.project.clone(),
        current_session_id: scope.session_id.clone(),
        trust_boundary:
            "project_persona_scope_contract_is_read_only: describes persona/project/session boundaries only; records no capture, mutates no persona, creates no verification event, and does not claim historical project isolation",
    }
}

fn render_current_terminal_scope_markdown(out: &mut String, report: &ProjectExperienceReport) {
    let scope = &report.current_terminal_scope;
    out.push_str("## Current terminal scope\n\n");
    out.push_str(&format!("- active_persona: `{}`\n", scope.active_persona));
    out.push_str(&format!("- persona_activation_status: `{}`\n", scope.persona_activation_status));
    out.push_str(&format!("- capture_scope_status: `{}`\n", scope.capture_scope_status));
    out.push_str(&format!(
        "- ready_for_project_scoped_capture: `{}`\n",
        scope.ready_for_project_scoped_capture
    ));
    out.push_str(&format!("- storage_write_ready: `{}`\n", scope.storage_write_ready));
    out.push_str(&format!("- storage_write_status: `{}`\n", scope.storage_write_status));
    if !scope.missing_scope_envs.is_empty() {
        out.push_str(&format!("- missing_scope_envs: `{}`\n", scope.missing_scope_envs.join(", ")));
    }
    out.push_str(&format!("- db_path: `{}`\n", scope.db_path));
    out.push_str(&format!("- db_path_source: `{}`\n", scope.db_path_source));
    out.push_str(&format!("- db_path_matches_report: `{}`\n", scope.db_path_matches_report));
    out.push_str(&format!(
        "- session_id: `{}`\n",
        scope.session_id.as_deref().unwrap_or("<unset>")
    ));
    out.push_str(&format!("- client: `{}`\n", scope.client.as_deref().unwrap_or("<unset>")));
    out.push_str(&format!("- project: `{}`\n", scope.project.as_deref().unwrap_or("<unset>")));
    out.push_str(&format!(
        "- thread_key: `{}`\n",
        scope.thread_key.as_deref().unwrap_or("<unset>")
    ));
    if let Some(matches) = scope.project_filter_matches_current {
        out.push_str(&format!("- project_filter_matches_current: `{matches}`\n"));
    }
    if !scope.warnings.is_empty() {
        out.push_str("- warnings:\n");
        for warning in &scope.warnings {
            out.push_str(&format!("  - {warning}\n"));
        }
    }
    if !scope.next_commands.is_empty() {
        out.push_str("- next_commands:\n");
        for command in &scope.next_commands {
            out.push_str(&format!("  - `{}`\n", command.join(" ")));
        }
    }
    if !scope.suggested_persona_call_commands.is_empty() {
        out.push_str("- suggested_persona_call_commands:\n");
        for command in &scope.suggested_persona_call_commands {
            out.push_str(&format!("  - `{}`\n", command.join(" ")));
        }
    }
    if !scope.suggested_session_start_commands.is_empty() {
        out.push_str("- suggested_session_start_commands:\n");
        for command in &scope.suggested_session_start_commands {
            out.push_str(&format!("  - `{}`\n", command.join(" ")));
        }
    }
    out.push('\n');
}

fn render_scope_contract_markdown(out: &mut String, contract: &ProjectPersonaScopeContract) {
    out.push_str("## Scope contract\n\n");
    out.push_str(&format!("- persona_store_contract: `{}`\n", contract.persona_store_contract));
    out.push_str(&format!(
        "- project_provenance_contract: `{}`\n",
        contract.project_provenance_contract
    ));
    out.push_str(&format!("- session_scope_contract: `{}`\n", contract.session_scope_contract));
    out.push_str(&format!(
        "- persona_is_storage_boundary: `{}`\n",
        contract.persona_is_storage_boundary
    ));
    out.push_str(&format!(
        "- project_is_metadata_inside_persona: `{}`\n",
        contract.project_is_metadata_inside_persona
    ));
    out.push_str(&format!(
        "- project_creates_separate_store: `{}`\n",
        contract.project_creates_separate_store
    ));
    out.push_str(&format!(
        "- ready_for_project_scoped_capture: `{}`\n",
        contract.ready_for_project_scoped_capture
    ));
    out.push_str(&format!(
        "- storage_write_required_for_capture: `{}`\n",
        contract.storage_write_required_for_capture
    ));
    out.push_str(&format!("- storage_write_ready: `{}`\n", contract.storage_write_ready));
    out.push_str(&format!("- storage_write_status: `{}`\n", contract.storage_write_status));
    out.push_str(&format!("- active_persona: `{}`\n", contract.active_persona));
    out.push_str(&format!("- db_path: `{}`\n", contract.db_path));
    if let Some(project) = contract.current_project.as_deref() {
        out.push_str(&format!("- current_project: `{project}`\n"));
    }
    if let Some(session_id) = contract.current_session_id.as_deref() {
        out.push_str(&format!("- current_session_id: `{session_id}`\n"));
    }
    out.push_str(&format!(
        "- required_scope_envs: `{}`\n",
        contract.required_scope_envs.join(", ")
    ));
    if !contract.missing_scope_envs.is_empty() {
        out.push_str(&format!(
            "- missing_scope_envs: `{}`\n",
            contract.missing_scope_envs.join(", ")
        ));
    }
    out.push_str(&format!("- trust_boundary: `{}`\n\n", contract.trust_boundary));
}

fn suggested_session_clients(current_client: Option<&str>) -> Vec<String> {
    if let Some(client) = nonempty_opt(current_client) {
        return vec![client];
    }
    vec![
        "codex-cli".to_string(),
        "claude-code".to_string(),
        "codex-app".to_string(),
        "terminal".to_string(),
    ]
}

fn suggested_session_start_commands(
    status: &str,
    clients: &[String],
    project: Option<&str>,
) -> Vec<Vec<String>> {
    if !matches!(status, "capture_scope_incomplete" | "project_filter_mismatch") {
        return Vec::new();
    }
    clients
        .iter()
        .map(|client| {
            let mut start = format!("soma session start --client {client}");
            if let Some(project) = nonempty_opt(project) {
                start.push_str(" --project ");
                start.push_str(&project);
            }
            vec!["eval".to_string(), format!("\"$({start})\"")]
        })
        .collect()
}

fn suggested_persona_call_commands(
    status: &str,
    report_persona: &str,
    clients: &[String],
    project: Option<&str>,
) -> Vec<Vec<String>> {
    if !matches!(
        status,
        "capture_scope_incomplete" | "project_filter_mismatch" | "persona_db_mismatch"
    ) {
        return Vec::new();
    }
    let persona = nonempty_opt(Some(report_persona)).unwrap_or_else(|| "<persona>".to_string());
    clients
        .iter()
        .map(|client| {
            let mut call = format!("soma call {persona} --client {client}");
            if let Some(project) = nonempty_opt(project) {
                call.push_str(" --project ");
                call.push_str(&project);
            }
            vec!["eval".to_string(), format!("\"$({call})\"")]
        })
        .collect()
}

fn current_terminal_next_commands(
    status: &str,
    project_filter: Option<&str>,
    suggested_persona_call_commands: &[Vec<String>],
    suggested_session_start_commands: &[Vec<String>],
) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    if !suggested_persona_call_commands.is_empty() {
        commands.extend(suggested_persona_call_commands.iter().cloned());
    } else if !suggested_session_start_commands.is_empty() {
        commands.extend(suggested_session_start_commands.iter().cloned());
    } else if matches!(status, "capture_scope_incomplete" | "project_filter_mismatch") {
        let mut start = "soma session start --client <client>".to_string();
        if let Some(project) = nonempty_opt(project_filter).or_else(crate::project::current_name) {
            start.push_str(" --project ");
            start.push_str(&project);
        }
        commands.push(vec!["eval".to_string(), format!("\"$({start})\"")]);
    }
    if status == "persona_db_mismatch" && suggested_persona_call_commands.is_empty() {
        commands.push(vec!["eval".to_string(), "\"$(soma call <persona>)\"".to_string()]);
    }
    if status == "project_scoped_capture_storage_not_writable" {
        commands.push(vec!["soma".to_string(), "diagnose".to_string()]);
        commands.push(vec![
            "soma".to_string(),
            "projects".to_string(),
            "--current-terminal".to_string(),
            "--json".to_string(),
        ]);
    }
    commands.push(vec!["soma".to_string(), "session".to_string(), "status".to_string()]);
    commands
}

fn report_persona_for_db_path(db_path: &Path) -> ReportPersonaRef {
    if default_db_path()
        .as_deref()
        .is_some_and(|default_db| same_path(db_path, Path::new(default_db)))
    {
        return ReportPersonaRef {
            name: DEFAULT_PERSONA.to_string(),
            source: "default_home_db_path",
        };
    }
    if let Some(name) = persona_name_from_registry_path(db_path) {
        return ReportPersonaRef { name, source: "persona_registry_db_path" };
    }
    if let Some(name) = persona_name_from_metadata(db_path) {
        return ReportPersonaRef { name, source: "persona_metadata" };
    }
    if let (Some(persona), Some(persona_home)) =
        (env_nonempty(PERSONA_ENV), env_nonempty(PERSONA_HOME_ENV))
    {
        let persona_db = PathBuf::from(persona_home).join(DB_FILE);
        if same_path(db_path, &persona_db) {
            return ReportPersonaRef { name: persona, source: "current_shell_persona_home" };
        }
    }
    if let (Some(persona), Some(db_env)) = (env_nonempty(PERSONA_ENV), env_nonempty(DB_ENV)) {
        if same_path(db_path, Path::new(&db_env)) {
            return ReportPersonaRef { name: persona, source: "current_shell_soma_db" };
        }
    }
    ReportPersonaRef {
        name: active_persona(),
        source: "current_shell_fallback_for_unrecognized_db_path",
    }
}

fn persona_name_from_registry_path(db_path: &Path) -> Option<String> {
    let personas_dir = personas_dir_path()?;
    let resolved_db_path = canonical_or_raw(db_path);
    let resolved_personas_dir = canonical_or_raw(&personas_dir);
    let relative = resolved_db_path.strip_prefix(&resolved_personas_dir).ok()?;
    let mut components = relative.components();
    let name = components.next()?.as_os_str().to_str()?;
    let db_file = components.next()?.as_os_str().to_str()?;
    if components.next().is_some() || db_file != DB_FILE {
        return None;
    }
    nonempty(name)
}

fn persona_name_from_metadata(db_path: &Path) -> Option<String> {
    if db_path.file_name().and_then(|value| value.to_str()) != Some(DB_FILE) {
        return None;
    }
    let metadata_path = db_path.parent()?.join(METADATA_FILE);
    let text = std::fs::read_to_string(metadata_path).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    json.get("name").and_then(Value::as_str).and_then(nonempty)
}

fn personas_dir_path() -> Option<PathBuf> {
    env_nonempty(PERSONAS_DIR_ENV)
        .map(PathBuf::from)
        .or_else(|| default_soma_home().map(|home| PathBuf::from(home).join("personas")))
}

fn same_path_string(left: &str, right: &str) -> bool {
    left == right || same_path(Path::new(left), Path::new(right))
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_or_raw(left) == canonical_or_raw(right)
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn project_scope_storage_write_status(db_path: &str) -> &'static str {
    let trimmed = db_path.trim();
    if trimmed.is_empty() || trimmed == "<unresolved>" {
        return "db_path_unresolved";
    }
    let path = Path::new(trimmed);
    if path.exists() {
        if !path.is_file() {
            return "not_regular_file";
        }
        return if path_is_writable_without_mutation(path) {
            "writable_unproven"
        } else {
            "not_writable"
        };
    }
    match path.parent() {
        Some(parent) if parent.exists() => {
            if path_is_writable_without_mutation(parent) {
                "parent_writable_unproven"
            } else {
                "parent_not_writable"
            }
        }
        Some(_) | None => "parent_missing",
    }
}

fn project_scope_storage_write_ready(status: &str) -> bool {
    matches!(status, "writable_unproven" | "parent_writable_unproven")
}

#[cfg(unix)]
fn path_is_writable_without_mutation(path: &Path) -> bool {
    std::process::Command::new("/bin/test")
        .arg("-w")
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn path_is_writable_without_mutation(path: &Path) -> bool {
    std::fs::metadata(path).map(|metadata| !metadata.permissions().readonly()).unwrap_or(false)
}

fn projects_storage_diagnostic_db_path() -> String {
    std::env::temp_dir()
        .join(format!("soma-projects-diagnostic-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

fn projects_storage_diagnostic_command() -> Vec<String> {
    vec![
        "soma".to_string(),
        "projects".to_string(),
        "--db-path".to_string(),
        projects_storage_diagnostic_db_path(),
        "--brief".to_string(),
    ]
}

fn projects_storage_recovery_commands(project_filter: Option<&str>) -> Vec<Vec<String>> {
    vec![
        sandbox_persona_activation_command(project_filter),
        vec!["soma".to_string(), "projects".to_string(), "--brief".to_string()],
        projects_storage_diagnostic_command(),
        vec![
            "soma".to_string(),
            "projects".to_string(),
            "--db-path".to_string(),
            "<readable-soma.db>".to_string(),
            "--brief".to_string(),
        ],
        vec!["soma".to_string(), "diagnose".to_string()],
    ]
}

fn sandbox_persona_activation_command(project_filter: Option<&str>) -> Vec<String> {
    let project = nonempty_opt(project_filter)
        .or_else(crate::project::current_name)
        .unwrap_or_else(|| "<project>".to_string());
    vec![
        "eval".to_string(),
        format!(
            "\"$(SOMA_PERSONAS_DIR=\"$PWD/.soma/personas\" soma call codex-app --create --client codex-cli --project {project})\""
        ),
    ]
}

fn scoped_projects_command(args: &ProjectExperienceArgs, base: &[&str]) -> Vec<String> {
    let mut command = base.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
    if let Some(db_path) = nonempty_opt(args.db_path.as_deref()) {
        command.extend(["--db-path".to_string(), db_path]);
    }
    if let Some(project) = nonempty_opt(args.project.as_deref()) {
        command.extend(["--project".to_string(), project]);
    }
    command
}

fn map_summary(map: &BTreeMap<String, usize>) -> String {
    if map.is_empty() {
        return "-".to_string();
    }
    map.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", ")
}

fn active_persona() -> String {
    std::env::var(PERSONA_ENV)
        .ok()
        .and_then(|value| nonempty_opt(Some(&value)))
        .unwrap_or_else(|| "default".to_string())
}

fn default_db_path() -> Option<String> {
    default_soma_home().map(|home| PathBuf::from(home).join("soma.db").display().to_string())
}

fn default_soma_home() -> Option<String> {
    dirs::home_dir().map(|home| home.join(".soma").display().to_string())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| nonempty_opt(Some(&value)))
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn nonempty_opt(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToOwned::to_owned)
}
