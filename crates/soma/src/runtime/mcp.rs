//! MCP resource server — stdio JSON-RPC 2.0 subset (discussion
//! 0029 §A + §F).
//!
//! Entry point: `run_stdio(reader, writer, db_path)` drives the
//!   request/response loop. The CLI dispatcher calls
//!   [`run_stdio_default`] which wires it to the real `stdin`/
//!   `stdout` + `resolve_db_path()`. Tests call `run_stdio`
//!   directly with in-memory pipes so the JSON-RPC surface is
//!   testable without a child process.
//!
//! Supported methods (discussion 0029 §F):
//!
//! * `initialize` — capabilities + serverInfo handshake.
//! * `resources/list` — advertised ContextEnvelope URIs.
//! * `resources/read` — ContextEnvelope payload, with developer/debug
//!   MemoryPack direct-read URIs for raw retrieval inspection.
//! * `tools/list` — active cloud-LLM tools (`soma_recall`,
//!   `soma_capture_turn`, `soma_capture_cloud_output`,
//!   `soma_verify_claim`, `soma_learning_proposals_*`,
//!   `soma_review_queue`, `soma_review_actions`, `soma_review_batch_template`,
//!   `soma_review_report`, `soma_review_digest`, `soma_review_digest_ack`,
//!   `soma_review_render`, `soma_client_binding_proofs`,
//!   `soma_client_binding_record_proof`,
//!   `soma_client_binding_install_plan`, `soma_client_binding_evidence_bundle`,
//!   `soma_client_binding_proof_session`,
//!   `soma_client_render_evidence_packet`,
//!   `soma_review_drain`, `soma_latent_predict`, `soma_latent_packet`,
//!   `soma_semantic_proposals`,
//!   `soma_review_action`, `soma_review_batch`, `soma_scheduler_run`, `soma_trust_boundary_audit`,
//!   `soma_record_correction`, `soma_context_why`, `soma_context_audit`).
//! * `tools/call` — invoke recall, turn capture, correction capture,
//!   cloud-output claim capture, claim/proposal review, or envelope
//!   why/audit inspection.
//! * `notifications/initialized` — MCP spec handshake ack; ignored.
//!
//! Unknown methods → JSON-RPC error `-32601` (Method not found).
//! Malformed JSON → JSON-RPC error `-32700` (Parse error).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::capture::ai_cli::{
    run_adapter_capture_json, AdapterCaptureDefaults, IngestContext, IngestOutcome,
};
use crate::cli::adapter_binding_proof::{
    build_client_binding_status_report, proof_session_render_evidence_artifact_path,
    run_blocking as run_client_binding_proof_blocking, run_discover_installed_config_blocking,
    run_evidence_bundle_blocking, run_installed_config_check_blocking,
    run_prepare_installed_config_blocking, run_proof_session_blocking,
    run_render_evidence_packet_blocking, run_render_installed_config_blocking,
    AdapterBindingProofContext,
};
use crate::cli::AdapterBindingProofArgs;
use crate::context::cloud_prompt::render_cloud_context_artifact;
use crate::context::compiler::{
    load_local_compiler_config_from_home, resolve_local_compiler_runtime,
    try_attach_local_compiler_note_from_env,
};
use crate::context::correction::{record_correction_with_report, CorrectionInput};
use crate::context::critic::{
    capture_cloud_output_claims, learning_critic_proposal_from_capture, select_cloud_output_claims,
    CloudOutputCaptureInput, ControlCriticDecision, ControlCriticResult, ExtractedCloudClaim,
    LocalClaimExtractorRuntime, VerificationRequest,
};
use crate::context::envelope::{
    append_relevant_memory_items, apply_correction_overrides, attach_corrections,
    attach_open_decisions, attach_short_term_candidates, attach_stable_facts, attach_thread_state,
    attach_user_policy, build_context_envelope, render_json as render_context_json,
    render_xml as render_context_xml, ContextEnvelope, ContextScope,
};
use crate::context::eval::{
    attach_required_client_proof_session_probe,
    attach_required_client_render_evidence_artifact_scan, audit_context_envelope,
    audit_latent_interface_packet, audit_review_backlog, audit_review_control_binding_manifest,
    audit_review_interaction_contract, audit_storage_trust_boundary, audit_task_frame_projection,
    audit_task_frame_retention_hygiene, build_product_hardening_report,
    build_product_hardening_scope_resolution, build_required_client_proof_matrix,
    client_binding_config_root_probe_hint, effective_required_client_names,
    normalize_required_client_names, refresh_required_client_proof_matrix_operator_action,
    ClientBindingHardeningAudit, ClientBindingHardeningClientSnapshot,
    ProductHardeningEvidenceArtifactFailure, ProductHardeningRequirements,
    ProductHardeningScopeResolutionInput,
};
use crate::context::latent_eval::{
    build_storage_latent_eval_cases, build_task_frame_outcome_latent_eval_cases,
    evaluate_latent_predictor, LatentProxyEvalCase, LatentProxyEvalInput,
    DEFAULT_LATENT_EVAL_CASE_LIMIT,
};
use crate::context::latent_predictor::{
    predict_latent_proxies, render_latent_interface_packet, LatentInterfacePacketInput,
    LatentProxyPredictionInput, DEFAULT_LATENT_PREDICTOR_LIMIT,
    DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE, DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
};
use crate::context::open_decision_review::{
    propose_open_decision_reviews, OpenDecisionProposalInput,
};
use crate::context::pack::{
    build_memory_pack, render_json as render_pack_json, render_markdown, PackConfig,
};
use crate::context::quality::{
    correction_signals_from_storage_scoped, correction_signals_from_storage_session_set,
    open_decisions_from_storage_scoped_with_corrections,
    open_decisions_from_storage_session_set_with_corrections, relevant_memory_proxies_from_storage,
    relevant_memory_proxies_from_storage_session_set, short_term_candidates_from_storage,
    short_term_candidates_from_storage_session_set, stable_facts_from_storage,
    stable_facts_from_storage_session_set, user_policy_from_storage_with_corrections,
    user_policy_from_storage_with_corrections_session_set, DEFAULT_CORRECTION_LIMIT,
    DEFAULT_OPEN_DECISION_LIMIT, DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT,
    DEFAULT_SHORT_TERM_CANDIDATE_LIMIT, DEFAULT_STABLE_FACT_LIMIT,
};
use crate::context::review::{
    acknowledge_review_digest, build_review_action_plan, build_review_batch_template,
    build_review_digest, build_review_queue, build_review_render_plan, build_review_report,
    render_review_render_plan_html, resolve_verification_targets, ReviewActionPlanInput,
    ReviewBatchTemplateInput, ReviewDigestAckInput, ReviewDigestInput, ReviewQueueInput,
    ReviewRenderInput, ReviewReportInput, VerificationTargetInput,
};
use crate::context::review_action::{
    apply_review_action, apply_review_batch, ReviewAction, ReviewActionInput, ReviewBatchInput,
    ReviewTarget,
};
use crate::context::review_apply::{apply_ready_learning_proposals, ApplyReadyInput};
use crate::context::review_drain::{drain_review_queue, ReviewDrainInput};
use crate::context::scheduler_control::{
    normalize_scheduler_control_passes, run_scheduler_control, SchedulerControlInput,
    DEFAULT_L2_PROMOTION_ANOMALY_MIN_CONFIDENCE, DEFAULT_L2_PROMOTION_MIN_CONFIDENCE,
    DEFAULT_L2_PROMOTION_MIN_REPEATED_SUPPORT, DEFAULT_L2_PROMOTION_REASON,
    DEFAULT_L3_DECAY_MAX_ACCESS_COUNT, DEFAULT_L3_DECAY_OLDER_THAN_DAYS, DEFAULT_L3_DECAY_REASON,
    DEFAULT_TASK_FRAME_RETENTION_REASON,
};
use crate::context::scope::inferred_project_scope_from_anil;
use crate::context::semantic_learning::{propose_semantic_consolidations, SemanticLearningInput};
use crate::context::task_frame::task_frame_thread_state_section;
use crate::runtime::mcp_cache::{CacheKey, CacheStatus, MemoryPackCache};
use crate::storage::{
    task_frame_retention_cutoff_ns, ClientBindingProofLevel, LearningCriticAction,
    LearningCriticApplyOptions, LearningCriticProposalStatus, LifecycleState, Storage,
    StoredEvidenceRef, StoredTaskFrame, StoredThreadIdentity, TaskFrameOutcomeDraft,
    TaskFrameOutcomeType, VerificationEventDraft, VerificationResult, VerifierType,
    THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED,
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "soma";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const URI_CURRENT: &str = "soma://memory-pack/current";
const URI_BY_QUERY_PREFIX: &str = "soma://memory-pack/by-query";
const URI_CONTEXT_CURRENT: &str = "soma://context/current";
const URI_CONTEXT_BY_QUERY_PREFIX: &str = "soma://context/by-query";
const URI_CONTEXT_PROJECT_PREFIX: &str = "soma://context/project/";
const URI_CONTEXT_SESSION_PREFIX: &str = "soma://context/session/";
const URI_CONTEXT_THREAD_PREFIX: &str = "soma://context/thread/";
/// D161 — project-narrow developer/debug MemoryPack URI prefix.
/// Path tail is the raw project name (cwd basename matching
/// `episodes.project`). `?q=<text>` query string optional —
/// without it, the project pack is recent-only; with it, semantic
/// + recent both narrow to that project.
const URI_PROJECT_PREFIX: &str = "soma://memory-pack/project/";

/// Drive the MCP loop over arbitrary reader/writer — the real
/// dispatcher uses `stdin().lock()` + `stdout().lock()`; tests use
/// `Cursor<Vec<u8>>`. Runs until the reader returns EOF (Claude
/// Code closes stdio on session end).
pub fn run_stdio<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    db_path: &Path,
) -> std::io::Result<()> {
    let cache = MemoryPackCache::with_default_ttl();
    run_stdio_with_cache(&mut reader, &mut writer, db_path, &cache)
}

/// Variant that takes an externally-owned cache. Tests use this
/// to pre-construct a cache with custom TTL or to inspect
/// `builder_calls` after the loop.
pub fn run_stdio_with_cache<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF — client closed.
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => handle_request(req, db_path, cache),
            Err(e) => Some(error_response(None, -32700, &format!("parse error: {e}"))),
        };
        if let Some(resp) = response {
            writeln!(writer, "{resp}")?;
            writer.flush()?;
        }
    }
}

/// Production entry — wires `stdin()` / `stdout()` to `run_stdio`.
pub fn run_stdio_default(db_path: PathBuf) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = stdin.lock();
    let writer = stdout.lock();
    run_stdio(reader, writer, &db_path)
}

/// Return true when the resident rooted at `soma_root` would read the same DB
/// as the current MCP child. The resident is still rooted at `~/.soma`, while
/// `soma call <persona>` switches MCP children through `$SOMA_DB`; if those
/// paths differ, forwarding would cross persona boundaries.
#[cfg(unix)]
pub fn resident_default_db_matches(soma_root: &Path, child_db_path: &Path) -> bool {
    paths_same(&soma_root.join("soma.db"), child_db_path)
}

#[cfg(unix)]
fn paths_same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// D91 §B — preflight probe. Tries `connect + Hello` against the
/// resident socket *before* the caller (`Cmd::McpServe`) reads
/// stdin. On success returns `true` (caller switches to the
/// forwarding loop); on failure returns `false` (caller falls back
/// to standalone). Codex 2차 review #3 Q2 — pre-fix the forwarder
/// consumed the first stdin line *before* discovering the resident
/// was dead / version mismatched, leaving the user's first MCP
/// connection broken with no fallback path. Stale socket files
/// (e.g. from a crashed previous resident that never cleaned up)
/// are now caught here and the child silently degrades to
/// child-local cache.
///
/// `#[cfg(unix)]`-only — `runtime::resident` (the type carrier of
/// the protocol) is itself unix-gated since the resident plane is
/// POSIX-socket-based. Windows callers always take the standalone
/// path; D56-cand tracks Windows named-pipe parity.
#[cfg(unix)]
pub fn resident_preflight(socket_path: &Path) -> bool {
    use crate::runtime::resident::{ControlResponse, ResidentClient, PROTOCOL_VERSION};
    use std::time::Duration;
    if !socket_path.exists() {
        return false;
    }
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(_) => return false,
    };
    // D158 close — explicit timeout on the preflight handshake.
    // Pre-fix a stale socket file (resident crashed without
    // unlinking it, or LaunchAgent hand-off mid-restart) caused
    // connect/hello to hang since `ResidentClient` carried no
    // deadline. The MCP child stayed wedged before the user's
    // first request ever surfaced. 2s default is generous for a
    // localhost round-trip but tight enough that an unresponsive
    // resident falls back to standalone within one normal user
    // blink. D156-E — knob lifted to `[mcp] preflight_timeout_secs`.
    let timeout_secs = match dirs::home_dir() {
        Some(home) => {
            crate::config::Config::load_or_default(&home.join(".soma")).mcp.preflight_timeout_secs
        }
        None => 2,
    };
    rt.block_on(async {
        let probe = async {
            let client = ResidentClient::connect(socket_path).await.ok()?;
            match client.hello(PROTOCOL_VERSION).await {
                Ok(ControlResponse::HelloOk { .. }) => Some(true),
                _ => Some(false),
            }
        };
        match tokio::time::timeout(Duration::from_secs(timeout_secs), probe).await {
            Ok(Some(true)) => true,
            Ok(Some(false)) | Ok(None) => false,
            Err(_) => {
                tracing::warn!(
                    socket = %socket_path.display(),
                    "resident preflight timed out — falling back to standalone MCP"
                );
                false
            }
        }
    })
}

/// D91 §B — forwarding loop. Each JSON-RPC request from the MCP
/// client (Claude Code / Cursor) is parsed, then forwarded to the
/// resident over a fresh `ResidentClient` connection (Hello +
/// `McpFetch` + close, single-shot per request). The resident's
/// shared `MemoryPackCache` records the hit/miss; the response is
/// rewrapped in a JSON-RPC envelope and written to stdout.
///
/// `socket_path` points at `~/.soma/run/soma.sock`. The caller
/// must run [`resident_preflight`] first — Codex 2차 review #3 Q2
/// flagged that mid-stdin connect failure leaves the child's
/// stdin partially consumed with no fallback. Once preflight has
/// confirmed the resident is up, mid-session resident death is
/// surfaced as a JSON-RPC `-32603` (internal error) per request
/// and the loop continues.
///
/// Cost per fetch (Codex 2차 review #3 Q1): UDS connect + 2 NDJSON
/// round-trips (Hello + McpFetch). Sub-millisecond on localhost,
/// dominated by the MemoryPack build cost on the resident side
/// (cache miss only).
///
/// Notifications (no `id`) are dropped without forwarding — they
/// can't change cache state and the resident wouldn't have anything
/// useful to do with them.
///
/// `#[cfg(unix)]`-only (see [`resident_preflight`]).
#[cfg(unix)]
pub fn run_stdio_via_resident<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    socket_path: &Path,
) -> std::io::Result<()> {
    use crate::runtime::resident::{
        ControlRequest, ControlResponse, ResidentClient, PROTOCOL_VERSION,
    };

    // Single tokio current-thread runtime drives all forwards. Each
    // line opens a fresh connection — UDS connect is µs on localhost.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                writeln!(writer, "{}", error_response(None, -32700, &format!("parse error: {e}")))?;
                writer.flush()?;
                continue;
            }
        };
        let id = match req.get("id").cloned() {
            None => continue, // notification — drop
            Some(v) => v,
        };
        // P1 fix (in-house ultrareview): JSON-RPC 2.0 spec — id must
        // be number, string, or null. Standalone path validates this
        // already; mirror here for the resident-forwarded path so a
        // spec-violating client (id = array/object) gets the correct
        // -32600 Invalid Request response instead of a silent echo.
        match &id {
            serde_json::Value::Null
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
            _ => {
                writeln!(
                    writer,
                    "{}",
                    error_response_v(
                        Some(serde_json::Value::Null),
                        -32600,
                        "id must be a number, string, or null",
                    )
                )?;
                writer.flush()?;
                continue;
            }
        }
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = req.get("params").cloned();

        // D130 — span the forwarded dispatch with a fresh local
        // `request_id` (UUID v4). When the upstream JSON-RPC request
        // carries a `params._meta.request_id` (MCP spec allows the
        // `_meta` envelope), record it as `forwarded_request_id` so
        // an operator can correlate cross-process flow. Held as named
        // `_span` so it stays active through the resident round-trip
        // + response serialization.
        let request_id = uuid::Uuid::new_v4();
        let forwarded_request_id = params
            .as_ref()
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get("request_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let _span = tracing::info_span!(
            "mcp_fetch_forward",
            request_id = %request_id,
            forwarded_request_id = forwarded_request_id.as_deref().unwrap_or(""),
            method = %method,
        )
        .entered();

        let resp_line = rt.block_on(async {
            let client = match ResidentClient::connect(socket_path).await {
                Ok(c) => c,
                Err(e) => return ForwardResult::ConnectFailed(e.to_string()),
            };
            match client.hello(PROTOCOL_VERSION).await {
                Ok(ControlResponse::HelloOk { .. }) => {}
                Ok(other) => return ForwardResult::HelloRejected(format!("{other:?}")),
                Err(e) => return ForwardResult::ConnectFailed(e.to_string()),
            }
            match client.request(&ControlRequest::McpFetch { method: method.clone(), params }).await
            {
                Ok(ControlResponse::McpFetchOk { result }) => ForwardResult::Ok(result),
                Ok(ControlResponse::Error { message, .. }) => ForwardResult::DispatchError(message),
                Ok(other) => ForwardResult::DispatchError(format!("unexpected: {other:?}")),
                Err(e) => ForwardResult::ConnectFailed(e.to_string()),
            }
        });

        let response = match resp_line {
            ForwardResult::Ok(v) => ok_response(Some(id), v),
            ForwardResult::DispatchError(msg) => {
                // Map structured McpDispatch errors back to the
                // JSON-RPC numeric code the client expects. Method-
                // not-found (-32601) vs invalid-params (-32602) is
                // disambiguated by the message prefix `dispatch`
                // sets up — keep it simple: -32602 is the safe
                // default since the `dispatch` fn is the only
                // method router and `-32601` would be a lie if
                // the cache itself errored.
                error_response_v(Some(id), -32602, &msg)
            }
            ForwardResult::HelloRejected(msg) | ForwardResult::ConnectFailed(msg) => {
                // Codex 2차 review #3 Q2 — mid-session resident
                // failure (preflight succeeded, but a later fetch
                // can't reach the resident: it died, was restarted
                // with a different socket, etc.). Surface as JSON-
                // RPC `-32603` (Internal error) per spec; do NOT
                // bail the whole loop, the next request will retry
                // the connection. The MCP client can decide whether
                // to keep retrying or fall back gracefully.
                error_response_v(Some(id), -32603, &format!("resident unreachable: {msg}"))
            }
        };
        writeln!(writer, "{response}")?;
        writer.flush()?;
    }
}

#[cfg(unix)]
enum ForwardResult {
    Ok(Value),
    DispatchError(String),
    HelloRejected(String),
    ConnectFailed(String),
}

/// Handle one JSON-RPC request. Returns `None` for notifications
/// (no `id` field) — they don't elicit a response.
///
/// D130 — every dispatch is wrapped in an `mcp_fetch` info span with a
/// fresh UUID v4 `request_id`. Operators reading `RUST_LOG=info`
/// traces can then tie a specific MemoryPack build (logged inside the
/// `dispatch` → `resources_read` → `build_memory_pack` chain) to the
/// originating JSON-RPC call. The span is held in `_span` (named
/// binding, not `_`) so it stays active for the full dispatch +
/// serialization scope.
fn handle_request(req: Value, db_path: &Path, cache: &MemoryPackCache) -> Option<String> {
    // Notifications (no `id` field) elicit no response per JSON-RPC
    // 2.0. `?` propagates the None early-return.
    let id = req.get("id").cloned()?;
    // D114-cand close (R5 audit, 2026-04-29) — JSON-RPC 2.0 specifies
    // `id` must be a number, string, or null. Reject array/object
    // ids so a malformed request produces a clean -32600 (Invalid
    // Request) instead of being echoed back unchecked in subsequent
    // responses.
    match &id {
        Value::Null | Value::Number(_) | Value::String(_) => {}
        _ => {
            return Some(error_response_v(
                Some(Value::Null),
                -32600,
                "id must be a number, string, or null",
            ));
        }
    }
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = Some(id);

    let request_id = uuid::Uuid::new_v4();
    let _span =
        tracing::info_span!("mcp_fetch", request_id = %request_id, method = method).entered();
    // Emit an explicit info event so the request_id field surfaces in
    // the rolling log + test assertions regardless of the subscriber's
    // FmtSpan policy. Pre-fix the test relied on FmtSpan::NEW which
    // some feature builds (cognitive-train on macOS) omitted, leaving
    // an empty captured buffer. Inline event auto-inherits enclosing
    // span fields so request_id always lands.
    tracing::info!(target: "soma::mcp", "mcp dispatch");

    match dispatch(method, req.get("params"), db_path, cache) {
        DispatchOutcome::Ok(v) => Some(ok_response(id, v)),
        DispatchOutcome::InvalidParams(msg) => Some(error_response_v(id, -32602, &msg)),
        DispatchOutcome::MethodNotFound => {
            Some(error_response_v(id, -32601, &format!("method not found: {method}")))
        }
    }
}

/// D91 §B — pure dispatch surface, callable both from the standalone
/// stdio loop and from the resident socket forwarder. The resident
/// uses this to share a single `MemoryPackCache` instance across
/// all child `mcp-serve` processes — pre-fix each child held its
/// own cache and `soma status` showed `0 fetches` permanently.
///
/// Method routing matches `handle_request` exactly. The error
/// variants here map to JSON-RPC codes at the call site so the
/// resident's `ControlResponse::Error` stays a structured envelope
/// and the standalone stdio loop emits the spec'd JSON-RPC numeric
/// codes.
pub enum DispatchOutcome {
    Ok(Value),
    InvalidParams(String),
    MethodNotFound,
}

