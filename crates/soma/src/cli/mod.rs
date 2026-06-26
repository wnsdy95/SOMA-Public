//! CLI subcommand handlers. Each file is one verb from the plan's
//! canonical surface.

pub mod adapter_binding_proof;
pub mod adapter_capture;
pub mod adapter_cloud_output;
pub mod adapter_lifecycle;
pub mod adapter_spool;
pub mod backfill;
pub mod binary_identity;
#[cfg(feature = "pty-capture")]
pub mod capture;
pub mod client_status;
pub mod context;
pub mod diagnose;
pub mod forget;
pub mod ingest;
pub mod inspect;
pub mod install;
pub mod learning_status;
pub mod logs;
pub mod mcp_config;
pub mod persona;
pub mod persona_registry;
pub mod profile;
pub mod projects;
pub mod recall;
#[cfg(feature = "dashboard")]
pub mod serve;
pub mod session;
#[cfg(unix)]
pub mod shell_init;
#[cfg(unix)]
pub mod start;
#[cfg(unix)]
pub mod status;
#[cfg(unix)]
pub mod stop;
// Legacy CLAUDE.md SOMA-section migration/debug helper.
pub mod sync;

use clap::{Parser, Subcommand, ValueEnum};

/// D134 close — `--color` policy. `auto` honours `NO_COLOR` and tty
/// detection downstream; `always` and `never` are blunt overrides.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

/// SOMA — local context layer for cloud LLMs. Captures local work
/// history and renders cited ContextEnvelopes through CLI and MCP
/// surfaces.
///
/// Global flags apply to every subcommand; `RUST_LOG` can still
/// override the verbosity-derived logging filter.
#[derive(Debug, Parser)]
#[command(name = "soma", version, about = "SOMA — local context layer for cloud LLMs")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// Color policy for diagnostic output. `auto` (default) honours
    /// `NO_COLOR` env and tty detection.
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,

    /// Increase verbosity. Stacks: `-v` info → `-vv` debug → `-vvv`
    /// trace. `RUST_LOG` env always wins if set.
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress info-level output (drops the base level to `warn`).
    /// Mutually informative with `-v`: when both are passed, the
    /// verbosity arithmetic in `main::compute_verbosity` resolves it.
    #[arg(short = 'q', long, global = true, action = clap::ArgAction::SetTrue)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand)]
// `IngestArgs` carries 11 Option<String> fields — the whole enum is
// ~270 bytes. Boxing the largest variant would save ~200 bytes per
// `Cmd` value, but clap's generated `Parser` still owns one at a
// time (top-level Cli::parse), and the CLI binary instantiates
// exactly one per process. `cargo build --release` binary size is
// the same either way. Suppressing the clippy warning keeps the
// match arms in `main.rs` straightforward (`Cmd::Ingest(args)`
// instead of `Cmd::Ingest(boxed)` + deref).
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    /// List local named SOMA personas and their isolated stores.
    #[command(
        name = "list",
        long_about = "List local named SOMA personas. Each persona maps to its own local SOMA_DB so learning state stays isolated per terminal when activated with `soma call <name>`."
    )]
    List(PersonaListArgs),
    /// Create a local named SOMA persona with its own private store.
    #[command(
        name = "create",
        long_about = "Create a local named SOMA persona under ~/.soma/personas/<name>/ with an isolated soma.db. Use `eval \"$(soma call <name>)\"` in a terminal to make that shell use the persona."
    )]
    Create(PersonaCreateArgs),
    /// Activate a local named SOMA persona for the current shell.
    #[command(
        name = "call",
        alias = "activate",
        long_about = "Print shell exports that activate a local named SOMA persona. Use `eval \"$(soma call <name>)\"`; add `--client <client> --project <project>` to activate persona, session, and project scope in one terminal command. Alias: `soma activate <name>`."
    )]
    Call(PersonaCallArgs),
    /// Start the resident runtime (blocking).
    Start,
    /// Signal the resident runtime to shut down.
    Stop,
    /// Report resident runtime status.
    Status(StatusArgs),
    /// Manage shell-visible SOMA session scope for multi-terminal work.
    #[command(
        name = "session",
        long_about = "Manage shell-visible SOMA session scope. Use `eval \"$(soma session start --client codex-cli)\"` or `eval \"$(soma session start --client claude-code)\"` to stamp terminal capture and cloud-CLI adapters with the same local session_id."
    )]
    Session(SessionArgs),
    /// Install the LaunchAgent for always-on resident operation.
    Install(InstallArgs),
    /// Remove the LaunchAgent.
    Uninstall(InstallArgs),
    /// Record an AI interaction or terminal episode.
    Ingest(IngestArgs),
    /// Record one normalized editor-adapter turn payload.
    #[command(
        name = "adapter-capture",
        long_about = "Record one normalized editor-adapter turn payload. Reads one JSON object, fills missing local metadata, and writes through the normal ingest pipeline."
    )]
    AdapterCapture(AdapterCaptureArgs),
    /// Record one cloud-output payload as untrusted draft claims.
    #[command(
        name = "adapter-cloud-output",
        long_about = "Record one cloud-output payload as untrusted draft claims tied to a persisted TaskFrame. Reads one JSON object, accepts either task_frame_id or task_frame_query, applies the same critic/proposal gates as soma_capture_cloud_output, and never promotes without later verification."
    )]
    AdapterCloudOutput(AdapterCloudOutputArgs),
    /// Normalize one raw editor lifecycle event into the adapter spool contract.
    #[command(
        name = "adapter-lifecycle",
        long_about = "Normalize one raw editor lifecycle event into SOMA's adapter spool contract. Reads one client hook JSON object, emits {kind,payload}, and optionally appends it to a JSONL spool without ingesting or promoting directly."
    )]
    AdapterLifecycle(AdapterLifecycleArgs),
    /// Drain normalized adapter JSONL spool events.
    #[command(
        name = "adapter-spool",
        long_about = "Drain a checkpointed JSONL spool of normalized editor adapter events. Each line is {kind,payload}; kind=turn forwards to adapter-capture and kind=cloud_output forwards to adapter-cloud-output."
    )]
    AdapterSpool(AdapterSpoolArgs),
    /// Append one normalized event to an adapter JSONL spool.
    #[command(
        name = "adapter-spool-append",
        long_about = "Append one normalized editor adapter event to a JSONL spool. Reads a payload JSON object, wraps it as {kind,payload}, validates the trust-boundary shape, and leaves capture to adapter-spool."
    )]
    AdapterSpoolAppend(AdapterSpoolAppendArgs),
    /// Record observed proof for a client binding manifest.
    #[command(
        name = "adapter-binding-proof",
        long_about = "Record observed proof for a Codex app/Cursor/Continue client binding. Reference manifests and event files are not treated as private app-hook proof unless --proof-level observed_app_hook is paired with explicit operator confirmation. In-client review rendering is tracked separately as observed_in_client_render and requires structured soma.in_client_render_evidence.v1 render evidence bound to the target client, review-render report fingerprint, review workbench version, and review interaction contract version. Review action loop proof is tracked separately as observed_review_action and requires an operator-confirmed soma_review_action report whose control_id was visible in a prior in-client render proof."
    )]
    AdapterBindingProof(AdapterBindingProofArgs),
    /// Summarize MCP/runtime/private-capture readiness for supported clients.
    #[command(
        name = "clients",
        long_about = "Summarize SOMA client readiness for Claude Code, Codex CLI, Codex app, Cursor, and Continue. This read-only status combines generated MCP config checks with stored client-binding proof rows; it records no proof, installs no hook, creates no verification event, and never promotes cloud drafts."
    )]
    Clients(ClientStatusArgs),
    /// Summarize semantic learning/L4 review readiness.
    #[command(
        name = "learning",
        long_about = "Summarize SOMA semantic learning review readiness. This read-only status previews L4 semantic_fact candidates, shows pending semantic review work, identifies cloud-draft blockers, and records no proposal, verification event, apply action, or cloud-draft promotion."
    )]
    Learning(LearningStatusArgs),
    /// Show project provenance accumulated inside the active persona store.
    #[command(
        name = "projects",
        long_about = "Show project experience provenance inside the active SOMA persona store. This read-only view groups stored episodes by project so operators can verify which projects a persona has learned from, which sessions/sources prove it, and whether project scopes are mixing."
    )]
    Projects(ProjectExperienceArgs),
    /// Recall ranked local episodes for context inspection.
    Recall(RecallArgs),
    /// Hidden optional context-profile diagnostic. Use `--recompute`
    /// to re-run extractors before printing. Core clients should prefer
    /// `soma context render` or MCP `soma://context/*`.
    #[command(hide = true)]
    Profile(ProfileArgs),
    /// Render the ContextEnvelope cloud LLM clients receive from SOMA.
    Context(ContextArgs),
    /// Print the current config (resolved values).
    Config,
    /// Inspect local context store diagnostics.
    Inspect(InspectArgs),
    /// Delete stored context episodes (audited).
    Forget(ForgetArgs),
    /// MCP resource server — Claude Code / Codex CLI / Codex app / Cursor / Continue spawn
    /// this over stdio and read `soma://context/*` resources.
    #[command(name = "mcp-serve")]
    McpServe,
    /// Generate and check dry-run MCP client registration JSON.
    #[command(
        name = "mcp-config",
        long_about = "Generate dry-run MCP client registration JSON for Claude Code, Codex CLI, Codex app, Cursor, or Continue. The emitted config launches `soma mcp-serve` only; it does not install private editor lifecycle hooks."
    )]
    McpConfig(McpConfigArgs),
    /// Hidden legacy context/profile diagnostic — force the slow_loop's
    /// narrative synthesis to run *now*. Deletion candidate unless it feeds a
    /// ContextEnvelope field; core context-layer clients should prefer
    /// `soma context render` or MCP `soma://context/*`.
    #[command(name = "synthesize-narrative")]
    #[command(hide = true)]
    SynthesizeNarrative,
    /// `soma capture --pty` — spawn the user's shell inside a pty
    /// and capture every command via OSC 133 boundaries. Available
    /// only when built with `--features pty-capture`.
    #[cfg(feature = "pty-capture")]
    Capture(CaptureArgs),
    /// Print a single diagnostic JSON object for support/debugging.
    /// Includes version, enabled cargo features, resident liveness,
    /// DB stats, weight shapes, ContextEnvelope disposition, resident
    /// status, and sub-step failures.
    Diagnose(DiagnoseArgs),
    /// Backfill primary embedder vectors so recall quality catches up
    /// immediately after a model or index change.
    Backfill,
    /// Read SOMA's rolling local log file. Subcommand `tail [-n N]`
    /// prints the last N lines.
    Logs(logs::LogsArgs),
    /// Emit a shell completion script for bash, zsh, fish, elvish, or
    /// PowerShell.
    Completions(CompletionsArgs),
    /// Hidden compatibility namespace for local named personas and legacy
    /// context/profile helper artifacts. Root-level `soma list/create/call`
    /// remains the primary persona registry surface; `soma persona
    /// list/create/call` exists for operators who look for an explicit
    /// namespace. Legacy `read/regen/inject` subcommands stay available only
    /// for migration/debug and disabled legacy prompt-injection hooks; this is
    /// not a first-person identity surface, and core clients should prefer
    /// `soma context render` or MCP `soma://context/*`.
    #[command(hide = true)]
    Persona(PersonaArgs),
    /// Hidden legacy CLAUDE.md migration helper — splice SOMA's debug/migration
    /// context into `<cwd>/CLAUDE.md`.
    /// Core clients should prefer MCP `soma://context/*` resources.
    #[command(name = "sync-claudemd", hide = true)]
    SyncClaudemd(SyncClaudemdArgs),
    /// Local web dashboard for SOMA's transparency surface. Binds an
    /// axum HTTP server on `127.0.0.1:8765` (override with `--port`
    /// / `--bind`) and blocks until Ctrl-C. Requires
    /// `--features dashboard`.
    #[cfg(feature = "dashboard")]
    Serve(ServeArgs),
}

