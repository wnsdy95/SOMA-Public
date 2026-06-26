//! Generate and check MCP client registration snippets.
//!
//! This is deliberately a dry-run surface. It gives operators a canonical
//! config artifact for each supported client without claiming private editor
//! lifecycle hooks are installed.

use std::path::{Component, Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::binary_identity::BinaryIdentity;
use crate::cli::McpConfigArgs;
use crate::context::cloud_prompt::{
    CLOUD_CONTEXT_CAPTURE_ECHO_ARTIFACT_VERSION_FIELD, CLOUD_CONTEXT_CAPTURE_ECHO_CONTRACT_FIELD,
    CLOUD_CONTEXT_CAPTURE_TOOL, CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY, CLOUD_CONTEXT_CONTRACT,
};

const CLOUD_CONTEXT_CAPTURE_ECHO_HANDOFF_FIELD: &str = "handoff_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpClientKind {
    ClaudeCode,
    CodexCli,
    CodexApp,
    Cursor,
    Continue,
}

impl McpClientKind {
    pub fn all() -> &'static [McpClientKind] {
        &[
            McpClientKind::ClaudeCode,
            McpClientKind::CodexCli,
            McpClientKind::CodexApp,
            McpClientKind::Cursor,
            McpClientKind::Continue,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            McpClientKind::ClaudeCode => "claude-code",
            McpClientKind::CodexCli => "codex-cli",
            McpClientKind::CodexApp => "codex-app",
            McpClientKind::Cursor => "cursor",
            McpClientKind::Continue => "continue",
        }
    }

    pub fn parse_slug(value: &str) -> Option<McpClientKind> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "claude-code" => Some(McpClientKind::ClaudeCode),
            "codex-cli" => Some(McpClientKind::CodexCli),
            "codex-app" => Some(McpClientKind::CodexApp),
            "cursor" => Some(McpClientKind::Cursor),
            "continue" => Some(McpClientKind::Continue),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            McpClientKind::ClaudeCode => "Claude Code",
            McpClientKind::CodexCli => "Codex CLI",
            McpClientKind::CodexApp => "Codex app",
            McpClientKind::Cursor => "Cursor",
            McpClientKind::Continue => "Continue",
        }
    }

    pub fn target_path_hint(self) -> &'static str {
        match self {
            McpClientKind::ClaudeCode => "~/.claude/mcp_servers.json",
            McpClientKind::CodexCli => "~/.codex/config.toml",
            McpClientKind::CodexApp => "~/.codex/config.toml",
            McpClientKind::Cursor => "~/.cursor/mcp.json",
            McpClientKind::Continue => "~/.continue/mcpServers/soma.json",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpConfigCheckReport {
    pub client: &'static str,
    pub target_path_hint: &'static str,
    pub command: String,
    pub valid: bool,
    pub checks: Vec<McpConfigCheck>,
    pub readiness: McpClientReadiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpConfigCheck {
    pub name: &'static str,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpClientReadiness {
    pub status: &'static str,
    pub mcp_registration_ready: bool,
    pub client_runtime: McpClientRuntime,
    pub private_capture_ready: bool,
    pub private_capture_boundary: &'static str,
    pub next_step: String,
    pub card: McpClientReadinessCard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpClientReadinessCard {
    pub source: &'static str,
    pub state: &'static str,
    pub headline: String,
    pub safe_to_claim: Vec<String>,
    pub blocked_claims: Vec<String>,
    pub next_cli_commands: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_mcp_tool: Option<&'static str>,
    pub trust_boundary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpClientRuntime {
    pub target: &'static str,
    pub status: &'static str,
    pub detection_method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_probe_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_probe_note: Option<String>,
    pub required_for_mcp_registration: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpConfigOutcome {
    pub client: &'static str,
    pub target_path_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_identity: Option<BinaryIdentity>,
    pub config: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<McpConfigCheckReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_plan: Option<McpHookPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpConfigAggregateSummary {
    pub client_count: usize,
    pub valid: bool,
    pub mcp_registration_ready_count: usize,
    pub runtime_missing_count: usize,
    pub private_capture_ready_count: usize,
    pub private_capture_unproven_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpConfigAggregateOutcome {
    pub source: &'static str,
    pub command: String,
    pub binary_identity: BinaryIdentity,
    pub check: bool,
    pub hook_plan: bool,
    pub trust_boundary: &'static str,
    pub summary: McpConfigAggregateSummary,
    pub clients: Vec<McpConfigOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpConfigRunOutcome {
    Single(McpConfigOutcome),
    Aggregate(McpConfigAggregateOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpHookPlan {
    pub status: &'static str,
    pub boundary: &'static str,
    pub spool_path_hint: &'static str,
    pub wrapper_entrypoints: Vec<&'static str>,
    pub cloud_output_capture_template: McpCloudOutputCaptureTemplate,
    pub install_steps: Vec<McpHookPlanStep>,
    pub proof_commands: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpHookPlanStep {
    pub name: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpCloudOutputCaptureTemplate {
    pub source: &'static str,
    pub capture_tool: &'static str,
    pub wrapper: &'static str,
    pub trust_boundary: &'static str,
    pub required_context_source: &'static str,
    pub required_echo_fields: Vec<&'static str>,
    pub payload_template_fields: Vec<McpCloudOutputCaptureTemplateField>,
    pub guardrails: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpCloudOutputCaptureTemplateField {
    pub name: &'static str,
    pub required: bool,
    pub value_hint: &'static str,
}

#[derive(Debug)]
pub enum McpConfigError {
    CurrentExe(std::io::Error),
    CurrentDir(std::io::Error),
    RelativeCommand(PathBuf),
    CheckFailed(Box<McpConfigCheckReport>),
    Render(serde_json::Error),
}

impl McpConfigError {
    pub fn exit_code(&self) -> i32 {
        match self {
            McpConfigError::CurrentExe(_)
            | McpConfigError::CurrentDir(_)
            | McpConfigError::Render(_) => 2,
            McpConfigError::RelativeCommand(_) | McpConfigError::CheckFailed(_) => 1,
        }
    }
}

impl std::fmt::Display for McpConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpConfigError::CurrentExe(err) => write!(f, "resolve current executable: {err}"),
            McpConfigError::CurrentDir(err) => write!(f, "resolve current directory: {err}"),
            McpConfigError::RelativeCommand(path) => {
                write!(
                    f,
                    "MCP client command must be an absolute path or a relative path with a directory component, got `{}`",
                    path.display()
                )
            }
            McpConfigError::CheckFailed(report) => write!(
                f,
                "MCP config check failed for {} command `{}`",
                report.client, report.command
            ),
            McpConfigError::Render(err) => write!(f, "render MCP config JSON: {err}"),
        }
    }
}

impl std::error::Error for McpConfigError {}

pub fn run(args: &McpConfigArgs) -> Result<McpConfigRunOutcome, McpConfigError> {
    let command = resolve_command(args.command.as_deref())?;
    if !command.is_absolute() {
        return Err(McpConfigError::RelativeCommand(command));
    }
    let (binary_identity, _binary_identity_errors) =
        crate::cli::binary_identity::collect_binary_identity();
    if args.all {
        return run_all(args, &command, binary_identity);
    }
    let client = args.client.expect("clap requires --client unless --all");
    run_one(client, args, &command, Some(binary_identity)).map(McpConfigRunOutcome::Single)
}

fn run_all(
    args: &McpConfigArgs,
    command: &Path,
    binary_identity: BinaryIdentity,
) -> Result<McpConfigRunOutcome, McpConfigError> {
    let mut clients = Vec::new();
    for client in McpClientKind::all() {
        clients.push(run_one(*client, args, command, None)?);
    }
    let reports = clients.iter().filter_map(|outcome| outcome.check.as_ref()).collect::<Vec<_>>();
    let summary = McpConfigAggregateSummary {
        client_count: clients.len(),
        valid: reports.iter().all(|report| report.valid),
        mcp_registration_ready_count: reports
            .iter()
            .filter(|report| report.readiness.mcp_registration_ready)
            .count(),
        runtime_missing_count: reports
            .iter()
            .filter(|report| report.readiness.client_runtime.status == "missing")
            .count(),
        private_capture_ready_count: reports
            .iter()
            .filter(|report| report.readiness.private_capture_ready)
            .count(),
        private_capture_unproven_count: reports
            .iter()
            .filter(|report| !report.readiness.private_capture_ready)
            .count(),
    };
    Ok(McpConfigRunOutcome::Aggregate(McpConfigAggregateOutcome {
        source: "soma.mcp_config.aggregate_readiness.v1",
        command: command.to_string_lossy().to_string(),
        binary_identity,
        check: args.check,
        hook_plan: args.hook_plan,
        trust_boundary: "aggregate_mcp_config_report_is_read_only: it validates generated MCP config shapes and summarizes readiness cards, but records no client-binding proof row, creates no verification event, installs no hook, and never promotes cloud drafts",
        summary,
        clients,
    }))
}

fn run_one(
    client: McpClientKind,
    args: &McpConfigArgs,
    command: &Path,
    binary_identity: Option<BinaryIdentity>,
) -> Result<McpConfigOutcome, McpConfigError> {
    let config = render_client_config(client, command);
    let check = if args.check {
        let report = check_client_config(client, command, &config);
        if !report.valid {
            return Err(McpConfigError::CheckFailed(Box::new(report)));
        }
        Some(report)
    } else {
        None
    };

    Ok(McpConfigOutcome {
        client: client.as_str(),
        target_path_hint: client.target_path_hint(),
        binary_identity,
        config,
        check,
        hook_plan: args.hook_plan.then(|| hook_plan(client, command)),
    })
}

pub fn render_json(outcome: &McpConfigRunOutcome) -> Result<String, McpConfigError> {
    let value = match outcome {
        McpConfigRunOutcome::Single(outcome) => {
            if outcome.check.is_some() || outcome.hook_plan.is_some() {
                serde_json::to_value(outcome).map_err(McpConfigError::Render)?
            } else {
                outcome.config.clone()
            }
        }
        McpConfigRunOutcome::Aggregate(outcome) => {
            serde_json::to_value(outcome).map_err(McpConfigError::Render)?
        }
    };
    serde_json::to_string_pretty(&value)
        .map(|mut text| {
            text.push('\n');
            text
        })
        .map_err(McpConfigError::Render)
}

pub fn render_brief(outcome: &McpConfigRunOutcome) -> String {
    match outcome {
        McpConfigRunOutcome::Single(outcome) => render_single_brief(outcome),
        McpConfigRunOutcome::Aggregate(outcome) => render_aggregate_brief(outcome),
    }
}

fn render_single_brief(outcome: &McpConfigOutcome) -> String {
    let mut text = String::new();
    text.push_str("SOMA MCP config brief\n");
    push_kv(&mut text, "client", outcome.client);
    push_kv(&mut text, "target", outcome.target_path_hint);
    if let Some(binary) = &outcome.binary_identity {
        push_kv(
            &mut text,
            "binary",
            format!(
                "status={} current={} path_soma={} same_fingerprint={}",
                binary.status,
                binary.current_exe.as_deref().unwrap_or("unknown"),
                binary.path_soma.as_deref().unwrap_or("not_found"),
                binary.same_fingerprint
            ),
        );
    }

    if let Some(report) = &outcome.check {
        push_kv(&mut text, "valid", report.valid);
        push_kv(&mut text, "status", report.readiness.status);
        push_kv(&mut text, "mcp_registration_ready", report.readiness.mcp_registration_ready);
        push_kv(&mut text, "private_capture_ready", report.readiness.private_capture_ready);
        push_kv(
            &mut text,
            "runtime",
            format!(
                "{} ({})",
                report.readiness.client_runtime.status,
                report.readiness.client_runtime.detection_method
            ),
        );
        if let Some(path) = &report.readiness.client_runtime.path {
            push_kv(&mut text, "runtime_path", path);
        }
        if let Some(command) = &report.readiness.client_runtime.launch_probe_command {
            push_kv(&mut text, "runtime_launch_probe", command.join(" "));
        }
        if let Some(note) = &report.readiness.client_runtime.launch_probe_note {
            push_kv(&mut text, "runtime_launch_probe_note", note);
        }
        push_kv(&mut text, "next_step", &report.readiness.next_step);
        push_list(
            &mut text,
            "next_cli_commands",
            &format_commands(&report.readiness.card.next_cli_commands),
        );
        push_list(&mut text, "safe_to_claim", &report.readiness.card.safe_to_claim);
        push_list(&mut text, "blocked_claims", &report.readiness.card.blocked_claims);
        push_kv(&mut text, "trust_boundary", report.readiness.card.trust_boundary);
    } else {
        push_kv(&mut text, "status", "config_rendered");
        push_kv(
            &mut text,
            "next_step",
            format!(
                "Run `soma mcp-config --client {} --check --brief` to inspect readiness before claiming client integration.",
                outcome.client
            ),
        );
    }

    if let Some(plan) = &outcome.hook_plan {
        push_kv(&mut text, "hook_plan", plan.status);
        push_kv(&mut text, "hook_boundary", plan.boundary);
        push_list(&mut text, "proof_commands", &format_commands(&plan.proof_commands));
    }

    text
}

fn render_aggregate_brief(outcome: &McpConfigAggregateOutcome) -> String {
    let mut text = String::new();
    text.push_str("SOMA MCP config brief\n");
    push_kv(&mut text, "source", outcome.source);
    push_kv(
        &mut text,
        "binary",
        format!(
            "status={} current={} path_soma={} same_fingerprint={}",
            outcome.binary_identity.status,
            outcome.binary_identity.current_exe.as_deref().unwrap_or("unknown"),
            outcome.binary_identity.path_soma.as_deref().unwrap_or("not_found"),
            outcome.binary_identity.same_fingerprint
        ),
    );
    push_kv(&mut text, "check", outcome.check);
    push_kv(&mut text, "clients", outcome.summary.client_count);
    push_kv(&mut text, "valid", outcome.summary.valid);
    push_kv(&mut text, "mcp_registration_ready", outcome.summary.mcp_registration_ready_count);
    push_kv(&mut text, "runtime_missing", outcome.summary.runtime_missing_count);
    push_kv(&mut text, "private_capture_ready", outcome.summary.private_capture_ready_count);
    push_kv(&mut text, "private_capture_unproven", outcome.summary.private_capture_unproven_count);
    push_kv(&mut text, "trust_boundary", outcome.trust_boundary);
    text.push_str("client_cards:\n");
    for client in &outcome.clients {
        if let Some(report) = &client.check {
            text.push_str(&format!(
                "  - {}: valid={} status={} runtime={} private_capture_ready={}\n",
                client.client,
                report.valid,
                report.readiness.status,
                report.readiness.client_runtime.status,
                report.readiness.private_capture_ready
            ));
            text.push_str(&format!("    next_step: {}\n", report.readiness.next_step));
            if let Some(command) =
                first_proof_session_command(&report.readiness.card.next_cli_commands)
            {
                text.push_str(&format!("    proof_session: {command}\n"));
            }
        } else {
            text.push_str(&format!(
                "  - {}: config_rendered target={}\n",
                client.client, client.target_path_hint
            ));
        }
    }
    text
}

fn first_proof_session_command(commands: &[Vec<String>]) -> Option<String> {
    commands.iter().find_map(|command| {
        if command.iter().any(|part| part == "--proof-session")
            && command.iter().any(|part| part == "--brief")
        {
            Some(command.join(" "))
        } else {
            None
        }
    })
}

fn format_commands(commands: &[Vec<String>]) -> Vec<String> {
    commands.iter().map(|command| command.join(" ")).collect()
}

fn push_kv<T: std::fmt::Display>(text: &mut String, key: &str, value: T) {
    text.push_str(&format!("  {key}: {value}\n"));
}

fn push_list(text: &mut String, label: &str, values: &[String]) {
    text.push_str(&format!("  {label}:\n"));
    if values.is_empty() {
        text.push_str("    - none\n");
        return;
    }
    for value in values {
        text.push_str(&format!("    - {value}\n"));
    }
}

fn resolve_command(command: Option<&str>) -> Result<PathBuf, McpConfigError> {
    match command.map(str::trim).filter(|value| !value.is_empty()) {
        Some(command) => absolutize_command_path(PathBuf::from(command)),
        None => std::env::current_exe().map_err(McpConfigError::CurrentExe),
    }
}

fn absolutize_command_path(path: PathBuf) -> Result<PathBuf, McpConfigError> {
    if path.is_absolute() {
        return Ok(path);
    }
    if is_bare_command_name(&path) {
        return Ok(path);
    }
    let cwd = std::env::current_dir().map_err(McpConfigError::CurrentDir)?;
    Ok(cwd.join(path))
}

fn is_bare_command_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(name)) if !name.to_string_lossy().trim().is_empty())
        && components.next().is_none()
}

fn render_client_config(client: McpClientKind, command: &Path) -> Value {
    let command = command.to_string_lossy();
    let comment = format!(
        "{} MCP registration for SOMA. The command is absolute and the server \
         launches `soma mcp-serve`; this config does not install private editor \
         lifecycle hooks.",
        client.display_name()
    );
    match client {
        McpClientKind::ClaudeCode | McpClientKind::Cursor => json!({
            "_comment": comment,
            "mcpServers": {
                "soma": {
                    "command": command,
                    "args": ["mcp-serve"]
                }
            }
        }),
        McpClientKind::CodexCli | McpClientKind::CodexApp => json!({
            "_comment": comment,
            "target": "~/.codex/config.toml",
            "install_command": ["codex", "mcp", "add", "soma", "--", command, "mcp-serve"],
            "desktop_app_note": "Codex app integrations should use the same mcp_servers.soma stanza when the app reads Codex MCP settings; this artifact still installs no private turn hook.",
            "mcp_servers": {
                "soma": {
                    "command": command,
                    "args": ["mcp-serve"]
                }
            }
        }),
        McpClientKind::Continue => json!({
            "type": "stdio",
            "command": command,
            "args": ["mcp-serve"]
        }),
    }
}

fn hook_plan(client: McpClientKind, command: &Path) -> McpHookPlan {
    let command = command.to_string_lossy();
    let common_boundary =
        "Plan-only artifact: it does not edit editor settings, install private lifecycle hooks, \
         render UI inside the client, or prove that an editor emitted real turn events.";
    let spool_path_hint = "~/.soma/adapter/events.jsonl";
    let mut wrapper_entrypoints = vec![
        "tools/soma-codex-cli.sh",
        "tools/soma-codex-app-capture.sh",
        "tools/soma-codex-notify-bridge.sh",
        "tools/soma-claude-code-cli.sh",
        "tools/soma-adapter-lifecycle.sh",
        "tools/soma-adapter-spool-append.sh",
        "tools/soma-adapter-spool-watch.sh",
        "tools/soma-adapter-cloud-output.sh",
        "tools/soma-continue-devdata-collector.py",
        "tools/soma-continue-devdata-install.py",
        "tools/soma-review-render.sh",
        "tools/soma-review-actions.sh",
        "tools/soma-review-digest.sh",
        "tools/soma-review-digest-ack.sh",
        "tools/soma-review-report.sh",
        "tools/soma-review-batch-template.sh",
    ];
    if matches!(client, McpClientKind::Cursor) {
        wrapper_entrypoints.push(".cursor/hooks/soma-lifecycle.sh");
    }
    let mut proof_commands = vec![
        vec!["tools/mcp-adapter-config-smoke.sh".to_string()],
        vec!["tools/context-bridge-smoke.sh".to_string()],
        vec!["tools/client-integration-eval.sh".to_string(), "--check-docs-report".to_string()],
        vec![
            command.to_string(),
            "context".to_string(),
            "trust-audit".to_string(),
            "--limit".to_string(),
            "1000".to_string(),
        ],
    ];
    let install_steps = match client {
        McpClientKind::ClaudeCode => vec![
            McpHookPlanStep {
                name: "register_mcp_server",
                detail: format!(
                    "Place the generated MCP config in {} and confirm it launches `{command} mcp-serve`.",
                    client.target_path_hint()
                ),
            },
            McpHookPlanStep {
                name: "optional_stop_hook",
                detail: "If using Claude Code Stop hooks, wire the hook to `tools/claude-code-stop-hook.sh` or an equivalent wrapper that calls `soma ingest --source claude-code`.".to_string(),
            },
            McpHookPlanStep {
                name: "cloud_output_capture",
                detail: "For assistant responses, prefer `soma_capture_cloud_output` through MCP or `tools/soma-adapter-cloud-output.sh`; copy `handoff_id`, `protocol_contract`, and `artifact_version` from the `soma-cloud-context` artifact. Echo mismatches are rejected before claim capture, and captured claims remain cloud_draft until verified.".to_string(),
            },
            review_render_step(client),
        ],
        McpClientKind::CodexCli => vec![
            McpHookPlanStep {
                name: "register_mcp_server",
                detail: format!(
                    "Run `codex mcp add soma -- {command} mcp-serve` or merge the generated `mcp_servers.soma` entry into {}.",
                    client.target_path_hint()
                ),
            },
            McpHookPlanStep {
                name: "start_managed_session",
                detail: "Launch Codex from a SOMA-managed shell scope with `eval \"$(soma session start --client codex-cli)\"` or use `tools/soma-codex-cli.sh`; this stamps SOMA_SESSION_ID/SOMA_CLIENT so terminal capture, adapter capture, and ContextEnvelope session reads stay separated across terminals.".to_string(),
            },
            McpHookPlanStep {
                name: "cloud_output_capture",
                detail: "For assistant responses, prefer `soma_capture_cloud_output` through MCP or `tools/soma-adapter-cloud-output.sh`; copy `handoff_id`, `protocol_contract`, and `artifact_version` from the `soma-cloud-context` artifact. Captured claims remain cloud_draft until verified.".to_string(),
            },
            review_render_step(client),
        ],
        McpClientKind::CodexApp => vec![
            McpHookPlanStep {
                name: "register_mcp_server",
                detail: format!(
                    "Merge the generated `mcp_servers.soma` entry into {} or the Codex app MCP settings surface, then confirm it launches `{command} mcp-serve`.",
                    client.target_path_hint()
                ),
            },
            McpHookPlanStep {
                name: "choose_write_path",
                detail: "Use MCP `soma_capture_turn` for explicit turn capture, `soma_capture_cloud_output` for assistant outputs bound to a TaskFrame handoff, configure `tools/soma-codex-app-capture.sh` for private assistant/turn payloads, or wrap Codex app `notify` with `tools/soma-codex-notify-bridge.sh --chain <existing-notify-command> ...` for a turn-ended heartbeat. None of these paths proves Codex app invoked the hook until observed_app_hook evidence is recorded from a real app call.".to_string(),
            },
            McpHookPlanStep {
                name: "wire_notify_bridge",
                detail: "For Codex app desktop installs that already have a `notify` command, run `tools/soma-codex-notify-install.sh` or place `tools/soma-codex-notify-bridge.sh` first and pass the existing command after `--chain`. The bridge preserves the existing notify command, reads `$HOME/.codex/soma-installed-binding.json` (or `SOMA_CODEX_NOTIFY_INSTALLED_CONFIG`), emits `hook_adapter=codex_notify_bridge` with `event_source=codex-app_private_lifecycle_hook` and the installed binding nonce, and writes only a heartbeat turn event, not cloud output or verification evidence. If Codex app was already running when the config was patched, quit or restart the stale Codex app process, then reopen it before expecting a real turn-ended hook event; `open -a Codex` alone does not force a running app to reload the notify config.".to_string(),
            },
            McpHookPlanStep {
                name: "normalize_private_events",
                detail: format!(
                    "First run `soma adapter-binding-proof --real-app-proof-kit --client codex-app` to render the operator evidence plan, then `--discover-installed-config` to find existing config candidates and `--prepare-installed-config` to generate a per-install nonce. Private Codex app payloads should call `tools/soma-codex-app-capture.sh` or `tools/soma-adapter-lifecycle.sh` with `SOMA_ADAPTER_LIFECYCLE_JSONL={spool_path_hint}`, `SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE=codex-app_private_lifecycle_hook`, and that `SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE`; notify-only installs can call `tools/soma-codex-notify-bridge.sh` against `$HOME/.codex/soma-installed-binding.json` to write a heartbeat turn event. assistant_response events normalize to cloud_output, should echo `handoff_id`, `protocol_contract`, and `artifact_version` from `soma-cloud-context` when present, and remain draft claims."
                ),
            },
            McpHookPlanStep {
                name: "drain_and_verify",
                detail: "Run `tools/soma-adapter-spool-watch.sh` or `soma adapter-spool`, then verify recall/review output and run trust-audit. This proves the wrapper path, not Codex app private lifecycle capture until the app actually calls it.".to_string(),
            },
            McpHookPlanStep {
                name: "record_binding_proof",
                detail: "Before persisting app-hook proof, run `soma adapter-binding-proof --check-installed-config --client codex-app --installed-config <codex-app-hook-config>`. Record wrapper evidence with `--proof-level observed_event_file`; use `observed_app_hook --installed-config <codex-app-hook-config> --operator-confirm-real-app-invocation --operator-confirm-release-grade-evidence` only when the event file came from a real Codex app invocation, includes `event_source=codex-app_private_lifecycle_hook`, matches the installed config `binding_nonce`, carries `soma_adapter_spool_append_v1` writer metadata, and has `observed_at_ns` at or after the installed config timestamp. Use `observed_in_client_render --render-evidence <codex-app-render-evidence> --operator-confirm-in-client-render --operator-confirm-release-grade-evidence` separately only after the read-only review plan is visible inside Codex app. Use `observed_review_action --review-action-report <codex-app-review-action-report> --operator-confirm-review-action --operator-confirm-release-grade-evidence` only after a rendered control_id produced a storage-gated review-action report with non-cloud verification evidence.".to_string(),
            },
            review_render_step(client),
        ],
        McpClientKind::Cursor => vec![
            McpHookPlanStep {
                name: "register_mcp_server",
                detail: format!(
                    "Place the generated MCP config in {} and confirm it launches `{command} mcp-serve`.",
                    client.target_path_hint()
                ),
            },
            McpHookPlanStep {
                name: "enable_checked_in_project_hook",
                detail: "This repository includes `.cursor/hooks.json` and `.cursor/hooks/soma-lifecycle.sh`. Open or reload the SOMA workspace in Cursor so the project hook can run on `sessionStart` and `afterAgentResponse`; the hook writes `event_source=cursor_private_lifecycle_hook` plus the installed-config `binding_nonce` into the adapter spool, but proof still requires a real Cursor invocation and explicit operator confirmation.".to_string(),
            },
            McpHookPlanStep {
                name: "choose_write_path",
                detail: "Use the checked-in project hook for automatic Cursor lifecycle capture, MCP `soma_capture_turn` for explicit turn capture, or configure another Cursor-specific watcher/command to write normalized events into the adapter spool.".to_string(),
            },
            McpHookPlanStep {
                name: "normalize_private_events",
                detail: format!(
                    "First run `soma adapter-binding-proof --real-app-proof-kit --client cursor` to render the operator evidence plan, then `--discover-installed-config` to find existing config candidates and `--prepare-installed-config` to generate a per-install nonce. Private Cursor lifecycle payloads should call `tools/soma-adapter-lifecycle.sh` with `SOMA_ADAPTER_LIFECYCLE_JSONL={spool_path_hint}`, `SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE=cursor_private_lifecycle_hook`, and that `SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE`; normalized payloads can use `tools/soma-adapter-spool-append.sh` with `SOMA_ADAPTER_BINDING_NONCE`. If the payload contains assistant/cloud output derived from `soma-cloud-context`, carry `handoff_id`, `protocol_contract`, and `artifact_version` into cloud_output capture."
                ),
            },
            McpHookPlanStep {
                name: "drain_and_verify",
                detail: "Run `tools/soma-adapter-spool-watch.sh` or `soma adapter-spool`, then verify recall/review output and run trust-audit. This proves the wrapper path, not Cursor's private hook until Cursor actually calls it.".to_string(),
            },
            McpHookPlanStep {
                name: "record_binding_proof",
                detail: "Before persisting app-hook proof, run `soma adapter-binding-proof --check-installed-config --client cursor --installed-config <cursor-hook-config>`. Record wrapper evidence with `--proof-level observed_event_file`; use `observed_app_hook --installed-config <cursor-hook-config> --operator-confirm-real-app-invocation --operator-confirm-release-grade-evidence` only when the event file came from a real Cursor hook invocation, includes `event_source=cursor_private_lifecycle_hook`, matches the installed config `binding_nonce`, carries `soma_adapter_spool_append_v1` writer metadata, and has `observed_at_ns` at or after the installed config timestamp. Use `observed_in_client_render --render-evidence <cursor-render-evidence> --operator-confirm-in-client-render --operator-confirm-release-grade-evidence` separately only after the read-only review plan is visible inside Cursor. Use `observed_review_action --review-action-report <cursor-review-action-report> --operator-confirm-review-action --operator-confirm-release-grade-evidence` only after a rendered control_id produced a storage-gated review-action report with non-cloud verification evidence.".to_string(),
            },
            review_render_step(client),
        ],
        McpClientKind::Continue => vec![
            McpHookPlanStep {
                name: "register_mcp_server",
                detail: format!(
                    "Merge the generated MCP server entry into {} and confirm it launches `{command} mcp-serve`.",
                    client.target_path_hint()
                ),
            },
            McpHookPlanStep {
                name: "choose_write_path",
                detail: "Use MCP `soma_capture_turn` for explicit turn capture, or run `tools/soma-continue-devdata-collector.py` as a local Continue dev-data destination for real chat/edit/review events. Custom Continue/VS Code wrappers can still write normalized turn and assistant_response events into the adapter spool.".to_string(),
            },
            McpHookPlanStep {
                name: "normalize_private_events",
                detail: format!(
                    "First run `soma adapter-binding-proof --real-app-proof-kit --client continue` to render the operator evidence plan, then `--discover-installed-config` to find existing config candidates and `--prepare-installed-config` to generate a per-install nonce. For Continue dev-data, run `tools/soma-continue-devdata-install.py --dry-run`, then `--write` if the planned local `data` destination is correct, and keep `tools/soma-continue-devdata-collector.py` running so `chatInteraction`, `editInteraction`, `editOutcome`, and `quickEdit` POSTs append to `{spool_path_hint}` with `event_source=continue_private_lifecycle_hook`. The collector shape-checks Continue dev-data fields and marks malformed/manual localhost POSTs as `manual_invocation_policy=non_release_debug_only`, which readiness/proof scanners ignore for `observed_app_hook`. Private Continue lifecycle payloads can also call `tools/soma-adapter-lifecycle.sh` with `SOMA_ADAPTER_LIFECYCLE_JSONL={spool_path_hint}`, `SOMA_ADAPTER_LIFECYCLE_EVENT_SOURCE=continue_private_lifecycle_hook`, and that `SOMA_ADAPTER_LIFECYCLE_BINDING_NONCE`; assistant_response events normalize to cloud_output, should echo `handoff_id`, `protocol_contract`, and `artifact_version` from `soma-cloud-context` when present, and remain draft claims."
                ),
            },
            McpHookPlanStep {
                name: "drain_and_verify",
                detail: "Run `tools/soma-adapter-spool-watch.sh` or `soma adapter-spool`, then verify recall/review output and run trust-audit. This proves the wrapper path, not Continue's private lifecycle hook until Continue actually calls it.".to_string(),
            },
            McpHookPlanStep {
                name: "record_binding_proof",
                detail: "Before persisting app-hook proof, run `soma adapter-binding-proof --check-installed-config --client continue --installed-config <continue-hook-config>`. Record wrapper evidence with `--proof-level observed_event_file`; use `observed_app_hook --installed-config <continue-hook-config> --operator-confirm-real-app-invocation --operator-confirm-release-grade-evidence` only when the event file came from a real Continue hook invocation, includes `event_source=continue_private_lifecycle_hook`, matches the installed config `binding_nonce`, carries `soma_adapter_spool_append_v1` writer metadata, and has `observed_at_ns` at or after the installed config timestamp. Use `observed_in_client_render --render-evidence <continue-render-evidence> --operator-confirm-in-client-render --operator-confirm-release-grade-evidence` separately only after the read-only review plan is visible inside Continue. Use `observed_review_action --review-action-report <continue-review-action-report> --operator-confirm-review-action --operator-confirm-release-grade-evidence` only after a rendered control_id produced a storage-gated review-action report with non-cloud verification evidence.".to_string(),
            },
            review_render_step(client),
        ],
    };
    if matches!(client, McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue) {
        let manifest = match client {
            McpClientKind::CodexApp => "tools/client-bindings/codex-app-soma-binding.json.example",
            McpClientKind::Cursor => "tools/client-bindings/cursor-soma-binding.json.example",
            McpClientKind::Continue => "tools/client-bindings/continue-soma-binding.json.example",
            McpClientKind::ClaudeCode | McpClientKind::CodexCli => unreachable!(),
        };
        proof_commands.push(vec!["tools/client-binding-smoke.sh".to_string()]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--real-app-proof-kit".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--discover-installed-config".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--prepare-installed-config".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--check-installed-config".to_string(),
            "--client".to_string(),
            client.as_str().to_string(),
            "--installed-config".to_string(),
            format!("<{}-hook-config>", client.as_str()),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_event_file".to_string(),
            "--event-jsonl".to_string(),
            "$SOMA_ADAPTER_SPOOL_JSONL".to_string(),
            "--drain-report".to_string(),
            "$DRAIN_REPORT_JSON".to_string(),
            "--review-render-report".to_string(),
            "$REVIEW_RENDER_JSON".to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_app_hook".to_string(),
            "--event-jsonl".to_string(),
            "$SOMA_ADAPTER_SPOOL_JSONL".to_string(),
            "--drain-report".to_string(),
            "$DRAIN_REPORT_JSON".to_string(),
            "--installed-config".to_string(),
            format!("<{}-hook-config>", client.as_str()),
            "--evidence-source".to_string(),
            format!("private_client_operator_observed_{}_observed_app_hook", client.as_str()),
            "--operator-confirm-real-app-invocation".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_in_client_render".to_string(),
            "--installed-config".to_string(),
            format!("<{}-review-config>", client.as_str()),
            "--review-render-report".to_string(),
            "$REVIEW_RENDER_JSON".to_string(),
            "--render-evidence".to_string(),
            format!("<{}-render-evidence>", client.as_str()),
            "--evidence-source".to_string(),
            format!(
                "private_client_operator_observed_{}_observed_in_client_render",
                client.as_str()
            ),
            "--operator-confirm-in-client-render".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ]);
        proof_commands.push(vec![
            command.to_string(),
            "adapter-binding-proof".to_string(),
            "--manifest".to_string(),
            manifest.to_string(),
            "--proof-level".to_string(),
            "observed_review_action".to_string(),
            "--installed-config".to_string(),
            format!("<{}-review-config>", client.as_str()),
            "--review-action-report".to_string(),
            format!("<{}-review-action-report>", client.as_str()),
            "--evidence-source".to_string(),
            format!("private_client_operator_observed_{}_observed_review_action", client.as_str()),
            "--operator-confirm-review-action".to_string(),
            "--operator-confirm-release-grade-evidence".to_string(),
        ]);
    }

    McpHookPlan {
        status: "plan_only",
        boundary: common_boundary,
        spool_path_hint,
        wrapper_entrypoints,
        cloud_output_capture_template: cloud_output_capture_template(),
        install_steps,
        proof_commands,
    }
}

fn cloud_output_capture_template() -> McpCloudOutputCaptureTemplate {
    McpCloudOutputCaptureTemplate {
        source: "soma.mcp_config.cloud_output_capture_template.v1",
        capture_tool: CLOUD_CONTEXT_CAPTURE_TOOL,
        wrapper: "tools/soma-adapter-cloud-output.sh",
        trust_boundary: CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY,
        required_context_source: "soma_compiled_context or `soma context prompt` rendered `soma-cloud-context` artifact",
        required_echo_fields: vec![
            CLOUD_CONTEXT_CAPTURE_ECHO_HANDOFF_FIELD,
            CLOUD_CONTEXT_CAPTURE_ECHO_CONTRACT_FIELD,
            CLOUD_CONTEXT_CAPTURE_ECHO_ARTIFACT_VERSION_FIELD,
        ],
        payload_template_fields: vec![
            McpCloudOutputCaptureTemplateField {
                name: "task_frame_id",
                required: true,
                value_hint: "copy from the shaping TaskFrame in the compiled context artifact",
            },
            McpCloudOutputCaptureTemplateField {
                name: CLOUD_CONTEXT_CAPTURE_ECHO_HANDOFF_FIELD,
                required: true,
                value_hint: "copy exactly from the `soma-cloud-context` handoff_id",
            },
            McpCloudOutputCaptureTemplateField {
                name: CLOUD_CONTEXT_CAPTURE_ECHO_CONTRACT_FIELD,
                required: true,
                value_hint: CLOUD_CONTEXT_CONTRACT,
            },
            McpCloudOutputCaptureTemplateField {
                name: CLOUD_CONTEXT_CAPTURE_ECHO_ARTIFACT_VERSION_FIELD,
                required: true,
                value_hint: "copy the integer artifact_version from the protocol block",
            },
            McpCloudOutputCaptureTemplateField {
                name: "output_text",
                required: true,
                value_hint: "raw cloud assistant response text",
            },
            McpCloudOutputCaptureTemplateField {
                name: "decision",
                required: true,
                value_hint: "`accept`, `revise`, or `reject` as a critic decision; not a verification result",
            },
            McpCloudOutputCaptureTemplateField {
                name: "enqueue_proposal",
                required: false,
                value_hint: "true when the draft should enter review queue",
            },
        ],
        guardrails: vec![
            "cloud_output_is_cloud_draft_until_verified",
            "protocol_echo_binds_response_to_context_artifact_not_verification",
            "mismatched_handoff_or_protocol_echo_is_rejected_before_claim_capture",
        ],
    }
}

fn review_render_step(client: McpClientKind) -> McpHookPlanStep {
    McpHookPlanStep {
        name: "render_review_ui",
        detail: format!(
            "Use `tools/soma-review-render.sh` with `SOMA_REVIEW_CLIENT={}` to render the read-only review plan: show `client_ui`/`surfaces`, follow `mcp_call_order` or `wrapper_call_order`, call digest ack only after visible notification, and call review-action or review-batch only after trusted user/tool/test/local evidence.",
            client.as_str()
        ),
    }
}

fn check_client_config(
    client: McpClientKind,
    command: &Path,
    config: &Value,
) -> McpConfigCheckReport {
    let command_string = command.to_string_lossy().to_string();
    let launches_mcp_serve = match client {
        McpClientKind::ClaudeCode | McpClientKind::Cursor => config
            .pointer("/mcpServers/soma/args")
            .and_then(Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("mcp-serve"))),
        McpClientKind::CodexCli | McpClientKind::CodexApp => config
            .pointer("/mcp_servers/soma/args")
            .and_then(Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("mcp-serve"))),
        McpClientKind::Continue => config
            .pointer("/args")
            .and_then(Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("mcp-serve"))),
    };
    let serialized = serde_json::to_string(config).unwrap_or_default();
    let no_prompt_prefix_or_persona_path = !serialized.contains("prompt-prefix")
        && !serialized.contains("UserPromptSubmit")
        && !serialized.contains("persona");
    let command_is_absolute = command.is_absolute();
    let command_exists = command.exists();
    let checks = vec![
        McpConfigCheck { name: "command_is_absolute", passed: command_is_absolute },
        McpConfigCheck { name: "command_exists", passed: command_exists },
        McpConfigCheck { name: "launches_mcp_serve", passed: launches_mcp_serve },
        McpConfigCheck {
            name: "no_prompt_prefix_or_persona_path",
            passed: no_prompt_prefix_or_persona_path,
        },
    ];
    let valid = checks.iter().all(|check| check.passed);
    let readiness = client_readiness(client, command, valid);

    McpConfigCheckReport {
        client: client.as_str(),
        target_path_hint: client.target_path_hint(),
        command: command_string,
        valid,
        checks,
        readiness,
    }
}

fn client_readiness(
    client: McpClientKind,
    command: &Path,
    mcp_registration_ready: bool,
) -> McpClientReadiness {
    let client_runtime = client_runtime(client);
    let status = if !mcp_registration_ready {
        "mcp_registration_invalid"
    } else if client_runtime.required_for_mcp_registration && client_runtime.status == "missing" {
        "mcp_registration_config_ready_client_runtime_missing"
    } else {
        "mcp_registration_ready_private_capture_unproven"
    };
    let next_step = readiness_next_step(client, command, mcp_registration_ready, &client_runtime);
    let card = readiness_card(client, command, mcp_registration_ready, &client_runtime, status);
    McpClientReadiness {
        status,
        mcp_registration_ready,
        private_capture_ready: false,
        private_capture_boundary: "MCP registration only proves the server config shape. Automatic private capture remains unproven until release-grade client binding proof records the real app hook, in-client render, and review-action path; cloud output stays draft until user/tool/test/local/correction verification.",
        next_step,
        card,
        client_runtime,
    }
}

fn readiness_card(
    client: McpClientKind,
    command: &Path,
    mcp_registration_ready: bool,
    client_runtime: &McpClientRuntime,
    state: &'static str,
) -> McpClientReadinessCard {
    let client_name = client.display_name();
    let command_string = command.to_string_lossy().to_string();
    let headline = if !mcp_registration_ready {
        format!("{client_name} MCP registration is not ready; fix failed config checks first.")
    } else if client_runtime.required_for_mcp_registration && client_runtime.status == "missing" {
        format!(
            "{client_name} MCP config is shaped correctly, but `{}` was not found on PATH.",
            client_runtime.target
        )
    } else {
        format!(
            "{client_name} can register SOMA MCP; private capture and in-client review remain unproven."
        )
    };

    let mut safe_to_claim = Vec::new();
    if mcp_registration_ready {
        safe_to_claim
            .push("MCP registration config points at the SOMA `mcp-serve` command.".to_string());
        safe_to_claim.push(
            "Explicit MCP/context/capture paths can be dogfooded without claiming private hooks."
                .to_string(),
        );
        if client_runtime.status == "detected" {
            safe_to_claim.push(client_runtime_detected_claim(client_runtime));
        } else if client_runtime.status == "not_cli_detectable" {
            safe_to_claim.push(
                "This client requires a manual app or extension settings check rather than CLI runtime detection."
                    .to_string(),
            );
        }
    } else {
        safe_to_claim.push("No readiness claim is safe until MCP config checks pass.".to_string());
    }

    let blocked_claims = vec![
        "automatic private app/CLI lifecycle capture".to_string(),
        "in-client review UI render".to_string(),
        "review-action execution from a rendered client control".to_string(),
        "cloud draft truth, verification, or L3/L4 promotion without user/tool/test/local/correction evidence".to_string(),
    ];

    let mut next_cli_commands = vec![vec![
        command_string.clone(),
        "mcp-config".to_string(),
        "--client".to_string(),
        client.as_str().to_string(),
        "--check".to_string(),
    ]];

    if client_runtime.required_for_mcp_registration && client_runtime.status == "missing" {
        next_cli_commands.push(vec!["which".to_string(), client_runtime.target.to_string()]);
    }

    match client {
        McpClientKind::ClaudeCode => {
            next_cli_commands.push(vec![
                command_string.clone(),
                "session".to_string(),
                "start".to_string(),
                "--client".to_string(),
                "claude-code".to_string(),
            ]);
            next_cli_commands.push(vec!["tools/soma-claude-code-cli.sh".to_string()]);
        }
        McpClientKind::CodexCli => {
            next_cli_commands.push(vec![
                command_string.clone(),
                "session".to_string(),
                "start".to_string(),
                "--client".to_string(),
                "codex-cli".to_string(),
            ]);
            next_cli_commands.push(vec!["tools/soma-codex-cli.sh".to_string()]);
        }
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            next_cli_commands.push(vec![
                command_string.clone(),
                "clients".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--brief".to_string(),
            ]);
            next_cli_commands.push(vec![
                command_string.clone(),
                "adapter-binding-proof".to_string(),
                "--proof-session".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--brief".to_string(),
            ]);
            next_cli_commands.push(vec![
                command_string,
                "adapter-binding-proof".to_string(),
                "--proof-session".to_string(),
                "--client".to_string(),
                client.as_str().to_string(),
                "--json".to_string(),
            ]);
        }
    }

    next_cli_commands.push(vec!["tools/client-dogfood-report.sh".to_string()]);

    let next_mcp_tool = match client {
        McpClientKind::CodexApp | McpClientKind::Cursor | McpClientKind::Continue => {
            Some("soma_client_binding_proof_session")
        }
        McpClientKind::ClaudeCode | McpClientKind::CodexCli => Some("soma_client_binding_proofs"),
    };

    McpClientReadinessCard {
        source: "soma.mcp_config.readiness_card.v1",
        state,
        headline,
        safe_to_claim,
        blocked_claims,
        next_cli_commands,
        next_mcp_tool,
        trust_boundary: "read-only setup card: records no proof row, creates no verification event, promotes no cloud draft, and does not prove private app installation, rendering, or review-action execution",
    }
}

fn client_runtime(client: McpClientKind) -> McpClientRuntime {
    match client {
        McpClientKind::ClaudeCode => path_client_runtime("claude", true),
        McpClientKind::CodexCli => path_client_runtime("codex", true),
        McpClientKind::Cursor => cursor_runtime(),
        McpClientKind::Continue => continue_runtime(),
        McpClientKind::CodexApp => McpClientRuntime {
            target: "Codex app settings",
            status: "not_cli_detectable",
            detection_method: "manual_app_settings_check",
            path: None,
            launch_probe_command: None,
            launch_probe_note: None,
            required_for_mcp_registration: false,
        },
    }
}

fn client_runtime_detected_claim(client_runtime: &McpClientRuntime) -> String {
    match client_runtime.detection_method {
        "path_executable_scan" => {
            format!("Observable client runtime `{}` was found on PATH.", client_runtime.target)
        }
        "macos_app_bundle_scan" => {
            format!("Observable client app bundle `{}` was found.", client_runtime.target)
        }
        "continue_config_scan" => {
            format!("Observable `{}` was found.", client_runtime.target)
        }
        method => format!(
            "Observable client runtime `{}` was detected by {method}.",
            client_runtime.target
        ),
    }
}

fn cursor_runtime() -> McpClientRuntime {
    cursor_runtime_from(
        find_executable_on_path("cursor"),
        first_existing_path(&cursor_app_bundle_candidates()),
    )
}

fn cursor_runtime_from(
    cli_path: Option<PathBuf>,
    app_bundle_path: Option<PathBuf>,
) -> McpClientRuntime {
    if let Some(path) = cli_path {
        return McpClientRuntime {
            target: "cursor",
            status: "detected",
            detection_method: "path_executable_scan",
            path: Some(path.to_string_lossy().to_string()),
            launch_probe_command: None,
            launch_probe_note: None,
            required_for_mcp_registration: false,
        };
    }
    if let Some(path) = app_bundle_path {
        let path = path.to_string_lossy().to_string();
        return McpClientRuntime {
            target: "Cursor.app",
            status: "detected",
            detection_method: "macos_app_bundle_scan",
            path: Some(path.clone()),
            launch_probe_command: Some(vec!["/usr/bin/open".to_string(), path]),
            launch_probe_note: Some(
                "Cursor.app bundle detection proves only that an app bundle exists; run the launch probe successfully, or use a working `cursor` CLI, before treating this runtime as capable of private in-client render proof. If the probe reports kLSNoExecutableErr, repair or reinstall Cursor before collecting release proof.".to_string(),
            ),
            required_for_mcp_registration: false,
        };
    }
    McpClientRuntime {
        target: "Cursor app or cursor CLI",
        status: "not_cli_detectable",
        detection_method: "manual_gui_app_or_cli_check",
        path: None,
        launch_probe_command: None,
        launch_probe_note: Some(
            "Cursor private proof still requires a real Cursor UI or `cursor` CLI session; app-hook, render, and review-action evidence cannot be inferred from MCP config alone."
                .to_string(),
        ),
        required_for_mcp_registration: false,
    }
}

fn continue_runtime() -> McpClientRuntime {
    continue_runtime_from(
        find_executable_on_path("continue"),
        first_existing_path(&continue_config_candidates()),
    )
}

fn continue_runtime_from(
    cli_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> McpClientRuntime {
    if let Some(path) = cli_path {
        return McpClientRuntime {
            target: "continue",
            status: "detected",
            detection_method: "path_executable_scan",
            path: Some(path.to_string_lossy().to_string()),
            launch_probe_command: None,
            launch_probe_note: None,
            required_for_mcp_registration: false,
        };
    }
    if let Some(path) = config_path {
        return McpClientRuntime {
            target: "Continue MCP config",
            status: "detected",
            detection_method: "continue_config_scan",
            path: Some(path.to_string_lossy().to_string()),
            launch_probe_command: None,
            launch_probe_note: None,
            required_for_mcp_registration: false,
        };
    }
    McpClientRuntime {
        target: "Continue extension/config",
        status: "not_cli_detectable",
        detection_method: "manual_extension_config_check",
        path: None,
        launch_probe_command: None,
        launch_probe_note: None,
        required_for_mcp_registration: false,
    }
}

fn path_client_runtime(
    target: &'static str,
    required_for_mcp_registration: bool,
) -> McpClientRuntime {
    let path = find_executable_on_path(target).map(|path| path.to_string_lossy().to_string());
    let status = if path.is_some() { "detected" } else { "missing" };
    McpClientRuntime {
        target,
        status,
        detection_method: "path_executable_scan",
        path,
        launch_probe_command: None,
        launch_probe_note: None,
        required_for_mcp_registration,
    }
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            if let Some(pathext) = std::env::var_os("PATHEXT") {
                for ext in std::env::split_paths(&pathext) {
                    let ext = ext.to_string_lossy();
                    let candidate = dir.join(format!("{name}{ext}"));
                    if is_executable_file(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

fn cursor_app_bundle_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/Cursor.app")];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Applications/Cursor.app"));
    }
    candidates
}

fn continue_config_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let continue_dir = home.join(".continue");
        candidates.push(continue_dir.join("mcpServers/soma.json"));
        candidates.push(continue_dir.join("config.yaml"));
        candidates.push(continue_dir.join("config.yml"));
        candidates.push(continue_dir.join("config.json"));
        candidates.push(continue_dir.join("config.ts"));
    }
    candidates
}

fn first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.exists()).cloned()
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn readiness_next_step(
    client: McpClientKind,
    command: &Path,
    mcp_registration_ready: bool,
    client_runtime: &McpClientRuntime,
) -> String {
    if !mcp_registration_ready {
        return "Fix the failed MCP config checks before registering this client.".to_string();
    }
    if client_runtime.required_for_mcp_registration && client_runtime.status == "missing" {
        return format!(
            "Install or expose `{}` on PATH, then rerun this check before registering SOMA.",
            client_runtime.target
        );
    }

    let command = command.to_string_lossy();
    match client {
        McpClientKind::ClaudeCode => {
            format!(
                "Add or verify the generated mcpServers.soma entry in {}; use explicit MCP capture or a verified Stop hook before claiming automatic capture.",
                client.target_path_hint()
            )
        }
        McpClientKind::CodexCli => {
            format!(
                "Run or verify `codex mcp add soma -- {command} mcp-serve`, or merge mcp_servers.soma into {}; launch with `tools/soma-codex-cli.sh` for managed shell scope.",
                client.target_path_hint()
            )
        }
        McpClientKind::CodexApp => {
            format!(
                "Merge or verify mcp_servers.soma in {}; then inspect `{} clients --client codex-app --brief` for the one-screen readiness handoff and `{} adapter-binding-proof --proof-session --client codex-app --brief` for the proof-session card before claiming automatic app capture or in-client review actions.",
                client.target_path_hint(),
                command,
                command
            )
        }
        McpClientKind::Cursor => {
            format!(
                "Place or verify the generated mcpServers.soma entry in {}; then inspect `{} clients --client cursor --brief` for the one-screen readiness handoff and `{} adapter-binding-proof --proof-session --client cursor --brief` for the proof-session card before private hook/render/review-action proof.",
                client.target_path_hint(),
                command,
                command
            )
        }
        McpClientKind::Continue => {
            format!(
                "Write or verify the generated MCP server JSON at {}; then inspect `{} clients --client continue --brief` for the one-screen readiness handoff and `{} adapter-binding-proof --proof-session --client continue --brief` for the proof-session card before private hook/render/review-action proof.",
                client.target_path_hint(),
                command,
                command
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_runtime_detects_app_bundle_without_requiring_cli() {
        let runtime = cursor_runtime_from(None, Some(PathBuf::from("/Applications/Cursor.app")));

        assert_eq!(runtime.status, "detected");
        assert_eq!(runtime.target, "Cursor.app");
        assert_eq!(runtime.detection_method, "macos_app_bundle_scan");
        assert_eq!(runtime.path.as_deref(), Some("/Applications/Cursor.app"));
        assert_eq!(
            runtime.launch_probe_command.as_deref(),
            Some(&["/usr/bin/open".to_string(), "/Applications/Cursor.app".to_string()][..])
        );
        assert!(runtime
            .launch_probe_note
            .as_deref()
            .is_some_and(|note| note.contains("bundle detection proves only")));
        assert!(runtime
            .launch_probe_note
            .as_deref()
            .is_some_and(|note| note.contains("kLSNoExecutableErr")));
        assert!(!runtime.required_for_mcp_registration);
    }

    #[test]
    fn cursor_runtime_without_cli_or_app_is_manual_check_not_missing() {
        let runtime = cursor_runtime_from(None, None);

        assert_eq!(runtime.status, "not_cli_detectable");
        assert_eq!(runtime.target, "Cursor app or cursor CLI");
        assert_eq!(runtime.detection_method, "manual_gui_app_or_cli_check");
        assert!(runtime.launch_probe_command.is_none());
        assert!(runtime
            .launch_probe_note
            .as_deref()
            .is_some_and(|note| note.contains("requires a real Cursor UI")));
        assert!(!runtime.required_for_mcp_registration);
    }

    #[test]
    fn continue_runtime_detects_config_without_requiring_cli() {
        let runtime = continue_runtime_from(
            None,
            Some(PathBuf::from("/Users/example/.continue/mcpServers/soma.json")),
        );

        assert_eq!(runtime.status, "detected");
        assert_eq!(runtime.target, "Continue MCP config");
        assert_eq!(runtime.detection_method, "continue_config_scan");
        assert_eq!(runtime.path.as_deref(), Some("/Users/example/.continue/mcpServers/soma.json"));
        assert!(!runtime.required_for_mcp_registration);
    }

    #[test]
    fn continue_runtime_without_cli_or_config_is_manual_check_not_missing() {
        let runtime = continue_runtime_from(None, None);

        assert_eq!(runtime.status, "not_cli_detectable");
        assert_eq!(runtime.target, "Continue extension/config");
        assert_eq!(runtime.detection_method, "manual_extension_config_check");
        assert!(!runtime.required_for_mcp_registration);
    }
}