pub fn dispatch(
    method: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> DispatchOutcome {
    match method {
        "initialize" => DispatchOutcome::Ok(initialize_result()),
        "resources/list" => DispatchOutcome::Ok(resources_list_result(db_path)),
        "resources/read" => match resources_read(params, db_path, cache) {
            Ok(v) => DispatchOutcome::Ok(v),
            Err(msg) => DispatchOutcome::InvalidParams(msg),
        },
        // D151 close — active tool surface. resources/* 는 passive
        // (cloud LLM 이 명시적 attach 해야), tools/* 는 active
        // (LLM 이 user prompt 기반 으로 자율 invoke). recall tool
        // 가 build_memory_pack(by-query) 의 wrapper.
        "tools/list" => DispatchOutcome::Ok(tools_list_result()),
        "tools/call" => match tools_call(params, db_path, cache) {
            Ok(v) => DispatchOutcome::Ok(v),
            Err(msg) => DispatchOutcome::InvalidParams(msg),
        },
        _ => DispatchOutcome::MethodNotFound,
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        // D151 close — `tools` capability 도 advertise. cloud LLM
        // 이 tools/list + tools/call 을 auto-invoke 가능.
        "capabilities": { "resources": {}, "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
    })
}

/// D151 close + context-layer reset — active tool surface. `recall`
/// is the active counterpart of `soma://context/current` /
/// `soma://context/project/<name>` with an explicit query. Cloud LLM
/// clients auto-invoke when the user's prompt implies a recall query
/// (e.g. "what did I do last week on auth?"). MCP spec: each tool
/// needs name + description + inputSchema (JSON Schema for params).
fn tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "soma_recall",
                "description": "Search SOMA's local memory for episodes related to a query. \
                    Returns a query-scoped ContextEnvelope JSON with ranked relevant_memory, \
                    evidence, typed policy/decision/correction sections, and a compiled \
                    thread_state. Use this when the user asks about past work, earlier \
                    conversations, or 'what did I do' style queries.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Free-text query. Korean / English / mixed. \
                                Open-ended ('what was I working on') is fine — \
                                SOMA's softmax-weighted retrieval handles it."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional. Narrow recall to a single project \
                                (matches `episodes.project`, basename of cwd at \
                                capture-time). Omit for cross-project view."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional. Narrow recall to one captured \
                                episodes.session_id. Can be combined with project."
                        },
                        "thread_key": {
                            "type": "string",
                            "description": "Optional. Narrow recall to an operator-confirmed \
                                SOMA thread identity. The key must already exist in the local \
                                thread identity ledger."
                        },
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional persisted TaskFrame id. When supplied, \
                                SOMA uses the TaskFrame goal/scope to shape query, project, \
                                session, and thread_state."
                        }
                    }
                }
            },
            {
                "name": "soma_compiled_context",
                "description": "Return one cloud-facing prompt artifact that wraps the \
                    trust boundary, optional cloud-redacted TaskFrame, and cited \
                    ContextEnvelope XML. The response also exposes a structured protocol \
                    block with the supported soma-cloud-context artifact version and \
                    handoff prefix. Use this when a client wants a single payload instead \
                    of managing TaskFrame and ContextEnvelope separately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Optional semantic query. Required when task_frame_id is absent."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope."
                        },
                        "thread_key": {
                            "type": "string",
                            "description": "Optional operator-confirmed SOMA thread identity scope."
                        },
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional persisted TaskFrame id to include and use for scope/query/thread_state."
                        }
                    }
                }
            },
            {
                "name": "soma_capture_turn",
                "description": "Record one cloud/editor LLM turn into SOMA's local episode store \
                    through the same adapter-capture path used by editor hooks. Use this only \
                    when the client has the user's current turn payload and wants future \
                    ContextEnvelopes to cite it as local evidence.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": {
                            "type": "string",
                            "description": "Required adapter source, e.g. `claude-code`, \
                                `cursor`, `continue`, or another kebab-case client id."
                        },
                        "prompt_text": {
                            "type": "string",
                            "description": "Optional user prompt text for the captured turn."
                        },
                        "response_text": {
                            "type": "string",
                            "description": "Optional assistant response text for the captured turn."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Defaults to the MCP process cwd basename."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional grouping key for the captured turn."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional working directory at capture time."
                        },
                        "git_branch": {
                            "type": "string",
                            "description": "Optional git branch at capture time."
                        }
                    },
                    "required": ["source"]
                }
            },
            {
                "name": "soma_record_correction",
                "description": "Record a user correction into SOMA's local memory so future \
                    ContextEnvelopes can cite it as evidence. Use this when the user says \
                    a prior assumption, memory, or plan is wrong and gives the current truth.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "correction": {
                            "type": "string",
                            "description": "The current truth the user wants SOMA and cloud LLMs \
                                to follow going forward."
                        },
                        "claim": {
                            "type": "string",
                            "description": "Optional stale claim, assumption, or memory being corrected."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Matches `episodes.project`."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional grouping key for the correction event."
                        }
                    },
                    "required": ["correction"]
                }
            },
            {
                "name": "soma_capture_cloud_output",
                "description": "Capture a cloud LLM output as untrusted cloud_draft claim \
                    records tied to an existing TaskFrame. This never verifies or promotes \
                    the claims; by default it queues a verification request proposal that \
                    still must pass SOMA's review/apply gates. Clients should echo handoff_id, \
                    protocol_contract, and artifact_version from soma_compiled_context; SOMA \
                    rejects unsupported versions, mismatched handoff ids, and protocol echo \
                    mismatches before claim capture.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Persisted TaskFrame id that shaped the cloud call."
                        },
                        "output_text": {
                            "type": "string",
                            "description": "Raw cloud output to capture as untrusted work product."
                        },
                        "handoff_id": {
                            "type": "string",
                            "description": "Optional soma-cloud-context handoff_id echoed from soma_compiled_context. When supplied, SOMA rejects unsupported protocol versions and rejects ids that do not match the TaskFrame cloud projection."
                        },
                        "protocol_contract": {
                            "type": "string",
                            "description": "Optional protocol contract echoed from soma_compiled_context protocol.contract. If supplied, artifact_version must also be supplied and must match the supported soma-cloud-context contract."
                        },
                        "artifact_version": {
                            "type": "integer",
                            "description": "Optional artifact version echoed from soma_compiled_context protocol.artifact_version. If supplied, protocol_contract must also be supplied and the version must be supported."
                        },
                        "decision": {
                            "type": "string",
                            "description": "Control critic decision: accept, revise, or reject. Defaults to accept."
                        },
                        "extracted_claims": {
                            "type": "array",
                            "description": "Optional claim strings or {text,evidence_refs} objects extracted from output."
                        },
                        "local_claim_extractor": {
                            "type": "boolean",
                            "description": "Optional local-only assisted extractor. Used only when extracted_claims is empty and deterministic extraction finds no anchored claims."
                        },
                        "local_claim_extractor_endpoint": {
                            "type": "string",
                            "description": "Optional local extractor endpoint override. Defaults to the local compiler runtime config."
                        },
                        "local_claim_extractor_model": {
                            "type": "string",
                            "description": "Optional local extractor model override. Defaults to the local compiler runtime config."
                        },
                        "required_edits": {
                            "type": "array",
                            "description": "Required edit strings. Mandatory when decision is revise."
                        },
                        "verification_requests": {
                            "type": "array",
                            "description": "Optional {claim_text,reason,acceptable_verifiers} review requests."
                        },
                        "evidence_refs": {
                            "type": "array",
                            "description": "Optional local evidence refs attached to the critic result."
                        },
                        "enqueue_proposal": {
                            "type": "boolean",
                            "description": "When true or omitted, queue a learning critic proposal for review."
                        },
                        "proposal_action": {
                            "type": "string",
                            "description": "Optional proposal action: request_verification, propose_promotion, decay, create_candidate, or noop. Defaults to request_verification."
                        },
                        "proposal_target_lifecycle_state": {
                            "type": "string",
                            "description": "Optional lifecycle target for promotion/decay proposals."
                        },
                        "proposal_reason": {
                            "type": "string",
                            "description": "Optional proposal reason. Defaults to verification-required wording."
                        }
                    },
                    "required": ["task_frame_id", "output_text"]
                }
            },
            {
                "name": "soma_verify_claim",
                "description": "Record a user/tool/test/local/correction verification event for \
                    a claim_record. This is the only MCP review path that can add promotion \
                    trust for a cloud_draft claim.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "claim_id": {
                            "type": "integer",
                            "description": "Claim record id to verify."
                        },
                        "proposal_id": {
                            "type": "integer",
                            "description": "Learning critic proposal id whose linked claims should be verified. For confirmed promotion proposals, already trusted claims are skipped."
                        },
                        "verifier_type": {
                            "type": "string",
                            "description": "Verifier: user, test, tool, local_observation, or correction."
                        },
                        "result": {
                            "type": "string",
                            "description": "Verification result: confirmed, contradicted, superseded, or inconclusive."
                        },
                        "evidence_ref": {
                            "type": "object",
                            "description": "Evidence ref object: {kind,id,source?}."
                        },
                        "evidence_kind": {
                            "type": "string",
                            "description": "Fallback evidence kind when evidence_ref is not supplied."
                        },
                        "evidence_id": {
                            "type": "string",
                            "description": "Fallback evidence id when evidence_ref is not supplied."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Optional fallback evidence source."
                        }
                    },
                    "required": ["verifier_type", "result"]
                }
            },
            {
                "name": "soma_learning_proposals_list",
                "description": "List learning critic proposals for review without mutating \
                    claims or lifecycle state.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "status": {
                            "type": "string",
                            "description": "Optional status: queued, waiting_verification, accepted, rejected, or applied."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum proposals to return. Defaults to 20."
                        }
                    }
                }
            },
            {
                "name": "soma_learning_proposals_apply",
                "description": "Apply one learning critic proposal through SOMA's storage-layer \
                    verification/lifecycle gates. Unverified promotion proposals move to \
                    waiting_verification instead of becoming durable memory. Destructive decay \
                    or forget proposals require confirm_destructive=true.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "proposal_id": {
                            "type": "integer",
                            "description": "Learning critic proposal id to apply."
                        },
                        "confirm_destructive": {
                            "type": "boolean",
                            "description": "Required true for destructive decay/forget proposals. Defaults false."
                        }
                    },
                    "required": ["proposal_id"]
                }
            },
            {
                "name": "soma_learning_proposals_apply_ready",
                "description": "Batch-apply currently ready learning critic proposals through \
                    the same gated apply path. By default this applies only verified promotion \
                    proposals; decay and no-op closure require explicit include flags.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum open proposals to consider. Defaults to 20."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "When true, return ready/skipped decisions without mutating memory."
                        },
                        "include_decay": {
                            "type": "boolean",
                            "description": "Also apply decay proposals. Defaults false."
                        },
                        "include_noop": {
                            "type": "boolean",
                            "description": "Also close create-candidate/no-op proposals. Defaults false."
                        }
                    }
                }
            },
            {
                "name": "soma_learning_proposals_set_status",
                "description": "Mark a learning critic proposal accepted or rejected for review. \
                    This cannot mark a proposal applied; use soma_learning_proposals_apply \
                    so lifecycle gates are enforced.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "proposal_id": {
                            "type": "integer",
                            "description": "Learning critic proposal id to update."
                        },
                        "status": {
                            "type": "string",
                            "description": "Status: queued, waiting_verification, accepted, or rejected. Applied is rejected here."
                        },
                        "note": {
                            "type": "string",
                            "description": "Optional reviewer note."
                        }
                    },
                    "required": ["proposal_id", "status"]
                }
            },
            {
                "name": "soma_review_queue",
                "description": "Return SOMA's pending review queue: unverified \
                    cloud_draft claims plus open learning critic proposals. This is \
                    read-only and never creates verification or promotion trust.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to return per section. Defaults to 20."
                        }
                    }
                }
            },
            {
                "name": "soma_review_actions",
                "description": "Return a flat client action plan derived from \
                    soma_review_queue action_options. This is read-only; clients can \
                    render the returned soma_review_action argument templates as \
                    buttons or commands.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to inspect before flattening actions. Defaults to 20."
                        },
                        "include_disabled": {
                            "type": "boolean",
                            "description": "Include disabled actions with disabled_reason. Defaults false."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: json (default) or markdown."
                        }
                    }
                }
            },
            {
                "name": "soma_review_batch_template",
                "description": "Build a read-only soma_review_batch payload template \
                    from enabled review action options. This composes only verification \
                    actions (confirm, contradict, supersede, inconclusive), never applies \
                    proposals, and never writes verification events.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to inspect before composing the template. Defaults to 20."
                        },
                        "action": {
                            "type": "string",
                            "description": "Verification action to template: confirm, contradict, supersede, or inconclusive. Defaults to confirm."
                        },
                        "target_type": {
                            "type": "string",
                            "description": "Target type to include: any, claim, or proposal. Defaults to any."
                        },
                        "verifier_type": {
                            "type": "string",
                            "description": "Optional verifier type to prefill: user, test, tool, local_observation, or correction."
                        },
                        "evidence_kind": {
                            "type": "string",
                            "description": "Optional evidence kind to prefill."
                        },
                        "evidence_id": {
                            "type": "string",
                            "description": "Optional evidence id to prefill."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Optional evidence source to prefill."
                        }
                    }
                }
            },
            {
                "name": "soma_review_report",
                "description": "Render a read-only review report that combines \
                    pending cloud_draft claims, open learning proposals, client action \
                    affordances, and a dry-run-first soma_review_batch payload template. \
                    It never records verification events and never applies proposals.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to inspect before rendering the report. Defaults to 20."
                        },
                        "include_disabled": {
                            "type": "boolean",
                            "description": "Include disabled actions with disabled_reason. Defaults false."
                        },
                        "action": {
                            "type": "string",
                            "description": "Verification action to template: confirm, contradict, supersede, or inconclusive. Defaults to confirm."
                        },
                        "target_type": {
                            "type": "string",
                            "description": "Target type to include in the batch template: any, claim, or proposal. Defaults to any."
                        },
                        "verifier_type": {
                            "type": "string",
                            "description": "Optional verifier type to prefill: user, test, tool, local_observation, or correction."
                        },
                        "evidence_kind": {
                            "type": "string",
                            "description": "Optional evidence kind to prefill."
                        },
                        "evidence_id": {
                            "type": "string",
                            "description": "Optional evidence id to prefill."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Optional evidence source to prefill."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: markdown (default) or json."
                        }
                    }
                }
            },
            {
                "name": "soma_review_digest",
                "description": "Render a compact read-only client notification digest \
                    for interruptible review work. By default it surfaces only \
                    non-blocking L4 semantic review_digest items; queue-only work stays \
                    in soma_review_queue. It never records verification events and never \
                    applies proposals.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum proposals to inspect before rendering the digest. Defaults to 20."
                        },
                        "client": {
                            "type": "string",
                            "description": "Client adapter hint: generic, codex-app, cursor, continue, or claude-code."
                        },
                        "include_queue_only": {
                            "type": "boolean",
                            "description": "Include queue-only proposals in addition to interruptible digest items. Defaults false."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: json (default) or markdown."
                        }
                    }
                }
            },
            {
                "name": "soma_review_digest_ack",
                "description": "Acknowledge that a client rendered a review digest \
                    notification. This mutates only the notification cooldown ledger; \
                    it never records verification events and never applies proposals.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum proposals to inspect before recording the current digest signature. Defaults to 20."
                        },
                        "client": {
                            "type": "string",
                            "description": "Client adapter identity: generic, codex-app, cursor, continue, or claude-code."
                        },
                        "batch_key": {
                            "type": "string",
                            "description": "Optional digest batch key. Defaults to the current interruptible batch key."
                        },
                        "cooldown_seconds": {
                            "type": "integer",
                            "description": "Override cooldown window in seconds. Defaults to the digest policy."
                        }
                    }
                }
            },
            {
                "name": "soma_review_render",
                "description": "Compile a read-only client-specific review render \
                    plan from soma_review_digest, soma_review_actions, and \
                    soma_review_batch_template. The plan tells clients what to \
                    render and when to call ack or mutation tools, but this tool \
                    never records verification events, never applies proposals, and \
                    never acknowledges notifications.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to inspect while compiling the render plan. Defaults to 20."
                        },
                        "client": {
                            "type": "string",
                            "description": "Client adapter hint: generic, codex-app, cursor, continue, or claude-code."
                        },
                        "include_disabled": {
                            "type": "boolean",
                            "description": "Include disabled actions in the render plan. Defaults false."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: json (default), markdown, or html."
                        }
                    }
                }
            },
            {
                "name": "soma_client_binding_proofs",
                "description": "Read-only inspection of the Codex app/Cursor/Continue/Claude client \
                    binding proof ledger. This reports proof rows, derived readiness, \
                    latest proof stage, and artifact replay status for reference bindings, \
                    observed event files, observed_app_hook, and \
                    observed_in_client_render proofs, but it never records proof rows, never verifies \
                    claims, never promotes cloud drafts, and never applies review actions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "client": {
                            "type": "string",
                            "description": "Optional client filter, e.g. codex-app, cursor, continue, or claude-code."
                        },
                        "proof_id": {
                            "type": "integer",
                            "description": "Optional proof row id to inspect."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum proof rows to inspect when proof_id is absent. Defaults to 20."
                        }
                    }
                }
            },
            {
                "name": "soma_client_binding_record_proof",
                "description": "Record one operator-confirmed client binding proof row using \
                    the same storage gates as `soma adapter-binding-proof`. This is the MCP write \
                    surface for app-hook/render/review-action evidence: observed_app_hook requires \
                    explicit operator confirmation plus installed config, private event JSONL, and drain \
                    report evidence; observed_in_client_render requires explicit operator confirmation \
                    plus review-render and structured render evidence; observed_review_action requires \
                    explicit operator confirmation plus a storage-gated review-action report. It records \
                    a client-binding proof row only; it creates no verification event, promotes no cloud \
                    draft, and applies no proposal.",
                "inputSchema": {
                    "type": "object",
                    "required": ["manifest", "proof_level"],
                    "properties": {
                        "manifest": {
                            "type": "string",
                            "description": "Checked-in or installed client binding manifest path."
                        },
                        "client": {
                            "type": "string",
                            "description": "Optional client identity override; must match the manifest client when supplied."
                        },
                        "proof_level": {
                            "type": "string",
                            "enum": [
                                "reference_binding",
                                "observed_event_file",
                                "observed_app_hook",
                                "observed_in_client_render",
                                "observed_review_action"
                            ],
                            "description": "Proof level to record."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Human-readable source label for this proof row. Defaults to mcp_client_binding_record_proof."
                        },
                        "event_jsonl": {
                            "type": "string",
                            "description": "Private adapter event JSONL path for observed_event_file or observed_app_hook proof."
                        },
                        "installed_config": {
                            "type": "string",
                            "description": "Installed client config or hook file path required for observed_app_hook and review-action proof."
                        },
                        "drain_report": {
                            "type": "string",
                            "description": "Saved JSON report from soma adapter-spool, required for observed_app_hook proof."
                        },
                        "review_render_report": {
                            "type": "string",
                            "description": "Saved soma_review_render JSON report required for observed_in_client_render proof."
                        },
                        "render_evidence": {
                            "type": "string",
                            "description": "Structured soma.in_client_render_evidence.v1 artifact required for observed_in_client_render proof."
                        },
                        "review_action_report": {
                            "type": "string",
                            "description": "Saved soma_review_action report required for observed_review_action proof."
                        },
                        "operator_confirm_real_app_invocation": {
                            "type": "boolean",
                            "description": "Must be true for observed_app_hook proof."
                        },
                        "operator_confirm_in_client_render": {
                            "type": "boolean",
                            "description": "Must be true for observed_in_client_render proof."
                        },
                        "operator_confirm_review_action": {
                            "type": "boolean",
                            "description": "Must be true for observed_review_action proof."
                        },
                        "operator_confirm_release_grade_evidence": {
                            "type": "boolean",
                            "description": "Must be true for observed_app_hook, observed_in_client_render, or observed_review_action rows to count toward ready_for_private_client_claim."
                        }
                    }
                }
            },
            {
                "name": "soma_client_binding_install_plan",
                "description": "Read-only client binding installation plan for Codex app/Cursor/Continue/Claude clients. \
                    It renders a proof-free installed hook config artifact and optional local preflight \
                    scans so a client can guide the operator toward observed_app_hook proof, but it writes \
                    no files, records no proof row, creates no verification event, promotes no cloud draft, \
                    and does not prove private app installation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "client": {
                            "type": "string",
                            "description": "Client adapter identity, e.g. codex-app, cursor, continue, or claude-code. Required when manifest is omitted."
                        },
                        "manifest": {
                            "type": "string",
                            "description": "Optional checked-in client binding manifest path."
                        },
                        "binding_nonce": {
                            "type": "string",
                            "description": "Optional per-install binding nonce. Generated when omitted."
                        },
                        "installed_config": {
                            "type": "string",
                            "description": "Optional existing installed config path to preflight read-only."
                        },
                        "config_root": {
                            "type": "string",
                            "description": "Optional config root for installed config discovery."
                        },
                        "include_discovery": {
                            "type": "boolean",
                            "description": "When true, scan likely installed config paths read-only."
                        }
                    }
                }
            },
            {
                "name": "soma_client_binding_evidence_bundle",
                "description": "Read-only operator evidence bundle for Codex app/Cursor/Continue/Claude client binding readiness. \
                    It composes readiness, installed-config discovery, proof-free installed-config preview, \
                    real-app proof-kit guidance, proof_session release gate, operator flow steps, and blocking gaps so a client can \
                    guide the operator toward observed_app_hook and observed_in_client_render proof. It \
                    writes no files, records no proof row, creates no verification event, promotes no cloud \
                    draft, applies no proposal, and does not prove private app installation, rendering, or review-action execution.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "client": {
                            "type": "string",
                            "description": "Client adapter identity, e.g. codex-app, cursor, continue, or claude-code. Required when manifest is omitted."
                        },
                        "manifest": {
                            "type": "string",
                            "description": "Optional checked-in client binding manifest path."
                        },
                        "binding_nonce": {
                            "type": "string",
                            "description": "Optional per-install binding nonce. Generated when omitted."
                        },
                        "installed_config": {
                            "type": "string",
                            "description": "Optional existing installed config path to preflight read-only and include in operator commands."
                        },
                        "config_root": {
                            "type": "string",
                            "description": "Optional config root for installed config discovery."
                        },
                        "artifact_dir": {
                            "type": "string",
                            "description": "Optional durable evidence artifact directory for proof-session/operator commands. Use workspace fallback paths here when the default home evidence directory is not writable. Read-only by itself."
                        },
                        "event_jsonl": {
                            "type": "string",
                            "description": "Optional private adapter JSONL path to preflight in the proof kit and include in operator commands."
                        },
                        "review_render_report": {
                            "type": "string",
                            "description": "Optional review-render report path to preflight render evidence binding."
                        },
                        "render_evidence": {
                            "type": "string",
                            "description": "Optional structured soma.in_client_render_evidence.v1 artifact path to preflight."
                        },
                        "review_action_report": {
                            "type": "string",
                            "description": "Optional saved soma_review_action report path to preflight observed_review_action readiness."
                        }
                    }
                }
            },
            {
                "name": "soma_client_binding_proof_session",
                "description": "Read-only compact proof-session card for Codex app/Cursor/Continue/Claude client binding readiness. \
                    It exposes the release_gate, operator_next_action_id, operator_card, next_step_id, next_operator_step, pending proof levels, \
                    blocking gaps, and schema-versioned operator runbook from the evidence bundle contract so a client can guide setup/status UX \
                    without parsing the full bundle. It writes no files, records no proof row, creates no \
                    verification event, promotes no cloud draft, applies no proposal, and does not prove \
                    private app installation, rendering, or review-action execution.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "client": {
                            "type": "string",
                            "description": "Client adapter identity, e.g. codex-app, cursor, continue, or claude-code. Required when manifest is omitted."
                        },
                        "manifest": {
                            "type": "string",
                            "description": "Optional checked-in client binding manifest path."
                        },
                        "binding_nonce": {
                            "type": "string",
                            "description": "Optional per-install binding nonce. Generated when omitted."
                        },
                        "installed_config": {
                            "type": "string",
                            "description": "Optional existing installed config path to preflight read-only and include in operator commands."
                        },
                        "config_root": {
                            "type": "string",
                            "description": "Optional config root for installed config discovery."
                        },
                        "artifact_dir": {
                            "type": "string",
                            "description": "Optional durable evidence artifact directory for proof-session/operator commands. Use workspace fallback paths here when the default home evidence directory is not writable. Read-only by itself."
                        },
                        "event_jsonl": {
                            "type": "string",
                            "description": "Optional private adapter JSONL path to preflight app-hook readiness."
                        },
                        "review_render_report": {
                            "type": "string",
                            "description": "Optional review-render report path to preflight render evidence binding."
                        },
                        "render_evidence": {
                            "type": "string",
                            "description": "Optional structured soma.in_client_render_evidence.v1 artifact path to preflight."
                        },
                        "review_action_report": {
                            "type": "string",
                            "description": "Optional saved soma_review_action report path to preflight observed_review_action readiness."
                        }
                    }
                }
            },
            {
                "name": "soma_client_render_evidence_packet",
                "description": "Read-only in-client render evidence packet renderer. \
                    It materializes a proof-free soma.in_client_render_evidence.v1 packet from a saved \
                    review-render JSON report by filling only non-observational bindings such as the \
                    review-render fingerprint, workbench version, interaction contract version, and \
                    current control ids. It writes no files, records no proof row, creates no verification \
                    event, promotes no cloud draft, applies no proposal, and cannot prove in-client rendering \
                    until a client/operator fills visible-render observations and records observed_in_client_render \
                    with explicit operator confirmation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "client": {
                            "type": "string",
                            "description": "Client adapter identity, e.g. codex-app, cursor, continue, or claude-code. Required when manifest is omitted."
                        },
                        "manifest": {
                            "type": "string",
                            "description": "Optional checked-in client binding manifest path."
                        },
                        "review_render_report": {
                            "type": "string",
                            "description": "Required saved JSON report from soma_review_render or soma context review-render --format json."
                        }
                    }
                }
            },
            {
                "name": "soma_latent_predict",
                "description": "Read-only prediction over SOMA's evidence-backed latent \
                    proxy substrate. Use this to inspect which L2/L3/L4 latent proxies \
                    would be active for a query before relying on a ContextEnvelope. \
                    The predictor excludes cloud_draft proxies, never records claims \
                    or verification events, never creates proposals, never mutates \
                    lifecycle state, and falls back to deterministic projection when \
                    confidence is low.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Required query used to score active evidence-backed latent proxies."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Matches episodes.project."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope. Matches episodes.session_id."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum predictions to return. Defaults to 8."
                        },
                        "scan_limit": {
                            "type": "integer",
                            "description": "Maximum active latent proxies to inspect before scoring. Defaults to 160."
                        },
                        "min_confidence": {
                            "type": "number",
                            "description": "Minimum prediction score before deterministic fallback. Defaults to 0.35."
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "soma_latent_packet",
                "description": "Read-only advanced latent interface packet renderer. \
                    It packages predicted evidence-backed latent proxies for cloud-local \
                    handoff while including no raw vectors, no hidden-state injection, \
                    and no uninspectable latent payload. The packet provides an explicit \
                    textual fallback over current text/JSON channels, excludes cloud_draft \
                    proxies, and never records claims or verification events, creates \
                    proposals, mutates lifecycle state, or changes the ContextEnvelope.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Required query used to select evidence-backed latent proxies for the packet."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Matches episodes.project."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope. Matches episodes.session_id."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum proxy bindings to include. Defaults to 8."
                        },
                        "scan_limit": {
                            "type": "integer",
                            "description": "Maximum active latent proxies to inspect before scoring. Defaults to 160."
                        },
                        "min_confidence": {
                            "type": "number",
                            "description": "Minimum prediction score before a proxy becomes a packet binding. Defaults to 0.35."
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "soma_latent_eval",
                "description": "Read-only evaluation of SOMA's latent proxy predictor \
                    against inline or storage-derived evidence cases. It compares prediction \
                    hits with the deterministic active-proxy baseline, creates no claims or \
                    verification events, creates no proposals, mutates no lifecycle state, \
                    and treats any cloud_draft prediction as a trust-boundary failure signal.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "cases": {
                            "type": "array",
                            "description": "Optional inline cases. Each case needs id, query, and expected_proxy_ids.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "description": { "type": "string" },
                                    "source": { "type": "string" },
                                    "query": { "type": "string" },
                                    "project": { "type": "string" },
                                    "session_id": { "type": "string" },
                                    "expected_proxy_ids": {
                                        "type": "array",
                                        "items": { "type": "integer" }
                                    }
                                },
                                "required": ["id", "query", "expected_proxy_ids"]
                            }
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Used for storage-derived cases and as fallback for inline cases."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope. Used for storage-derived cases and as fallback for inline cases."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum predictions to return per case. Defaults to 8."
                        },
                        "scan_limit": {
                            "type": "integer",
                            "description": "Maximum active latent proxies to inspect per case. Defaults to 160."
                        },
                        "case_limit": {
                            "type": "integer",
                            "description": "Maximum storage-derived cases when cases is absent. Defaults to 32."
                        },
                        "case_source": {
                            "type": "string",
                            "description": "Optional case source. Use task_frame_outcomes to derive cases from recorded TaskFrame outcomes; defaults to active proxy storage cases."
                        },
                        "min_confidence": {
                            "type": "number",
                            "description": "Minimum prediction score before deterministic fallback. Defaults to 0.35."
                        }
                    }
                }
            },
            {
                "name": "soma_task_frame_outcome",
                "description": "Record an evidence-backed outcome for a persisted \
                    TaskFrame. This closes the cloud-local loop for evaluation only: it \
                    creates no claim, verification event, proposal, lifecycle transition, \
                    semantic fact, or ContextEnvelope mutation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Persisted TaskFrame id."
                        },
                        "outcome_type": {
                            "type": "string",
                            "description": "accepted, revised, rejected, verified, applied, failed, or abandoned."
                        },
                        "summary": {
                            "type": "string",
                            "description": "Outcome summary used later as eval corpus text."
                        },
                        "evidence_kind": {
                            "type": "string",
                            "description": "Evidence kind, e.g. user, tool, test, local_observation, correction."
                        },
                        "evidence_id": {
                            "type": "string",
                            "description": "Evidence identifier."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Optional evidence source."
                        },
                        "claim_ids": {
                            "type": "array",
                            "items": { "type": "integer" }
                        },
                        "proposal_ids": {
                            "type": "array",
                            "items": { "type": "integer" }
                        },
                        "latent_proxy_ids": {
                            "type": "array",
                            "items": { "type": "integer" }
                        }
                    },
                    "required": ["task_frame_id", "outcome_type", "summary", "evidence_kind", "evidence_id"]
                }
            },
            {
                "name": "soma_task_frame_outcomes",
                "description": "Read-only list of evidence-backed TaskFrame outcome \
                    records for review panes and eval corpus inspection.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional TaskFrame id to inspect."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum outcome rows to return. Defaults to 20."
                        }
                    }
                }
            },
            {
                "name": "soma_review_drain",
                "description": "Run SOMA's safe review drain policy: apply only \
                    verified, non-destructive promotion proposals through storage \
                    gates, then return before/after review snapshots. It never \
                    verifies cloud drafts and never applies decay/forget proposals.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims and proposals to inspect/drain. Defaults to 20."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Preview the drain without mutating proposals or claim lifecycle. Defaults false."
                        }
                    }
                }
            },
            {
                "name": "soma_scheduler_run",
                "description": "Run selected scheduler review/learning subpasses \
                    through existing SOMA gates. Supports dry_run previews for \
                    open_decision_proposals, semantic_proposals, review_drain, \
                    and explicitly selected l2_promote/l3_decay/task_frame_retention. \
                    The default/all pass set excludes l2_promote, l3_decay, and \
                    task_frame_retention. \
                    This never creates verification events and never bypasses \
                    review/storage gates.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional project scope for review/learning passes."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session scope for review/learning passes."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum claims, proposals, or open-decision signals to inspect per pass. Defaults to 32."
                        },
                        "semantic_min_support": {
                            "type": "integer",
                            "description": "Minimum repeated verified L3 claims required for semantic proposals. Defaults to 2."
                        },
                        "l2_promotion_min_confidence": {
                            "type": "number",
                            "description": "Promote durable L2 proxy types when confidence is at least this value for pass=l2_promote. Defaults to 0.90."
                        },
                        "l2_promotion_anomaly_min_confidence": {
                            "type": "number",
                            "description": "Promote anomaly/conflict L2 proxy types when confidence is at least this value. Defaults to 0.85."
                        },
                        "l2_promotion_min_repeated_support": {
                            "type": "integer",
                            "description": "Promote repeated active L2 claims after this many scoped support rows. Defaults to 2."
                        },
                        "l2_promotion_reason": {
                            "type": "string",
                            "description": "Lifecycle transition reason for explicitly selected l2_promote pass."
                        },
                        "l3_decay_older_than_days": {
                            "type": "integer",
                            "description": "Consider L3 decay candidates older than this many days when passes includes l3_decay. Defaults to 90."
                        },
                        "l3_decay_cutoff_ns": {
                            "type": "integer",
                            "description": "Explicit L3 decay cutoff timestamp in nanoseconds for reproducible audits. Overrides l3_decay_older_than_days."
                        },
                        "l3_decay_max_access_count": {
                            "type": "integer",
                            "description": "Decay only L3 proxies with access_count at or below this value. Defaults to 0."
                        },
                        "l3_decay_reason": {
                            "type": "string",
                            "description": "Lifecycle transition reason for explicitly selected l3_decay pass."
                        },
                        "task_frame_retention_days": {
                            "type": "integer",
                            "description": "Retain TaskFrames at least this many days when passes includes task_frame_retention. Defaults to 30."
                        },
                        "task_frame_retention_cutoff_ns": {
                            "type": "integer",
                            "description": "Explicit TaskFrame retention cutoff timestamp in nanoseconds for reproducible audits. Overrides task_frame_retention_days."
                        },
                        "task_frame_retention_reason": {
                            "type": "string",
                            "description": "Audit/display reason for explicitly selected task_frame_retention pass."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Preview all selected passes without creating proposals or applying drains. Defaults false."
                        },
                        "passes": {
                            "type": "array",
                            "description": "Optional pass list: all, open_decision_proposals, semantic_proposals, review_drain, l2_promote, l3_decay, task_frame_retention. l2_promote, l3_decay, and task_frame_retention are explicit only."
                        }
                    }
                }
            },
            {
                "name": "soma_semantic_proposals",
                "description": "Preview or create L4 semantic_fact promotion \
                    proposals from repeated verified L3 claim evidence. Repetition \
                    is exact normalized text or conservative token signature, not \
                    open-ended paraphrase judgment. This never promotes directly; \
                    resulting proposals still apply through \
                    review drain/action storage gates.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum long-term claims to inspect. Defaults to 100."
                        },
                        "min_support": {
                            "type": "integer",
                            "description": "Minimum repeated long-term claims required. Defaults to 2 and must be at least 2."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Preview semantic promotion proposals without inserting them. Defaults false."
                        }
                    }
                }
            },
            {
                "name": "soma_open_decision_proposals",
                "description": "Preview or create request-verification proposals \
                    from unresolved L2 open decisions such as contradictions and \
                    iPC anomalies. This captures local short-term claims for \
                    review only; it does not resolve conflicts or write L4 facts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional source episode project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional source episode session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum unresolved open decisions to inspect. Defaults to 20."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Preview without inserting TaskFrames, claims, or proposals. Defaults false."
                        }
                    }
                }
            },
            {
                "name": "soma_review_action",
                "description": "Take one operator review action on a queued claim or proposal. \
                    Verification actions record trusted evidence; apply actions still go through \
                    SOMA's storage-layer verification/lifecycle gates. The control_id from \
                    soma_review_render or soma_review_actions is required and must match a \
                    currently enabled rendered action.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "claim_id": {
                            "type": "integer",
                            "description": "Claim record id to review. Mutually exclusive with proposal_id."
                        },
                        "proposal_id": {
                            "type": "integer",
                            "description": "Learning critic proposal id to review. Mutually exclusive with claim_id."
                        },
                        "action": {
                            "type": "string",
                            "description": "Action: confirm, contradict, supersede, inconclusive, accept, reject, wait, apply, or confirm_and_apply."
                        },
                        "control_id": {
                            "type": "string",
                            "description": "Rendered review control id, such as claim:12:confirm. It must match a currently enabled action option."
                        },
                        "verifier_type": {
                            "type": "string",
                            "description": "Verifier for actions that record claim verification: user, test, tool, local_observation, or correction. Defaults to user."
                        },
                        "evidence_ref": {
                            "type": "object",
                            "description": "Evidence ref object for verification actions: {kind,id,source?}."
                        },
                        "evidence_kind": {
                            "type": "string",
                            "description": "Fallback evidence kind when evidence_ref is not supplied."
                        },
                        "evidence_id": {
                            "type": "string",
                            "description": "Fallback evidence id when evidence_ref is not supplied."
                        },
                        "evidence_source": {
                            "type": "string",
                            "description": "Optional fallback evidence source note."
                        },
                        "note": {
                            "type": "string",
                            "description": "Optional reviewer note for proposal status actions."
                        },
                        "confirm_destructive": {
                            "type": "boolean",
                            "description": "Required true to apply destructive decay/forget proposals. Defaults false."
                        }
                    },
                    "required": ["action", "control_id"]
                }
            },
            {
                "name": "soma_review_batch",
                "description": "Record a verification-only batch of review actions. \
                    Each operation can confirm, contradict, supersede, or mark inconclusive \
                    for a claim or proposal with trusted evidence. This never applies \
                    proposals, never performs destructive changes, and supports dry_run preflight.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "operations": {
                            "type": "array",
                            "description": "Array of operations. Each item uses claim_id or proposal_id, action, optional control_id, verifier_type, and evidence_ref or evidence_kind/evidence_id."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "description": "Validate the batch without inserting verification events. Defaults false."
                        }
                    },
                    "required": ["operations"]
                }
            },
            {
                "name": "soma_trust_boundary_audit",
                "description": "Audit persisted claim/proposal trust-boundary invariants. \
                    This read-only evaluation checks that cloud_draft claims have not become \
                    L3/L4 memory without trusted verification and that applied promotion \
                    proposals still satisfy storage trust gates.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": {
                            "type": "string",
                            "description": "Optional TaskFrame project scope."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional TaskFrame session scope."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum recent claim/proposal rows to inspect. Defaults to 1000."
                        }
                    }
                }
            },
            {
                "name": "soma_context_why",
                "description": "Explain why claims or memories appear in SOMA's current \
                    ContextEnvelope or audit ledger. Returns matching envelope or audit \
                    sections with their evidence references and inclusion reasons so a \
                    cloud LLM can audit before relying on local context.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Optional semantic query used to assemble the envelope."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Matches `episodes.project`."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope. Matches \
                                `episodes.session_id`; can be combined with project."
                        },
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional persisted TaskFrame id. When supplied, \
                                the explanation uses the TaskFrame-shaped ContextEnvelope."
                        },
                        "section": {
                            "type": "string",
                            "description": "Optional section filter: thread_state, compiler_notes, \
                                relevant_memory, short_term_candidates, project_experience, \
                                stable_facts, user_policy, open_decisions, corrections, \
                                claim_records, or learning_critic_proposals."
                        },
                        "contains": {
                            "type": "string",
                            "description": "Optional case-insensitive text filter for matching claims."
                        }
                    }
                }
            },
            {
                "name": "soma_context_audit",
                "description": "Audit the cloud-facing ContextEnvelope evidence contract and, \
                    optionally, a persisted TaskFrame cloud projection. Use this before relying \
                    on context or before sending TaskFrame-shaped context to a cloud model.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Optional semantic query used to assemble the audited envelope."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope. Matches `episodes.project`."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope. Matches `episodes.session_id`."
                        },
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional persisted TaskFrame id to audit for privacy projection leaks."
                        }
                    }
                }
            },
            {
                "name": "soma_product_hardening_report",
                "description": "Compose SOMA's read-only product hardening gates into one \
                    client-facing report. This audits ContextEnvelope evidence, storage trust \
                    boundaries, review backlog, review interaction contracts, TaskFrame \
                    retention hygiene, optional TaskFrame projection privacy, and optional \
                    client binding readiness without recording proof, verification, \
                    acknowledgement, promotion, TaskFrame deletion, or apply trust.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Optional semantic query used to assemble the audited envelope."
                        },
                        "project": {
                            "type": "string",
                            "description": "Optional project scope for envelope and trust audits."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional captured session scope for envelope and trust audits."
                        },
                        "task_frame_id": {
                            "type": "integer",
                            "description": "Optional persisted TaskFrame id to audit for privacy projection leaks."
                        },
                        "client": {
                            "type": "string",
                            "description": "Optional client binding proof filter, such as codex-app, cursor, continue, or claude-code."
                        },
                        "required_clients": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of clients that must all have ready app-hook, in-client render, and review-action proof before private-client release."
                        },
                        "trust_limit": {
                            "type": "integer",
                            "description": "Maximum recent claim/proposal rows to inspect. Defaults to 1000."
                        },
                        "review_limit": {
                            "type": "integer",
                            "description": "Maximum pending review queue rows to inspect. Defaults to 1000."
                        },
                        "client_proof_limit": {
                            "type": "integer",
                            "description": "Maximum recent client binding proof rows to inspect. Defaults to 20."
                        },
                        "client_binding_config_root": {
                            "type": "string",
                            "description": "Optional config root for read-only client binding proof-session discovery."
                        },
                        "require_client_binding_ready": {
                            "type": "boolean",
                            "description": "When true, missing or unready client binding proof is a blocking failure instead of a warning. Defaults to codex-app, cursor, and continue unless scoped by client or required_clients."
                        },
                        "require_review_queue_clear": {
                            "type": "boolean",
                            "description": "When true, any pending review queue item is a blocking release failure instead of a warning."
                        },
                        "require_task_frame_retention_clean": {
                            "type": "boolean",
                            "description": "When true, stale unreferenced TaskFrame retention candidates are blocking release failures instead of warnings."
                        },
                        "require_task_frame_projection": {
                            "type": "boolean",
                            "description": "When true, missing TaskFrame cloud projection privacy proof is a blocking release failure instead of a warning."
                        },
                        "task_frame_retention_days": {
                            "type": "integer",
                            "description": "Retain TaskFrames at least this many days before reporting retention candidates. Defaults to 30."
                        },
                        "skip_client_binding": {
                            "type": "boolean",
                            "description": "When true, skip client binding readiness inspection and report it as unproven."
                        }
                    }
                }
            }
        ]
    })
}