/// `soma context <sub>` flags.
#[derive(Debug, Parser)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub mode: ContextMode,
}

#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct McpConfigArgs {
    /// Target MCP client config shape.
    #[arg(long, value_enum, required_unless_present = "all", conflicts_with = "all")]
    pub client: Option<mcp_config::McpClientKind>,
    /// Render/check every supported MCP client in one aggregate report.
    #[arg(long)]
    pub all: bool,
    /// Path to the soma binary. Relative paths with a directory component are resolved from cwd.
    #[arg(long)]
    pub command: Option<String>,
    /// Validate the generated config and print a check report instead.
    #[arg(long)]
    pub check: bool,
    /// Include a read-only client hook/watcher plan. This does not install private editor hooks.
    #[arg(long)]
    pub hook_plan: bool,
    /// Render a compact human readiness handoff instead of JSON.
    #[arg(long)]
    pub brief: bool,
    /// Explicit JSON output alias for automation. JSON is already the default.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub json: bool,
    /// Output format selector. MCP config artifacts emit JSON by default;
    /// `brief` renders a compact human readiness handoff for check reports.
    #[arg(long, default_value = "json", value_parser = ["json", "brief"])]
    pub format: String,
}

impl McpConfigArgs {
    pub fn wants_brief_output(&self) -> bool {
        !self.json && (self.brief || self.format.trim().eq_ignore_ascii_case("brief"))
    }
}

#[derive(Debug, Clone, Parser)]
pub struct DiagnoseArgs {
    /// Explicit JSON output alias for automation. Diagnose always emits JSON.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub json: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct StatusArgs {
    /// Output format: `text` (default) or `json`.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
    /// Explicit JSON output alias for status automation.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub json: bool,
}

impl Default for StatusArgs {
    fn default() -> Self {
        Self { format: "text".to_string(), json: false }
    }
}

impl StatusArgs {
    pub fn wants_json_output(&self) -> bool {
        self.json || self.format.trim().eq_ignore_ascii_case("json")
    }
}

#[derive(Debug, Clone, Parser)]
pub struct ClientStatusArgs {
    /// Limit to one supported client, or pass `all` for the full readiness matrix.
    #[arg(long, value_parser = parse_client_status_filter)]
    pub client: Option<String>,
    /// Scope semantic review/project provenance guidance to one project.
    #[arg(long)]
    pub project: Option<String>,
    /// Absolute path to the soma binary to place in generated MCP config checks.
    /// Defaults to the current executable.
    #[arg(long)]
    pub command: Option<String>,
    /// Override the SOMA DB used for stored client-binding proof status.
    #[arg(long)]
    pub db_path: Option<String>,
    /// Optional soma.client_dogfood_report.v1 JSON artifact from tools/client-dogfood-report.sh.
    /// Defaults to $SOMA_CLIENT_DOGFOOD_REPORT or ~/.soma/reports/client-dogfood-latest.json
    /// when present, so soma clients can cite external multi-terminal/persona/project evidence
    /// without recording proof rows or mutating learning state.
    #[arg(long)]
    pub dogfood_report: Option<String>,
    /// Optional soma.real_cli_dogfood_probe.v1 JSON artifact from tools/real-cli-dogfood-probe.sh.
    /// Defaults to $SOMA_REAL_CLI_DOGFOOD_REPORT or ~/.soma/reports/real-cli-dogfood-latest.json
    /// when present, so soma clients can show real Codex/Claude CLI approval/auth blockers.
    #[arg(long)]
    pub real_cli_dogfood_report: Option<String>,
    /// Maximum client-binding proof rows to inspect.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
    /// Output format: `text` (default) or `json`.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
    /// Render a compact operator handoff card instead of the full text matrix.
    /// Ignored when JSON output is selected.
    #[arg(long)]
    pub brief: bool,
    /// Output a machine-readable JSON readiness report.
    #[arg(long)]
    pub json: bool,
}

impl ClientStatusArgs {
    pub fn wants_json_output(&self) -> bool {
        self.json || self.format.trim().eq_ignore_ascii_case("json")
    }

    pub fn wants_brief_output(&self) -> bool {
        self.brief
    }
}

fn parse_client_status_filter(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    if normalized.is_empty() {
        return Err(
            "expected all, claude-code, codex-cli, codex-app, cursor, or continue".to_string()
        );
    }
    if normalized == "all" || mcp_config::McpClientKind::parse_slug(&normalized).is_some() {
        Ok(normalized)
    } else {
        Err(format!(
            "invalid client `{value}`; expected all, claude-code, codex-cli, codex-app, cursor, or continue"
        ))
    }
}

#[derive(Debug, Clone, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct LearningStatusArgs {
    /// Hidden compatibility alias: `soma learning status` renders the same
    /// read-only semantic learning status as `soma learning`.
    #[arg(value_name = "STATUS", hide = true, value_parser = ["status"])]
    pub status_alias: Option<String>,
    /// Optional project scope for semantic/review status.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope for semantic/review status.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Client hint for review digest layout.
    #[arg(long)]
    pub client: Option<String>,
    /// Maximum long-term claims and review rows to inspect.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Minimum repeated verified L3 support required for L4 semantic preview.
    #[arg(long, default_value_t = 2)]
    pub min_support: usize,
    /// Maximum semantic candidate rows to print.
    #[arg(long, default_value_t = 10)]
    pub candidate_limit: usize,
    /// Maximum pending review rows to print.
    #[arg(long, default_value_t = 10)]
    pub review_limit: usize,
    /// Override the SOMA DB used for stored learning/review state.
    #[arg(long)]
    pub db_path: Option<String>,
    /// Optional soma.client_dogfood_report.v1 JSON artifact from tools/client-dogfood-report.sh.
    /// Defaults to $SOMA_CLIENT_DOGFOOD_REPORT or ~/.soma/reports/client-dogfood-latest.json
    /// when present, so learning can cite last-run semantic dogfood evidence without claiming
    /// live review queues are clear.
    #[arg(long)]
    pub dogfood_report: Option<String>,
    /// Output format: `text` (default), `brief`, `markdown`/`md`, or `json`.
    #[arg(long, default_value = "text", value_parser = ["text", "brief", "markdown", "md", "json"])]
    pub format: String,
    /// Render a compact human handoff card instead of the full text matrix.
    /// Ignored when JSON output is selected.
    #[arg(long)]
    pub brief: bool,
    /// Output a machine-readable JSON learning review report.
    #[arg(long)]
    pub json: bool,
}

impl LearningStatusArgs {
    pub fn wants_json_output(&self) -> bool {
        self.json || self.format.trim().eq_ignore_ascii_case("json")
    }

    pub fn wants_brief_output(&self) -> bool {
        self.brief || self.format.trim().eq_ignore_ascii_case("brief")
    }
}

#[derive(Debug, Parser)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub mode: SessionMode,
}

#[derive(Debug, Parser)]
pub struct PersonaListArgs {
    /// Output format: `text` (default) or `json`.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
    /// Output a machine-readable JSON report.
    #[arg(long)]
    pub json: bool,
}

impl PersonaListArgs {
    pub fn wants_json_output(&self) -> bool {
        self.json || self.format.trim().eq_ignore_ascii_case("json")
    }
}