/// D151 close — `tools/call` dispatcher. `soma_recall` reuses the
/// same retrieval substrate as `resources/read` but returns a
/// query-scoped ContextEnvelope, not the developer/debug MemoryPack artifact.
/// Returns MCP-spec `content` array with a single JSON text item.
fn tools_call(
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| "missing `name` param".to_string())?;

    match name {
        "soma_recall" => tools_call_recall(name, params, db_path, cache),
        "soma_compiled_context" => tools_call_compiled_context(name, params, db_path, cache),
        "soma_capture_turn" => tools_call_capture_turn(name, params, db_path, cache),
        "soma_capture_cloud_output" => tools_call_capture_cloud_output(name, params, db_path),
        "soma_verify_claim" => tools_call_verify_claim(name, params, db_path),
        "soma_learning_proposals_list" => tools_call_learning_proposals_list(name, params, db_path),
        "soma_learning_proposals_apply" => {
            tools_call_learning_proposals_apply(name, params, db_path)
        }
        "soma_learning_proposals_apply_ready" => {
            tools_call_learning_proposals_apply_ready(name, params, db_path)
        }
        "soma_learning_proposals_set_status" => {
            tools_call_learning_proposals_set_status(name, params, db_path)
        }
        "soma_review_queue" => tools_call_review_queue(name, params, db_path),
        "soma_review_actions" => tools_call_review_actions(name, params, db_path),
        "soma_review_batch_template" => tools_call_review_batch_template(name, params, db_path),
        "soma_review_report" => tools_call_review_report(name, params, db_path),
        "soma_review_digest" => tools_call_review_digest(name, params, db_path),
        "soma_review_digest_ack" => tools_call_review_digest_ack(name, params, db_path),
        "soma_review_render" => tools_call_review_render(name, params, db_path),
        "soma_client_binding_proofs" => tools_call_client_binding_proofs(name, params, db_path),
        "soma_client_binding_record_proof" => {
            tools_call_client_binding_record_proof(name, params, db_path)
        }
        "soma_client_binding_install_plan" => tools_call_client_binding_install_plan(name, params),
        "soma_client_binding_evidence_bundle" => {
            tools_call_client_binding_evidence_bundle(name, params, db_path)
        }
        "soma_client_binding_proof_session" => {
            tools_call_client_binding_proof_session(name, params, db_path)
        }
        "soma_client_render_evidence_packet" => {
            tools_call_client_render_evidence_packet(name, params)
        }
        "soma_latent_predict" => tools_call_latent_predict(name, params, db_path),
        "soma_latent_packet" => tools_call_latent_packet(name, params, db_path),
        "soma_latent_eval" => tools_call_latent_eval(name, params, db_path),
        "soma_task_frame_outcome" => tools_call_task_frame_outcome(name, params, db_path),
        "soma_task_frame_outcomes" => tools_call_task_frame_outcomes(name, params, db_path),
        "soma_review_drain" => tools_call_review_drain(name, params, db_path),
        "soma_scheduler_run" => tools_call_scheduler_run(name, params, db_path),
        "soma_semantic_proposals" => tools_call_semantic_proposals(name, params, db_path),
        "soma_open_decision_proposals" => tools_call_open_decision_proposals(name, params, db_path),
        "soma_review_action" => tools_call_review_action(name, params, db_path),
        "soma_review_batch" => tools_call_review_batch(name, params, db_path),
        "soma_trust_boundary_audit" => tools_call_trust_boundary_audit(name, params, db_path),
        "soma_record_correction" => tools_call_record_correction(name, params, db_path),
        "soma_context_why" => tools_call_context_why(name, params, db_path, cache),
        "soma_context_audit" => tools_call_context_audit(name, params, db_path, cache),
        "soma_product_hardening_report" => {
            tools_call_product_hardening_report(name, params, db_path, cache)
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

fn tools_call_recall(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments.get("query").and_then(|q| q.as_str()).map(str::to_string);
    let project = arguments.get("project").and_then(|p| p.as_str()).map(|s| s.to_string());
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(|s| s.to_string());
    let thread_key = arguments.get("thread_key").and_then(|p| p.as_str()).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);
    if query.as_deref().is_none_or(|q| q.trim().is_empty()) && task_frame_id.is_none() {
        return Err("missing `arguments.query` or `arguments.task_frame_id`".to_string());
    }
    let envelope = build_context_envelope_for_mcp(
        db_path,
        cache,
        query.clone(),
        project.clone(),
        session_id.clone(),
        thread_key.clone(),
        task_frame_id,
    )?;
    let js = render_context_json(&envelope);
    Ok(json!({
        "content": [
            { "type": "text", "text": js }
        ],
        "_debug": {
            "tool": name,
            "query": query,
            "project": project,
            "session_id": session_id,
            "thread_key": thread_key,
            "task_frame_id": task_frame_id,
            "contract": "context-envelope"
        }
    }))
}

fn tools_call_compiled_context(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments.get("query").and_then(|q| q.as_str()).map(str::to_string);
    let project = arguments.get("project").and_then(|p| p.as_str()).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(str::to_string);
    let thread_key = arguments.get("thread_key").and_then(|p| p.as_str()).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);
    if query.as_deref().is_none_or(|q| q.trim().is_empty()) && task_frame_id.is_none() {
        return Err("missing `arguments.query` or `arguments.task_frame_id`".to_string());
    }
    let envelope = build_context_envelope_for_mcp(
        db_path,
        cache,
        query.clone(),
        project.clone(),
        session_id.clone(),
        thread_key.clone(),
        task_frame_id,
    )?;
    let task_frame = match task_frame_id {
        Some(task_frame_id) => {
            let storage =
                Storage::open(db_path).map_err(|e| format!("TaskFrame {task_frame_id}: {e}"))?;
            Some(
                storage
                    .task_frame(task_frame_id)
                    .map_err(|e| format!("TaskFrame {task_frame_id}: {e}"))?
                    .ok_or_else(|| format!("TaskFrame {task_frame_id} not found"))?,
            )
        }
        None => None,
    };
    let text = render_cloud_context_artifact(&envelope, task_frame.as_ref());
    let handoff_id =
        task_frame.as_ref().map(crate::context::cloud_prompt::expected_cloud_context_handoff_id);
    let protocol = crate::context::cloud_prompt::cloud_context_protocol();
    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "query": query,
            "project": project,
            "session_id": session_id,
            "thread_key": thread_key,
            "task_frame_id": task_frame_id,
            "handoff_id": handoff_id,
            "protocol": protocol,
            "contract": "soma-cloud-context"
        }
    }))
}

fn tools_call_capture_turn(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let source = arguments
        .get("source")
        .and_then(|s| s.as_str())
        .ok_or_else(|| "missing `arguments.source`".to_string())?
        .to_string();
    let mut payload = json!({ "source": source });
    for key in
        ["session_id", "prompt_text", "response_text", "project", "cwd", "git_branch", "digest"]
    {
        if let Some(value) = arguments.get(key).and_then(|v| v.as_str()) {
            payload[key] = json!(value);
        }
    }
    for key in ["ts_start_ns", "ts_end_ns"] {
        if let Some(value) = arguments.get(key).and_then(|v| v.as_i64()) {
            payload[key] = json!(value);
        }
    }

    let cwd =
        payload.get("cwd").and_then(|v| v.as_str()).map(str::to_string).or_else(current_cwd_string);
    let project =
        payload.get("project").and_then(|v| v.as_str()).map(str::to_string).or_else(|| {
            cwd.as_deref().and_then(|p| crate::project::name_from_path(Some(Path::new(p))))
        });
    let git_branch = payload
        .get("git_branch")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| current_git_branch(cwd.as_deref()));
    if payload.get("cwd").and_then(|v| v.as_str()).is_none() {
        if let Some(cwd) = cwd.as_deref() {
            payload["cwd"] = json!(cwd);
        }
    }
    if payload.get("project").and_then(|v| v.as_str()).is_none() {
        if let Some(project) = project.as_deref() {
            payload["project"] = json!(project);
        }
    }
    if payload.get("git_branch").and_then(|v| v.as_str()).is_none() {
        if let Some(git_branch) = git_branch.as_deref() {
            payload["git_branch"] = json!(git_branch);
        }
    }
    let session_id =
        payload.get("session_id").and_then(|v| v.as_str()).map(str::to_string).or_else(|| {
            std::env::var("SOMA_SESSION_ID").ok().filter(|value| !value.trim().is_empty())
        });
    let defaults = AdapterCaptureDefaults { cwd, project, session_id, git_branch };
    let raw = serde_json::to_string(&payload).map_err(|e| format!("capture turn: {e}"))?;
    let ctx = IngestContext { db_path: db_path.to_path_buf() };
    let outcome = run_adapter_capture_json(&raw, None, defaults, &ctx)
        .map_err(|e| format!("capture turn: {e}"))?;
    let IngestOutcome::Stored { episode_id } = outcome;
    cache.invalidate_all();

    let text = serde_json::to_string(&json!({
        "episode_id": episode_id,
        "source": payload["source"],
        "project": payload.get("project").and_then(|v| v.as_str()),
        "session_id": payload.get("session_id").and_then(|v| v.as_str()),
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": { "tool": name, "episode_id": episode_id }
    }))
}

fn current_cwd_string() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.display().to_string())
}

fn current_git_branch(cwd: Option<&str>) -> Option<String> {
    let cwd = cwd?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn tools_call_record_correction(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let correction = arguments
        .get("correction")
        .and_then(|q| q.as_str())
        .ok_or_else(|| "missing `arguments.correction`".to_string())?
        .to_string();
    let claim = arguments.get("claim").and_then(|p| p.as_str()).map(|s| s.to_string());
    let project = arguments.get("project").and_then(|p| p.as_str()).map(|s| s.to_string());
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(|s| s.to_string());

    let mut storage = Storage::open(db_path).map_err(|e| format!("record correction: {e}"))?;
    let report = record_correction_with_report(
        &mut storage,
        CorrectionInput { claim, correction, project: project.clone(), session_id },
    )
    .map_err(|e| format!("record correction: {e}"))?;
    let corrected_claim_count = report.corrected_claim_ids.len();
    let text = serde_json::to_string(&json!({
        "episode_id": report.episode_id,
        "source": "correction",
        "project": project,
        "corrected_claim_ids": report.corrected_claim_ids,
        "corrected_claim_count": corrected_claim_count,
        "resolved_contradiction_count": report.resolved_contradiction_count,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "episode_id": report.episode_id,
            "corrected_claim_count": corrected_claim_count,
            "resolved_contradiction_count": report.resolved_contradiction_count
        }
    }))
}

fn tools_call_capture_cloud_output(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let task_frame_id = arguments
        .get("task_frame_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "missing `arguments.task_frame_id`".to_string())?;
    let output_text = arguments
        .get("output_text")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `arguments.output_text`".to_string())?
        .to_string();
    let handoff_id = arguments.get("handoff_id").and_then(Value::as_str).map(str::to_string);
    let protocol_contract =
        arguments.get("protocol_contract").and_then(Value::as_str).map(str::to_string);
    let artifact_version = match arguments.get("artifact_version") {
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                "`arguments.artifact_version` must be an unsigned integer".to_string()
            })?;
            Some(
                u32::try_from(raw)
                    .map_err(|_| "`arguments.artifact_version` exceeds u32".to_string())?,
            )
        }
        None => None,
    };
    let decision = parse_control_critic_decision(
        arguments.get("decision").and_then(Value::as_str).unwrap_or("accept"),
    )?;
    let extracted_claims = parse_extracted_claims(arguments.get("extracted_claims"))?;
    let local_claim_extractor =
        arguments.get("local_claim_extractor").and_then(Value::as_bool).unwrap_or(false);
    let local_runtime = if local_claim_extractor {
        let local_config = load_local_compiler_config_from_home();
        let (endpoint, model) = resolve_local_compiler_runtime(
            arguments.get("local_claim_extractor_endpoint").and_then(Value::as_str),
            arguments.get("local_claim_extractor_model").and_then(Value::as_str),
            &local_config,
        );
        Some((endpoint, model))
    } else {
        None
    };
    let local_runtime_ref = local_runtime.as_ref().map(|(endpoint, model)| {
        LocalClaimExtractorRuntime { endpoint: endpoint.as_str(), model: model.as_str() }
    });
    let (extracted_claims, claim_extraction) =
        select_cloud_output_claims(&output_text, extracted_claims, local_runtime_ref)
            .map_err(|e| format!("local claim extractor: {e}"))?;
    let required_edits = parse_string_array(arguments.get("required_edits"), "required_edits")?;
    let verification_requests =
        parse_verification_requests(arguments.get("verification_requests"))?;
    let evidence_refs = parse_evidence_refs(arguments.get("evidence_refs"))?;

    let input = CloudOutputCaptureInput {
        output_text,
        handoff_id,
        protocol_contract,
        artifact_version,
        critic: ControlCriticResult {
            task_frame_id,
            decision,
            extracted_claims,
            verification_requests,
            required_edits,
            evidence_refs,
        },
    };
    let mut storage = Storage::open(db_path).map_err(|e| format!("capture cloud output: {e}"))?;
    let captured = capture_cloud_output_claims(&mut storage, &input)
        .map_err(|e| format!("capture cloud output: {e}"))?;

    let enqueue_proposal =
        arguments.get("enqueue_proposal").and_then(Value::as_bool).unwrap_or(true);
    let proposal_id = if enqueue_proposal {
        let action = parse_learning_critic_action(
            arguments
                .get("proposal_action")
                .and_then(Value::as_str)
                .unwrap_or("request_verification"),
        )?;
        let target_lifecycle_state =
            match arguments.get("proposal_target_lifecycle_state").and_then(Value::as_str) {
                Some(value) => Some(parse_lifecycle_state(value)?),
                None => None,
            };
        let reason = arguments.get("proposal_reason").and_then(Value::as_str).unwrap_or(
            "Cloud output captured; external verification required before durable promotion",
        );
        let draft = learning_critic_proposal_from_capture(
            &captured,
            action,
            target_lifecycle_state,
            reason,
        );
        Some(
            storage
                .insert_learning_critic_proposal(&draft)
                .map_err(|e| format!("capture cloud output proposal: {e}"))?,
        )
    } else {
        None
    };

    let protocol = crate::context::cloud_prompt::cloud_context_protocol();
    let text = serde_json::to_string(&json!({
        "task_frame_id": captured.task_frame_id,
        "handoff_id": captured.handoff_id,
        "decision": captured.decision.as_str(),
        "claim_ids": captured.claim_ids,
        "verification_event_ids": captured.verification_event_ids,
        "verification_requests": captured.verification_requests,
        "required_edits": captured.required_edits,
        "proposal_id": proposal_id,
        "claim_extraction": claim_extraction.as_str(),
        "protocol": protocol,
        "trust_boundary": crate::context::cloud_prompt::CLOUD_CONTEXT_CAPTURE_TRUST_BOUNDARY,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "task_frame_id": captured.task_frame_id,
            "handoff_id": captured.handoff_id,
            "claim_count": captured.claim_ids.len(),
            "claim_extraction": claim_extraction.as_str(),
            "protocol": protocol,
            "proposal_id": proposal_id
        }
    }))
}

fn tools_call_verify_claim(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let verifier_type = parse_verifier_type_for_mcp(
        arguments
            .get("verifier_type")
            .or_else(|| arguments.get("verifier"))
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `arguments.verifier_type`".to_string())?,
    )?;
    let result = parse_verification_result_for_mcp(
        arguments
            .get("result")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `arguments.result`".to_string())?,
    )?;
    let evidence_ref = parse_mcp_evidence_ref(&arguments)?;

    let mut storage = Storage::open(db_path).map_err(|e| format!("verify claim: {e}"))?;
    let target = verification_target_from_mcp(&arguments)?;
    let resolution = resolve_verification_targets(&storage, target, result)
        .map_err(|e| format!("verify claim: {e}"))?;
    let mut event_ids = Vec::new();
    for claim_id in &resolution.claim_ids {
        let event_id = storage
            .insert_verification_event(&VerificationEventDraft {
                claim_id: *claim_id,
                verifier_type,
                result,
                evidence_ref: evidence_ref.clone(),
            })
            .map_err(|e| format!("verify claim: {e}"))?;
        event_ids.push(event_id);
    }
    let mut claims = Vec::new();
    let mut events = Vec::new();
    for claim_id in &resolution.claim_ids {
        let claim = storage
            .claim_record(*claim_id)
            .map_err(|e| format!("verify claim: {e}"))?
            .ok_or_else(|| format!("claim {claim_id} disappeared after verification insert"))?;
        claims.push(claim);
        let mut claim_events = storage
            .verification_events_for_claim(*claim_id)
            .map_err(|e| format!("verify claim: {e}"))?;
        claim_events.retain(|event| event_ids.contains(&event.id));
        events.extend(claim_events);
    }
    let mut durable_promotion_trust = true;
    for claim_id in resolution.claim_ids.iter().chain(resolution.skipped_claim_ids.iter()) {
        durable_promotion_trust &= storage
            .claim_has_durable_promotion_trust(*claim_id)
            .map_err(|e| format!("verify claim: {e}"))?;
    }
    let event = events.first().cloned();
    let claim = claims.first().cloned();
    let target_type = resolution.target_type.clone();
    let target_id = resolution.target_id;
    let claim_ids = resolution.claim_ids.clone();
    let skipped_claim_ids = resolution.skipped_claim_ids.clone();
    let proposal = resolution.proposal.clone();

    let text = serde_json::to_string(&json!({
        "verification_event_id": event_ids.first().copied(),
        "verification_event_ids": event_ids.clone(),
        "claim_id": resolution.claim_ids.first().copied(),
        "claim_ids": claim_ids,
        "skipped_claim_ids": skipped_claim_ids,
        "verification_target": {
            "type": target_type.clone(),
            "id": target_id,
        },
        "durable_promotion_trust": durable_promotion_trust,
        "event": event,
        "events": events,
        "claim": claim,
        "claims": claims,
        "proposal": proposal,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "target_type": target_type,
            "target_id": target_id,
            "claim_ids": resolution.claim_ids,
            "verification_event_ids": event_ids,
            "durable_promotion_trust": durable_promotion_trust
        }
    }))
}

fn verification_target_from_mcp(arguments: &Value) -> Result<VerificationTargetInput, String> {
    match (
        arguments.get("claim_id").and_then(Value::as_i64),
        arguments.get("proposal_id").and_then(Value::as_i64),
    ) {
        (Some(claim_id), None) => Ok(VerificationTargetInput::Claim(claim_id)),
        (None, Some(proposal_id)) => Ok(VerificationTargetInput::Proposal(proposal_id)),
        (None, None) => {
            Err("one of `arguments.claim_id` or `arguments.proposal_id` is required".to_string())
        }
        (Some(_), Some(_)) => {
            Err("`arguments.claim_id` and `arguments.proposal_id` are mutually exclusive"
                .to_string())
        }
    }
}

fn review_target_from_mcp(arguments: &Value) -> Result<ReviewTarget, String> {
    match (
        arguments.get("claim_id").and_then(Value::as_i64),
        arguments.get("proposal_id").and_then(Value::as_i64),
    ) {
        (Some(claim_id), None) => Ok(ReviewTarget::Claim(claim_id)),
        (None, Some(proposal_id)) => Ok(ReviewTarget::Proposal(proposal_id)),
        (None, None) => {
            Err("one of `arguments.claim_id` or `arguments.proposal_id` is required".to_string())
        }
        (Some(_), Some(_)) => {
            Err("`arguments.claim_id` and `arguments.proposal_id` are mutually exclusive"
                .to_string())
        }
    }
}

fn tools_call_learning_proposals_list(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let status = match arguments.get("status").and_then(Value::as_str) {
        Some(value) => Some(parse_learning_critic_proposal_status(value)?),
        None => None,
    };

    let storage = Storage::open(db_path).map_err(|e| format!("list proposals: {e}"))?;
    let proposals = storage
        .learning_critic_proposals_scoped(project.as_deref(), session_id.as_deref(), status, limit)
        .map_err(|e| format!("list proposals: {e}"))?;
    let text = serde_json::to_string(&json!({
        "project": project,
        "session_id": session_id,
        "status": status.map(|status| status.as_str()),
        "limit": limit,
        "count": proposals.len(),
        "proposals": proposals,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "count": proposals.len()
        }
    }))
}

fn tools_call_learning_proposals_apply(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let proposal_id = arguments
        .get("proposal_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "missing `arguments.proposal_id`".to_string())?;
    let confirm_destructive =
        arguments.get("confirm_destructive").and_then(Value::as_bool).unwrap_or(false);
    let mut storage = Storage::open(db_path).map_err(|e| format!("apply proposal: {e}"))?;
    let outcome = storage
        .apply_learning_critic_proposal_with_options(
            proposal_id,
            LearningCriticApplyOptions { allow_destructive: confirm_destructive },
        )
        .map_err(|e| format!("apply proposal: {e}"))?;
    let proposal = storage
        .learning_critic_proposal(proposal_id)
        .map_err(|e| format!("apply proposal: {e}"))?
        .ok_or_else(|| format!("learning critic proposal {proposal_id} disappeared"))?;
    let text = serde_json::to_string(&json!({
        "proposal_id": proposal_id,
        "outcome": outcome,
        "proposal": proposal,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "proposal_id": proposal_id,
            "outcome": outcome,
            "confirm_destructive": confirm_destructive
        }
    }))
}

fn tools_call_learning_proposals_apply_ready(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let include_decay = arguments.get("include_decay").and_then(Value::as_bool).unwrap_or(false);
    let include_noop = arguments.get("include_noop").and_then(Value::as_bool).unwrap_or(false);
    let mut storage = Storage::open(db_path).map_err(|e| format!("apply ready proposals: {e}"))?;
    let report = apply_ready_learning_proposals(
        &mut storage,
        ApplyReadyInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            dry_run,
            include_decay,
            include_noop,
        },
    )
    .map_err(|e| format!("apply ready proposals: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "dry_run": dry_run,
            "considered_count": report.considered_count,
            "ready_count": report.ready_count,
            "applied_count": report.applied_count,
            "skipped_count": report.skipped_count
        }
    }))
}

fn tools_call_learning_proposals_set_status(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let proposal_id = arguments
        .get("proposal_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "missing `arguments.proposal_id`".to_string())?;
    let status = parse_learning_critic_proposal_status(
        arguments
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `arguments.status`".to_string())?,
    )?;
    if matches!(status, LearningCriticProposalStatus::Applied) {
        return Err("set_status cannot mark a proposal applied; use soma_learning_proposals_apply"
            .to_string());
    }
    let note = arguments
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_string);
    let result = json!({
        "review": "mcp_set_status",
        "note": note,
    });
    let mut storage = Storage::open(db_path).map_err(|e| format!("set proposal status: {e}"))?;
    storage
        .update_learning_critic_proposal_status(proposal_id, status, Some(&result))
        .map_err(|e| format!("set proposal status: {e}"))?;
    let proposal = storage
        .learning_critic_proposal(proposal_id)
        .map_err(|e| format!("set proposal status: {e}"))?
        .ok_or_else(|| format!("learning critic proposal {proposal_id} disappeared"))?;
    let text = serde_json::to_string(&json!({
        "proposal_id": proposal_id,
        "status": status.as_str(),
        "proposal": proposal,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "proposal_id": proposal_id,
            "status": status.as_str()
        }
    }))
}

fn tools_call_review_queue(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let storage = Storage::open(db_path).map_err(|e| format!("review queue: {e}"))?;
    let queue = build_review_queue(
        &storage,
        ReviewQueueInput { project: project.clone(), session_id: session_id.clone(), limit },
    )
    .map_err(|e| format!("review queue: {e}"))?;
    let text = serde_json::to_string(&queue).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "claim_count": queue.claim_count,
            "proposal_count": queue.proposal_count,
            "missing_verification_count": queue.missing_verification_count
        }
    }))
}

fn tools_call_review_actions(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let include_disabled =
        arguments.get("include_disabled").and_then(Value::as_bool).unwrap_or(false);
    let format = arguments.get("format").and_then(Value::as_str).unwrap_or("json");
    let storage = Storage::open(db_path).map_err(|e| format!("review actions: {e}"))?;
    let plan = build_review_action_plan(
        &storage,
        ReviewActionPlanInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            include_disabled,
        },
    )
    .map_err(|e| format!("review actions: {e}"))?;
    let text = match format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string(&plan).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => crate::context::review::render_review_action_plan_markdown(&plan),
        other => {
            return Err(format!("`arguments.format` must be `json` or `markdown`; got `{other}`"))
        }
    };

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "include_disabled": include_disabled,
            "format": format,
            "action_count": plan.action_count,
            "disabled_action_count": plan.disabled_action_count
        }
    }))
}

fn tools_call_review_batch_template(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let action = parse_review_batch_template_action_for_mcp(
        arguments.get("action").and_then(Value::as_str).unwrap_or("confirm"),
    )?;
    let target_type = parse_review_batch_template_target_type_for_mcp(
        arguments.get("target_type").and_then(Value::as_str).unwrap_or("any"),
    )?;
    let verifier_type = arguments
        .get("verifier_type")
        .or_else(|| arguments.get("verifier"))
        .and_then(Value::as_str)
        .map(parse_verifier_type_for_mcp)
        .transpose()?
        .map(|verifier| verifier.as_str().to_string());
    let storage = Storage::open(db_path).map_err(|e| format!("review batch template: {e}"))?;
    let template = build_review_batch_template(
        &storage,
        ReviewBatchTemplateInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            action,
            target_type: Some(target_type),
            verifier_type,
            evidence_kind: arguments
                .get("evidence_kind")
                .and_then(Value::as_str)
                .map(str::to_string),
            evidence_id: arguments.get("evidence_id").and_then(Value::as_str).map(str::to_string),
            evidence_source: arguments
                .get("evidence_source")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
    )
    .map_err(|e| format!("review batch template: {e}"))?;
    let text = serde_json::to_string(&template).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "operation_count": template.operation_count,
            "requires_evidence_fill": template.requires_evidence_fill
        }
    }))
}

fn tools_call_review_report(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let include_disabled =
        arguments.get("include_disabled").and_then(Value::as_bool).unwrap_or(false);
    let action = parse_review_batch_template_action_for_mcp(
        arguments.get("action").and_then(Value::as_str).unwrap_or("confirm"),
    )?;
    let target_type = parse_review_batch_template_target_type_for_mcp(
        arguments.get("target_type").and_then(Value::as_str).unwrap_or("any"),
    )?;
    let verifier_type = arguments
        .get("verifier_type")
        .or_else(|| arguments.get("verifier"))
        .and_then(Value::as_str)
        .map(parse_verifier_type_for_mcp)
        .transpose()?
        .map(|verifier| verifier.as_str().to_string());
    let format = arguments.get("format").and_then(Value::as_str).unwrap_or("markdown");
    let storage = Storage::open(db_path).map_err(|e| format!("review report: {e}"))?;
    let report = build_review_report(
        &storage,
        ReviewReportInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            include_disabled,
            action,
            target_type: Some(target_type),
            verifier_type,
            evidence_kind: arguments
                .get("evidence_kind")
                .and_then(Value::as_str)
                .map(str::to_string),
            evidence_id: arguments.get("evidence_id").and_then(Value::as_str).map(str::to_string),
            evidence_source: arguments
                .get("evidence_source")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
    )
    .map_err(|e| format!("review report: {e}"))?;
    let text = match format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => report.operator_markdown.clone(),
        other => {
            return Err(format!("`arguments.format` must be `json` or `markdown`; got `{other}`"))
        }
    };

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "include_disabled": include_disabled,
            "format": format,
            "claim_count": report.queue.claim_count,
            "proposal_count": report.queue.proposal_count,
            "action_count": report.action_plan.action_count,
            "operation_count": report.batch_template.operation_count
        }
    }))
}

fn tools_call_review_digest(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let client = arguments.get("client").and_then(Value::as_str).map(str::to_string);
    let include_queue_only =
        arguments.get("include_queue_only").and_then(Value::as_bool).unwrap_or(false);
    let format = arguments.get("format").and_then(Value::as_str).unwrap_or("json");
    let storage = Storage::open(db_path).map_err(|e| format!("review digest: {e}"))?;
    let digest = build_review_digest(
        &storage,
        ReviewDigestInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            client,
            include_queue_only,
        },
    )
    .map_err(|e| format!("review digest: {e}"))?;
    let text = match format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string(&digest).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => digest.operator_markdown.clone(),
        other => {
            return Err(format!("`arguments.format` must be `json` or `markdown`; got `{other}`"))
        }
    };

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "client": digest.client,
            "include_queue_only": include_queue_only,
            "format": format,
            "should_notify": digest.should_notify,
            "notification_count": digest.notification_count,
            "queue_only_count": digest.queue_only_count,
            "item_count": digest.item_count
        }
    }))
}

fn tools_call_review_digest_ack(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let client = arguments.get("client").and_then(Value::as_str).map(str::to_string);
    let batch_key = arguments.get("batch_key").and_then(Value::as_str).map(str::to_string);
    let cooldown_seconds = arguments.get("cooldown_seconds").and_then(Value::as_u64);
    let mut storage = Storage::open(db_path).map_err(|e| format!("review digest ack: {e}"))?;
    let report = acknowledge_review_digest(
        &mut storage,
        ReviewDigestAckInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            client,
            batch_key,
            cooldown_seconds,
        },
    )
    .map_err(|e| format!("review digest ack: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "client": report.client,
            "batch_key": report.batch_key,
            "ack_count": report.notification_state.ack_count,
            "pending_notification_count": report.pending_notification_count,
            "suppressed_by_cooldown_after_ack": report.digest_after_ack.suppressed_by_cooldown
        }
    }))
}

fn tools_call_review_render(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let client = arguments.get("client").and_then(Value::as_str).map(str::to_string);
    let include_disabled =
        arguments.get("include_disabled").and_then(Value::as_bool).unwrap_or(false);
    let format = arguments.get("format").and_then(Value::as_str).unwrap_or("json");
    let storage = Storage::open(db_path).map_err(|e| format!("review render: {e}"))?;
    let plan = build_review_render_plan(
        &storage,
        ReviewRenderInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            client,
            include_disabled,
        },
    )
    .map_err(|e| format!("review render: {e}"))?;
    let text = match format.trim().to_ascii_lowercase().as_str() {
        "json" => serde_json::to_string(&plan).unwrap_or_else(|_| "{}".to_string()),
        "markdown" | "md" => plan.operator_markdown.clone(),
        "html" => render_review_render_plan_html(&plan),
        other => {
            return Err(format!(
                "`arguments.format` must be `json`, `markdown`, or `html`; got `{other}`"
            ))
        }
    };

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "client": plan.client,
            "include_disabled": include_disabled,
            "format": format,
            "primary_surface": plan.primary_surface,
            "should_notify": plan.should_notify,
            "action_count": plan.action_count,
            "batch_operation_count": plan.batch_operation_count
        }
    }))
}

fn tools_call_client_binding_proofs(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let client = arguments
        .get("client")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let proof_id = match arguments.get("proof_id").and_then(Value::as_i64) {
        Some(value) if value <= 0 => {
            return Err("`arguments.proof_id` must be positive".to_string())
        }
        other => other,
    };
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(200),
        None => 20,
    };
    let storage = Storage::open(db_path).map_err(|e| format!("client binding proofs: {e}"))?;
    let proofs = if let Some(id) = proof_id {
        storage
            .client_binding_proof_by_id(id)
            .map_err(|e| format!("client binding proofs: {e}"))?
            .into_iter()
            .filter(|proof| client.as_deref().is_none_or(|filter| proof.client == filter))
            .collect::<Vec<_>>()
    } else {
        storage
            .recent_client_binding_proofs(client.as_deref(), limit)
            .map_err(|e| format!("client binding proofs: {e}"))?
    };
    let has_reference_binding = proofs
        .iter()
        .any(|proof| matches!(proof.proof_level, ClientBindingProofLevel::ReferenceBinding));
    let has_observed_event_file = proofs
        .iter()
        .any(|proof| matches!(proof.proof_level, ClientBindingProofLevel::ObservedEventFile));
    let has_observed_app_hook = proofs
        .iter()
        .any(|proof| matches!(proof.proof_level, ClientBindingProofLevel::ObservedAppHook));
    let has_observed_in_client_render = proofs
        .iter()
        .any(|proof| matches!(proof.proof_level, ClientBindingProofLevel::ObservedInClientRender));
    let latest = proofs.first();
    let status_report =
        build_client_binding_status_report(client.clone(), proof_id, limit, &proofs);
    let primary_readiness = status_report.clients.first().map(|status| status.readiness.clone());
    let primary_proof_stage =
        status_report.clients.first().map(|status| status.proof_stage.clone());
    let primary_ready_for_private_client_claim =
        status_report.clients.first().map(|status| status.ready_for_private_client_claim);
    let client_count = status_report.client_count;
    let all_latest_artifacts_verified = status_report.all_latest_artifacts_verified;
    let status_trust_boundary = status_report.trust_boundary.clone();
    let client_statuses = status_report.clients;
    let report = json!({
        "trust_boundary": "client_binding_proofs_read_only: inspects stored client binding proof rows only; records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation beyond existing ledger evidence",
        "status_trust_boundary": status_trust_boundary,
        "client": client,
        "proof_id": proof_id,
        "limit": limit,
        "proofs_found": proofs.len(),
        "client_count": client_count,
        "latest_proof_level": latest.map(|proof| proof.proof_level.as_str()),
        "latest_observed_at_ns": latest.map(|proof| proof.observed_at_ns),
        "has_reference_binding": has_reference_binding,
        "has_observed_event_file": has_observed_event_file,
        "has_observed_app_hook": has_observed_app_hook,
        "has_observed_in_client_render": has_observed_in_client_render,
        "primary_proof_stage": primary_proof_stage,
        "primary_readiness": primary_readiness,
        "primary_ready_for_private_client_claim": primary_ready_for_private_client_claim,
        "all_latest_artifacts_verified": all_latest_artifacts_verified,
        "client_statuses": client_statuses,
        "proofs": proofs,
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["client"].clone(),
            "proof_id": proof_id,
            "limit": limit,
            "proofs_found": report["proofs_found"].clone(),
            "client_count": report["client_count"].clone(),
            "primary_readiness": report["primary_readiness"].clone(),
            "primary_ready_for_private_client_claim": report["primary_ready_for_private_client_claim"].clone(),
            "all_latest_artifacts_verified": report["all_latest_artifacts_verified"].clone(),
            "has_observed_app_hook": has_observed_app_hook,
            "has_observed_in_client_render": has_observed_in_client_render
        }
    }))
}

fn tools_call_client_binding_record_proof(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let manifest = optional_trimmed_string_arg(&arguments, "manifest")
        .ok_or_else(|| "missing `arguments.manifest`".to_string())?;
    let proof_level = optional_trimmed_string_arg(&arguments, "proof_level")
        .ok_or_else(|| "missing `arguments.proof_level`".to_string())?;
    let client =
        optional_trimmed_string_arg(&arguments, "client").map(|value| value.to_ascii_lowercase());
    let evidence_source = optional_trimmed_string_arg(&arguments, "evidence_source")
        .unwrap_or_else(|| "mcp_client_binding_record_proof".to_string());
    let args = AdapterBindingProofArgs {
        manifest: Some(manifest),
        client,
        list: false,
        status: false,
        check_installed_config: false,
        discover_installed_config: false,
        real_app_proof_kit: false,
        evidence_bundle: false,
        proof_session: false,
        json: true,
        brief: false,
        format: "json".to_string(),
        prepare_installed_config: false,
        render_installed_config: false,
        write_installed_config: None,
        render_render_evidence: false,
        write_render_evidence: None,
        verify_evidence_artifacts: false,
        proof_id: None,
        limit: 20,
        proof_level,
        evidence_source,
        binding_nonce: None,
        config_root: None,
        artifact_dir: None,
        event_jsonl: optional_trimmed_string_arg(&arguments, "event_jsonl"),
        installed_config: optional_trimmed_string_arg(&arguments, "installed_config"),
        require_private_target_config_for_app_hook: false,
        render_evidence: optional_trimmed_string_arg(&arguments, "render_evidence"),
        review_action_report: optional_trimmed_string_arg(&arguments, "review_action_report"),
        drain_report: optional_trimmed_string_arg(&arguments, "drain_report"),
        review_render_report: optional_trimmed_string_arg(&arguments, "review_render_report"),
        operator_confirm_real_app_invocation: arguments
            .get("operator_confirm_real_app_invocation")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        operator_confirm_in_client_render: arguments
            .get("operator_confirm_in_client_render")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        operator_confirm_review_action: arguments
            .get("operator_confirm_review_action")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        operator_confirm_release_grade_evidence: arguments
            .get("operator_confirm_release_grade_evidence")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        db_path: None,
    };
    let ctx = AdapterBindingProofContext { db_path: db_path.to_path_buf() };
    let outcome = run_client_binding_proof_blocking(&args, &ctx)
        .map_err(|e| format!("client binding record proof: {e}"))?;
    let report = json!({
        "trust_boundary": "client_binding_record_proof_mcp_write: records exactly one client-binding proof row through adapter-binding-proof storage gates; creates no verification event, promotes no cloud draft, applies no proposal, and proves no private-client behavior beyond the supplied operator-confirmed evidence artifacts",
        "proof": outcome,
        "next_steps": [
            "rerun_soma_client_binding_proofs_or_status_to_replay_evidence_artifacts",
            "rerun_soma_client_binding_proof_session_until_release_gate_passes",
            "rerun_soma_product_hardening_report_before_claiming_client_readiness"
        ]
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["proof"]["client"].clone(),
            "proof_id": report["proof"]["proof_id"].clone(),
            "proof_level": report["proof"]["proof_level"].clone(),
            "evidence_source": report["proof"]["evidence_source"].clone(),
            "records_proof_row": true,
            "creates_verification_event": false,
            "promotes_cloud_draft": false,
            "applies_proposal": false
        }
    }))
}

fn tools_call_client_binding_install_plan(
    name: &str,
    params: Option<&Value>,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let client = optional_trimmed_string_arg(&arguments, "client")
        .map(|value| value.trim().to_ascii_lowercase());
    let manifest = optional_trimmed_string_arg(&arguments, "manifest");
    let binding_nonce = optional_trimmed_string_arg(&arguments, "binding_nonce");
    let installed_config = optional_trimmed_string_arg(&arguments, "installed_config");
    let config_root = optional_trimmed_string_arg(&arguments, "config_root");
    let include_discovery =
        arguments.get("include_discovery").and_then(Value::as_bool).unwrap_or(false);

    if client.is_none() && manifest.is_none() {
        return Err(
            "missing `arguments.client` or `arguments.manifest` for install plan".to_string()
        );
    }

    let prepare_args = client_binding_install_plan_args(
        manifest.clone(),
        client.clone(),
        binding_nonce,
        config_root.clone(),
        None,
        installed_config.clone(),
    );
    let prepare = run_prepare_installed_config_blocking(&prepare_args)
        .map_err(|e| format!("client binding install plan prepare: {e}"))?;
    let render_args = client_binding_install_plan_args(
        manifest.clone(),
        client.clone(),
        Some(prepare.binding_nonce.clone()),
        config_root.clone(),
        None,
        installed_config.clone(),
    );
    let rendered = run_render_installed_config_blocking(&render_args)
        .map_err(|e| format!("client binding install plan render: {e}"))?;
    let installed_config_check = if installed_config.is_some() {
        Some(
            run_installed_config_check_blocking(&render_args)
                .map_err(|e| format!("client binding install plan check: {e}"))?,
        )
    } else {
        None
    };
    let discovery = if include_discovery {
        Some(
            run_discover_installed_config_blocking(&render_args)
                .map_err(|e| format!("client binding install plan discover: {e}"))?,
        )
    } else {
        None
    };
    let check_eligible =
        installed_config_check.as_ref().map(|check| check.eligible_for_observed_app_hook);
    let discovery_candidates_found =
        discovery.as_ref().map(|report| report.candidates_found).unwrap_or(0);
    let discovery_eligible_candidates =
        discovery.as_ref().map(|report| report.eligible_candidates).unwrap_or(0);
    let report_client = rendered.client.clone();
    let report_manifest_path = rendered.manifest_path.clone();
    let report_event_source = rendered.event_source.clone();
    let report_binding_nonce = rendered.binding_nonce.clone();
    let report = json!({
        "trust_boundary": "client_binding_install_plan_read_only: renders a proof-free installed client binding config artifact and optional local preflight scans; writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation",
        "client": report_client,
        "manifest_path": report_manifest_path,
        "event_source": report_event_source,
        "binding_nonce": report_binding_nonce,
        "generated_binding_nonce": prepare.generated_binding_nonce,
        "prepare": prepare,
        "render": rendered,
        "installed_config_check": installed_config_check,
        "discovery": discovery,
        "next_steps": [
            "install_or_merge_rendered_config_artifact_into_private_client_config",
            "run_soma_client_binding_install_plan_or_adapter_binding_proof_check_installed_config",
            "record_observed_app_hook_only_after_matching_private_event_source_binding_nonce_writer_metadata_temporal_binding_and_operator_confirmation",
            "record_observed_in_client_render_separately_with_structured_render_evidence_bound_to_review_render_fingerprint"
        ]
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["client"].clone(),
            "event_source": report["event_source"].clone(),
            "binding_nonce": report["binding_nonce"].clone(),
            "generated_binding_nonce": report["generated_binding_nonce"].clone(),
            "has_installed_config_check": report["installed_config_check"].is_object(),
            "installed_config_eligible_for_observed_app_hook": check_eligible,
            "discovery_candidates_found": discovery_candidates_found,
            "discovery_eligible_candidates": discovery_eligible_candidates
        }
    }))
}