#[derive(Debug, Parser)]
pub struct PersonaCreateArgs {
    /// Local persona name. Directory separators and control characters are rejected.
    pub name: String,
    /// Optional human note stored in the persona metadata.
    #[arg(long)]
    pub description: Option<String>,
    /// Treat an existing persona as success and return its current metadata.
    #[arg(long)]
    pub if_not_exists: bool,
    /// Output a machine-readable JSON report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Parser)]
pub struct PersonaCallArgs {
    /// Local persona name to activate. `default` maps to ~/.soma/soma.db.
    pub name: String,
    /// Create the persona first if it does not already exist.
    #[arg(long)]
    pub create: bool,
    /// Shell syntax for printed exports.
    #[arg(long, value_enum, default_value_t = SessionShell::Auto)]
    pub shell: SessionShell,
    /// Also start or attach a SOMA session for this terminal with the given client/source.
    /// When omitted but --project, --session-id, or --thread-key is passed, defaults to `terminal`.
    #[arg(long)]
    pub client: Option<String>,
    /// Also export project scope for this terminal. Defaults to SOMA_PROJECT or basename($PWD)
    /// when any session option is used.
    #[arg(long)]
    pub project: Option<String>,
    /// Attach to an existing SOMA session id instead of generating a new one.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Optional operator-confirmed durable thread key to expose with the session scope.
    #[arg(long)]
    pub thread_key: Option<String>,
    /// Output a machine-readable JSON report instead of shell exports.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum SessionMode {
    /// Start a new SOMA-managed shell session and print eval-able exports.
    Start(SessionStartArgs),
    /// Attach the current shell to an existing SOMA session id.
    Attach(SessionAttachArgs),
    /// Show the SOMA session variables visible to this process.
    Status(SessionStatusArgs),
    /// Print commands that clear SOMA session variables from this shell.
    Clear(SessionClearArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum SessionShell {
    #[default]
    Auto,
    Sh,
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Parser)]
pub struct SessionStartArgs {
    /// Client/source scope to stamp into SOMA-managed CLI sessions.
    #[arg(long, default_value = "terminal")]
    pub client: String,
    /// Project scope. Defaults to SOMA_PROJECT or basename($PWD).
    #[arg(long)]
    pub project: Option<String>,
    /// Optional operator-confirmed durable thread key to expose to clients.
    #[arg(long)]
    pub thread_key: Option<String>,
    /// Shell syntax to render. Auto chooses fish only when $SHELL ends in fish.
    #[arg(long, value_enum, default_value_t = SessionShell::Auto)]
    pub shell: SessionShell,
    /// Render a JSON report instead of shell exports.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Parser)]
pub struct SessionAttachArgs {
    /// Existing SOMA session id to attach this terminal to.
    #[arg(long)]
    pub session_id: String,
    /// Client/source scope. Defaults to SOMA_CLIENT or terminal.
    #[arg(long)]
    pub client: Option<String>,
    /// Project scope. Defaults to SOMA_PROJECT or basename($PWD).
    #[arg(long)]
    pub project: Option<String>,
    /// Optional operator-confirmed durable thread key to expose to clients.
    #[arg(long)]
    pub thread_key: Option<String>,
    /// Shell syntax to render. Auto chooses fish only when $SHELL ends in fish.
    #[arg(long, value_enum, default_value_t = SessionShell::Auto)]
    pub shell: SessionShell,
    /// Render a JSON report instead of shell exports.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Parser)]
pub struct SessionStatusArgs {
    /// Render a JSON report instead of human text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Parser)]
pub struct SessionClearArgs {
    /// Shell syntax to render. Auto chooses fish only when $SHELL ends in fish.
    #[arg(long, value_enum, default_value_t = SessionShell::Auto)]
    pub shell: SessionShell,
    /// Render a JSON report instead of shell unset commands.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum ContextMode {
    /// Render a scoped ContextEnvelope for inspection or tooling.
    Render(ContextRenderArgs),
    /// Render a single cloud-facing artifact with TaskFrame and ContextEnvelope.
    Prompt(ContextPromptArgs),
    /// Build and persist a deterministic TaskFrame for inspection.
    #[command(name = "task-frame")]
    TaskFrame(ContextTaskFrameArgs),
    /// Inspect or apply TaskFrame retention/compaction policy.
    #[command(name = "task-frames")]
    TaskFrames(ContextTaskFramesArgs),
    /// Record an evidence-backed outcome for a persisted TaskFrame.
    #[command(name = "task-frame-outcome")]
    TaskFrameOutcome(ContextTaskFrameOutcomeArgs),
    /// Inspect or apply stale low-access L3 proxy decay policy.
    #[command(name = "l3-decay")]
    L3Decay(ContextL3DecayArgs),
    /// Promote eligible L2 latent proxies to L3 through explicit lifecycle policy.
    #[command(name = "l2-promote")]
    L2Promote(ContextL2PromoteArgs),
    /// Predict active evidence-backed latent proxies for a query without mutating memory.
    #[command(name = "latent-predict")]
    LatentPredict(ContextLatentPredictArgs),
    /// Render an inspectable latent interface packet for future cloud latent channels.
    #[command(name = "latent-packet")]
    LatentPacket(ContextLatentPacketArgs),
    /// Score latent predictor hits against JSONL or storage-derived evidence cases.
    #[command(name = "latent-eval")]
    LatentEval(ContextLatentEvalArgs),
    /// Preflight stable session-to-thread identity before enabling thread resources.
    #[command(name = "thread-identity")]
    ThreadIdentity(ContextThreadIdentityArgs),
    /// Record a user correction so future ContextEnvelopes can cite it.
    Correct(ContextCorrectArgs),
    /// Record user/tool/test/local verification for a claim record.
    #[command(name = "verify-claim")]
    VerifyClaim(ContextVerifyClaimArgs),
    /// Review and apply asynchronous learning critic proposals.
    #[command(name = "learning-proposals")]
    LearningProposals(ContextLearningProposalsArgs),
    /// Show pending claim verification and proposal review work.
    #[command(name = "review-queue")]
    ReviewQueue(ContextReviewQueueArgs),
    /// Show flattened client action options from the review queue.
    #[command(name = "review-actions")]
    ReviewActions(ContextReviewActionsArgs),
    /// Build a read-only soma_review_batch payload template from review actions.
    #[command(name = "review-batch-template")]
    ReviewBatchTemplate(ContextReviewBatchTemplateArgs),
    /// Render a read-only human review report with queue/actions/batch guidance.
    #[command(name = "review-report")]
    ReviewReport(ContextReviewReportArgs),
    /// Render a compact read-only client notification digest.
    #[command(name = "review-digest")]
    ReviewDigest(ContextReviewDigestArgs),
    /// Acknowledge a rendered review digest notification without changing trust.
    #[command(name = "review-digest-ack")]
    ReviewDigestAck(ContextReviewDigestAckArgs),
    /// Compile a read-only client-specific review rendering plan.
    #[command(name = "review-render")]
    ReviewRender(ContextReviewRenderArgs),
    /// Drain safe review work using the verified non-destructive policy.
    #[command(name = "review-drain")]
    ReviewDrain(ContextReviewDrainArgs),
    /// Run selected scheduler review/learning subpasses through existing gates.
    #[command(name = "scheduler-run")]
    SchedulerRun(ContextSchedulerRunArgs),
    /// Propose L4 semantic promotions from repeated verified L3 evidence.
    #[command(name = "semantic-proposals")]
    SemanticProposals(ContextSemanticProposalsArgs),
    /// Create review proposals from unresolved L2 open decisions.
    #[command(name = "open-decision-proposals")]
    OpenDecisionProposals(ContextOpenDecisionProposalsArgs),
    /// Take one action on a review queue claim or proposal.
    #[command(name = "review-action")]
    ReviewAction(ContextReviewActionArgs),
    /// Record a verification-only batch of review actions.
    #[command(name = "review-batch")]
    ReviewBatch(ContextReviewBatchArgs),
    /// Audit ContextEnvelope evidence and optional TaskFrame privacy projection.
    Audit(ContextAuditArgs),
    /// Audit persisted claim/proposal trust-boundary invariants.
    #[command(name = "trust-audit")]
    TrustAudit(ContextTrustAuditArgs),
    /// Compose release/client hardening gates from existing read-only audits.
    #[command(name = "hardening-report")]
    HardeningReport(ContextHardeningReportArgs),
    /// Explain why ContextEnvelope sections were included, with evidence.
    Why(ContextWhyArgs),
    /// Compare HNSW vs opt-in Hopfield at the ContextEnvelope ranking boundary.
    #[cfg(feature = "cognitive")]
    #[command(name = "compare-ranking")]
    CompareRanking(ContextCompareRankingArgs),
}

#[derive(Debug, Parser)]
pub struct ContextRenderArgs {
    /// Optional semantic query. When absent, the envelope is recent-only.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Optional persisted TaskFrame id to shape scope/query/thread_state.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Output format: `xml` (default) or `json`.
    #[arg(long, default_value = "xml")]
    pub format: String,
    /// Ask the local Ollama helper to add a cited `compiler_notes`
    /// section for a cloud LLM. This is not a final-answer surface;
    /// if Ollama is unavailable, rendering falls back to the
    /// deterministic ContextEnvelope.
    #[arg(long)]
    pub local_compiler: bool,
    /// Override the local compiler endpoint. Defaults to Ollama's
    /// standard localhost endpoint.
    #[arg(long)]
    pub local_compiler_endpoint: Option<String>,
    /// Override the local compiler model. Defaults to SOMA's local
    /// context compiler preview model.
    #[arg(long)]
    pub local_compiler_model: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextPromptArgs {
    /// Optional semantic query. When absent, a TaskFrame goal can supply it.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Optional persisted TaskFrame id to include and use for scope/query/thread_state.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Opt into local compiler notes when a configured local model can cite evidence.
    #[arg(long)]
    pub local_compiler: bool,
    /// Override local compiler endpoint.
    #[arg(long)]
    pub local_compiler_endpoint: Option<String>,
    /// Override local compiler preview model.
    #[arg(long)]
    pub local_compiler_model: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextTaskFrameArgs {
    /// Current task query or user request. When absent, recent scoped evidence is used.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Defaults to the current directory basename.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Override the cwd recorded in the TaskFrame scope.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Client surface requesting the frame.
    #[arg(long, default_value = "cli")]
    pub client: Option<String>,
    /// Explicitly allow local_private fields to project to cloud-facing TaskFrame JSON.
    #[arg(long)]
    pub allow_local_private_projection: bool,
    /// Audit reason required when --allow-local-private-projection is set.
    #[arg(long)]
    pub local_private_projection_reason: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextTaskFramesArgs {
    #[command(subcommand)]
    pub mode: ContextTaskFramesMode,
}

#[derive(Debug, Subcommand)]
pub enum ContextTaskFramesMode {
    /// Report or apply TaskFrame retention for unreferenced old frames.
    Retention(ContextTaskFrameRetentionArgs),
    /// List evidence-backed TaskFrame outcome records.
    Outcomes(ContextTaskFrameOutcomesArgs),
}

#[derive(Debug, Parser)]
pub struct ContextTaskFrameRetentionArgs {
    /// Retain TaskFrames at least this many days before they become prune candidates.
    #[arg(long, default_value_t = crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS)]
    pub older_than_days: i64,
    /// Optional project scope for retention.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope for retention.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Actually delete eligible unreferenced TaskFrames. Without this flag, dry-run only.
    #[arg(long)]
    pub apply: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextTaskFrameOutcomesArgs {
    /// Optional persisted TaskFrame id to inspect.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Optional project scope.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum outcome rows to return.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextTaskFrameOutcomeArgs {
    /// Persisted TaskFrame id this outcome closes or evaluates.
    #[arg(long)]
    pub task_frame_id: i64,
    /// Outcome type: accepted, revised, rejected, verified, applied, failed, or abandoned.
    #[arg(long)]
    pub outcome_type: String,
    /// Human-readable outcome summary. This is corpus text, not promotion trust.
    #[arg(long)]
    pub summary: String,
    /// Evidence kind for the outcome, e.g. user, test, tool, local_observation, correction.
    #[arg(long)]
    pub evidence_kind: String,
    /// Evidence id for the outcome.
    #[arg(long)]
    pub evidence_id: String,
    /// Optional source/name for the evidence.
    #[arg(long)]
    pub evidence_source: Option<String>,
    /// Claim records linked to this outcome. Repeatable.
    #[arg(long = "claim-id")]
    pub claim_ids: Vec<i64>,
    /// Learning critic proposals linked to this outcome. Repeatable.
    #[arg(long = "proposal-id")]
    pub proposal_ids: Vec<i64>,
    /// Latent proxies this outcome says were useful. Repeatable.
    #[arg(long = "latent-proxy-id")]
    pub latent_proxy_ids: Vec<i64>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextL3DecayArgs {
    /// Consider only L3 proxies whose last access or update is older than this many days.
    #[arg(long, default_value_t = 90)]
    pub older_than_days: i64,
    /// Explicit cutoff timestamp in nanoseconds for reproducible audits/tests.
    #[arg(long)]
    pub cutoff_ns: Option<i64>,
    /// Decay only proxies with access_count at or below this value.
    #[arg(long, default_value_t = 0)]
    pub max_access_count: i64,
    /// Lifecycle transition reason recorded in memory_lifecycle_events.
    #[arg(long, default_value = "stale low-access L3 proxy")]
    pub reason: String,
    /// Preview matching L3 proxies without changing lifecycle state.
    #[arg(long)]
    pub dry_run: bool,
    /// Maximum L3 proxies to inspect.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextL2PromoteArgs {
    /// Optional project scope used to find L2 proxy candidates.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope used to find L2 proxy candidates.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Promote durable proxy types when confidence is at least this value.
    #[arg(long, default_value_t = 0.90)]
    pub min_confidence: f32,
    /// Promote anomaly/conflict proxy types when confidence is at least this value.
    #[arg(long, default_value_t = 0.85)]
    pub anomaly_min_confidence: f32,
    /// Promote repeated active L2 claims after this many scoped support rows.
    #[arg(long, default_value_t = 2)]
    pub min_repeated_support: usize,
    /// Explicitly pin one or more L2 proxy ids into L3, subject to trust/privacy gates.
    #[arg(long = "manual-proxy-id")]
    pub manual_proxy_ids: Vec<i64>,
    /// Lifecycle transition reason recorded in memory_lifecycle_events.
    #[arg(long, default_value = "policy-selected L2 proxy promotion")]
    pub reason: String,
    /// Apply promotion. Omit this flag to preview candidates without mutation.
    #[arg(long)]
    pub apply: bool,
    /// Maximum L2 proxies to inspect.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLatentPredictArgs {
    /// Query used to predict active evidence-backed latent proxies.
    #[arg(long)]
    pub query: String,
    /// Optional project scope. Must match stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum predictions to return.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_LIMIT)]
    pub limit: usize,
    /// Maximum active latent proxies to inspect before scoring.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT)]
    pub scan_limit: usize,
    /// Minimum score required before predictor avoids deterministic fallback.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE)]
    pub min_confidence: f32,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLatentPacketArgs {
    /// Query used to select evidence-backed latent proxies for the packet.
    #[arg(long)]
    pub query: String,
    /// Optional project scope. Must match stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum proxy bindings to include.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_LIMIT)]
    pub limit: usize,
    /// Maximum active latent proxies to inspect before scoring.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT)]
    pub scan_limit: usize,
    /// Minimum score required before a proxy becomes a packet binding.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE)]
    pub min_confidence: f32,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLatentEvalArgs {
    /// Optional JSONL corpus. Each line needs query plus expected_proxy_id or expected_proxy_ids.
    #[arg(long)]
    pub case_jsonl: Option<String>,
    /// Derive expected proxy cases from task_frame_outcomes instead of active proxy self-cases.
    #[arg(long)]
    pub outcome_cases: bool,
    /// Optional project scope. Used as a filter for storage-derived cases and fallback for JSONL cases.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Used as a filter for storage-derived cases and fallback for JSONL cases.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum predictions to return per case.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_LIMIT)]
    pub limit: usize,
    /// Maximum active latent proxies to inspect per case.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT)]
    pub scan_limit: usize,
    /// Maximum storage-derived cases to build when --case-jsonl is omitted.
    #[arg(long, default_value_t = crate::context::latent_eval::DEFAULT_LATENT_EVAL_CASE_LIMIT)]
    pub case_limit: usize,
    /// Minimum prediction score before deterministic fallback.
    #[arg(long, default_value_t = crate::context::latent_predictor::DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE)]
    pub min_confidence: f32,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextThreadIdentityArgs {
    /// Optional project scope. Must match stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Store an operator-confirmed durable thread identity instead of rendering only the preflight report.
    #[arg(long)]
    pub confirm: bool,
    /// Explicit session to bind into the confirmed thread identity. Repeat for multi-session identity.
    #[arg(long = "confirm-session")]
    pub confirm_sessions: Vec<String>,
    /// Required for confirming more than one session into one durable identity.
    #[arg(long)]
    pub confirm_cross_session: bool,
    /// Optional durable thread key. Defaults to a deterministic key from project and confirmed sessions.
    #[arg(long)]
    pub thread_key: Option<String>,
    /// Operator identity or client surface recording the confirmation.
    #[arg(long, default_value = "cli-operator")]
    pub confirmed_by: String,
    /// Required human reason/evidence note for `--confirm`.
    #[arg(long)]
    pub confirmation_reason: Option<String>,
    /// List existing operator-confirmed thread identities instead of rendering the preflight report.
    #[arg(long)]
    pub list_confirmed: bool,
    /// Maximum recent live episodes to inspect.
    #[arg(long, default_value_t = crate::context::thread_identity::DEFAULT_THREAD_IDENTITY_LIMIT)]
    pub limit: usize,
    /// Review threshold for gaps inside a session or between join candidates.
    #[arg(
        long,
        default_value_t = crate::context::thread_identity::DEFAULT_THREAD_JOIN_WINDOW_MINUTES
    )]
    pub join_window_minutes: i64,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextCorrectArgs {
    /// Optional stale claim or assumption being corrected.
    #[arg(long)]
    pub claim: Option<String>,
    /// Current truth to record.
    #[arg(long)]
    pub correction: String,
    /// Optional project scope. Defaults to the current directory basename.
    #[arg(long)]
    pub project: Option<String>,
    /// Record and resolve the correction without a project filter.
    #[arg(long, conflicts_with = "project")]
    pub all_projects: bool,
    /// Optional session identifier for grouping correction events.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextVerifyClaimArgs {
    /// Claim record id to verify, contradict, supersede, or mark inconclusive.
    #[arg(long)]
    pub claim_id: Option<i64>,
    /// Learning critic proposal id whose linked claims should be verified.
    /// For confirmed promotion proposals, already trusted claims are skipped.
    #[arg(long)]
    pub proposal_id: Option<i64>,
    /// Verifier: user, test, tool, local_observation, or correction.
    #[arg(long)]
    pub verifier: String,
    /// Result: confirmed, contradicted, superseded, or inconclusive.
    #[arg(long, default_value = "confirmed")]
    pub result: String,
    /// Evidence ref kind, for example test, tool_result, user, or local_observation.
    #[arg(long)]
    pub evidence_kind: String,
    /// Evidence ref id, such as a test name, tool call id, or local observation id.
    #[arg(long)]
    pub evidence_id: String,
    /// Optional evidence source note.
    #[arg(long)]
    pub evidence_source: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLearningProposalsArgs {
    #[command(subcommand)]
    pub mode: ContextLearningProposalMode,
}

#[derive(Debug, Subcommand)]
pub enum ContextLearningProposalMode {
    /// List learning critic proposals for review.
    List(ContextLearningProposalListArgs),
    /// Apply one proposal through existing verification/lifecycle gates.
    Apply(ContextLearningProposalApplyArgs),
    /// Apply all currently ready proposals through existing gates.
    #[command(name = "apply-ready")]
    ApplyReady(ContextLearningProposalApplyReadyArgs),
    /// Mark a proposal accepted/rejected/waiting for external review.
    #[command(name = "set-status")]
    SetStatus(ContextLearningProposalSetStatusArgs),
}

#[derive(Debug, Parser)]
pub struct ContextLearningProposalListArgs {
    /// Optional project scope. Matches the proposal TaskFrame project.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches the proposal TaskFrame session.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Optional status filter: queued, waiting_verification, accepted, rejected, or applied.
    #[arg(long)]
    pub status: Option<String>,
    /// Maximum proposals to return.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLearningProposalApplyArgs {
    /// Learning critic proposal id to apply.
    #[arg(long)]
    pub proposal_id: i64,
    /// Required to apply destructive decay/forget proposals.
    #[arg(long)]
    pub confirm_destructive: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLearningProposalApplyReadyArgs {
    /// Optional project scope. Matches the proposal TaskFrame project.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches the proposal TaskFrame session.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum open proposals to consider.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Show what would apply without mutating proposal or claim lifecycle.
    #[arg(long)]
    pub dry_run: bool,
    /// Also apply decay proposals. Defaults off because decay is destructive.
    #[arg(long)]
    pub include_decay: bool,
    /// Also close create-candidate/no-op proposals. Defaults off to keep review explicit.
    #[arg(long)]
    pub include_noop: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextLearningProposalSetStatusArgs {
    /// Learning critic proposal id to update.
    #[arg(long)]
    pub proposal_id: i64,
    /// New status: queued, waiting_verification, accepted, rejected, or applied.
    #[arg(long)]
    pub status: String,
    /// Optional review note stored in proposal result JSON.
    #[arg(long)]
    pub note: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewQueueArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to return per queue section.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Output format: `json` (default) or `markdown`.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Explicitly request JSON output. Equivalent to `--format json`.
    #[arg(long)]
    pub json: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewActionsArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to inspect before flattening actions.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Include disabled actions with disabled_reason for client UI previews.
    #[arg(long)]
    pub include_disabled: bool,
    /// Output format: `json` (default), `brief`, or `markdown`.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Explicitly request JSON output. Equivalent to `--format json`.
    #[arg(long)]
    pub json: bool,
    /// Render a compact operator shortlist instead of full JSON.
    #[arg(long)]
    pub brief: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewBatchTemplateArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to inspect before composing the template.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Verification action to template: confirm, contradict, supersede, or inconclusive.
    #[arg(long, default_value = "confirm")]
    pub action: String,
    /// Target type to include: any, claim, or proposal.
    #[arg(long, default_value = "any")]
    pub target_type: String,
    /// Optional verifier type to prefill: user, test, tool, local_observation, correction.
    #[arg(long)]
    pub verifier: Option<String>,
    /// Optional evidence kind to prefill in each operation.
    #[arg(long)]
    pub evidence_kind: Option<String>,
    /// Optional evidence id to prefill in each operation.
    #[arg(long)]
    pub evidence_id: Option<String>,
    /// Optional evidence source to prefill in each operation.
    #[arg(long)]
    pub evidence_source: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewReportArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to inspect before rendering the report.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Include disabled actions with disabled_reason for client UI previews.
    #[arg(long)]
    pub include_disabled: bool,
    /// Verification action to template: confirm, contradict, supersede, or inconclusive.
    #[arg(long, default_value = "confirm")]
    pub action: String,
    /// Target type to include in the batch template: any, claim, or proposal.
    #[arg(long, default_value = "any")]
    pub target_type: String,
    /// Optional verifier type to prefill: user, test, tool, local_observation, correction.
    #[arg(long)]
    pub verifier: Option<String>,
    /// Optional evidence kind to prefill in each templated operation.
    #[arg(long)]
    pub evidence_kind: Option<String>,
    /// Optional evidence id to prefill in each templated operation.
    #[arg(long)]
    pub evidence_id: Option<String>,
    /// Optional evidence source to prefill in each templated operation.
    #[arg(long)]
    pub evidence_source: Option<String>,
    /// Output format: `markdown` (default) or `json`.
    #[arg(long, default_value = "markdown")]
    pub format: String,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewDigestArgs {
    /// Optional project scope. Matches TaskFrame project for proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum proposals to inspect before rendering the digest.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Client adapter hint: generic, codex-app, cursor, continue, or claude-code.
    #[arg(long)]
    pub client: Option<String>,
    /// Include queue-only proposals in addition to interruptible digest items.
    #[arg(long)]
    pub include_queue_only: bool,
    /// Output format: `json` (default) or `markdown`.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewDigestAckArgs {
    /// Optional project scope. Matches the digest project scope.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches the digest session scope.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum proposals to inspect before recording the current digest signature.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Client adapter identity: generic, codex-app, cursor, continue, or claude-code.
    #[arg(long)]
    pub client: Option<String>,
    /// Optional digest batch key. Defaults to the current interruptible batch key.
    #[arg(long)]
    pub batch_key: Option<String>,
    /// Override the cooldown window in seconds. Defaults to digest policy.
    #[arg(long)]
    pub cooldown_seconds: Option<u64>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewRenderArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to inspect while compiling the render plan.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Client adapter hint: generic, codex-app, cursor, continue, or claude-code.
    #[arg(long)]
    pub client: Option<String>,
    /// Include disabled review actions in the render plan.
    #[arg(long)]
    pub include_disabled: bool,
    /// Output format: `json` (default), `markdown`, or `html`.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Explicitly request JSON output. Equivalent to `--format json`.
    #[arg(long)]
    pub json: bool,
    /// Write the JSON review-render report to this path. Refuses to overwrite.
    #[arg(long)]
    pub write_report: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewDrainArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims and proposals to inspect/drain.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Preview the policy drain without mutating proposals or claim lifecycle.
    #[arg(long)]
    pub dry_run: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextSchedulerRunArgs {
    /// Optional project scope for review/learning passes.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope for review/learning passes.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum claims, proposals, or open-decision signals to inspect per pass.
    #[arg(long, default_value_t = 32)]
    pub limit: usize,
    /// Minimum repeated verified L3 claims required for semantic proposals.
    #[arg(long, default_value_t = 2)]
    pub semantic_min_support: usize,
    /// Promote durable L2 proxy types when confidence is at least this value for pass=l2_promote.
    #[arg(
        long,
        default_value_t = crate::context::scheduler_control::DEFAULT_L2_PROMOTION_MIN_CONFIDENCE
    )]
    pub l2_promotion_min_confidence: f32,
    /// Promote anomaly/conflict L2 proxy types when confidence is at least this value.
    #[arg(
        long,
        default_value_t = crate::context::scheduler_control::DEFAULT_L2_PROMOTION_ANOMALY_MIN_CONFIDENCE
    )]
    pub l2_promotion_anomaly_min_confidence: f32,
    /// Promote repeated active L2 claims after this many scoped support rows.
    #[arg(
        long,
        default_value_t = crate::context::scheduler_control::DEFAULT_L2_PROMOTION_MIN_REPEATED_SUPPORT
    )]
    pub l2_promotion_min_repeated_support: usize,
    /// Lifecycle transition reason for pass=l2_promote.
    #[arg(long, default_value = "scheduler-control L2 proxy promotion")]
    pub l2_promotion_reason: String,
    /// Consider L3 decay candidates older than this many days when pass=l3_decay.
    #[arg(
        long,
        default_value_t = crate::context::scheduler_control::DEFAULT_L3_DECAY_OLDER_THAN_DAYS
    )]
    pub l3_decay_older_than_days: i64,
    /// Explicit L3 decay cutoff timestamp in nanoseconds for reproducible scheduler audits.
    #[arg(long)]
    pub l3_decay_cutoff_ns: Option<i64>,
    /// Decay only L3 proxies with access_count at or below this value.
    #[arg(
        long,
        default_value_t = crate::context::scheduler_control::DEFAULT_L3_DECAY_MAX_ACCESS_COUNT
    )]
    pub l3_decay_max_access_count: i64,
    /// Lifecycle transition reason for pass=l3_decay.
    #[arg(long, default_value = "scheduler-control stale low-access L3 proxy")]
    pub l3_decay_reason: String,
    /// Retain TaskFrames at least this many days when pass=task_frame_retention.
    #[arg(
        long,
        default_value_t = crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS
    )]
    pub task_frame_retention_days: i64,
    /// Explicit TaskFrame retention cutoff timestamp in nanoseconds for reproducible scheduler audits.
    #[arg(long)]
    pub task_frame_retention_cutoff_ns: Option<i64>,
    /// Audit/display reason for pass=task_frame_retention.
    #[arg(long, default_value = "scheduler-control unreferenced TaskFrame retention cleanup")]
    pub task_frame_retention_reason: String,
    /// Preview all selected passes without creating proposals or applying drains.
    #[arg(long)]
    pub dry_run: bool,
    /// Pass to run: all, open_decision_proposals, semantic_proposals, review_drain, l2_promote, l3_decay, task_frame_retention.
    /// Can be repeated or comma-delimited.
    #[arg(long = "pass", value_delimiter = ',')]
    pub passes: Vec<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextSemanticProposalsArgs {
    /// Optional project scope. Matches TaskFrame project for claims/proposals.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claims/proposals.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum long-term claims to inspect.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Minimum grouped long-term claims required before proposing L4 promotion.
    #[arg(long, default_value_t = 2)]
    pub min_support: usize,
    /// Preview semantic promotion proposals without inserting them.
    #[arg(long)]
    pub dry_run: bool,
    /// Output format. Supported: json, brief.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Alias for `--format json`.
    #[arg(long)]
    pub json: bool,
    /// Alias for `--format brief`.
    #[arg(long)]
    pub brief: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextOpenDecisionProposalsArgs {
    /// Optional project scope. Matches source episodes for contradictions/anomalies.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches source episodes for contradictions/anomalies.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum unresolved open decisions to inspect.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Preview review proposals without inserting TaskFrames, claims, or proposals.
    #[arg(long)]
    pub dry_run: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewActionArgs {
    /// Claim record id to review. Mutually exclusive with `--proposal-id`.
    #[arg(long)]
    pub claim_id: Option<i64>,
    /// Learning critic proposal id to review. Mutually exclusive with `--claim-id`.
    #[arg(long)]
    pub proposal_id: Option<i64>,
    /// Action: confirm, contradict, supersede, inconclusive, accept, reject, wait, apply, or confirm_and_apply.
    #[arg(long)]
    pub action: String,
    /// Rendered review control id from a currently enabled action option, such as claim:12:confirm.
    #[arg(long)]
    pub control_id: Option<String>,
    /// Verifier for actions that record claim verification. Defaults to user.
    #[arg(long, default_value = "user")]
    pub verifier: String,
    /// Evidence ref kind for verification actions.
    #[arg(long)]
    pub evidence_kind: Option<String>,
    /// Evidence ref id for verification actions.
    #[arg(long)]
    pub evidence_id: Option<String>,
    /// Optional evidence source note.
    #[arg(long)]
    pub evidence_source: Option<String>,
    /// Optional reviewer note for proposal status actions.
    #[arg(long)]
    pub note: Option<String>,
    /// Required to apply destructive decay/forget proposals.
    #[arg(long)]
    pub confirm_destructive: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextReviewBatchArgs {
    /// JSON array of verification operations. Each item uses claim_id or proposal_id plus action/evidence.
    #[arg(long)]
    pub operations_json: Option<String>,
    /// Path to a JSON file containing the same operations array.
    #[arg(long)]
    pub operations_file: Option<std::path::PathBuf>,
    /// Validate operations without inserting verification events.
    #[arg(long)]
    pub dry_run: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextAuditArgs {
    /// Optional semantic query used to assemble the audited envelope.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Must match stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Include local compiler notes in the audited envelope when available.
    #[arg(long)]
    pub local_compiler: bool,
    /// Override the local compiler endpoint. Defaults to config/env fallback.
    #[arg(long)]
    pub local_compiler_endpoint: Option<String>,
    /// Override the local compiler model. Defaults to config/env fallback.
    #[arg(long)]
    pub local_compiler_model: Option<String>,
    /// Optional persisted TaskFrame id to audit for cloud projection privacy.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextTrustAuditArgs {
    /// Optional project scope. Matches TaskFrame project for claim/proposal rows.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Matches TaskFrame session for claim/proposal rows.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Maximum recent claim/proposal rows to inspect.
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct ContextHardeningReportArgs {
    /// Optional semantic query used to assemble the audited envelope.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Matches episodes, TaskFrames, and claim/proposal rows.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Attach default-off local compiler notes before auditing the envelope.
    #[arg(long)]
    pub local_compiler: bool,
    /// Override the local compiler endpoint. Defaults to config/env fallback.
    #[arg(long)]
    pub local_compiler_endpoint: Option<String>,
    /// Override the local compiler model. Defaults to config/env fallback.
    #[arg(long)]
    pub local_compiler_model: Option<String>,
    /// Optional persisted TaskFrame id to audit for cloud projection privacy.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Optional client binding status filter, such as codex-app, cursor, continue, or claude-code.
    #[arg(long)]
    pub client: Option<String>,
    /// Require specific clients to be ready before private-client release. Repeat or comma-separate.
    #[arg(long = "required-client", value_delimiter = ',')]
    pub required_clients: Vec<String>,
    /// Maximum recent claim/proposal rows to inspect.
    #[arg(long, default_value_t = 1000)]
    pub trust_limit: usize,
    /// Maximum pending review queue rows to inspect.
    #[arg(long, default_value_t = 1000)]
    pub review_limit: usize,
    /// Maximum recent client binding proof rows to inspect.
    #[arg(long, default_value_t = 20)]
    pub client_proof_limit: usize,
    /// Optional config root for read-only client binding proof-session discovery.
    #[arg(long)]
    pub client_binding_config_root: Option<String>,
    /// Treat missing or unready client binding proof as a blocking release failure. Defaults to codex-app, cursor, and continue unless scoped by --client or --required-client.
    #[arg(long)]
    pub require_client_binding_ready: bool,
    /// Treat any pending review queue item as a blocking release failure.
    #[arg(long)]
    pub require_review_queue_clear: bool,
    /// Treat any stale unreferenced TaskFrame retention candidate as a blocking release failure.
    #[arg(long)]
    pub require_task_frame_retention_clean: bool,
    /// Treat missing TaskFrame cloud projection privacy proof as a blocking release failure.
    #[arg(long)]
    pub require_task_frame_projection: bool,
    /// Retain TaskFrames at least this many days before hardening reports retention candidates.
    #[arg(long, default_value_t = crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS)]
    pub task_frame_retention_days: i64,
    /// Skip client binding readiness inspection.
    #[arg(long)]
    pub skip_client_binding: bool,
    /// Explicitly request JSON output. Hardening reports are JSON by default.
    #[arg(long)]
    pub json: bool,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ContextWhyArgs {
    /// Optional semantic query. When absent, the envelope is recent-only.
    #[arg(long)]
    pub query: Option<String>,
    /// Optional project scope. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Optional persisted TaskFrame id to shape scope/query/thread_state before explaining.
    #[arg(long)]
    pub task_frame_id: Option<i64>,
    /// Optional section filter: thread_state, compiler_notes, relevant_memory,
    /// short_term_candidates, project_experience, stable_facts, user_policy,
    /// open_decisions, corrections, claim_records, or learning_critic_proposals.
    #[arg(long)]
    pub section: Option<String>,
    /// Optional case-insensitive text filter applied to section text.
    #[arg(long)]
    pub contains: Option<String>,
    /// Ask the local Ollama helper to add a cited `compiler_notes`
    /// section for cloud LLM audit before explaining matches. This
    /// is not a final-answer surface; if Ollama is unavailable, the
    /// deterministic ContextEnvelope is still explained.
    #[arg(long)]
    pub local_compiler: bool,
    /// Override the local compiler endpoint. Defaults to Ollama's
    /// standard localhost endpoint.
    #[arg(long)]
    pub local_compiler_endpoint: Option<String>,
    /// Override the local compiler model. Defaults to SOMA's local
    /// context compiler preview model.
    #[arg(long)]
    pub local_compiler_model: Option<String>,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[cfg(feature = "cognitive")]
#[derive(Debug, Parser)]
pub struct ContextCompareRankingArgs {
    /// Semantic query used to build both candidate ContextEnvelopes.
    #[arg(long)]
    pub query: Option<String>,
    /// JSON array or JSONL corpus of `{query, expected_episode_ids}` cases.
    /// When set, do not pass `--query` or `--expected-episode`.
    #[arg(long)]
    pub corpus: Option<std::path::PathBuf>,
    /// Optional project scope. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Expected relevant episode id. Repeat to compute recall@k for this query.
    #[arg(long = "expected-episode")]
    pub expected_episodes: Vec<i64>,
    /// Number of semantic hits to compare in `ContextEnvelope.relevant_memory`.
    #[arg(long, default_value_t = crate::context::pack::DEFAULT_SEMANTIC_K)]
    pub semantic_k: usize,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma sync-claudemd` flags. `--project <name>` overrides the
/// cwd-resolved project name; defaults to `basename($PWD)`.
#[derive(Debug, Parser)]
pub struct SyncClaudemdArgs {
    /// Project name to splice. Default: `basename($PWD)`. Use this
    /// flag when the cwd has no project tag (e.g. running from
    /// `~`) but the user wants to write a project-specific
    /// CLAUDE.md from elsewhere.
    #[arg(long)]
    pub project: Option<String>,
}

/// `soma serve --gui` flags (D152 chunk 1.1).
#[cfg(feature = "dashboard")]
#[derive(Debug, Parser)]
pub struct ServeArgs {
    /// Open the dashboard GUI server (axum + view tabs). Currently
    /// the only `serve` mode; future modes (e.g. `--mcp`) may share
    /// the verb with their own flag.
    #[arg(long)]
    pub gui: bool,
    /// TCP port for the dashboard. Default 8765.
    #[arg(long, default_value = "8765")]
    pub port: u16,
    /// Bind address. Default `127.0.0.1` (localhost only). Pass
    /// `0.0.0.0` to expose to the local network — auth is no-op in
    /// v1.x, so only do this if the network is trusted.
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: std::net::IpAddr,
    /// Launch the OS-native browser at the bound URL once the server
    /// is up. macOS uses `open`, Linux `xdg-open`, Windows `start`.
    #[arg(long)]
    pub open: bool,
}

/// `soma persona <sub>` flags.
#[derive(Debug, Parser)]
pub struct PersonaArgs {
    #[command(subcommand)]
    pub mode: PersonaMode,
}

#[derive(Debug, Subcommand)]
pub enum PersonaMode {
    /// List local named SOMA personas and their isolated stores.
    #[command(
        name = "list",
        long_about = "Compatibility alias for `soma list`. Lists local named SOMA personas and their isolated stores."
    )]
    List(PersonaListArgs),
    /// Create a local named SOMA persona with its own private store.
    #[command(
        name = "create",
        long_about = "Compatibility alias for `soma create <name>`. Creates a local named SOMA persona with an isolated soma.db."
    )]
    Create(PersonaCreateArgs),
    /// Activate a local named SOMA persona for the current shell.
    #[command(
        name = "call",
        alias = "activate",
        long_about = "Compatibility alias for `soma call <name>`. Prints shell exports that activate a local named SOMA persona for the current shell; optional --client/--project flags also start session scope."
    )]
    Call(PersonaCallArgs),
    /// Print legacy `identity.md` context/profile diagnostic text to stdout.
    Read,
    /// Force-rebuild legacy context/profile helper artifacts now.
    Regen,
    /// Print the short legacy context/profile helper for hook flows.
    Inject,
}

/// `soma completions <shell>` flags.
#[derive(Debug, Parser)]
pub struct CompletionsArgs {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

/// `soma capture --pty` flags. Only the `--pty` mode is wired; the
/// flag is kept for symmetry with future `--shell-init` mode.
#[cfg(feature = "pty-capture")]
#[derive(Debug, Parser)]
pub struct CaptureArgs {
    /// Wrap the user's `$SHELL` in a pty + OSC 133 boundary capture.
    #[arg(long)]
    pub pty: bool,
    /// Project tag attached to every captured episode.
    #[arg(long)]
    pub project: Option<String>,
    /// Session id stamped on every captured episode. Default =
    /// process-id-based.
    #[arg(long)]
    pub session: Option<String>,
}

/// `soma ingest` — Claude Code stop-hook entry point. Two input
/// modes (mutually exclusive, discussion 0027 §A + §I):
///
/// 1. **Flags**: `--source=<kind>` plus zero or more of
///    `--prompt / --response / --command / --stdout-file / …`.
///    Source-specific validation happens in `run_ingest`.
/// 2. **JSON**: `--source=<kind> --json <path>` where `<path>` is
///    a file holding the 14-field episode JSON (or `-` to read
///    from stdin).
///
/// The two groups must not be mixed; `run_ingest` surfaces a
/// `MalformedInput` error if any flag field is set alongside
/// `--json`.
#[derive(Debug, Parser)]
pub struct IngestArgs {
    /// Source discriminator — `claude-code` / `codex-cli` / `codex-app`
    /// / `terminal` / `cursor` / `continue` / any ad-hoc name. Always required.
    #[arg(long)]
    pub source: String,

    // ---- Flag-mode fields ----
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub response: Option<String>,
    #[arg(long)]
    pub command: Option<String>,
    /// Path to a file whose contents populate `episodes.stdout`
    /// (BLOB). Terminal source typical.
    #[arg(long)]
    pub stdout_file: Option<std::path::PathBuf>,
    #[arg(long)]
    pub exit_code: Option<i32>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long)]
    pub git_branch: Option<String>,
    #[arg(long)]
    pub project: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub digest: Option<String>,

    // ---- JSON-mode field ----
    /// `<path>` or `-` (stdin). Mutually exclusive with flag-mode
    /// payload fields.
    #[arg(long)]
    pub json: Option<String>,

    /// Override the DB path. Precedence: `--db-path` → `$SOMA_DB`
    /// → `~/.soma/soma.db`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma adapter-capture` — stable write entrypoint for editor wrappers.
///
/// Reads one JSON object using the `soma ingest --json` schema, fills missing
/// local metadata from the process cwd, and writes through the normal ingest
/// pipeline. This proves adapter write/read behavior without modifying cloud
/// client prompts.
#[derive(Debug, Parser)]
pub struct AdapterCaptureArgs {
    /// `<path>` or `-` (stdin). Defaults to stdin for hook/wrapper use.
    #[arg(long, default_value = "-")]
    pub json: String,

    /// Source fallback when the JSON object omits `source`.
    #[arg(long)]
    pub source: Option<String>,

    /// CWD fallback when the JSON object omits `cwd`.
    #[arg(long)]
    pub cwd: Option<String>,

    /// Project fallback when the JSON object omits `project`.
    #[arg(long)]
    pub project: Option<String>,

    /// Session default when the JSON object omits `session_id`.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Git branch fallback when the JSON object omits `git_branch`.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Override the DB path. Precedence: `--db-path` → `$SOMA_DB`
    /// → `~/.soma/soma.db`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma adapter-cloud-output` — stable cloud-output capture entrypoint for
/// editor hooks and watcher wrappers.
///
/// Reads one JSON object with `output_text` plus either `task_frame_id` or
/// `task_frame_query`. When only `task_frame_query` is supplied, SOMA builds and
/// persists a local TaskFrame first. Optional critic/proposal fields match
/// `soma_capture_cloud_output`, and the response is stored as `cloud_draft`
/// claims.
#[derive(Debug, Parser)]
pub struct AdapterCloudOutputArgs {
    /// `<path>` or `-` (stdin). Defaults to stdin for hook/wrapper use.
    #[arg(long, default_value = "-")]
    pub json: String,

    /// Override the DB path. Precedence: `--db-path` → `$SOMA_DB`
    /// → `~/.soma/soma.db`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma adapter-lifecycle` — normalize app-specific hook events to the shared
/// adapter spool event contract.
#[derive(Debug, Parser)]
pub struct AdapterLifecycleArgs {
    /// Raw lifecycle JSON object path, or `-` for stdin.
    #[arg(long, default_value = "-")]
    pub json: String,

    /// Client/source fallback, such as codex-app, cursor, continue, or claude-code.
    #[arg(long)]
    pub client: Option<String>,

    /// Lifecycle event override: turn_completed, assistant_response, or auto.
    #[arg(long)]
    pub event: Option<String>,

    /// Private app-hook event source marker copied into the normalized payload.
    #[arg(long)]
    pub event_source: Option<String>,

    /// Per-install binding nonce copied into the normalized payload.
    #[arg(long)]
    pub binding_nonce: Option<String>,

    /// Optional hook adapter marker copied into the normalized payload.
    /// Values containing `manual_debug` or `non_release` are ignored for release proof.
    #[arg(long)]
    pub hook_adapter: Option<String>,

    /// Optional JSONL spool path to append the normalized event to.
    #[arg(long)]
    pub jsonl: Option<String>,

    /// Source fallback for normalized turn events whose payload omits source.
    #[arg(long)]
    pub source: Option<String>,

    /// CWD default for normalized events whose payload omits cwd.
    #[arg(long)]
    pub cwd: Option<String>,

    /// Project default for normalized events whose payload omits project.
    #[arg(long)]
    pub project: Option<String>,

    /// Session default for normalized events whose payload omits session_id.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Git branch default for normalized turn events whose payload omits git_branch.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Fsync the spool file before returning when `--jsonl` is provided.
    #[arg(long)]
    pub fsync: bool,

    /// Output format: `event` emits the normalized {kind,payload};
    /// `report` emits normalization and append metadata.
    #[arg(long, default_value = "event")]
    pub format: String,
}

/// `soma adapter-spool` — checkpointed JSONL bridge for watcher wrappers.
///
/// Each new line after the checkpoint must be:
/// `{ "kind": "turn", "payload": { ...adapter-capture payload... } }` or
/// `{ "kind": "cloud_output", "payload": { ...adapter-cloud-output payload... } }`.
#[derive(Debug, Parser)]
pub struct AdapterSpoolArgs {
    /// JSONL spool path to drain.
    #[arg(long)]
    pub jsonl: String,

    /// Checkpoint path storing the last consumed byte offset.
    #[arg(long)]
    pub checkpoint: Option<String>,

    /// Source fallback for `kind=turn` events whose payload omits `source`.
    #[arg(long)]
    pub source: Option<String>,

    /// CWD fallback for `kind=turn` events whose payload omits `cwd`.
    #[arg(long)]
    pub cwd: Option<String>,

    /// Project fallback for `kind=turn` events whose payload omits `project`.
    #[arg(long)]
    pub project: Option<String>,

    /// Session fallback for `kind=turn` events whose payload omits `session_id`.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Git branch fallback for `kind=turn` events whose payload omits `git_branch`.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Override the DB path. Precedence: `--db-path` → `$SOMA_DB`
    /// → `~/.soma/soma.db`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma adapter-spool-append` — safe JSONL writer for editor wrappers.
///
/// The payload is not ingested directly. It is appended as one compact JSONL
/// event and later drained by `soma adapter-spool`.
#[derive(Debug, Parser)]
pub struct AdapterSpoolAppendArgs {
    /// JSONL spool path to append to.
    #[arg(long)]
    pub jsonl: String,

    /// Event kind: turn or cloud_output.
    #[arg(long)]
    pub kind: String,

    /// Payload JSON object path, or `-` for stdin.
    #[arg(long, default_value = "-")]
    pub json: String,

    /// Source default for turn events whose payload omits `source`.
    #[arg(long)]
    pub source: Option<String>,

    /// CWD default for events whose payload omits `cwd`.
    #[arg(long)]
    pub cwd: Option<String>,

    /// Project default for events whose payload omits `project`.
    #[arg(long)]
    pub project: Option<String>,

    /// Session default for events whose payload omits `session_id`.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Git branch default for turn events whose payload omits `git_branch`.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Client default for cloud_output events whose payload omits `client`.
    #[arg(long)]
    pub client: Option<String>,

    /// Per-install binding nonce for proof binding.
    #[arg(long)]
    pub binding_nonce: Option<String>,

    /// Fsync the spool file before returning.
    #[arg(long)]
    pub fsync: bool,
}

/// `soma adapter-binding-proof` — persist a client binding proof row.
#[derive(Debug, Clone, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct AdapterBindingProofArgs {
    /// Checked-in or installed client binding manifest path.
    #[arg(long)]
    pub manifest: Option<String>,

    /// Client override. Defaults to the manifest client.
    #[arg(long)]
    pub client: Option<String>,

    /// List recent stored binding proof rows instead of recording a new proof.
    #[arg(long)]
    pub list: bool,

    /// Summarize client binding readiness from stored proof rows without storing anything.
    #[arg(long)]
    pub status: bool,

    /// Check whether an installed client config is eligible for observed_app_hook proof without storing anything.
    #[arg(long)]
    pub check_installed_config: bool,

    /// Discover likely installed client config files and preflight each one without storing proof.
    #[arg(long)]
    pub discover_installed_config: bool,

    /// Render a read-only operator proof kit for real app-hook/render evidence without storing proof.
    #[arg(long)]
    pub real_app_proof_kit: bool,

    /// Render one read-only operator evidence bundle: readiness, discovery, config preview, and proof kit.
    #[arg(long)]
    pub evidence_bundle: bool,

    /// Render a compact read-only proof-session card with release gate and next operator step.
    #[arg(long)]
    pub proof_session: bool,

    /// Explicit JSON output flag for status/proof-session/readiness automation. Adapter binding proof commands already emit JSON; this flag is accepted for CLI consistency.
    #[arg(long)]
    pub json: bool,

    /// Render proof-session as a compact human handoff card instead of JSON.
    #[arg(long)]
    pub brief: bool,

    /// Output format selector. Adapter binding proof commands emit JSON by default;
    /// proof-session also accepts `brief` for operator handoff.
    #[arg(long, default_value = "json", value_parser = ["json", "brief"])]
    pub format: String,

    /// Generate a per-install binding nonce and installed-config environment snippet without storing proof.
    #[arg(long)]
    pub prepare_installed_config: bool,

    /// Render a complete installed client hook config artifact without storing proof.
    #[arg(long)]
    pub render_installed_config: bool,

    /// Write the rendered installed client hook config to this path. Refuses to overwrite.
    #[arg(long)]
    pub write_installed_config: Option<String>,

    /// Render a structured in-client render evidence packet template from a review-render report without storing proof.
    #[arg(long)]
    pub render_render_evidence: bool,

    /// Write the rendered in-client render evidence packet to this path. Refuses to overwrite.
    #[arg(long)]
    pub write_render_evidence: Option<String>,

    /// Re-read stored proof artifact paths and compare byte length/fingerprint without mutating trust state.
    #[arg(long)]
    pub verify_evidence_artifacts: bool,

    /// Optional proof row id for --verify-evidence-artifacts.
    #[arg(long)]
    pub proof_id: Option<i64>,

    /// Maximum rows to list when --list is used.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Proof level: reference_binding, observed_event_file, observed_app_hook, observed_in_client_render, or observed_review_action.
    #[arg(long, default_value = "observed_event_file")]
    pub proof_level: String,

    /// Human-readable evidence source, e.g. binding_smoke or cursor_hook_log.
    #[arg(long, default_value = "binding_smoke")]
    pub evidence_source: String,

    /// Optional per-install binding nonce for --prepare-installed-config. Generated when omitted.
    #[arg(long)]
    pub binding_nonce: Option<String>,

    /// Optional root directory for --discover-installed-config. Defaults to the user's home directory.
    #[arg(long)]
    pub config_root: Option<String>,

    /// Optional durable evidence artifact directory for --evidence-bundle/--proof-session.
    /// Defaults to <config-root>/.soma/client-evidence/<client>/<binding-nonce>.
    #[arg(long)]
    pub artifact_dir: Option<String>,

    /// Optional observed JSONL spool/event file.
    #[arg(long)]
    pub event_jsonl: Option<String>,

    /// Optional installed client config or hook file. Required for observed_app_hook proof.
    #[arg(long)]
    pub installed_config: Option<String>,

    /// Internal proof-session guard: auto-discovered setup artifacts cannot make app-hook proof ready.
    #[arg(skip)]
    pub require_private_target_config_for_app_hook: bool,

    /// Optional external proof artifact path for observed_in_client_render, such as a screenshot or app log.
    #[arg(long)]
    pub render_evidence: Option<String>,

    /// Optional JSON report from `soma context review-action` or MCP `soma_review_action`.
    #[arg(long)]
    pub review_action_report: Option<String>,

    /// Optional JSON report from `soma adapter-spool`.
    #[arg(long)]
    pub drain_report: Option<String>,

    /// Optional JSON report from `soma context review-render` or wrapper.
    #[arg(long)]
    pub review_render_report: Option<String>,

    /// Required when --proof-level observed_app_hook is used.
    #[arg(long)]
    pub operator_confirm_real_app_invocation: bool,

    /// Required when --proof-level observed_in_client_render is used.
    #[arg(long)]
    pub operator_confirm_in_client_render: bool,

    /// Required when --proof-level observed_review_action is used.
    #[arg(long)]
    pub operator_confirm_review_action: bool,

    /// Required for observed_app_hook / observed_in_client_render / observed_review_action rows to count toward ready_for_private_client_claim.
    #[arg(long)]
    pub operator_confirm_release_grade_evidence: bool,

    /// Override the DB path. Precedence: `--db-path` → `$SOMA_DB`
    /// → `~/.soma/soma.db`.
    #[arg(long)]
    pub db_path: Option<String>,
}

impl AdapterBindingProofArgs {
    pub fn wants_brief_output(&self) -> bool {
        self.brief || self.format.trim().eq_ignore_ascii_case("brief")
    }
}

/// Hidden `soma profile` diagnostic — render the context profile snapshot.
#[derive(Debug, Parser)]
pub struct ProfileArgs {
    /// Re-run all extractors before reading.
    #[arg(long)]
    pub recompute: bool,
    /// Output format: `markdown` (default) or `json`.
    #[arg(long, default_value = "markdown")]
    pub format: String,
    /// Override the DB path. Same precedence as ingest.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma projects` - active-persona project experience provenance.
#[derive(Debug, Parser)]
pub struct ProjectExperienceArgs {
    /// Optional project filter. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Maximum recent evidence episode IDs to show per project.
    #[arg(long, default_value = "5")]
    pub evidence_limit: usize,
    /// Output format: `markdown` (default), `brief`, or `json`.
    #[arg(long, default_value = "markdown")]
    pub format: String,
    /// Render a compact terminal/persona/project scope handoff instead of the full markdown report.
    #[arg(long)]
    pub brief: bool,
    /// Render only the current terminal persona/project/session scope contract.
    #[arg(long)]
    pub current_terminal: bool,
    /// Exit non-zero unless the current process has persona/session/client/project scope exports.
    /// This is a read-only gate for client wrappers and dogfood scripts.
    #[arg(long)]
    pub require_current_terminal_scope: bool,
    /// Output a machine-readable JSON project provenance report.
    #[arg(long)]
    pub json: bool,
    /// Override the DB path. Same precedence as ingest.
    #[arg(long)]
    pub db_path: Option<String>,
    /// Optional soma.client_dogfood_report.v1 JSON artifact from tools/client-dogfood-report.sh.
    /// Defaults to $SOMA_CLIENT_DOGFOOD_REPORT or ~/.soma/reports/client-dogfood-latest.json
    /// when present, so project scope handoffs can cite last-run dogfood evidence without
    /// claiming live storage, current terminal scope, or clean project/session isolation.
    #[arg(long)]
    pub dogfood_report: Option<String>,
}

/// `soma recall` — top-k semantic recall over `episode_vectors`.
#[derive(Debug, Parser)]
pub struct RecallArgs {
    /// Query text to embed + search for. Required — `recall`
    /// without a query is undefined.
    #[arg(long)]
    pub query: String,
    /// Maximum number of results.
    #[arg(long, default_value = "5")]
    pub limit: usize,
    /// Optional graph expansion hop budget. `0` (default) keeps
    /// single-hop semantic search; `1`/`2` also include linked local
    /// episodes. Higher values broaden context at more noise/cost;
    /// `2` is usually the practical maximum.
    #[arg(long, default_value = "0")]
    pub multi_hop: usize,
    /// Optional project scope. Must match the stored `episodes.project`.
    #[arg(long)]
    pub project: Option<String>,
    /// Optional captured session scope. Must match `episodes.session_id`.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Output format: `markdown` (default) or `json`.
    #[arg(long, default_value = "markdown")]
    pub format: String,
    /// Override the DB path. Same precedence as ingest's `--db-path`.
    #[arg(long)]
    pub db_path: Option<String>,
}

/// `soma install` / `soma uninstall` flags. v1.1 adds shell-init —
/// inject `soma ingest --source terminal` hooks into the user's
/// shell rc files.
#[derive(Debug, Parser, Default, Clone)]
pub struct InstallArgs {
    /// Inject (or remove) the shell-init hook block in
    /// `~/.bashrc` / `~/.zshrc` / `~/.config/fish/config.fish`.
    /// Without this flag, `install` only writes the LaunchAgent
    /// plist; with it, terminal capture activates after the user
    /// re-sources the rc file.
    #[arg(long)]
    pub shell_init: bool,
    /// Skip the LaunchAgent plist (only touch shell rc files).
    /// Useful when running on a non-resident workstation.
    #[arg(long)]
    pub no_launch_agent: bool,
    /// Download an ONNX embedding model used for ContextEnvelope
    /// evidence ranking into `~/.soma/models/<id>/`. Supported ids:
    /// `paraphrase-multilingual-MiniLM-L12-v2` / `minilm-l12-v2-384d`
    /// (Mini) and `multilingual-e5-large` /
    /// `multilingual-e5-large-1024d` (Studio). Requires the
    /// `embed-onnx` cargo feature; without it this flag exits with a
    /// typed error pointing at the rebuild instruction.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
}

#[derive(Debug, Parser)]
pub struct InspectArgs {
    /// What kind of diagnostic to inspect: `episode` / `vector` /
    /// `pin` / `edges` / `weights`. Legacy context/profile diagnostics:
    /// `narrative` / `centroid`.
    #[arg(value_name = "KIND")]
    pub kind: String,
    /// Episode id (required for `episode` / `vector` / `pin` /
    /// `edges`; ignored for legacy diagnostics and `weights`).
    #[arg(long)]
    pub id: Option<i64>,
    /// Surface forgotten episodes too. Without this, the inspect
    /// path mirrors the recall filter.
    #[arg(long)]
    pub include_forgotten: bool,
    /// Output format: `json` (default) or `markdown`.
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Override the DB path. Same precedence as ingest.
    #[arg(long)]
    pub db_path: Option<String>,
}

#[derive(Debug, Parser)]
pub struct ForgetArgs {
    /// Soft-delete one episode by id.
    #[arg(long)]
    pub episode: Option<i64>,
    /// Soft-delete every episode tagged with this project name.
    #[arg(long)]
    pub project: Option<String>,
    /// Soft-delete every episode whose ts_start_ns is before this
    /// value. Accepts either a unix-epoch ns integer or
    /// `YYYY-MM-DDThh:mm:ssZ`.
    #[arg(long)]
    pub before: Option<String>,
    /// Audit reason recorded in `note_pins.reason`. Default
    /// `user-request`.
    #[arg(long)]
    pub reason: Option<String>,
    /// Override the DB path.
    #[arg(long)]
    pub db_path: Option<String>,
}