fn tools_call_client_binding_evidence_bundle(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let client = optional_trimmed_string_arg(&arguments, "client")
        .map(|value| value.trim().to_ascii_lowercase());
    let manifest = optional_trimmed_string_arg(&arguments, "manifest");
    let binding_nonce = optional_trimmed_string_arg(&arguments, "binding_nonce");
    let installed_config = optional_trimmed_string_arg(&arguments, "installed_config");
    let config_root = optional_trimmed_string_arg(&arguments, "config_root");
    let artifact_dir = optional_trimmed_string_arg(&arguments, "artifact_dir");
    let event_jsonl = optional_trimmed_string_arg(&arguments, "event_jsonl");
    let review_render_report = optional_trimmed_string_arg(&arguments, "review_render_report");
    let render_evidence = optional_trimmed_string_arg(&arguments, "render_evidence");
    let review_action_report = optional_trimmed_string_arg(&arguments, "review_action_report");

    if client.is_none() && manifest.is_none() {
        return Err(
            "missing `arguments.client` or `arguments.manifest` for evidence bundle".to_string()
        );
    }

    let mut args = client_binding_install_plan_args(
        manifest,
        client,
        binding_nonce,
        config_root,
        artifact_dir,
        installed_config,
    );
    args.evidence_bundle = true;
    args.render_installed_config = false;
    args.evidence_source = "mcp_client_binding_evidence_bundle".to_string();
    args.event_jsonl = event_jsonl;
    args.review_render_report = review_render_report;
    args.render_evidence = render_evidence;
    args.review_action_report = review_action_report;
    args.db_path = Some(db_path.to_string_lossy().into_owned());
    let ctx = AdapterBindingProofContext { db_path: db_path.to_path_buf() };
    let bundle = run_evidence_bundle_blocking(&args, &ctx)
        .map_err(|e| format!("client binding evidence bundle: {e}"))?;
    let readiness = bundle.readiness.clone();
    let proof_count = readiness.proofs_found;
    let client_count = readiness.client_count;
    let primary_readiness = readiness.clients.first().map(|status| status.readiness.clone());
    let primary_ready_for_private_client_claim =
        readiness.clients.first().map(|status| status.ready_for_private_client_claim);
    let installed_config_eligible = bundle.installed_config_discovery.eligible_candidates;
    let blocking_gap_count = bundle.blocking_gaps.len();
    let operator_action_count =
        bundle.operator_flow.iter().filter(|step| step.requires_operator_action).count();
    let proof_recording_step_count =
        bundle.operator_flow.iter().filter(|step| step.records_proof).count();
    let proof_session_status = bundle.proof_session.status.clone();
    let proof_session_release_gate = bundle.proof_session.release_gate.clone();
    let proof_session_next_step_id = bundle.proof_session.next_step_id.clone();
    let proof_session_next_mcp_tool =
        bundle.proof_session.next_mcp_call.as_ref().map(|call| call.tool.clone());
    let report = json!({
        "trust_boundary": "client_binding_evidence_bundle_mcp_read_only: composes readiness, installed-config discovery, proof-free config preview, real-app proof-kit guidance, operator flow, and blocking gaps; writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation, rendering, or review-action execution",
        "bundle": bundle,
        "next_steps": [
            "install_or_merge_rendered_config_artifact_into_private_client_config",
            "trigger_real_private_client_hook_and_drain_adapter_spool",
            "record_observed_app_hook_only_after_private_event_source_binding_nonce_writer_metadata_temporal_binding_and_operator_confirmation",
            "render_review_surface_in_private_client_and_capture_structured_render_evidence",
            "record_observed_in_client_render_only_after_structured_ui_evidence_and_operator_confirmation",
            "execute_rendered_review_control_in_private_client_and_save_soma_review_action_report",
            "record_observed_review_action_only_after_rendered_control_id_storage_gated_non_cloud_verification_and_operator_confirmation"
        ]
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["bundle"]["client"].clone(),
            "event_source": report["bundle"]["expected_event_source"].clone(),
            "binding_nonce": report["bundle"]["binding_nonce"].clone(),
            "generated_binding_nonce": report["bundle"]["generated_binding_nonce"].clone(),
            "readiness_proofs_found": proof_count,
            "readiness_client_count": client_count,
            "primary_readiness": primary_readiness,
            "primary_ready_for_private_client_claim": primary_ready_for_private_client_claim,
            "installed_config_eligible_candidates": installed_config_eligible,
            "blocking_gap_count": blocking_gap_count,
            "operator_action_count": operator_action_count,
            "proof_recording_step_count": proof_recording_step_count,
            "proof_session_status": proof_session_status,
            "proof_session_release_gate": proof_session_release_gate,
            "proof_session_next_step_id": proof_session_next_step_id,
            "proof_session_next_mcp_tool": proof_session_next_mcp_tool
        }
    }))
}

fn tools_call_client_binding_proof_session(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let client = optional_trimmed_string_arg(&arguments, "client")
        .map(|value| value.trim().to_ascii_lowercase());
    let manifest = optional_trimmed_string_arg(&arguments, "manifest");
    let binding_nonce = optional_trimmed_string_arg(&arguments, "binding_nonce");
    let installed_config = optional_trimmed_string_arg(&arguments, "installed_config");
    let config_root = optional_trimmed_string_arg(&arguments, "config_root");
    let artifact_dir = optional_trimmed_string_arg(&arguments, "artifact_dir");
    let event_jsonl = optional_trimmed_string_arg(&arguments, "event_jsonl");
    let review_render_report = optional_trimmed_string_arg(&arguments, "review_render_report");
    let render_evidence = optional_trimmed_string_arg(&arguments, "render_evidence");
    let review_action_report = optional_trimmed_string_arg(&arguments, "review_action_report");

    if client.is_none() && manifest.is_none() {
        return Err(
            "missing `arguments.client` or `arguments.manifest` for proof session".to_string()
        );
    }

    let mut args = client_binding_install_plan_args(
        manifest,
        client,
        binding_nonce,
        config_root,
        artifact_dir,
        installed_config,
    );
    args.proof_session = true;
    args.render_installed_config = false;
    args.evidence_source = "mcp_client_binding_proof_session".to_string();
    args.event_jsonl = event_jsonl;
    args.review_render_report = review_render_report;
    args.render_evidence = render_evidence;
    args.review_action_report = review_action_report;
    args.db_path = Some(db_path.to_string_lossy().into_owned());
    let ctx = AdapterBindingProofContext { db_path: db_path.to_path_buf() };
    let outcome = run_proof_session_blocking(&args, &ctx)
        .map_err(|e| format!("client binding proof session: {e}"))?;
    let proof_session_status = outcome.proof_session.status.clone();
    let proof_session_release_gate = outcome.proof_session.release_gate.clone();
    let proof_session_next_step_id = outcome.proof_session.next_step_id.clone();
    let proof_session_next_operator_step_id =
        outcome.proof_session.next_operator_step.as_ref().map(|step| step.id.clone());
    let proof_session_next_mcp_tool =
        outcome.proof_session.next_mcp_call.as_ref().map(|call| call.tool.clone());
    let ready_for_private_client_claim = outcome.proof_session.ready_for_private_client_claim;
    let operator_next_action_id = outcome.operator_next_action_id.clone();
    let operator_next_action_label = outcome.operator_next_action_label.clone();
    let blocking_gap_count = outcome.blocking_gaps.len();
    let operator_action_count =
        outcome.operator_flow.iter().filter(|step| step.requires_operator_action).count();
    let proof_recording_step_count =
        outcome.operator_flow.iter().filter(|step| step.records_proof).count();
    let report = json!({
        "trust_boundary": "client_binding_proof_session_mcp_read_only: composes stored proof readiness and currently supplied artifact readiness through the evidence bundle contract; writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove private app installation, rendering, or review-action execution",
        "proof_session": outcome,
        "next_steps": [
            "follow_proof_session_next_operator_step",
            "record_proof_only_after_real_private_client_observation_and_operator_confirmation",
            "rerun_soma_client_binding_proof_session_until_release_gate_passes",
            "rerun_soma_product_hardening_report_require_client_binding_ready_before_release"
        ]
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["proof_session"]["client"].clone(),
            "event_source": report["proof_session"]["expected_event_source"].clone(),
            "binding_nonce": report["proof_session"]["binding_nonce"].clone(),
            "generated_binding_nonce": report["proof_session"]["generated_binding_nonce"].clone(),
            "proof_session_status": proof_session_status,
            "proof_session_release_gate": proof_session_release_gate,
            "proof_session_next_step_id": proof_session_next_step_id,
            "proof_session_next_operator_step_id": proof_session_next_operator_step_id,
            "operator_next_action_id": operator_next_action_id,
            "operator_next_action_label": operator_next_action_label,
            "proof_session_next_mcp_tool": proof_session_next_mcp_tool,
            "ready_for_private_client_claim": ready_for_private_client_claim,
            "blocking_gap_count": blocking_gap_count,
            "operator_action_count": operator_action_count,
            "proof_recording_step_count": proof_recording_step_count
        }
    }))
}

fn tools_call_client_render_evidence_packet(
    name: &str,
    params: Option<&Value>,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let client = optional_trimmed_string_arg(&arguments, "client");
    let manifest = optional_trimmed_string_arg(&arguments, "manifest");
    let review_render_report = optional_trimmed_string_arg(&arguments, "review_render_report")
        .ok_or_else(|| {
            "missing `arguments.review_render_report` for render evidence packet".to_string()
        })?;
    if client.is_none() && manifest.is_none() {
        return Err(
            "missing `arguments.client` or `arguments.manifest` for render evidence packet"
                .to_string(),
        );
    }

    let mut args = client_binding_install_plan_args(manifest, client, None, None, None, None);
    args.render_installed_config = false;
    args.render_render_evidence = true;
    args.review_render_report = Some(review_render_report);
    args.evidence_source = "mcp_client_render_evidence_packet".to_string();

    let packet = run_render_evidence_packet_blocking(&args)
        .map_err(|e| format!("client render evidence packet: {e}"))?;
    let rendered_control_id_count = packet
        .render_evidence
        .get("rendered_control_ids")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let report = json!({
        "trust_boundary": "client_render_evidence_packet_mcp_read_only: materializes a proof-free soma.in_client_render_evidence.v1 packet from a saved review-render report; writes no files, records no proof row, creates no verification event, promotes no cloud draft, applies no proposal, and does not prove in-client rendering until visible observations are filled and observed_in_client_render is recorded with operator confirmation",
        "packet": packet,
        "next_steps": [
            "render_the_review_surface_visibly_inside_the_private_client",
            "fill_source_observed_at_ns_and_rendered_surfaces_after_visible_client_render",
            "record_observed_in_client_render_only_with_structured_ui_evidence_and_operator_confirmation",
            "replay_evidence_artifacts_after_storage"
        ]
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "client": report["packet"]["client"].clone(),
            "review_render_report_path": report["packet"]["review_render_report_path"].clone(),
            "wrote_file": report["packet"]["wrote_file"].clone(),
            "packet_schema": report["packet"]["render_evidence"]["schema"].clone(),
            "placeholder_requirement_count": report["packet"]["missing_requirements"].as_array().map(Vec::len).unwrap_or(0),
            "rendered_control_id_count": rendered_control_id_count
        }
    }))
}

fn optional_trimmed_string_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn client_binding_install_plan_args(
    manifest: Option<String>,
    client: Option<String>,
    binding_nonce: Option<String>,
    config_root: Option<String>,
    artifact_dir: Option<String>,
    installed_config: Option<String>,
) -> AdapterBindingProofArgs {
    AdapterBindingProofArgs {
        manifest,
        client,
        list: false,
        status: false,
        check_installed_config: false,
        discover_installed_config: false,
        real_app_proof_kit: false,
        evidence_bundle: false,
        proof_session: false,
        json: true,
        brief: false,
        format: "json".to_string(),
        prepare_installed_config: false,
        render_installed_config: true,
        write_installed_config: None,
        render_render_evidence: false,
        write_render_evidence: None,
        verify_evidence_artifacts: false,
        proof_id: None,
        limit: 20,
        proof_level: "observed_event_file".to_string(),
        evidence_source: "mcp_client_binding_install_plan".to_string(),
        binding_nonce,
        config_root,
        artifact_dir,
        event_jsonl: None,
        installed_config,
        require_private_target_config_for_app_hook: false,
        render_evidence: None,
        review_action_report: None,
        drain_report: None,
        review_render_report: None,
        operator_confirm_real_app_invocation: false,
        operator_confirm_in_client_render: false,
        operator_confirm_review_action: false,
        operator_confirm_release_grade_evidence: false,
        db_path: None,
    }
}

fn tools_call_latent_predict(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing non-empty `arguments.query`".to_string())?
        .to_string();
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(100),
        None => DEFAULT_LATENT_PREDICTOR_LIMIT,
    };
    let scan_limit = match arguments.get("scan_limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.scan_limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(1000),
        None => DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
    };
    if scan_limit < limit {
        return Err("`arguments.scan_limit` must be greater than or equal to `limit`".to_string());
    }
    let min_confidence = match arguments.get("min_confidence").and_then(Value::as_f64) {
        Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => value as f32,
        Some(_) => return Err("`arguments.min_confidence` must be finite within [0,1]".to_string()),
        None => DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE,
    };
    let storage = Storage::open(db_path).map_err(|e| format!("latent predict: {e}"))?;
    let report = predict_latent_proxies(
        &storage,
        LatentProxyPredictionInput {
            query,
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            scan_limit,
            min_confidence,
        },
    )
    .map_err(|e| format!("latent predict: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "rule": report.rule,
            "mode": report.mode,
            "predicted_count": report.predicted_count,
            "deterministic_baseline_count": report.deterministic_baseline_count,
            "fallback_to_deterministic_projection": report.fallback_to_deterministic_projection,
            "skipped_untrusted_cloud_draft_count": report.skipped_untrusted_cloud_draft_count
        }
    }))
}

fn tools_call_latent_packet(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing non-empty `arguments.query`".to_string())?
        .to_string();
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(100),
        None => DEFAULT_LATENT_PREDICTOR_LIMIT,
    };
    let scan_limit = match arguments.get("scan_limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.scan_limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(1000),
        None => DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
    };
    if scan_limit < limit {
        return Err("`arguments.scan_limit` must be greater than or equal to `limit`".to_string());
    }
    let min_confidence = match arguments.get("min_confidence").and_then(Value::as_f64) {
        Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => value as f32,
        Some(_) => return Err("`arguments.min_confidence` must be finite within [0,1]".to_string()),
        None => DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE,
    };
    let storage = Storage::open(db_path).map_err(|e| format!("latent packet: {e}"))?;
    let packet = render_latent_interface_packet(
        &storage,
        LatentInterfacePacketInput {
            query,
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            scan_limit,
            min_confidence,
        },
    )
    .map_err(|e| format!("latent packet: {e}"))?;
    let text = serde_json::to_string(&packet).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "schema": packet.schema,
            "mode": packet.mode,
            "proxy_binding_count": packet.proxy_binding_count,
            "vector_payload_included": packet.latent_channel.vector_payload_included,
            "hidden_state_injection_supported": packet.latent_channel.hidden_state_injection_supported,
            "fallback_to_deterministic_projection": packet.prediction_report.fallback_to_deterministic_projection,
            "skipped_untrusted_cloud_draft_count": packet.prediction_report.skipped_untrusted_cloud_draft_count
        }
    }))
}

fn tools_call_latent_eval(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(100),
        None => DEFAULT_LATENT_PREDICTOR_LIMIT,
    };
    let scan_limit = match arguments.get("scan_limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.scan_limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(1000),
        None => DEFAULT_LATENT_PREDICTOR_SCAN_LIMIT,
    };
    if scan_limit < limit {
        return Err("`arguments.scan_limit` must be greater than or equal to `limit`".to_string());
    }
    let case_limit = match arguments.get("case_limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.case_limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(1000),
        None => DEFAULT_LATENT_EVAL_CASE_LIMIT,
    };
    let min_confidence = match arguments.get("min_confidence").and_then(Value::as_f64) {
        Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => value as f32,
        Some(_) => return Err("`arguments.min_confidence` must be finite within [0,1]".to_string()),
        None => DEFAULT_LATENT_PREDICTOR_MIN_CONFIDENCE,
    };
    let storage = Storage::open(db_path).map_err(|e| format!("latent eval: {e}"))?;
    let (cases, case_source) = if let Some(cases_value) = arguments.get("cases") {
        let cases: Vec<LatentProxyEvalCase> = serde_json::from_value(cases_value.clone())
            .map_err(|e| format!("latent eval: invalid `arguments.cases`: {e}"))?;
        (cases, "inline_cases".to_string())
    } else if arguments.get("case_source").and_then(Value::as_str) == Some("task_frame_outcomes") {
        (
            build_task_frame_outcome_latent_eval_cases(
                &storage,
                project.as_deref(),
                session_id.as_deref(),
                case_limit,
            )
            .map_err(|e| format!("latent eval: {e}"))?,
            "task_frame_outcome".to_string(),
        )
    } else {
        (
            build_storage_latent_eval_cases(
                &storage,
                project.as_deref(),
                session_id.as_deref(),
                scan_limit,
                case_limit,
            )
            .map_err(|e| format!("latent eval: {e}"))?,
            "storage_active_prediction_eligible_proxy".to_string(),
        )
    };
    let report = evaluate_latent_predictor(
        &storage,
        LatentProxyEvalInput {
            cases,
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            scan_limit,
            min_confidence,
            case_source,
        },
    )
    .map_err(|e| format!("latent eval: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "case_count": report.case_count,
            "prediction_hit_rate": report.prediction_hit_rate,
            "deterministic_baseline_hit_rate": report.deterministic_baseline_hit_rate,
            "fallback_count": report.fallback_count,
            "cloud_draft_prediction_count": report.cloud_draft_prediction_count
        }
    }))
}

fn tools_call_task_frame_outcome(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let task_frame_id = arguments
        .get("task_frame_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| "missing integer `arguments.task_frame_id`".to_string())?;
    let outcome_type = arguments
        .get("outcome_type")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string `arguments.outcome_type`".to_string())
        .and_then(|value| TaskFrameOutcomeType::parse(value).map_err(|e| e.to_string()))?;
    let summary = arguments
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing non-empty `arguments.summary`".to_string())?
        .to_string();
    let evidence_kind = arguments
        .get("evidence_kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing non-empty `arguments.evidence_kind`".to_string())?
        .to_string();
    let evidence_id = arguments
        .get("evidence_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "missing non-empty `arguments.evidence_id`".to_string())?
        .to_string();
    let evidence_source =
        arguments.get("evidence_source").and_then(Value::as_str).map(str::to_string);
    let claim_ids = parse_i64_array(arguments.get("claim_ids"), "claim_ids")?;
    let proposal_ids = parse_i64_array(arguments.get("proposal_ids"), "proposal_ids")?;
    let latent_proxy_ids = parse_i64_array(arguments.get("latent_proxy_ids"), "latent_proxy_ids")?;
    let mut storage = Storage::open(db_path).map_err(|e| format!("task frame outcome: {e}"))?;
    let outcome_id = storage
        .insert_task_frame_outcome(&TaskFrameOutcomeDraft {
            task_frame_id,
            outcome_type,
            summary,
            evidence_refs: vec![StoredEvidenceRef {
                kind: evidence_kind,
                id: evidence_id,
                source: evidence_source,
            }],
            claim_ids,
            proposal_ids,
            latent_proxy_ids,
        })
        .map_err(|e| format!("task frame outcome: {e}"))?;
    let outcome = storage
        .task_frame_outcomes_scoped(None, None, Some(task_frame_id), 100)
        .map_err(|e| format!("task frame outcome: {e}"))?
        .into_iter()
        .find(|outcome| outcome.id == outcome_id)
        .ok_or_else(|| format!("task frame outcome: inserted outcome {outcome_id} not readable"))?;
    let report = json!({
        "kind": "task_frame_outcome",
        "trust_boundary": "TaskFrame outcome records evaluation evidence only; it creates no claim, verification event, proposal, lifecycle transition, semantic fact, or ContextEnvelope mutation",
        "outcome": outcome,
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "task_frame_id": task_frame_id,
            "outcome_id": outcome_id,
            "outcome_type": outcome.outcome_type.as_str(),
            "latent_proxy_count": outcome.latent_proxy_ids.len()
        }
    }))
}

fn tools_call_task_frame_outcomes(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => (value as usize).min(1000),
        None => 20,
    };
    let storage = Storage::open(db_path).map_err(|e| format!("task frame outcomes: {e}"))?;
    let outcomes = storage
        .task_frame_outcomes_scoped(project.as_deref(), session_id.as_deref(), task_frame_id, limit)
        .map_err(|e| format!("task frame outcomes: {e}"))?;
    let report = json!({
        "kind": "task_frame_outcomes",
        "mode": "read_only",
        "trust_boundary": "TaskFrame outcome listing is read-only and records no verification, promotion, proposal, lifecycle, semantic, or ContextEnvelope mutation",
        "project": project,
        "session_id": session_id,
        "task_frame_id": task_frame_id,
        "limit": limit,
        "outcome_count": outcomes.len(),
        "outcomes": outcomes,
    });
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "outcome_count": report["outcome_count"].clone()
        }
    }))
}

fn parse_i64_array(value: Option<&Value>, field: &str) -> Result<Vec<i64>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(format!("`arguments.{field}` must be an array of integers"));
    };
    let mut out = Vec::new();
    for item in items {
        let id = item
            .as_i64()
            .ok_or_else(|| format!("`arguments.{field}` must be an array of integers"))?;
        out.push(id);
    }
    Ok(out)
}

fn parse_review_batch_template_action_for_mcp(input: &str) -> Result<String, String> {
    let action = input.trim().to_ascii_lowercase();
    match action.as_str() {
        "confirm" | "contradict" | "supersede" | "inconclusive" => Ok(action),
        other => Err(format!(
            "`arguments.action` must be confirm, contradict, supersede, or inconclusive; got `{other}`"
        )),
    }
}

fn parse_review_batch_template_target_type_for_mcp(input: &str) -> Result<String, String> {
    let target_type = input.trim().to_ascii_lowercase();
    match target_type.as_str() {
        "any" | "claim" | "proposal" => Ok(target_type),
        other => {
            Err(format!("`arguments.target_type` must be any, claim, or proposal; got `{other}`"))
        }
    }
}

fn tools_call_review_drain(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let mut storage = Storage::open(db_path).map_err(|e| format!("review drain: {e}"))?;
    let report = drain_review_queue(
        &mut storage,
        ReviewDrainInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            dry_run,
        },
    )
    .map_err(|e| format!("review drain: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "policy": report.policy.clone(),
            "dry_run": dry_run,
            "auto_applied_count": report.auto_applied_count,
            "auto_skipped_count": report.auto_skipped_count,
            "manual_action_count_after": report.manual_action_count_after
        }
    }))
}

fn tools_call_scheduler_run(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 32,
    };
    let semantic_min_support = match arguments.get("semantic_min_support").and_then(Value::as_u64) {
        Some(0 | 1) => {
            return Err("`arguments.semantic_min_support` must be at least 2".to_string())
        }
        Some(value) => value as usize,
        None => 2,
    };
    let l2_promotion_min_confidence =
        match arguments.get("l2_promotion_min_confidence").and_then(Value::as_f64) {
            Some(value) if !value.is_finite() || !(0.0..=1.0).contains(&value) => {
                return Err("`arguments.l2_promotion_min_confidence` must be finite within [0,1]"
                    .to_string())
            }
            Some(value) => value as f32,
            None => DEFAULT_L2_PROMOTION_MIN_CONFIDENCE,
        };
    let l2_promotion_anomaly_min_confidence =
        match arguments.get("l2_promotion_anomaly_min_confidence").and_then(Value::as_f64) {
            Some(value) if !value.is_finite() || !(0.0..=1.0).contains(&value) => {
                return Err(
                    "`arguments.l2_promotion_anomaly_min_confidence` must be finite within [0,1]"
                        .to_string(),
                )
            }
            Some(value) => value as f32,
            None => DEFAULT_L2_PROMOTION_ANOMALY_MIN_CONFIDENCE,
        };
    let l2_promotion_min_repeated_support =
        match arguments.get("l2_promotion_min_repeated_support").and_then(Value::as_u64) {
            Some(0 | 1) => {
                return Err(
                    "`arguments.l2_promotion_min_repeated_support` must be at least 2".to_string()
                )
            }
            Some(value) => value as usize,
            None => DEFAULT_L2_PROMOTION_MIN_REPEATED_SUPPORT,
        };
    let l2_promotion_reason = arguments
        .get("l2_promotion_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(DEFAULT_L2_PROMOTION_REASON)
        .to_string();
    let l3_decay_older_than_days =
        match arguments.get("l3_decay_older_than_days").and_then(Value::as_i64) {
            Some(value) if value < 1 => {
                return Err("`arguments.l3_decay_older_than_days` must be at least 1".to_string())
            }
            Some(value) => value,
            None => DEFAULT_L3_DECAY_OLDER_THAN_DAYS,
        };
    let l3_decay_cutoff_ns = match arguments.get("l3_decay_cutoff_ns").and_then(Value::as_i64) {
        Some(value) if value <= 0 => {
            return Err("`arguments.l3_decay_cutoff_ns` must be positive".to_string())
        }
        Some(value) => value,
        None => task_frame_retention_cutoff_ns(now_ns(), l3_decay_older_than_days)
            .map_err(|e| format!("scheduler run L3 decay cutoff: {e}"))?,
    };
    let l3_decay_max_access_count =
        match arguments.get("l3_decay_max_access_count").and_then(Value::as_i64) {
            Some(value) if value < 0 => {
                return Err("`arguments.l3_decay_max_access_count` must be non-negative".to_string())
            }
            Some(value) => value,
            None => DEFAULT_L3_DECAY_MAX_ACCESS_COUNT,
        };
    let l3_decay_reason = arguments
        .get("l3_decay_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(DEFAULT_L3_DECAY_REASON)
        .to_string();
    let task_frame_retention_days =
        match arguments.get("task_frame_retention_days").and_then(Value::as_i64) {
            Some(value) if value < 1 => {
                return Err("`arguments.task_frame_retention_days` must be at least 1".to_string())
            }
            Some(value) => value,
            None => crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS,
        };
    let task_frame_retention_cutoff_ns = match arguments
        .get("task_frame_retention_cutoff_ns")
        .and_then(Value::as_i64)
    {
        Some(value) if value <= 0 => {
            return Err("`arguments.task_frame_retention_cutoff_ns` must be positive".to_string())
        }
        Some(value) => value,
        None => task_frame_retention_cutoff_ns(now_ns(), task_frame_retention_days)
            .map_err(|e| format!("scheduler run TaskFrame retention cutoff: {e}"))?,
    };
    let task_frame_retention_reason = arguments
        .get("task_frame_retention_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(DEFAULT_TASK_FRAME_RETENTION_REASON)
        .to_string();
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let raw_passes = scheduler_passes_from_mcp_arguments(&arguments)?;
    let passes = normalize_scheduler_control_passes(&raw_passes)?;
    let mut storage = Storage::open(db_path).map_err(|e| format!("scheduler run: {e}"))?;
    let report = run_scheduler_control(
        &mut storage,
        SchedulerControlInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            semantic_min_support,
            l2_promotion_min_confidence,
            l2_promotion_anomaly_min_confidence,
            l2_promotion_min_repeated_support,
            l2_promotion_reason,
            l3_decay_cutoff_ns,
            l3_decay_max_access_count,
            l3_decay_reason,
            task_frame_retention_cutoff_ns,
            task_frame_retention_days,
            task_frame_retention_reason,
            dry_run,
            passes,
        },
    )
    .map_err(|e| format!("scheduler run: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "dry_run": dry_run,
            "pass_count": report.pass_count
        }
    }))
}

fn scheduler_passes_from_mcp_arguments(arguments: &Value) -> Result<Vec<String>, String> {
    let Some(raw) = arguments.get("passes").or_else(|| arguments.get("pass")) else {
        return Ok(Vec::new());
    };
    if let Some(pass) = raw.as_str() {
        return Ok(pass
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect());
    }
    if let Some(items) = raw.as_array() {
        let mut passes = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let Some(pass) = item.as_str() else {
                return Err(format!("`arguments.passes[{index}]` must be a string"));
            };
            passes.push(pass.to_string());
        }
        return Ok(passes);
    }
    Err("`arguments.passes` must be a string or array of strings".to_string())
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn tools_call_semantic_proposals(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 100,
    };
    let min_support = match arguments.get("min_support").and_then(Value::as_u64) {
        Some(value) if value < 2 => {
            return Err("`arguments.min_support` must be at least 2".to_string())
        }
        Some(value) => value as usize,
        None => 2,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let mut storage = Storage::open(db_path).map_err(|e| format!("semantic proposals: {e}"))?;
    let report = propose_semantic_consolidations(
        &mut storage,
        SemanticLearningInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            min_support,
            dry_run,
        },
    )
    .map_err(|e| format!("semantic proposals: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "rule": report.rule.clone(),
            "dry_run": dry_run,
            "repeated_group_count": report.repeated_group_count,
            "proposed_count": report.proposed_count
        }
    }))
}

fn tools_call_open_decision_proposals(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 20,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let mut storage =
        Storage::open(db_path).map_err(|e| format!("open decision proposals: {e}"))?;
    let report = propose_open_decision_reviews(
        &mut storage,
        OpenDecisionProposalInput {
            project: project.clone(),
            session_id: session_id.clone(),
            limit,
            dry_run,
        },
    )
    .map_err(|e| format!("open decision proposals: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "rule": report.rule.clone(),
            "dry_run": dry_run,
            "inspected_signal_count": report.inspected_signal_count,
            "proposed_count": report.proposed_count,
            "skipped_existing_proposal_count": report.skipped_existing_proposal_count
        }
    }))
}

fn tools_call_review_action(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let target = review_target_from_mcp(&arguments)?;
    let action = parse_review_action_for_mcp(
        arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing `arguments.action`".to_string())?,
    )?;
    let verifier_type = match arguments
        .get("verifier_type")
        .or_else(|| arguments.get("verifier"))
        .and_then(Value::as_str)
    {
        Some(value) => Some(parse_verifier_type_for_mcp(value)?),
        None => Some(VerifierType::User),
    };
    let evidence_ref = optional_mcp_evidence_ref(&arguments)?;
    let input = ReviewActionInput {
        target,
        action,
        control_id: arguments.get("control_id").and_then(Value::as_str).map(str::to_string),
        verifier_type,
        evidence_ref,
        note: arguments.get("note").and_then(Value::as_str).map(str::to_string),
        confirm_destructive: arguments
            .get("confirm_destructive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let mut storage = Storage::open(db_path).map_err(|e| format!("review action: {e}"))?;
    let report =
        apply_review_action(&mut storage, input).map_err(|e| format!("review action: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "target_type": report.target_type,
            "target_id": report.target_id,
            "action": report.action,
            "control_id": report.control_id,
            "control_binding_verified": report.control_binding_verified,
            "claim_ids": report.claim_ids,
            "verification_event_ids": report.verification_event_ids,
            "apply_outcome": report.apply_outcome
        }
    }))
}

fn tools_call_review_batch(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let operations = parse_review_batch_operations_for_mcp(
        arguments.get("operations").ok_or_else(|| "missing `arguments.operations`".to_string())?,
    )?;
    let dry_run = arguments.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let mut storage = Storage::open(db_path).map_err(|e| format!("review batch: {e}"))?;
    let report = apply_review_batch(&mut storage, ReviewBatchInput { operations, dry_run })
        .map_err(|e| format!("review batch: {e}"))?;
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "dry_run": dry_run,
            "requested_count": report.requested_count,
            "applied_count": report.applied_count,
            "failed_count": report.failed_count
        }
    }))
}

fn tools_call_trust_boundary_audit(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let limit = match arguments.get("limit").and_then(Value::as_u64) {
        Some(0) => return Err("`arguments.limit` must be greater than 0".to_string()),
        Some(value) => value as usize,
        None => 1000,
    };
    let project = arguments.get("project").and_then(Value::as_str).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(Value::as_str).map(str::to_string);
    let storage = Storage::open(db_path).map_err(|e| format!("trust boundary audit: {e}"))?;
    let audit =
        audit_storage_trust_boundary(&storage, project.as_deref(), session_id.as_deref(), limit)
            .map_err(|e| format!("trust boundary audit: {e}"))?;
    let text = serde_json::to_string(&audit).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "project": project,
            "session_id": session_id,
            "passed": audit.passed,
            "checked_claim_count": audit.checked_claim_count,
            "checked_proposal_count": audit.checked_proposal_count
        }
    }))
}

fn parse_review_batch_operations_for_mcp(value: &Value) -> Result<Vec<ReviewActionInput>, String> {
    let operations =
        value.as_array().ok_or_else(|| "`arguments.operations` must be an array".to_string())?;
    operations
        .iter()
        .enumerate()
        .map(|(index, item)| parse_review_batch_operation_for_mcp(index, item))
        .collect()
}

fn parse_review_batch_operation_for_mcp(
    index: usize,
    item: &Value,
) -> Result<ReviewActionInput, String> {
    let target = review_target_from_mcp(item).map_err(|e| format!("operations[{index}]: {e}"))?;
    let action = parse_review_action_for_mcp(
        item.get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("operations[{index}]: missing `action`"))?,
    )
    .map_err(|e| format!("operations[{index}]: {e}"))?;
    let verifier_type =
        match item.get("verifier_type").or_else(|| item.get("verifier")).and_then(Value::as_str) {
            Some(value) => Some(parse_verifier_type_for_mcp(value)?),
            None => Some(VerifierType::User),
        };
    let evidence_ref =
        optional_mcp_evidence_ref(item).map_err(|e| format!("operations[{index}]: {e}"))?;
    Ok(ReviewActionInput {
        target,
        action,
        control_id: item.get("control_id").and_then(Value::as_str).map(str::to_string),
        verifier_type,
        evidence_ref,
        note: item.get("note").and_then(Value::as_str).map(str::to_string),
        confirm_destructive: item
            .get("confirm_destructive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_control_critic_decision(input: &str) -> Result<ControlCriticDecision, String> {
    match normalized_token(input).as_str() {
        "accept" => Ok(ControlCriticDecision::Accept),
        "revise" => Ok(ControlCriticDecision::Revise),
        "reject" => Ok(ControlCriticDecision::Reject),
        other => Err(format!(
            "unknown cloud output decision `{other}`; expected accept, revise, or reject"
        )),
    }
}

fn parse_learning_critic_action(input: &str) -> Result<LearningCriticAction, String> {
    match normalized_token(input).as_str() {
        "create_candidate" => Ok(LearningCriticAction::CreateCandidate),
        "propose_promotion" => Ok(LearningCriticAction::ProposePromotion),
        "decay" => Ok(LearningCriticAction::Decay),
        "request_verification" => Ok(LearningCriticAction::RequestVerification),
        "noop" => Ok(LearningCriticAction::Noop),
        other => Err(format!(
            "unknown proposal action `{other}`; expected create_candidate, propose_promotion, decay, request_verification, or noop"
        )),
    }
}

fn parse_lifecycle_state(input: &str) -> Result<LifecycleState, String> {
    match normalized_token(input).as_str() {
        "captured" => Ok(LifecycleState::Captured),
        "working" => Ok(LifecycleState::Working),
        "short_term_candidate" => Ok(LifecycleState::ShortTermCandidate),
        "long_term_memory" => Ok(LifecycleState::LongTermMemory),
        "semantic_fact" => Ok(LifecycleState::SemanticFact),
        "corrected" => Ok(LifecycleState::Corrected),
        "decayed" => Ok(LifecycleState::Decayed),
        "forgotten" => Ok(LifecycleState::Forgotten),
        other => Err(format!("unknown lifecycle state `{other}`")),
    }
}

fn parse_verifier_type_for_mcp(input: &str) -> Result<VerifierType, String> {
    match normalized_token(input).as_str() {
        "user" => Ok(VerifierType::User),
        "test" => Ok(VerifierType::Test),
        "tool" => Ok(VerifierType::Tool),
        "local_observation" => Ok(VerifierType::LocalObservation),
        "correction" => Ok(VerifierType::Correction),
        other => Err(format!(
            "unknown verifier `{other}`; expected user, test, tool, local_observation, or correction"
        )),
    }
}

fn parse_verification_result_for_mcp(input: &str) -> Result<VerificationResult, String> {
    match normalized_token(input).as_str() {
        "confirmed" => Ok(VerificationResult::Confirmed),
        "contradicted" => Ok(VerificationResult::Contradicted),
        "superseded" => Ok(VerificationResult::Superseded),
        "inconclusive" => Ok(VerificationResult::Inconclusive),
        other => Err(format!(
            "unknown verification result `{other}`; expected confirmed, contradicted, superseded, or inconclusive"
        )),
    }
}

fn parse_review_action_for_mcp(input: &str) -> Result<ReviewAction, String> {
    match normalized_token(input).as_str() {
        "confirm" | "confirmed" => Ok(ReviewAction::Confirm),
        "contradict" | "contradicted" | "reject_claim" => Ok(ReviewAction::Contradict),
        "supersede" | "superseded" => Ok(ReviewAction::Supersede),
        "inconclusive" => Ok(ReviewAction::Inconclusive),
        "accept" | "accepted" => Ok(ReviewAction::Accept),
        "reject" | "rejected" => Ok(ReviewAction::Reject),
        "wait" | "waiting" | "waiting_verification" => Ok(ReviewAction::Wait),
        "apply" => Ok(ReviewAction::Apply),
        "confirm_and_apply" | "approve_and_apply" => Ok(ReviewAction::ConfirmAndApply),
        other => Err(format!(
            "unknown review action `{other}`; expected confirm, contradict, supersede, inconclusive, accept, reject, wait, apply, or confirm_and_apply"
        )),
    }
}

fn parse_learning_critic_proposal_status(
    input: &str,
) -> Result<LearningCriticProposalStatus, String> {
    match normalized_token(input).as_str() {
        "queued" => Ok(LearningCriticProposalStatus::Queued),
        "waiting_verification" => Ok(LearningCriticProposalStatus::WaitingVerification),
        "accepted" => Ok(LearningCriticProposalStatus::Accepted),
        "rejected" => Ok(LearningCriticProposalStatus::Rejected),
        "applied" => Ok(LearningCriticProposalStatus::Applied),
        other => Err(format!(
            "unknown proposal status `{other}`; expected queued, waiting_verification, accepted, rejected, or applied"
        )),
    }
}

fn optional_mcp_evidence_ref(arguments: &Value) -> Result<Option<StoredEvidenceRef>, String> {
    let has_object = arguments.get("evidence_ref").is_some();
    let has_kind = arguments.get("evidence_kind").is_some();
    let has_id = arguments.get("evidence_id").is_some();
    if has_object || has_kind || has_id {
        parse_mcp_evidence_ref(arguments).map(Some)
    } else {
        Ok(None)
    }
}

fn parse_mcp_evidence_ref(arguments: &Value) -> Result<StoredEvidenceRef, String> {
    if let Some(value) = arguments.get("evidence_ref") {
        return parse_evidence_ref_object(value, "arguments.evidence_ref");
    }
    let kind = arguments
        .get("evidence_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `arguments.evidence_ref` or `arguments.evidence_kind`".to_string())?
        .trim();
    let id = arguments
        .get("evidence_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `arguments.evidence_ref` or `arguments.evidence_id`".to_string())?
        .trim();
    if kind.is_empty() || id.is_empty() {
        return Err("verification evidence kind and id must be non-empty".to_string());
    }
    let source = arguments
        .get("evidence_source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::to_string);
    Ok(StoredEvidenceRef { kind: kind.to_string(), id: id.to_string(), source })
}

fn parse_extracted_claims(value: Option<&Value>) -> Result<Vec<ExtractedCloudClaim>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "`arguments.extracted_claims` must be an array".to_string())?;
    let mut out = Vec::with_capacity(array.len());
    for (idx, item) in array.iter().enumerate() {
        if let Some(text) = item.as_str() {
            out.push(ExtractedCloudClaim::new(text.trim()));
            continue;
        }
        let object = item.as_object().ok_or_else(|| {
            format!("`arguments.extracted_claims[{idx}]` must be a string or object")
        })?;
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`arguments.extracted_claims[{idx}].text` is required"))?;
        let evidence_refs = parse_evidence_refs(object.get("evidence_refs"))?;
        out.push(ExtractedCloudClaim { text: text.trim().to_string(), evidence_refs });
    }
    Ok(out)
}

fn parse_verification_requests(value: Option<&Value>) -> Result<Vec<VerificationRequest>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "`arguments.verification_requests` must be an array".to_string())?;
    let mut out = Vec::with_capacity(array.len());
    for (idx, item) in array.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("`arguments.verification_requests[{idx}]` must be an object"))?;
        let claim_text = object
            .get("claim_text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("`arguments.verification_requests[{idx}].claim_text` is required")
            })?
            .trim()
            .to_string();
        let reason = object
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`arguments.verification_requests[{idx}].reason` is required"))?
            .trim()
            .to_string();
        let acceptable_verifiers = parse_verifier_array(object.get("acceptable_verifiers"))?;
        out.push(VerificationRequest { claim_text, reason, acceptable_verifiers });
    }
    Ok(out)
}

fn parse_verifier_array(value: Option<&Value>) -> Result<Vec<VerifierType>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array =
        value.as_array().ok_or_else(|| "`acceptable_verifiers` must be an array".to_string())?;
    array
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let text = item
                .as_str()
                .ok_or_else(|| format!("`acceptable_verifiers[{idx}]` must be a string"))?;
            parse_verifier_type_for_mcp(text)
        })
        .collect()
}

fn parse_string_array(value: Option<&Value>, field: &str) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| format!("`arguments.{field}` must be an array"))?;
    let mut out = Vec::with_capacity(array.len());
    for (idx, item) in array.iter().enumerate() {
        let text = item
            .as_str()
            .ok_or_else(|| format!("`arguments.{field}[{idx}]` must be a string"))?
            .trim();
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
    Ok(out)
}

fn parse_evidence_refs(value: Option<&Value>) -> Result<Vec<StoredEvidenceRef>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| "`evidence_refs` must be an array".to_string())?;
    let mut out = Vec::with_capacity(array.len());
    for (idx, item) in array.iter().enumerate() {
        out.push(parse_evidence_ref_object(item, &format!("evidence_refs[{idx}]"))?);
    }
    Ok(out)
}

fn parse_evidence_ref_object(value: &Value, label: &str) -> Result<StoredEvidenceRef, String> {
    let object = value.as_object().ok_or_else(|| format!("`{label}` must be an object"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{label}.kind` is required"))?
        .trim();
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{label}.id` is required"))?
        .trim();
    if kind.is_empty() || id.is_empty() {
        return Err(format!("`{label}` kind and id must be non-empty"));
    }
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::to_string);
    Ok(StoredEvidenceRef { kind: kind.to_string(), id: id.to_string(), source })
}

fn normalized_token(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

fn tools_call_context_why(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments.get("query").and_then(|q| q.as_str()).map(str::to_string);
    let project = arguments.get("project").and_then(|p| p.as_str()).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(str::to_string);
    let section_filter = arguments.get("section").and_then(|s| s.as_str()).map(str::to_string);
    let contains = arguments.get("contains").and_then(|s| s.as_str()).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);

    if let Some(section) = section_filter.as_deref() {
        if crate::context::why::validate_section(Some(section)).is_err() {
            return Err(format!(
                "unknown `arguments.section`: {section}; expected {}",
                crate::context::why::VALID_SECTIONS.join(", ")
            ));
        }
    }

    let envelope = build_context_envelope_for_mcp(
        db_path,
        cache,
        query.clone(),
        project.clone(),
        session_id.clone(),
        None,
        task_frame_id,
    )?;
    let storage = Storage::open(db_path).map_err(|e| format!("context why audit: {e}"))?;
    let matches = crate::context::why::why_matches_with_audit(
        &storage,
        &envelope,
        section_filter.as_deref(),
        contains.as_deref(),
    )
    .map_err(|e| format!("context why audit: {e}"))?;
    let task_frame_projection = match task_frame_id {
        Some(task_frame_id) => {
            let task_frame = storage
                .task_frame(task_frame_id)
                .map_err(|e| format!("context why task frame audit: {e}"))?
                .ok_or_else(|| format!("TaskFrame {task_frame_id} not found"))?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let text = serde_json::to_string(&json!({
        "scope": envelope.scope,
        "assembled_at_ns": envelope.assembled_at_ns,
        "task_frame_id": task_frame_id,
        "task_frame_projection": task_frame_projection,
        "section": section_filter,
        "contains": contains,
        "match_count": matches.len(),
        "matches": matches,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": { "tool": name, "query": query, "project": project, "session_id": session_id, "task_frame_id": task_frame_id }
    }))
}

fn tools_call_context_audit(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments.get("query").and_then(|q| q.as_str()).map(str::to_string);
    let project = arguments.get("project").and_then(|p| p.as_str()).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);

    let envelope = build_context_envelope_for_mcp(
        db_path,
        cache,
        query.clone(),
        project.clone(),
        session_id.clone(),
        None,
        task_frame_id,
    )?;
    let envelope_audit = audit_context_envelope(&envelope);
    let task_frame_audit = match task_frame_id {
        Some(task_frame_id) => {
            let storage = Storage::open(db_path).map_err(|e| format!("context audit: {e}"))?;
            let task_frame = storage
                .task_frame(task_frame_id)
                .map_err(|e| format!("context audit: {e}"))?
                .ok_or_else(|| format!("TaskFrame {task_frame_id} not found"))?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let task_frame_passed = task_frame_audit.as_ref().is_none_or(|audit| audit.passed());
    let text = serde_json::to_string(&json!({
        "scope": envelope.scope,
        "assembled_at_ns": envelope.assembled_at_ns,
        "task_frame_id": task_frame_id,
        "passed": envelope_audit.passed() && task_frame_passed,
        "envelope": envelope_audit,
        "task_frame": task_frame_audit,
    }))
    .unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": { "tool": name, "query": query, "project": project, "session_id": session_id }
    }))
}

fn tools_call_product_hardening_report(
    name: &str,
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let arguments = params.and_then(|p| p.get("arguments")).cloned().unwrap_or(json!({}));
    let query = arguments.get("query").and_then(|q| q.as_str()).map(str::to_string);
    let project = arguments.get("project").and_then(|p| p.as_str()).map(str::to_string);
    let session_id = arguments.get("session_id").and_then(|p| p.as_str()).map(str::to_string);
    let task_frame_id = arguments.get("task_frame_id").and_then(Value::as_i64);
    let client = arguments
        .get("client")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let explicit_required_clients = required_clients_arg(&arguments, "required_clients")?;
    let trust_limit = positive_usize_arg(&arguments, "trust_limit", 1000)?;
    let review_limit = positive_usize_arg(&arguments, "review_limit", 1000)?;
    let client_proof_limit =
        positive_usize_arg(&arguments, "client_proof_limit", 20)?.clamp(1, 200);
    let client_binding_config_root =
        optional_trimmed_string_arg(&arguments, "client_binding_config_root");
    let task_frame_retention_days = positive_i64_arg(
        &arguments,
        "task_frame_retention_days",
        crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS,
    )?;
    let require_client_binding_ready_requested =
        arguments.get("require_client_binding_ready").and_then(Value::as_bool).unwrap_or(false);
    let required_clients = effective_required_client_names(
        require_client_binding_ready_requested,
        client.as_deref(),
        explicit_required_clients,
    );
    let require_client_binding_ready =
        require_client_binding_ready_requested || !required_clients.is_empty();
    let require_review_queue_clear =
        arguments.get("require_review_queue_clear").and_then(Value::as_bool).unwrap_or(false);
    let require_task_frame_retention_clean = arguments
        .get("require_task_frame_retention_clean")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let require_task_frame_projection =
        arguments.get("require_task_frame_projection").and_then(Value::as_bool).unwrap_or(false);
    let skip_client_binding =
        arguments.get("skip_client_binding").and_then(Value::as_bool).unwrap_or(false);

    let envelope = build_context_envelope_for_mcp(
        db_path,
        cache,
        query.clone(),
        project.clone(),
        session_id.clone(),
        None,
        task_frame_id,
    )?;
    let envelope_audit = audit_context_envelope(&envelope);
    let mut storage =
        Storage::open(db_path).map_err(|e| format!("product hardening report: {e}"))?;
    let storage_trust = audit_storage_trust_boundary(
        &storage,
        envelope.scope.project.as_deref(),
        envelope.scope.session_id.as_deref(),
        trust_limit,
    )
    .map_err(|e| format!("product hardening report: {e}"))?;
    let review_backlog = audit_review_backlog(
        &storage,
        envelope.scope.project.as_deref(),
        envelope.scope.session_id.as_deref(),
        review_limit,
    )
    .map_err(|e| format!("product hardening report: {e}"))?;
    let review_render_plan = build_review_render_plan(
        &storage,
        ReviewRenderInput {
            project: envelope.scope.project.clone(),
            session_id: envelope.scope.session_id.clone(),
            limit: review_limit,
            client: client.clone(),
            include_disabled: false,
        },
    )
    .map_err(|e| format!("product hardening report: {e}"))?;
    let review_interaction = audit_review_interaction_contract(&review_render_plan);
    let review_control_binding = audit_review_control_binding_manifest(&review_render_plan);
    let task_frame_retention = audit_task_frame_retention_hygiene(
        &mut storage,
        envelope.scope.project.as_deref(),
        envelope.scope.session_id.as_deref(),
        task_frame_retention_days,
        now_ns(),
    )
    .map_err(|e| format!("product hardening report: {e}"))?;
    let latent_interface = audit_latent_interface_packet(
        &storage,
        query.as_deref(),
        envelope.scope.project.as_deref(),
        envelope.scope.session_id.as_deref(),
    )
    .map_err(|e| format!("product hardening report: {e}"))?;
    let task_frame_audit = match task_frame_id {
        Some(task_frame_id) => {
            let task_frame = storage
                .task_frame(task_frame_id)
                .map_err(|e| format!("product hardening report: {e}"))?
                .ok_or_else(|| format!("TaskFrame {task_frame_id} not found"))?;
            Some(audit_task_frame_projection(&task_frame))
        }
        None => None,
    };
    let client_binding = if skip_client_binding {
        None
    } else {
        Some(client_binding_hardening_audit_for_mcp(
            &storage,
            db_path,
            client.clone(),
            required_clients.clone(),
            client_proof_limit,
            client_binding_config_root.as_deref(),
        )?)
    };
    let cwd_project = crate::project::current_name();
    let scope_resolution =
        build_product_hardening_scope_resolution(ProductHardeningScopeResolutionInput {
            scope: &envelope.scope,
            explicit_project: project.as_deref(),
            explicit_session_id: session_id.as_deref(),
            task_frame_id,
            cwd_project: cwd_project.clone(),
            override_command: product_hardening_scope_override_command_for_mcp(
                ProductHardeningScopeOverrideCommandInput {
                    cwd_project: cwd_project.as_deref(),
                    query: query.as_deref(),
                    session_id: session_id.as_deref(),
                    client: client.as_deref(),
                    required_clients: &required_clients,
                    client_binding_config_root: client_binding_config_root.as_deref(),
                    task_frame_id,
                    trust_limit,
                    review_limit,
                    client_proof_limit,
                    require_client_binding_ready: require_client_binding_ready_requested,
                    require_review_queue_clear,
                    require_task_frame_retention_clean,
                    require_task_frame_projection,
                    task_frame_retention_days,
                    skip_client_binding,
                },
            ),
        });
    let report = build_product_hardening_report(
        envelope.scope,
        scope_resolution,
        envelope.assembled_at_ns,
        task_frame_id,
        client,
        envelope_audit,
        storage_trust,
        review_backlog,
        review_interaction,
        review_control_binding,
        task_frame_retention,
        latent_interface,
        task_frame_audit,
        client_binding,
        required_clients,
        ProductHardeningRequirements {
            client_binding_ready: require_client_binding_ready,
            review_queue_clear: require_review_queue_clear,
            task_frame_retention_clean: require_task_frame_retention_clean,
            task_frame_projection: require_task_frame_projection,
        },
    );
    let text = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());

    Ok(json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "_debug": {
            "tool": name,
            "query": query,
            "project": project,
            "session_id": session_id,
            "task_frame_id": task_frame_id,
            "client": report.client,
            "required_clients": report.required_clients,
            "client_binding_required": report.client_binding_required,
            "review_queue_clear_required": report.review_queue_clear_required,
            "task_frame_retention_clean_required": report.task_frame_retention_clean_required,
            "task_frame_projection_required": report.task_frame_projection_required,
            "status": report.status,
            "passed": report.passed,
            "failed_gate_count": report.failed_gate_count,
            "warning_gate_count": report.warning_gate_count,
            "control_plan_step_count": report.control_plan.step_count,
            "objective_coverage_total_count": report.objective_coverage_total_count,
            "objective_coverage_fail_count": report.objective_coverage_fail_count,
            "latent_interface_passed": report.latent_interface.passed,
            "latent_interface_schema": report.latent_interface.schema
        }
    }))
}

#[allow(clippy::struct_excessive_bools)]
struct ProductHardeningScopeOverrideCommandInput<'a> {
    cwd_project: Option<&'a str>,
    query: Option<&'a str>,
    session_id: Option<&'a str>,
    client: Option<&'a str>,
    required_clients: &'a [String],
    client_binding_config_root: Option<&'a str>,
    task_frame_id: Option<i64>,
    trust_limit: usize,
    review_limit: usize,
    client_proof_limit: usize,
    require_client_binding_ready: bool,
    require_review_queue_clear: bool,
    require_task_frame_retention_clean: bool,
    require_task_frame_projection: bool,
    task_frame_retention_days: i64,
    skip_client_binding: bool,
}

fn product_hardening_scope_override_command_for_mcp(
    input: ProductHardeningScopeOverrideCommandInput<'_>,
) -> Vec<String> {
    let Some(project) = input.cwd_project.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    let mut command = vec![
        "soma".to_string(),
        "context".to_string(),
        "hardening-report".to_string(),
        "--project".to_string(),
        project.to_string(),
    ];
    append_optional_command_value(&mut command, "--query", input.query);
    append_optional_command_value(&mut command, "--session-id", input.session_id);
    append_optional_command_value(&mut command, "--client", input.client);
    for client in input.required_clients {
        append_optional_command_value(&mut command, "--required-client", Some(client.as_str()));
    }
    append_optional_command_value(
        &mut command,
        "--client-binding-config-root",
        input.client_binding_config_root,
    );
    if let Some(task_frame_id) = input.task_frame_id {
        command.push("--task-frame-id".to_string());
        command.push(task_frame_id.to_string());
    }
    if input.trust_limit != 1000 {
        command.push("--trust-limit".to_string());
        command.push(input.trust_limit.to_string());
    }
    if input.review_limit != 1000 {
        command.push("--review-limit".to_string());
        command.push(input.review_limit.to_string());
    }
    if input.client_proof_limit != 20 {
        command.push("--client-proof-limit".to_string());
        command.push(input.client_proof_limit.to_string());
    }
    if input.require_client_binding_ready {
        command.push("--require-client-binding-ready".to_string());
    }
    if input.require_review_queue_clear {
        command.push("--require-review-queue-clear".to_string());
    }
    if input.require_task_frame_retention_clean {
        command.push("--require-task-frame-retention-clean".to_string());
    }
    if input.require_task_frame_projection {
        command.push("--require-task-frame-projection".to_string());
    }
    if input.task_frame_retention_days != crate::storage::DEFAULT_TASK_FRAME_RETENTION_DAYS {
        command.push("--task-frame-retention-days".to_string());
        command.push(input.task_frame_retention_days.to_string());
    }
    if input.skip_client_binding {
        command.push("--skip-client-binding".to_string());
    }
    command
}

fn append_optional_command_value(command: &mut Vec<String>, flag: &str, value: Option<&str>) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    command.push(flag.to_string());
    command.push(value.to_string());
}

fn positive_usize_arg(arguments: &Value, key: &str, default: usize) -> Result<usize, String> {
    match arguments.get(key).and_then(Value::as_u64) {
        Some(0) => Err(format!("`arguments.{key}` must be greater than 0")),
        Some(value) => Ok(value as usize),
        None => Ok(default),
    }
}

fn positive_i64_arg(arguments: &Value, key: &str, default: i64) -> Result<i64, String> {
    match arguments.get(key).and_then(Value::as_i64) {
        Some(value) if value < 1 => Err(format!("`arguments.{key}` must be at least 1")),
        Some(value) => Ok(value),
        None => Ok(default),
    }
}

fn required_clients_arg(arguments: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(Vec::new());
    };
    match value {
        Value::Array(items) => {
            let mut clients = Vec::new();
            for item in items {
                let Some(client) = item.as_str() else {
                    return Err(format!("`arguments.{key}` must contain only strings"));
                };
                clients.push(client);
            }
            Ok(normalize_required_client_names(clients.iter().copied()))
        }
        Value::String(value) => Ok(normalize_required_client_names(value.split(','))),
        _ => {
            Err(format!("`arguments.{key}` must be an array of strings or comma-separated string"))
        }
    }
}

fn client_binding_hardening_audit_for_mcp(
    storage: &Storage,
    db_path: &Path,
    client: Option<String>,
    required_clients: Vec<String>,
    limit: usize,
    client_binding_config_root: Option<&str>,
) -> Result<ClientBindingHardeningAudit, String> {
    let proofs = storage
        .recent_client_binding_proofs(
            if required_clients.is_empty() { client.as_deref() } else { None },
            limit,
        )
        .map_err(|e| format!("product hardening report: {e}"))?;
    let status = build_client_binding_status_report(client.clone(), None, limit, &proofs);
    let ready_client_count =
        status.clients.iter().filter(|client| client.ready_for_private_client_claim).count();
    let artifact_failure_count =
        status.clients.iter().map(|client| client.artifact_failures.len()).sum();
    let coherence_failure_count =
        status.clients.iter().map(|client| client.coherence_failures.len()).sum();
    let non_release_evidence_source_count: usize =
        status.clients.iter().map(|client| client.non_release_evidence_sources.len()).sum();
    let non_release_proof_levels: Vec<String> = status
        .clients
        .iter()
        .flat_map(|client| {
            client
                .non_release_evidence_sources
                .iter()
                .map(|source| source.proof_level.as_str().to_string())
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let primary_readiness = status.clients.first().map(|client| client.readiness.clone());
    let primary_coherence_failures =
        status.clients.first().map(|client| client.coherence_failures.clone()).unwrap_or_default();
    let primary_non_release_evidence_sources = status
        .clients
        .first()
        .map(|client| {
            client
                .non_release_evidence_sources
                .iter()
                .map(|source| {
                    format!(
                        "{}:{}:{}",
                        source.proof_level.as_str(),
                        source.evidence_source,
                        source.reason
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let readiness_values = status.clients.iter().map(|client| client.readiness.clone()).collect();
    let client_snapshots: Vec<ClientBindingHardeningClientSnapshot> = status
        .clients
        .iter()
        .map(|client| ClientBindingHardeningClientSnapshot {
            client: client.client.clone(),
            readiness: client.readiness.clone(),
            ready_for_private_client_claim: client.ready_for_private_client_claim,
            has_observed_app_hook: client.has_observed_app_hook,
            has_observed_in_client_render: client.has_observed_in_client_render,
            has_observed_review_action: client.has_observed_review_action,
            artifact_failure_count: client.artifact_failures.len(),
            artifact_failures: client
                .artifact_failures
                .iter()
                .map(|failure| ProductHardeningEvidenceArtifactFailure {
                    proof_id: failure.proof_id,
                    proof_level: failure.proof_level.as_str().to_string(),
                    kind: failure.kind.clone(),
                    path: failure.path.clone(),
                    status: failure.status.as_str().to_string(),
                    error: failure.error.clone(),
                })
                .collect(),
            coherence_failure_count: client.coherence_failures.len(),
            non_release_evidence_source_count: client.non_release_evidence_sources.len(),
            non_release_proof_levels: client
                .non_release_evidence_sources
                .iter()
                .map(|source| source.proof_level.as_str().to_string())
                .collect(),
        })
        .collect();
    let mut required_client_proof_matrix =
        build_required_client_proof_matrix(client.as_deref(), &required_clients, &client_snapshots);
    let mut missing_required_clients = Vec::new();
    let mut unready_required_clients = Vec::new();
    for required_client in &required_clients {
        match status.clients.iter().find(|status| status.client == *required_client) {
            Some(status) if status.ready_for_private_client_claim => {}
            Some(_) => unready_required_clients.push(required_client.clone()),
            None => missing_required_clients.push(required_client.clone()),
        }
    }
    let required_client_count = required_clients.len();
    let required_ready_client_count =
        required_client_count - missing_required_clients.len() - unready_required_clients.len();
    let (
        mut proof_session_status,
        mut proof_session_release_gate,
        mut proof_session_next_step_id,
        proof_session_target_clients,
    ) = client_binding_hardening_proof_session_summary_for_mcp(
        client.as_deref(),
        &required_clients,
        &missing_required_clients,
        &unready_required_clients,
        &status.clients,
        artifact_failure_count,
        coherence_failure_count,
    );
    let config_root = client_binding_config_root.map(str::trim).filter(|value| !value.is_empty());
    if let Some(config_root) = config_root {
        let mut first_probe = None;
        for row in required_client_proof_matrix.iter_mut().filter(|row| row.proof_session_required)
        {
            let probe = client_binding_hardening_probe_proof_session_for_mcp(
                db_path,
                &row.client,
                config_root,
                limit,
            )?;
            let next_mcp_call = probe.proof_session.next_mcp_call.as_ref();
            attach_required_client_proof_session_probe(
                row,
                probe.proof_session.next_step_id.clone(),
                probe.proof_session.next_command.clone(),
                next_mcp_call.map(|call| call.tool.clone()),
                next_mcp_call.map(|call| call.arguments.clone()),
                next_mcp_call.map(|call| call.trust_boundary.clone()),
            );
            let soma_bin = crate::cli::binary_identity::resolved_soma_bin_for_operator_command();
            row.proof_session_cli = format!(
                "{soma_bin} adapter-binding-proof --client {} --proof-session --config-root {}",
                row.client, config_root
            );
            row.proof_session_mcp_arguments
                .insert("config_root".to_string(), config_root.to_string());
            row.config_root_probe_hint =
                Some(client_binding_config_root_probe_hint(Some(&row.client), Some(config_root)));
            attach_required_client_render_evidence_artifact_scan(
                row,
                proof_session_render_evidence_artifact_path(&probe.proof_session),
            );
            refresh_required_client_proof_matrix_operator_action(row);
            if first_probe.is_none() {
                first_probe = Some(probe);
            }
        }
        if let Some(probe) = first_probe {
            proof_session_status = probe.proof_session.status;
            proof_session_release_gate = probe.proof_session.release_gate;
            proof_session_next_step_id = probe.proof_session.next_step_id;
        }
    }
    let proof_session_config_root_probe_hint =
        if required_client_proof_matrix.iter().any(|row| row.proof_session_required) {
            Some(client_binding_config_root_probe_hint(None, config_root))
        } else {
            None
        };
    Ok(ClientBindingHardeningAudit {
        client,
        required_clients,
        required_client_proof_matrix,
        proof_session_source: "soma_client_binding_proof_session".to_string(),
        proof_session_runbook_source: "soma_client_binding_proof_session".to_string(),
        proof_session_runbook_schema: "soma.client_binding_proof_session_runbook.v1".to_string(),
        proof_session_runbook_required: proof_session_release_gate != "pass",
        proof_session_runbook_next_step_id: proof_session_next_step_id.clone(),
        proof_session_status,
        proof_session_release_gate,
        proof_session_next_step_id,
        proof_session_target_clients,
        proof_session_config_root_probe_hint,
        required_client_count,
        required_ready_client_count,
        missing_required_clients,
        unready_required_clients,
        proof_limit: limit,
        proofs_found: status.proofs_found,
        client_count: status.client_count,
        ready_client_count,
        all_latest_artifacts_verified: status.all_latest_artifacts_verified,
        artifact_failure_count,
        coherence_failure_count,
        non_release_evidence_source_count,
        non_release_proof_levels,
        primary_readiness,
        primary_coherence_failures,
        primary_non_release_evidence_sources,
        readiness_values,
    })
}

fn client_binding_hardening_probe_proof_session_for_mcp(
    db_path: &Path,
    client: &str,
    config_root: &str,
    limit: usize,
) -> Result<crate::cli::adapter_binding_proof::AdapterBindingProofSessionOutcome, String> {
    let mut args = client_binding_install_plan_args(
        None,
        Some(client.to_string()),
        None,
        Some(config_root.to_string()),
        None,
        None,
    );
    args.proof_session = true;
    args.render_installed_config = false;
    args.limit = limit;
    args.evidence_source = "mcp_product_hardening_proof_session_probe".to_string();
    args.db_path = Some(db_path.to_string_lossy().into_owned());
    run_proof_session_blocking(
        &args,
        &AdapterBindingProofContext { db_path: db_path.to_path_buf() },
    )
    .map_err(|err| format!("client binding proof-session probe: {err}"))
}

fn client_binding_hardening_proof_session_summary_for_mcp(
    requested_client: Option<&str>,
    required_clients: &[String],
    missing_required_clients: &[String],
    unready_required_clients: &[String],
    client_statuses: &[crate::cli::adapter_binding_proof::ClientBindingReadinessStatus],
    artifact_failure_count: usize,
    coherence_failure_count: usize,
) -> (String, String, Option<String>, Vec<String>) {
    let ready = if required_clients.is_empty() {
        client_statuses.iter().any(|status| status.ready_for_private_client_claim)
    } else {
        missing_required_clients.is_empty()
            && unready_required_clients.is_empty()
            && required_clients.iter().all(|required| {
                client_statuses.iter().any(|status| {
                    status.client == *required && status.ready_for_private_client_claim
                })
            })
    };
    let mut target_clients = Vec::new();
    target_clients.extend(missing_required_clients.iter().cloned());
    target_clients.extend(unready_required_clients.iter().cloned());
    if target_clients.is_empty() && !ready {
        if let Some(client) = requested_client {
            target_clients.push(client.to_string());
        } else if let Some(status) = client_statuses.first() {
            target_clients.push(status.client.clone());
        }
    }
    if target_clients.is_empty() && !required_clients.is_empty() {
        target_clients.extend(required_clients.iter().cloned());
    }
    if ready {
        ("ready_for_private_client_claim".to_string(), "pass".to_string(), None, target_clients)
    } else if artifact_failure_count > 0 || coherence_failure_count > 0 {
        (
            "blocked_by_stored_proof_integrity_or_identity".to_string(),
            "fail".to_string(),
            Some("verify_evidence_artifacts_and_status".to_string()),
            target_clients,
        )
    } else {
        (
            "requires_client_binding_proof_session".to_string(),
            "fail".to_string(),
            Some("render_client_binding_proof_session".to_string()),
            target_clients,
        )
    }
}

fn resources_list_result(db_path: &Path) -> Value {
    let mut resources: Vec<Value> = vec![
        json!({
            "uri": URI_CONTEXT_CURRENT,
            "name": "Current ContextEnvelope",
            "description": "Cloud-LLM-facing context envelope for recent local work.",
            "mimeType": "text/xml"
        }),
        json!({
            "uri": format!("{URI_CONTEXT_BY_QUERY_PREFIX}?q="),
            "name": "Query ContextEnvelope",
            "description": "Cloud-LLM-facing context envelope conditioned on a semantic query.",
            "mimeType": "text/xml"
        }),
        json!({
            "uri": format!("{URI_CONTEXT_SESSION_PREFIX}<session_id>"),
            "name": "Session ContextEnvelope",
            "description": "Cloud-LLM-facing context envelope narrowed to one captured session_id.",
            "mimeType": "text/xml"
        }),
    ];
    // D161 — surface every active project as its own resource so a
    // Claude Code session inside a project can attach
    // the ContextEnvelope project resource directly. Active = any
    // project that appears in the most-recent 500 episodes (matches
    // memory_state's window). Failures degrade to the static
    // current/by-query/session envelope resources only.
    if let Ok(projects) = active_projects(db_path) {
        for (name, count) in projects.into_iter().take(8) {
            let encoded_name = urlencode_path_segment(&name);
            resources.push(json!({
                "uri": format!("{URI_CONTEXT_PROJECT_PREFIX}{encoded_name}"),
                "name": format!("ContextEnvelope — project: {name}"),
                "description": format!(
                    "Cloud-LLM-facing context envelope narrowed to project `{name}` ({count} episodes in last 500)."
                ),
                "mimeType": "text/xml"
            }));
        }
    }
    if let Ok(threads) = confirmed_thread_resources(db_path) {
        for identity in threads.into_iter().take(8) {
            let encoded_key = urlencode_path_segment(&identity.thread_key);
            resources.push(json!({
                "uri": format!("{URI_CONTEXT_THREAD_PREFIX}{encoded_key}"),
                "name": format!("ContextEnvelope — thread: {}", identity.thread_key),
                "description": format!(
                    "Cloud-LLM-facing context envelope narrowed to operator-confirmed thread `{}` (project `{}`, {} sessions).",
                    identity.thread_key,
                    identity.project,
                    identity.session_ids.len()
                ),
                "mimeType": "text/xml"
            }));
        }
    }
    json!({ "resources": resources })
}

/// D161 — distinct project names from the last 500 episodes,
/// sorted by descending episode count. Empty / NULL `episodes.project`
/// rows are skipped. Read-only — the dashboard's memory_state
/// helper does the same aggregation client-side; here we read once
/// per `resources/list` call (low frequency).
fn active_projects(db_path: &Path) -> Result<Vec<(String, usize)>, crate::storage::StorageError> {
    let store = Storage::open(db_path)?;
    let recent = store.recent_episodes(500)?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for ep in recent {
        if let Some(p) = ep.project {
            if !p.is_empty() {
                *counts.entry(p).or_insert(0) += 1;
            }
        }
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(sorted)
}

fn confirmed_thread_resources(
    db_path: &Path,
) -> Result<Vec<StoredThreadIdentity>, crate::storage::StorageError> {
    let store = Storage::open(db_path)?;
    Ok(store
        .recent_thread_identities(None, 32)?
        .into_iter()
        .filter(|identity| identity.status == THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED)
        .collect())
}

fn resources_read(
    params: Option<&Value>,
    db_path: &Path,
    cache: &MemoryPackCache,
) -> Result<Value, String> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| "missing `uri` param".to_string())?;

    let parsed = parse_uri(uri)?;
    let thread_identity = match parsed.thread_key.as_deref() {
        Some(thread_key) => Some(confirmed_thread_identity_for_key(db_path, thread_key)?),
        None => None,
    };
    let session_filters =
        thread_identity.as_ref().map(|identity| identity.session_ids.clone()).unwrap_or_default();

    // Discussion 0032 §G — TTL cache wraps the build path. db_path
    // is captured into the closure so a miss falls through to the
    // pre-cache builder unchanged. D161 — project segment becomes
    // part of the cache key so two clients hitting different
    // projects get separate slots.
    let effective_project = if let Some(identity) = &thread_identity {
        Some(identity.project.clone())
    } else if parsed.contract == ResourceContract::ContextEnvelope {
        effective_project_for_context_scope(
            db_path,
            parsed.project.clone(),
            parsed.session_id.as_deref(),
        )?
    } else {
        parsed.project.clone()
    };
    let cache_key = CacheKey {
        kind: parsed.kind,
        query: parsed.query.clone(),
        project: effective_project.clone(),
        session_id: parsed.session_id.clone(),
        thread_key: parsed.thread_key.clone(),
    };
    let owned_query = parsed.query.clone();
    let owned_project = effective_project.clone();
    let owned_session = parsed.session_id.clone();
    let owned_session_filters = session_filters.clone();
    let owned_db = db_path.to_path_buf();
    let (pack, status) = cache
        .get_or_build(cache_key, move || {
            let storage = Arc::new(Mutex::new(
                Storage::open(&owned_db).map_err(crate::context::pack::PackError::from)?,
            ));
            let cfg = PackConfig {
                project_filter: owned_project,
                session_filter: owned_session,
                session_filters: owned_session_filters,
                ..PackConfig::default()
            };
            build_memory_pack(storage, owned_query.as_deref(), cfg)
        })
        .map_err(|e| format!("memory pack build: {e}"))?;

    let (primary_mime, primary_text, json_text, disposition) = match parsed.contract {
        ResourceContract::MemoryPack => (
            "text/markdown",
            render_debug_memory_pack_markdown(&pack),
            render_pack_json(&pack),
            "developer-debug-direct-read",
        ),
        ResourceContract::ContextEnvelope => {
            let envelope = build_context_envelope_for_pack(
                &pack,
                db_path,
                parsed.query.clone(),
                effective_project.clone(),
                parsed.session_id.clone(),
                thread_identity.as_ref().map(|identity| identity.thread_key.clone()),
                &session_filters,
                None,
            )?;
            (
                "text/xml",
                render_context_xml(&envelope),
                render_context_json(&envelope),
                "cloud-llm-context-contract",
            )
        }
    };

    let cache_label = match status {
        CacheStatus::Hit => "hit",
        CacheStatus::Miss => "miss",
    };

    // Discussion 0023 G7 / 0029 §C — return both renderings. For
    // ContextEnvelope this is the cloud-LLM read contract; for
    // MemoryPack it is a developer/debug raw retrieval view. MCP
    // spec supports multiple `contents` entries on a single resource.
    Ok(json!({
        "contents": [
            {
                "uri": uri,
                "mimeType": primary_mime,
                "text": primary_text,
            },
            {
                "uri": uri,
                "mimeType": "application/json",
                "text": json_text,
            }
        ],
        "_debug": {
            "contract": parsed.contract.as_str(),
            "kind": parsed.kind,
            "query": parsed.query,
            "project": effective_project,
            "session_id": parsed.session_id,
            "thread_key": parsed.thread_key,
            "disposition": disposition,
            "cache": cache_label,
        }
    }))
}

fn render_debug_memory_pack_markdown(pack: &crate::context::pack::MemoryPack) -> String {
    let mut out = String::new();
    out.push_str("<!-- SOMA developer/debug direct-read: raw retrieval inspection only. ");
    out.push_str("Cloud LLM clients should use soma://context/* or soma_recall for cited ContextEnvelope output. -->\n\n");
    out.push_str(&render_markdown(pack));
    out
}

fn build_context_envelope_for_mcp(
    db_path: &Path,
    cache: &MemoryPackCache,
    query: Option<String>,
    project: Option<String>,
    session_id: Option<String>,
    thread_key: Option<String>,
    task_frame_id: Option<i64>,
) -> Result<ContextEnvelope, String> {
    let resolved = resolve_task_frame_context(db_path, task_frame_id, query, project, session_id)?;
    let thread_identity = match thread_key.as_deref() {
        Some(thread_key) => Some(confirmed_thread_identity_for_key(db_path, thread_key)?),
        None => None,
    };
    if let Some(identity) = &thread_identity {
        validate_thread_context_scope(
            identity,
            resolved.project.as_deref(),
            resolved.session_id.as_deref(),
        )?;
    }
    let session_filters =
        thread_identity.as_ref().map(|identity| identity.session_ids.clone()).unwrap_or_default();
    let effective_project = match &thread_identity {
        Some(identity) => Some(identity.project.clone()),
        None => effective_project_for_context_scope(
            db_path,
            resolved.project.clone(),
            resolved.session_id.as_deref(),
        )?,
    };
    let cache_key = CacheKey {
        kind: if thread_identity.is_some() {
            "context-thread"
        } else if resolved.session_id.is_some() {
            "context-session"
        } else if effective_project.is_some() {
            "context-project"
        } else {
            "context-current"
        },
        query: resolved.query.clone(),
        project: effective_project.clone(),
        session_id: resolved.session_id.clone(),
        thread_key: thread_key.clone(),
    };
    let owned_query = resolved.query.clone();
    let owned_project = effective_project.clone();
    let owned_session = resolved.session_id.clone();
    let owned_session_filters = session_filters.clone();
    let owned_db = db_path.to_path_buf();
    let (pack, _status) = cache
        .get_or_build(cache_key, move || {
            let storage = Arc::new(Mutex::new(
                Storage::open(&owned_db).map_err(crate::context::pack::PackError::from)?,
            ));
            let cfg = PackConfig {
                project_filter: owned_project,
                session_filter: owned_session,
                session_filters: owned_session_filters,
                ..PackConfig::default()
            };
            build_memory_pack(storage, owned_query.as_deref(), cfg)
        })
        .map_err(|e| format!("context why: {e}"))?;

    build_context_envelope_for_pack(
        &pack,
        db_path,
        resolved.query,
        effective_project,
        resolved.session_id,
        thread_identity.as_ref().map(|identity| identity.thread_key.clone()),
        &session_filters,
        resolved.task_frame.as_ref(),
    )
}

fn confirmed_thread_identity_for_key(
    db_path: &Path,
    thread_key: &str,
) -> Result<StoredThreadIdentity, String> {
    let store =
        Storage::open(db_path).map_err(|e| format!("thread identity `{thread_key}`: {e}"))?;
    let identity = store
        .thread_identity_by_key(thread_key)
        .map_err(|e| format!("thread identity `{thread_key}`: {e}"))?
        .ok_or_else(|| format!("thread identity `{thread_key}` not found"))?;
    if identity.status != THREAD_IDENTITY_STATUS_OPERATOR_CONFIRMED {
        return Err(format!(
            "thread identity `{thread_key}` is not operator confirmed (status `{}`)",
            identity.status
        ));
    }
    Ok(identity)
}

fn validate_thread_context_scope(
    identity: &StoredThreadIdentity,
    requested_project: Option<&str>,
    requested_session: Option<&str>,
) -> Result<(), String> {
    if let Some(project) = requested_project {
        if project != identity.project {
            return Err(format!(
                "thread identity `{}` belongs to project `{}`, not requested project `{project}`",
                identity.thread_key, identity.project
            ));
        }
    }
    if let Some(session_id) = requested_session {
        if !identity.session_ids.iter().any(|expected| expected == session_id) {
            return Err(format!(
                "thread identity `{}` does not include requested session `{session_id}`",
                identity.thread_key
            ));
        }
    }
    Ok(())
}

fn effective_project_for_context_scope(
    db_path: &Path,
    project: Option<String>,
    session_id: Option<&str>,
) -> Result<Option<String>, String> {
    if project.is_some() || session_id.is_some() {
        return Ok(project);
    }
    let store = Storage::open(db_path).map_err(|e| format!("context scope: {e}"))?;
    inferred_project_scope_from_anil(&store).map_err(|e| format!("context scope: {e}"))
}

struct ResolvedTaskFrameContext {
    task_frame: Option<StoredTaskFrame>,
    query: Option<String>,
    project: Option<String>,
    session_id: Option<String>,
}

fn resolve_task_frame_context(
    db_path: &Path,
    task_frame_id: Option<i64>,
    query: Option<String>,
    project: Option<String>,
    session_id: Option<String>,
) -> Result<ResolvedTaskFrameContext, String> {
    let Some(task_frame_id) = task_frame_id else {
        return Ok(ResolvedTaskFrameContext { task_frame: None, query, project, session_id });
    };
    let store = Storage::open(db_path).map_err(|e| format!("TaskFrame {task_frame_id}: {e}"))?;
    let task_frame = store
        .task_frame(task_frame_id)
        .map_err(|e| format!("TaskFrame {task_frame_id}: {e}"))?
        .ok_or_else(|| format!("TaskFrame {task_frame_id} not found"))?;
    validate_task_frame_context_scope(
        task_frame_id,
        project.as_deref(),
        session_id.as_deref(),
        &task_frame,
    )?;
    let query = query.or_else(|| Some(task_frame.goal_state.clone()));
    let project = project.or_else(|| task_frame.project.clone());
    let session_id = session_id.or_else(|| task_frame.session_id.clone());
    Ok(ResolvedTaskFrameContext { task_frame: Some(task_frame), query, project, session_id })
}

fn validate_task_frame_context_scope(
    task_frame_id: i64,
    project: Option<&str>,
    session_id: Option<&str>,
    task_frame: &StoredTaskFrame,
) -> Result<(), String> {
    if let (Some(project), Some(frame_project)) = (project, task_frame.project.as_deref()) {
        if project != frame_project {
            return Err(format!(
                "TaskFrame {task_frame_id} project `{frame_project}` does not match requested project `{project}`"
            ));
        }
    }
    if let (Some(session_id), Some(frame_session)) = (session_id, task_frame.session_id.as_deref())
    {
        if session_id != frame_session {
            return Err(format!(
                "TaskFrame {task_frame_id} session `{frame_session}` does not match requested session `{session_id}`"
            ));
        }
    }
    Ok(())
}

fn build_context_envelope_for_pack(
    pack: &crate::context::pack::MemoryPack,
    db_path: &Path,
    query: Option<String>,
    project: Option<String>,
    session_id: Option<String>,
    thread_key: Option<String>,
    session_filters: &[String],
    task_frame: Option<&StoredTaskFrame>,
) -> Result<ContextEnvelope, String> {
    let scope = if let Some(thread_key) = thread_key.clone() {
        ContextScope::thread(thread_key, project.clone(), query)
    } else {
        match session_id.clone() {
            Some(session_id) => ContextScope::session(session_id, project.clone(), query),
            None => match project.clone() {
                Some(project) => ContextScope::project(project, query),
                None => ContextScope::current(query),
            },
        }
    };
    let mut envelope = build_context_envelope(pack, scope);
    let quality = context_quality_sections(
        db_path,
        project.as_deref(),
        session_id.as_deref(),
        session_filters,
    )?;
    append_relevant_memory_items(
        &mut envelope,
        quality.relevant_memory_proxies,
        pack.thread_state_selection.as_ref(),
    );
    apply_correction_overrides(&mut envelope, &quality.correction_stale_claims);
    if let Some(task_frame) = task_frame {
        let section = task_frame_thread_state_section(task_frame, envelope.thread_state.as_ref());
        attach_thread_state(&mut envelope, Some(section));
    }
    attach_short_term_candidates(&mut envelope, quality.short_term_candidates);
    attach_stable_facts(&mut envelope, quality.stable_facts);
    attach_user_policy(&mut envelope, quality.user_policy);
    attach_open_decisions(&mut envelope, quality.open_decisions);
    attach_corrections(&mut envelope, quality.corrections);
    if let Err(e) = try_attach_local_compiler_note_from_env(&mut envelope) {
        tracing::debug!(
            error = %e,
            "local context compiler unavailable; using deterministic ContextEnvelope fallback"
        );
    }
    Ok(envelope)
}

struct ContextQualitySections {
    relevant_memory_proxies: Vec<crate::context::envelope::ContextItem>,
    short_term_candidates: Vec<crate::context::envelope::ContextSection>,
    stable_facts: Vec<crate::context::envelope::ContextSection>,
    user_policy: Vec<crate::context::envelope::ContextSection>,
    open_decisions: Vec<crate::context::envelope::ContextSection>,
    corrections: Vec<crate::context::envelope::ContextSection>,
    correction_stale_claims: Vec<String>,
}

fn context_quality_sections(
    db_path: &Path,
    project: Option<&str>,
    session_id: Option<&str>,
    session_filters: &[String],
) -> Result<ContextQualitySections, String> {
    let store = Storage::open(db_path).map_err(|e| format!("context quality: {e}"))?;
    let relevant_memory_proxies = if session_filters.is_empty() {
        relevant_memory_proxies_from_storage(
            &store,
            project,
            session_id,
            DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT,
        )
    } else {
        relevant_memory_proxies_from_storage_session_set(
            &store,
            project,
            session_filters,
            DEFAULT_RELEVANT_MEMORY_PROXY_LIMIT,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let correction_signals = if session_filters.is_empty() {
        correction_signals_from_storage_scoped(
            &store,
            project,
            session_id,
            DEFAULT_CORRECTION_LIMIT,
        )
    } else {
        correction_signals_from_storage_session_set(
            &store,
            project,
            session_filters,
            DEFAULT_CORRECTION_LIMIT,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let user_policy = if session_filters.is_empty() {
        user_policy_from_storage_with_corrections(&store, project, &correction_signals)
    } else {
        user_policy_from_storage_with_corrections_session_set(
            &store,
            project,
            session_filters,
            &correction_signals,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let correction_stale_claims = correction_signals
        .iter()
        .filter_map(|signal| signal.stale_claim.clone())
        .collect::<Vec<_>>();
    let short_term_candidates = if session_filters.is_empty() {
        short_term_candidates_from_storage(
            &store,
            project,
            session_id,
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )
    } else {
        short_term_candidates_from_storage_session_set(
            &store,
            project,
            session_filters,
            DEFAULT_SHORT_TERM_CANDIDATE_LIMIT,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let stable_facts = if session_filters.is_empty() {
        stable_facts_from_storage(&store, project, session_id, DEFAULT_STABLE_FACT_LIMIT)
    } else {
        stable_facts_from_storage_session_set(
            &store,
            project,
            session_filters,
            DEFAULT_STABLE_FACT_LIMIT,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let open_decisions = if session_filters.is_empty() {
        open_decisions_from_storage_scoped_with_corrections(
            &store,
            project,
            session_id,
            DEFAULT_OPEN_DECISION_LIMIT,
            &correction_stale_claims,
        )
    } else {
        open_decisions_from_storage_session_set_with_corrections(
            &store,
            project,
            session_filters,
            DEFAULT_OPEN_DECISION_LIMIT,
            &correction_stale_claims,
        )
    }
    .map_err(|e| format!("context quality: {e}"))?;
    let corrections = correction_signals.into_iter().map(|signal| signal.section).collect();
    Ok(ContextQualitySections {
        relevant_memory_proxies,
        short_term_candidates,
        stable_facts,
        user_policy,
        open_decisions,
        corrections,
        correction_stale_claims,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResourceContract {
    MemoryPack,
    ContextEnvelope,
}

impl ResourceContract {
    fn as_str(self) -> &'static str {
        match self {
            ResourceContract::MemoryPack => "memory-pack",
            ResourceContract::ContextEnvelope => "context-envelope",
        }
    }
}

/// D161 — parse_uri result. `kind` keeps the static-str
/// discriminator the cache uses; `query` carries the optional
/// `?q=<text>`; `project` carries the optional project name from
/// `soma://context/project/<name>` or developer/debug
/// `soma://memory-pack/project/<name>`; `session_id` carries the
/// optional session id from `soma://context/session/<id>`; `thread_key`
/// carries the operator-confirmed key from `soma://context/thread/<key>`.
pub(super) struct ParsedUri {
    pub contract: ResourceContract,
    pub kind: &'static str,
    pub query: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub thread_key: Option<String>,
}

/// Parse a SOMA MCP URI. `soma://context/*` is the product read
/// contract. `soma://memory-pack/*` is a developer/debug direct-read
/// surface for raw retrieval inspection. Accepts:
///
/// * `soma://memory-pack/current` → kind=`current`, query=None,
///   project=None.
/// * `soma://memory-pack/by-query?q=<text>` → kind=`by-query`,
///   query=Some, project=None.
/// * `soma://memory-pack/project/<name>` → kind=`project`,
///   query=None, project=Some.
/// * `soma://memory-pack/project/<name>?q=<text>` → kind=`project`,
///   query=Some, project=Some.
/// * `soma://context/current` → kind=`context-current`, query=None,
///   project=None.
/// * `soma://context/by-query?q=<text>` → kind=`context-by-query`,
///   query=Some, project=None.
/// * `soma://context/project/<name>` → kind=`context-project`,
///   query=None, project=Some.
/// * `soma://context/session/<id>` → kind=`context-session`,
///   query=None, session_id=Some.
/// * `soma://context/session/<id>?q=<text>` → kind=`context-session`,
///   query=Some, session_id=Some.
/// * `soma://context/thread/<key>` → kind=`context-thread`,
///   query=None, thread_key=Some.
/// * `soma://context/thread/<key>?q=<text>` → kind=`context-thread`,
///   query=Some, thread_key=Some.
///
/// URL-decode of `<text>` and `<name>` handles `+` → ` ` and `%NN`
/// via `urldecode`.
fn parse_uri(uri: &str) -> Result<ParsedUri, String> {
    if uri == URI_CONTEXT_CURRENT {
        return Ok(ParsedUri {
            contract: ResourceContract::ContextEnvelope,
            kind: "context-current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        });
    }
    if let Some(parsed) = parse_by_query_resource(
        uri,
        URI_CONTEXT_BY_QUERY_PREFIX,
        "context-by-query",
        ResourceContract::ContextEnvelope,
    ) {
        return parsed;
    }
    if let Some(tail) = uri.strip_prefix(URI_CONTEXT_PROJECT_PREFIX) {
        return parse_project_tail(uri, tail, "context-project", ResourceContract::ContextEnvelope);
    }
    if let Some(tail) = uri.strip_prefix(URI_CONTEXT_SESSION_PREFIX) {
        return parse_session_tail(uri, tail, "context-session", ResourceContract::ContextEnvelope);
    }
    if let Some(tail) = uri.strip_prefix(URI_CONTEXT_THREAD_PREFIX) {
        return parse_thread_tail(uri, tail, "context-thread", ResourceContract::ContextEnvelope);
    }
    if uri == URI_CURRENT {
        return Ok(ParsedUri {
            contract: ResourceContract::MemoryPack,
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        });
    }
    if let Some(parsed) =
        parse_by_query_resource(uri, URI_BY_QUERY_PREFIX, "by-query", ResourceContract::MemoryPack)
    {
        return parsed;
    }
    if let Some(tail) = uri.strip_prefix(URI_PROJECT_PREFIX) {
        return parse_project_tail(uri, tail, "project", ResourceContract::MemoryPack);
    }
    Err(format!("unknown URI: {uri}"))
}

fn parse_by_query_resource(
    uri: &str,
    prefix: &str,
    kind: &'static str,
    contract: ResourceContract,
) -> Option<Result<ParsedUri, String>> {
    if uri == prefix {
        return Some(Ok(ParsedUri {
            contract,
            kind,
            query: Some(String::new()),
            project: None,
            session_id: None,
            thread_key: None,
        }));
    }
    let rest = uri.strip_prefix(prefix)?.strip_prefix('?')?;
    Some(parse_by_query_tail(rest, kind, contract))
}

fn parse_by_query_tail(
    rest: &str,
    kind: &'static str,
    contract: ResourceContract,
) -> Result<ParsedUri, String> {
    for pair in rest.split('&') {
        if let Some(value) = pair.strip_prefix("q=") {
            let decoded = urldecode(value)?;
            return Ok(ParsedUri {
                contract,
                kind,
                query: Some(decoded),
                project: None,
                session_id: None,
                thread_key: None,
            });
        }
    }
    Ok(ParsedUri {
        contract,
        kind,
        query: Some(String::new()),
        project: None,
        session_id: None,
        thread_key: None,
    })
}

fn parse_project_tail(
    uri: &str,
    tail: &str,
    kind: &'static str,
    contract: ResourceContract,
) -> Result<ParsedUri, String> {
    // Split path tail vs query string. Path part is the project
    // name (URL-decoded); query part is the optional ?q=<text>.
    let (proj_raw, query_part) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    if proj_raw.is_empty() {
        return Err(format!("project URI missing name: {uri}"));
    }
    let project = urldecode(proj_raw)?;
    let query = match query_part {
        None => None,
        Some(qstr) => {
            let mut found: Option<String> = None;
            for pair in qstr.split('&') {
                if let Some(value) = pair.strip_prefix("q=") {
                    found = Some(urldecode(value)?);
                    break;
                }
            }
            found.or(Some(String::new()))
        }
    };
    Ok(ParsedUri {
        contract,
        kind,
        query,
        project: Some(project),
        session_id: None,
        thread_key: None,
    })
}

fn parse_session_tail(
    uri: &str,
    tail: &str,
    kind: &'static str,
    contract: ResourceContract,
) -> Result<ParsedUri, String> {
    let (session_raw, query_part) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    if session_raw.is_empty() {
        return Err(format!("session URI missing id: {uri}"));
    }
    let session_id = urldecode(session_raw)?;
    let query = match query_part {
        None => None,
        Some(qstr) => {
            let mut found: Option<String> = None;
            for pair in qstr.split('&') {
                if let Some(value) = pair.strip_prefix("q=") {
                    found = Some(urldecode(value)?);
                    break;
                }
            }
            found.or(Some(String::new()))
        }
    };
    Ok(ParsedUri {
        contract,
        kind,
        query,
        project: None,
        session_id: Some(session_id),
        thread_key: None,
    })
}

fn parse_thread_tail(
    uri: &str,
    tail: &str,
    kind: &'static str,
    contract: ResourceContract,
) -> Result<ParsedUri, String> {
    let (thread_raw, query_part) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    if thread_raw.is_empty() {
        return Err(format!("thread URI missing key: {uri}"));
    }
    let thread_key = urldecode(thread_raw)?;
    let query = match query_part {
        None => None,
        Some(qstr) => {
            let mut found: Option<String> = None;
            for pair in qstr.split('&') {
                if let Some(value) = pair.strip_prefix("q=") {
                    found = Some(urldecode(value)?);
                    break;
                }
            }
            found.or(Some(String::new()))
        }
    };
    Ok(ParsedUri {
        contract,
        kind,
        query,
        project: None,
        session_id: None,
        thread_key: Some(thread_key),
    })
}

/// Minimal URL decoder — handles `+` → ` ` and `%NN` hex. Malformed
/// percent escapes are rejected instead of being passed through as a
/// literal `%`, so invalid MCP URIs surface as invalid params. No
/// crate dep required for v1 scope.
///
/// D116-cand close (R5 audit, 2026-04-29) — pre-fix used
/// `String::from_utf8_lossy(&out).into_owned()` which silently
/// substituted `U+FFFD` for invalid UTF-8 sequences. A
/// percent-encoded query containing a valid-but-not-UTF-8 byte
/// stream would produce a lossy string that recall could match
/// against unintended episodic text. Reject explicitly so the
/// caller can return a clean JSON-RPC error to the MCP client.
fn urldecode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(format!("urldecode: incomplete percent escape at byte {i}"));
                }
                let hi = hex_nibble(bytes[i + 1])
                    .ok_or_else(|| format!("urldecode: invalid percent escape at byte {i}"))?;
                let lo = hex_nibble(bytes[i + 2])
                    .ok_or_else(|| format!("urldecode: invalid percent escape at byte {i}"))?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| format!("urldecode: invalid UTF-8 ({e})"))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-encode one URI path segment. Project names originate from
/// cwd basenames and may contain spaces or URI delimiters (`?`, `#`,
/// `%`, `/`). `resources/list` must emit an attachable URI that
/// `parse_uri` can round-trip back to the exact `episodes.project`
/// value.
fn urlencode_path_segment(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[(b >> 4) as usize]));
                out.push(char::from(HEX[(b & 0x0F) as usize]));
            }
        }
    }
    out
}

fn ok_response(id: Option<Value>, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
    .to_string()
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> String {
    error_response_v(id, code, message)
}

fn error_response_v(id: Option<Value>, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::envelope::{attach_compiler_notes, EvidenceRef};

    /// D116-cand close + R8 negative test — invalid UTF-8 percent-
    /// encoded sequence must surface a real error, not silently
    /// substitute U+FFFD.
    #[test]
    fn urldecode_rejects_invalid_utf8() {
        // 0xFF 0xFE alone are not valid UTF-8 leading bytes.
        let result = urldecode("%FF%FE");
        assert!(result.is_err(), "invalid UTF-8 must surface as Err");
    }

    #[test]
    fn urldecode_accepts_valid_ascii() {
        let result = urldecode("hello+world").expect("ascii ok");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn urldecode_accepts_valid_multibyte_utf8() {
        // 한 = U+D55C = E1 95 9C in UTF-8
        let result = urldecode("%ED%95%9C").expect("multibyte ok");
        assert_eq!(result, "한");
    }

    #[test]
    fn urldecode_rejects_malformed_percent_escape() {
        let bad_hex = urldecode("hello%ZZworld");
        assert!(bad_hex.is_err(), "invalid hex escape must reject");

        let incomplete = urldecode("hello%");
        assert!(incomplete.is_err(), "trailing percent escape must reject");

        let non_ascii_after_percent = urldecode("%한");
        assert!(non_ascii_after_percent.is_err(), "non-ASCII escape bytes must reject");
    }

    #[test]
    fn urlencode_path_segment_round_trips_project_delimiters() {
        let raw = "my app?100%/한";
        let encoded = urlencode_path_segment(raw);
        assert_eq!(encoded, "my%20app%3F100%25%2F%ED%95%9C");
        assert_eq!(urldecode(&encoded).expect("decode"), raw);
    }

    #[test]
    fn parse_uri_invalid_utf8_surfaces_error() {
        let result = parse_uri("soma://memory-pack/by-query?q=%FF%FE");
        assert!(result.is_err(), "by-query with invalid UTF-8 must error");
    }

    #[test]
    fn parse_uri_context_by_query_routes_to_context_contract() {
        let p = parse_uri("soma://context/by-query?q=hello+world").expect("ok");
        assert_eq!(p.contract.as_str(), "context-envelope");
        assert_eq!(p.kind, "context-by-query");
        assert_eq!(p.query.as_deref(), Some("hello world"));
        assert!(p.project.is_none());
        assert!(p.session_id.is_none());

        let p = parse_uri("soma://context/by-query").expect("ok");
        assert_eq!(p.contract.as_str(), "context-envelope");
        assert_eq!(p.kind, "context-by-query");
        assert_eq!(p.query.as_deref(), Some(""));
        assert!(p.session_id.is_none());
    }

    #[test]
    fn parse_uri_context_session_routes_to_session_scope() {
        let p = parse_uri("soma://context/session/claude%20session?q=auth+policy").expect("ok");
        assert_eq!(p.contract.as_str(), "context-envelope");
        assert_eq!(p.kind, "context-session");
        assert_eq!(p.session_id.as_deref(), Some("claude session"));
        assert_eq!(p.query.as_deref(), Some("auth policy"));
        assert!(p.project.is_none());

        let missing = parse_uri("soma://context/session/");
        assert!(missing.is_err(), "empty session id must reject");
    }

    #[test]
    fn parse_uri_context_thread_routes_to_thread_scope() {
        let p = parse_uri("soma://context/thread/thread%3Amyapp%3Aabc?q=auth+policy").expect("ok");
        assert_eq!(p.contract.as_str(), "context-envelope");
        assert_eq!(p.kind, "context-thread");
        assert_eq!(p.thread_key.as_deref(), Some("thread:myapp:abc"));
        assert_eq!(p.query.as_deref(), Some("auth policy"));
        assert!(p.session_id.is_none());
        assert!(p.project.is_none());

        let missing = parse_uri("soma://context/thread/");
        assert!(missing.is_err(), "empty thread key must reject");
    }

    #[test]
    fn parse_uri_project_segment_routes_to_project_kind() {
        let p = parse_uri("soma://memory-pack/project/aenv").expect("ok");
        assert_eq!(p.kind, "project");
        assert_eq!(p.project.as_deref(), Some("aenv"));
        assert!(p.query.is_none());

        // With ?q= the query portion populates too.
        let p = parse_uri("soma://memory-pack/project/aenv?q=hello").expect("ok");
        assert_eq!(p.kind, "project");
        assert_eq!(p.project.as_deref(), Some("aenv"));
        assert_eq!(p.query.as_deref(), Some("hello"));

        // URL-encoded project name (e.g. with hyphen percent-escaped)
        // round-trips via urldecode.
        let p = parse_uri("soma://memory-pack/project/agent-24h-news").expect("ok");
        assert_eq!(p.project.as_deref(), Some("agent-24h-news"));

        let p = parse_uri("soma://memory-pack/project/my%20app%3F100%25%2F%ED%95%9C")
            .expect("encoded project ok");
        assert_eq!(p.project.as_deref(), Some("my app?100%/한"));
    }

    #[test]
    fn parse_uri_project_empty_name_errors() {
        let r = parse_uri("soma://memory-pack/project/");
        assert!(r.is_err(), "empty project name must reject");
    }

    #[test]
    fn why_matches_exposes_local_compiler_notes_with_evidence() {
        let evidence = vec![EvidenceRef {
            kind: "episode".to_string(),
            id: "7".to_string(),
            source: Some("claude-code".to_string()),
        }];
        let mut envelope = ContextEnvelope {
            version: 1,
            assembled_at_ns: 42,
            scope: ContextScope::current(Some("compiler".to_string())),
            thread_state: None,
            compiler_notes: Vec::new(),
            relevant_memory: Vec::new(),
            short_term_candidates: Vec::new(),
            project_experience: Vec::new(),
            stable_facts: Vec::new(),
            user_policy: Vec::new(),
            open_decisions: Vec::new(),
            corrections: Vec::new(),
            evidence: Vec::new(),
        };

        attach_compiler_notes(
            &mut envelope,
            vec![crate::context::envelope::ContextSection::typed(
                "Local compiler note cites episode:7".to_string(),
                evidence,
                "local_compiler_note",
                "compiled",
                None,
            )],
        );

        let matches =
            crate::context::why::why_matches(&envelope, Some("compiler_notes"), Some("episode:7"));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["section"].as_str(), Some("compiler_notes"));
        assert!(matches[0]["reason"].as_str().unwrap().contains("local LLM compiler note"));
        assert_eq!(matches[0]["metadata"]["kind"].as_str(), Some("local_compiler_note"));
        assert_eq!(matches[0]["metadata"]["status"].as_str(), Some("compiled"));
        assert_eq!(matches[0]["evidence"][0]["kind"].as_str(), Some("episode"));
        assert_eq!(matches[0]["evidence"][0]["id"].as_str(), Some("7"));
    }

    /// D120-cand (R10 audit, 2026-04-30) — negative test for the
    /// JSON-RPC parse-error path. Malformed JSON on stdio must emit
    /// an error envelope with code `-32700` and `id: null`, never
    /// crash or silently drop the line.
    #[test]
    fn run_stdio_emits_parse_error_for_malformed_json() {
        use std::io::Cursor;

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("soma.db");
        // Trailing brace makes this not parseable as JSON.
        let input = b"{not valid json}\n";
        let mut writer = Cursor::new(Vec::<u8>::new());
        run_stdio(Cursor::new(&input[..]), &mut writer, &db_path).expect("loop returns on EOF");
        let output = String::from_utf8(writer.into_inner()).expect("utf-8 stdout");
        let parsed: Value = serde_json::from_str(output.trim()).expect("response is JSON");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], Value::Null);
        assert_eq!(parsed["error"]["code"], -32700);
    }
}
