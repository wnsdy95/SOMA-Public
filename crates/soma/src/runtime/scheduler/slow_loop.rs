//! Slow loop — Sleep Replay analog. Discussion 0037 §D91 / ADR
//! 0004 §B.
//!
//! Cycle (default 1 hour):
//!
//! 1. **Similar-episode merge** — for each (`hash-v1`) vector pair
//!    with cosine > 0.95, retire the *newer* episode by transferring
//!    its access_count to the older one and soft-deleting the
//!    duplicate (cold tier in D93). For now we mark via
//!    `note_pins.reason = 'merged-into:<id>'` audit trail.
//! 2. **Low-decay strip** — episodes whose Ebbinghaus
//!    `decay_weight` falls below `DEFAULT_COLD_TIER_THRESHOLD`
//!    *and* are not pinned *and* have access_count == 0 are
//!    surfaced for cold-tier demotion. v1.1 logs them; D93's
//!    schema split lands the actual demotion column.
//! 3. **Centroid recompute** — re-EMA the user_profile_centroid
//!    against the most recent N episodes so the salience kernel's
//!    `self_relevance` axis stays calibrated.
//! 4. **Open-decision review proposal pass** — turn unresolved L2
//!    contradiction/anomaly signals into request-verification proposals for the
//!    existing review queue. This does not resolve conflicts or write L4 facts.
//! 5. **Semantic learning proposal pass** — propose L4 semantic promotion only
//!    for repeated verified L3 claim evidence. The pass writes proposals, not
//!    direct semantic facts.
//! 6. **Learning critic proposal drain** — run the same safe review-drain
//!    policy exposed by the review CLI/MCP: batch-apply ready verified
//!    promotion proposals through storage gates. Decay/no-op/request-verification and unverified
//!    promotions stay in the explicit review path; no proposal ever
//!    becomes verification evidence by itself.
//!
//! These subpasses are *advisory* — the slow loop does not return
//! errors that abort `soma start`. Telemetry is emitted via
//! `tracing::info!` so a curious operator can see what each cycle
//! did.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::memory::forgetting::{
    decay_weight, DEFAULT_COLD_TIER_THRESHOLD, DEFAULT_LAMBDA, DEFAULT_MERGE_SIMILARITY,
};
use crate::memory::salience;
use crate::{
    context::open_decision_review::{propose_open_decision_reviews, OpenDecisionProposalInput},
    context::review_drain::{drain_review_queue, ReviewDrainInput},
    context::semantic_learning::{propose_semantic_consolidations, SemanticLearningInput},
    storage::{EpisodeId, LearningCriticApplyOutcome, Storage},
};
use tokio::sync::watch;

/// D133-cand close (R9 audit, 2026-04-30) — every Nth slow_loop cycle,
/// run a full O(N²) cosine scan on the most recent 500 episodes to
/// catch duplicates outside the EDGE_K=8 top-neighbors gate. Trades
/// 1 expensive pass per ~12 hours for lower per-ingest cost.
const RESILIENCE_SCAN_INTERVAL: u64 = 12;
const RESILIENCE_SCAN_WINDOW: usize = 500;
const LEARNING_CRITIC_PROPOSAL_DRAIN_LIMIT: usize = 32;
static RESILIENCE_SCAN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// D124+D131 — episode-delta gate state. Snapshot of `episode_count`
/// from the previous `run_one_cycle`; the next cycle computes
/// `delta = current - last`. Sentinel `-1` = cold start (delta
/// becomes `current_count` so a freshly-loaded resident still trains
/// on pre-existing rows).
///
/// `AtomicI64` (signed) is chosen over `AtomicU64` for the `-1`
/// sentinel; episode counts fit in `i64::MAX` (≈ 9.2 × 10^18). Single
/// global static (vs per-task counters) — all default gated tasks
/// (mLSTM / Hopfield + merge + backfill) consume the same
/// `episode_vectors` pool, so a shared idle signal is correct.
/// ANIL and iPC used to be part of this train set; ADR 0015 now
/// keeps them as explicit diagnostics only until they emit cited
/// ContextEnvelope fields.
static LAST_EPISODE_COUNT: AtomicI64 = AtomicI64::new(-1);

/// Test-only — reset `LAST_EPISODE_COUNT` to the `-1` sentinel between
/// integration tests that share the same binary. Production callers
/// should never invoke this; it exists because Rust integration tests
/// in the same `tests/<name>.rs` file share static state.
#[doc(hidden)]
pub fn reset_episode_delta_state_for_tests() {
    LAST_EPISODE_COUNT.store(-1, Ordering::SeqCst);
    // Round 1 in-house ultrareview fix: also reset RESILIENCE_SCAN_
    // COUNTER so the 12-cycle gate in `seed_beliefs_pass` fires
    // deterministically across shared-static integration tests in
    // the same binary. SeqCst pair with the load on line 257.
    RESILIENCE_SCAN_COUNTER.store(0, Ordering::SeqCst);
}

/// Tunable knobs.
#[derive(Debug, Clone, Copy)]
pub struct SlowLoopConfig {
    pub interval: Duration,
    pub delay_first: Duration,
    pub merge_similarity: f32,
    pub cold_tier_threshold: f32,
    pub lambda: f32,
}

impl SlowLoopConfig {
    /// 1 hour interval + 5 minute first-fire delay so the resident
    /// boots without an immediate consolidation pass burning CPU.
    pub const fn v1_default() -> Self {
        Self {
            interval: Duration::from_secs(3600),
            delay_first: Duration::from_secs(300),
            merge_similarity: DEFAULT_MERGE_SIMILARITY,
            cold_tier_threshold: DEFAULT_COLD_TIER_THRESHOLD,
            lambda: DEFAULT_LAMBDA,
        }
    }
}

impl Default for SlowLoopConfig {
    fn default() -> Self {
        Self::v1_default()
    }
}

impl SlowLoopConfig {
    /// D156-C close — config-aware variant. Reads
    /// `[memory] decay_lambda / merge_similarity / cold_tier_threshold`
    /// for the three forgetting/merge knobs. Time intervals stay
    /// hard-coded because they're invariants of the runtime
    /// architecture (SLO targets, not user knobs).
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let mem = &cfg.memory;
        Self {
            interval: Duration::from_secs(3600),
            delay_first: Duration::from_secs(300),
            merge_similarity: mem.merge_similarity,
            cold_tier_threshold: mem.cold_tier_threshold,
            lambda: mem.decay_lambda,
        }
    }
}

/// Statistics from one slow-loop cycle. Returned for tests +
/// telemetry; not persisted.
#[derive(Debug, Default, Clone, Copy)]
pub struct SlowLoopStats {
    pub merged_pairs: usize,
    pub cold_candidates: usize,
    pub centroid_updated: bool,
    pub compressed_groups: usize,
    pub narrative_synthesized: bool,
    /// v1.2 chunk 1.4 (ADR 0008 §D3) — number of mLSTM SGD steps
    /// taken in this cycle. `0` when `cognitive-train` feature is
    /// off, when no episode vectors exist, or when the train step
    /// fails. cumulative count (across cycles) lives in the
    /// `working_memory_weights.train_steps` BLOB column.
    pub train_steps: usize,
    /// ADR 0015 boundary — ANIL project_attribution can select
    /// default ContextEnvelope project scope when explicit filters are
    /// absent. The trainable ANIL slow-loop cycle does not run
    /// automatically, so this stays `0` outside explicit diagnostic
    /// calls to `run_train_anil_head`.
    pub anil_train_steps: usize,
    /// ADR 0015 boundary — iPC free-energy can emit cited
    /// `ContextEnvelope.open_decisions` anomalies at ingest time. The
    /// trainable iPC slow-loop cycle remains diagnostic and does not
    /// run automatically, so this stays `0` outside explicit calls to
    /// `run_train_pc_predictor`.
    pub pc_train_steps: usize,
    /// v1.2 chunk 4.4 (ADR 0011) — Hopfield Q/K/V SGD steps this
    /// cycle. `0` when feature off / no positive pairs in
    /// `episode_edges`.
    pub hopfield_train_steps: usize,
    /// P1-A external-review fix — Mini→Studio backfill: episodes
    /// originally embedded under MiniLM-L12 (or HashEmbedder) get
    /// a parallel `episode_vectors` row under e5-large 1024d so
    /// recall doesn't drop them after profile upgrade. `0` when
    /// not on Studio / no Mini-origin rows missing 1024d coverage.
    pub backfilled_vectors: usize,
    /// D133-cand close (R9 audit) — duplicate pairs detected by the
    /// periodic resilience scan that runs every 12 cycles. Catches
    /// duplicates outside the EDGE_K=8 top-neighbors gate. `0` on
    /// the 11/12 cycles where the scan is skipped, plus all cycles
    /// where no duplicates were found.
    pub merged_resilience: usize,
    /// D84 close (Batch 6, 2026-05-01) — belief candidate rows newly
    /// seeded this cycle. `seed_belief_candidates` walks the most
    /// recent episode window + their primary-model vectors and emits
    /// typed-relationship pairs (corroborates / contradicts) above a
    /// cosine threshold. `0` when the cycle was idle (`!should_train`),
    /// when fewer than 2 recent episodes exist, or when every candidate
    /// pair was already seeded on a prior cycle (UNIQUE constraint
    /// dedupes).
    pub beliefs_seeded: usize,
    /// Number of ready learning critic proposals that applied a
    /// lifecycle mutation through the gated apply-ready path in this
    /// cycle.
    pub learning_proposals_applied: usize,
    /// Number of open promotion proposals that remained blocked
    /// because durable trust was still missing. The slow loop does not
    /// turn this into verification evidence or mutate the proposal into
    /// a manual review decision.
    pub learning_proposals_waiting_verification: usize,
    /// Number of queued learning critic proposals already rejected by
    /// review when the worker observed them.
    pub learning_proposals_rejected: usize,
    /// Number of queued learning critic proposals that produced a
    /// no-op outcome.
    pub learning_proposals_noop: usize,
    /// Number of open learning critic proposals skipped by the
    /// apply-ready gate. This includes request-verification,
    /// decay/no-op proposals, and unverified promotions.
    pub learning_proposals_skipped: usize,
    /// Number of queued learning critic proposals that failed to
    /// apply due to storage/corruption errors. Errors are advisory
    /// and do not abort the slow-loop cycle.
    pub learning_proposals_errors: usize,
    /// Number of semantic promotion or semantic review-request proposals
    /// created from verified L3 claim evidence before the review drain ran.
    pub semantic_proposals_created: usize,
    /// Number of request-verification proposals created from unresolved
    /// L2 open decisions before the review drain ran.
    pub open_decision_proposals_created: usize,
}

/// Run the slow loop until `shutdown_rx` flips to `true`.
///
/// **Shutdown-signal invariant** (D127 lock; mirror of warm_loop
/// docstring). `tokio::sync::watch` is lossy — `changed.is_err() ||
/// *shutdown_rx.borrow()` is the correct exit shape: `is_err()`
/// catches sender drop, `*borrow()` catches missed transitions
/// regardless of edge count. Future refactors must preserve both
/// clauses; collapsing to either alone leaves the slow loop hung
/// on sender-drop or deaf to late shutdowns.
pub async fn run(
    storage: Arc<Mutex<Storage>>,
    mut shutdown_rx: watch::Receiver<bool>,
    cfg: SlowLoopConfig,
) -> Vec<SlowLoopStats> {
    // First-fire delay races against shutdown so a `soma stop`
    // issued during the boot window doesn't sit waiting for the
    // sleep (parallels D1 §B in warm_loop).
    tokio::select! {
        _ = tokio::time::sleep(cfg.delay_first) => {}
        changed = shutdown_rx.changed() => {
            if changed.is_err() || *shutdown_rx.borrow() {
                return Vec::new();
            }
        }
    }

    let mut interval = tokio::time::interval(cfg.interval);
    interval.tick().await;

    let mut history = Vec::new();
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let stats = run_one_cycle(&storage, &cfg);
                tracing::info!(
                    merged = stats.merged_pairs,
                    cold = stats.cold_candidates,
                    centroid = stats.centroid_updated,
                    "slow-loop cycle complete"
                );
                history.push(stats);
            }
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return history;
                }
            }
        }
    }
}

/// One slow-loop cycle, exposed for tests so they don't need a
/// running tokio runtime + interval to verify behaviour.
pub fn run_one_cycle(storage: &Arc<Mutex<Storage>>, cfg: &SlowLoopConfig) -> SlowLoopStats {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    // D124+D131 close (R8 partial → Batch 5 full, 2026-04-30) —
    // episode-delta gate. Train + merge + backfill skip when no NEW
    // episodes landed since the last cycle (an idle weekend
    // shouldn't keep retraining the same data). Centroid + narrative
    // + compression + maintenance stay time-driven — they consume
    // existing content that benefits from periodic refresh even
    // without new ingests. R8 first shipped a "count > 0" gate;
    // Batch 5 promotes to "delta > 0" via `LAST_EPISODE_COUNT`
    // (snapshotted at end of each cycle, sentinel `-1` = cold start).
    // `(current - last).max(0)` defends against the shrink case from
    // a forget operation between cycles.
    let episode_count_u64 =
        storage.lock().ok().and_then(|g| g.counters().ok().map(|(ep, _)| ep)).unwrap_or(0);
    let current_count = episode_count_u64 as i64;
    let last = LAST_EPISODE_COUNT.load(Ordering::SeqCst);
    let episode_delta = if last < 0 { current_count } else { (current_count - last).max(0) };
    let has_episodes = current_count > 0;
    let should_train = has_episodes && episode_delta > 0;

    let merged_pairs =
        if should_train { merge_similar_episodes(storage, cfg.merge_similarity) } else { 0 };
    let cold_candidates = if has_episodes { scan_cold_candidates(storage, cfg, now_ns) } else { 0 };
    let centroid_updated = recompute_centroid(storage);
    let compressed_groups = compress_repeated_signatures(storage);
    let narrative_synthesized = synthesize_narrative(storage);
    let train_steps = if should_train { train_mlstm(storage) } else { 0 };
    // ADR 0015 boundary: ANIL project_attribution participates in
    // explicit-filter-safe default ContextEnvelope scope selection.
    // Keep the explicit diagnostic `run_train_anil_head` entrypoint
    // for historical tests and operator inspection, but do not run it
    // from the default resident slow loop.
    let anil_train_steps = 0;
    // ADR 0015 boundary: ingest-time iPC free-energy can now emit
    // cited ContextEnvelope anomalies, but trainable iPC remains an
    // explicit diagnostic path. Keep `run_train_pc_predictor` for
    // historical tests and operator inspection, but do not run it
    // from the default resident slow loop.
    let pc_train_steps = 0;
    let hopfield_train_steps = if should_train { train_hopfield_head(storage) } else { 0 };
    let backfilled_vectors = if should_train { backfill_active_models(storage) } else { 0 };
    // D84 close (Batch 6) — seed belief candidates from the recent
    // episode window. Gated on `should_train` because the seeder
    // pulls *new* relationships from new episodes; on an idle delta
    // the UNIQUE constraint would silently absorb every call as 0
    // anyway, so the gate is just a CPU-saver, not a correctness
    // condition. Window=200 + threshold=0.85 are the v1 defaults
    // chosen in [[memory/beliefs.rs]] — operator-level tuning is a
    // follow-up `[memory] belief_window` / `belief_threshold` knob.
    let beliefs_seeded = if should_train { seed_beliefs_pass(storage) } else { 0 };
    // Policy rows are a live ContextEnvelope path: persisted
    // self_state(kind=policy) is later projected into
    // ContextEnvelope.user_policy. The extractor is deterministic and
    // uses only local episode statistics; legacy policy.md mirrors are
    // not emitted from the slow loop.
    if should_train {
        update_user_policy_pass(storage);
    }
    let open_decision_proposals_created = propose_open_decision_reviews_for_queue(storage);
    let semantic_proposals_created = propose_semantic_learning(storage);
    let learning_proposals = drain_learning_critic_proposals(storage);

    // D162 close — recurring chat_recall_trace pruning. This
    // legacy-named table is local debug/dashboard state, not the
    // cloud-LLM read path or bridge completion evidence. 30 days is
    // enough for operator recall-card inspection without accumulating
    // old diagnostic rows indefinitely.
    prune_chat_recall_trace_pass(storage);

    // D133-cand close (R9 audit, 2026-04-30) — resilience scan every
    // RESILIENCE_SCAN_INTERVAL cycles on the most recent
    // RESILIENCE_SCAN_WINDOW episodes. AtomicU64 counter avoids
    // threading a cycle index through `run_one_cycle` callers
    // (tests / unit calls). Skipped on idle (`!has_episodes`).
    let merged_resilience = if has_episodes {
        let cycle = RESILIENCE_SCAN_COUNTER.fetch_add(1, Ordering::SeqCst);
        if cycle % RESILIENCE_SCAN_INTERVAL == 0 {
            resilience_scan_recent_window(storage, cfg.merge_similarity)
        } else {
            0
        }
    } else {
        0
    };

    // Snapshot for the next cycle's delta. Always update (even on
    // `!has_episodes`) so a freshly-emptied DB doesn't leave a stale
    // sentinel that triggers spurious training on the next ingest.
    LAST_EPISODE_COUNT.store(current_count, Ordering::SeqCst);

    // D121-cand close (R7 audit, 2026-04-30) — advisory maintenance:
    // PRAGMA incremental_vacuum + optimize. Failure is non-fatal —
    // the cycle proceeds either way; vacuum just delays page reclaim
    // by one cycle. Always run (works on empty DB too).
    if let Ok(guard) = storage.lock() {
        if let Err(e) = guard.run_maintenance() {
            tracing::warn!(error = %e, "slow_loop maintenance pragmas failed (advisory)");
        }
    }

    SlowLoopStats {
        merged_pairs,
        cold_candidates,
        centroid_updated,
        compressed_groups,
        narrative_synthesized,
        train_steps,
        anil_train_steps,
        pc_train_steps,
        hopfield_train_steps,
        backfilled_vectors,
        merged_resilience,
        beliefs_seeded,
        learning_proposals_applied: learning_proposals.applied,
        learning_proposals_waiting_verification: learning_proposals.waiting_verification,
        learning_proposals_rejected: learning_proposals.rejected,
        learning_proposals_noop: learning_proposals.noop,
        learning_proposals_skipped: learning_proposals.skipped,
        learning_proposals_errors: learning_proposals.errors,
        semantic_proposals_created,
        open_decision_proposals_created,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct LearningCriticProposalDrainStats {
    applied: usize,
    waiting_verification: usize,
    rejected: usize,
    noop: usize,
    skipped: usize,
    errors: usize,
}

fn drain_learning_critic_proposals(
    storage: &Arc<Mutex<Storage>>,
) -> LearningCriticProposalDrainStats {
    let mut stats = LearningCriticProposalDrainStats::default();
    let drain_report = {
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        drain_review_queue(
            &mut guard,
            ReviewDrainInput {
                project: None,
                session_id: None,
                limit: LEARNING_CRITIC_PROPOSAL_DRAIN_LIMIT,
                dry_run: false,
            },
        )
    };
    let drain_report = match drain_report {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "learning critic review drain failed");
            return LearningCriticProposalDrainStats { errors: 1, ..Default::default() };
        }
    };

    for item in drain_report.apply_ready.items {
        match item.outcome {
            Some(LearningCriticApplyOutcome::Applied) => stats.applied += 1,
            Some(LearningCriticApplyOutcome::WaitingVerification) => {
                stats.waiting_verification += 1;
            }
            Some(LearningCriticApplyOutcome::Rejected) => stats.rejected += 1,
            Some(LearningCriticApplyOutcome::Noop) => stats.noop += 1,
            None => {
                stats.skipped += 1;
                if item.skipped_reason.as_deref() == Some("missing_confirmed_verification") {
                    stats.waiting_verification += 1;
                }
            }
        }
    }

    if stats.applied
        + stats.waiting_verification
        + stats.rejected
        + stats.noop
        + stats.skipped
        + stats.errors
        > 0
    {
        tracing::info!(
            applied = stats.applied,
            waiting_verification = stats.waiting_verification,
            rejected = stats.rejected,
            noop = stats.noop,
            skipped = stats.skipped,
            errors = stats.errors,
            policy = %drain_report.policy,
            manual_action_count_after = drain_report.manual_action_count_after,
            "slow-loop: learning critic safe review drain completed"
        );
    }
    stats
}

fn propose_semantic_learning(storage: &Arc<Mutex<Storage>>) -> usize {
    let report = {
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        propose_semantic_consolidations(
            &mut guard,
            SemanticLearningInput {
                project: None,
                session_id: None,
                limit: LEARNING_CRITIC_PROPOSAL_DRAIN_LIMIT,
                min_support: 2,
                dry_run: false,
            },
        )
    };
    match report {
        Ok(report) => {
            if report.proposed_count > 0
                || report.review_proposed_count > 0
                || report.skipped_existing_proposal_count > 0
                || report.skipped_existing_review_proposal_count > 0
                || report.skipped_existing_semantic_count > 0
            {
                tracing::info!(
                    proposed = report.proposed_count,
                    review_proposed = report.review_proposed_count,
                    repeated_groups = report.repeated_group_count,
                    skipped_existing_proposal = report.skipped_existing_proposal_count,
                    skipped_existing_review_proposal = report.skipped_existing_review_proposal_count,
                    skipped_existing_semantic = report.skipped_existing_semantic_count,
                    rule = %report.rule,
                    "slow-loop: semantic learning proposals evaluated"
                );
            }
            report.proposed_count + report.review_proposed_count
        }
        Err(e) => {
            tracing::warn!(error = %e, "semantic learning proposal pass failed (advisory)");
            0
        }
    }
}

fn propose_open_decision_reviews_for_queue(storage: &Arc<Mutex<Storage>>) -> usize {
    let report = {
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        propose_open_decision_reviews(
            &mut guard,
            OpenDecisionProposalInput {
                project: None,
                session_id: None,
                limit: LEARNING_CRITIC_PROPOSAL_DRAIN_LIMIT,
                dry_run: false,
            },
        )
    };
    match report {
        Ok(report) => {
            if report.proposed_count > 0 || report.skipped_existing_proposal_count > 0 {
                tracing::info!(
                    proposed = report.proposed_count,
                    inspected = report.inspected_signal_count,
                    skipped_existing_proposal = report.skipped_existing_proposal_count,
                    rule = %report.rule,
                    "slow-loop: open-decision review proposals evaluated"
                );
            }
            report.proposed_count
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "open-decision review proposal pass failed (advisory)"
            );
            0
        }
    }
}

/// D162 close — prune legacy-named `chat_recall_trace` diagnostic
/// rows older than 30 days. Cap chosen to balance: enough history for
/// dashboard timeline / recall card grep + low enough that a
/// year-old session's debug-noise doesn't accumulate as recall
/// surface noise.
fn prune_chat_recall_trace_pass(storage: &Arc<Mutex<Storage>>) {
    const PRUNE_DAYS: i64 = 30;
    let cutoff_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
        - (PRUNE_DAYS * 86_400 * 1_000_000_000);
    let mut guard = crate::util::mutex::lock_or_recover(storage);
    match guard.prune_chat_recall_trace_before(cutoff_ns) {
        Ok(n) if n > 0 => {
            tracing::info!(
                deleted = n,
                cutoff_days = PRUNE_DAYS,
                "slow-loop: chat_recall_trace pruned"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "chat_recall_trace prune failed (advisory)");
        }
    }
}

/// Extract active policy candidates and persist them to self_state so
/// ContextEnvelope.user_policy can cite them. This intentionally does not
/// write `~/.soma/policy/*.md`; hidden CLAUDE.md migration/debug commands render
/// markdown from self_state on demand.
fn update_user_policy_pass(storage: &Arc<Mutex<Storage>>) {
    // Resolve top-3 active project names from the recent episode window.
    let projects: Vec<Option<String>> = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        let recent = guard.recent_episodes(500).unwrap_or_default();
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
        let mut out: Vec<Option<String>> = Vec::with_capacity(4);
        out.push(None); // global
        for (name, _) in sorted.into_iter().take(3) {
            out.push(Some(name));
        }
        out
    };
    for project in projects {
        let guard = crate::util::mutex::lock_or_recover(storage);
        let rules = match crate::memory::policy::extract_policies(&guard, project.as_deref()) {
            Some(r) => r,
            None => {
                // no episodes / no repeated local signal / storage
                // failure — extract_policies 가 이미 traced.
                continue;
            }
        };
        drop(guard);
        {
            let mut guard = crate::util::mutex::lock_or_recover(storage);
            if let Err(e) =
                crate::memory::policy::upsert_policy_set(&mut guard, project.as_deref(), &rules)
            {
                tracing::warn!(
                    error = %e,
                    project = %project.as_deref().unwrap_or("global"),
                    "policy self_state write failed"
                );
            } else {
                tracing::info!(
                    rule_count = rules.len(),
                    project = %project.as_deref().unwrap_or("global"),
                    "slow-loop: user_policy self_state updated"
                );
            }
        }
    }
}

/// D84 close (Batch 6) — adapt `memory::beliefs::seed_belief_candidates`
/// to the slow_loop's `&Arc<Mutex<Storage>>` shape (rest of the cycle's
/// helpers follow this pattern). Failures degrade to `0` + a tracing
/// warning so a corrupt vector or transient lock contention doesn't
/// abort the cycle (consistent with backfill / compression / centroid
/// behavior).
///
/// Window 200 + threshold 0.85 are the defaults pinned in
/// `memory::beliefs` v1. Operator-level tuning is a follow-up
/// `[memory] belief_window` / `belief_threshold` config knob — not
/// landed yet because v1 dogfooding has only one user (jy) and one
/// machine, so default-tuning before any data is premature.
fn seed_beliefs_pass(storage: &Arc<Mutex<Storage>>) -> usize {
    // D156-A — belief seed knobs 가 이제 [memory] section 에서.
    // 기본값 200 / 0.85 와 동일.
    let mem_cfg =
        crate::config::Config::load_or_default(&dirs::home_dir().unwrap_or_default().join(".soma"))
            .memory;
    let belief_window: usize = mem_cfg.belief_window as usize;
    let belief_threshold: f32 = mem_cfg.belief_threshold;
    let mut guard = match storage.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    match crate::memory::beliefs::seed_belief_candidates(
        &mut guard,
        belief_window,
        belief_threshold,
    ) {
        Ok(n) => {
            if n > 0 {
                tracing::info!(seeded = n, "slow-loop: belief candidates seeded");
            }
            n
        }
        Err(e) => {
            tracing::warn!(error = %e, "seed_belief_candidates failed (advisory)");
            0
        }
    }
}

/// D133-cand close (R9 audit) — full O(N²) cosine scan on the most
/// recent `RESILIENCE_SCAN_WINDOW` episodes. Catches duplicates
/// outside the EDGE_K=8 top-neighbors gate that `merge_via_edges`
/// relies on. Returns the number of duplicate pairs detected (each
/// produces a `merged-via-resilience-scan:<older>` audit pin).
///
/// Cost ≈ 500² = 250k cosine evals at 1024d → tens of ms on Mini's
/// CPU. Gated by `RESILIENCE_SCAN_INTERVAL = 12` so this runs once
/// every ~12 hours of resident uptime, amortized.
/// R14 audit (2026-05-01) — exposed for integration tests so we can
/// pin the EDGE_K=8 false-negative recovery contract without spinning
/// up 12 actual slow_loop cycles. Production callers go through
/// `run_one_cycle` which gates the call on `RESILIENCE_SCAN_INTERVAL`.
#[doc(hidden)]
pub fn run_resilience_scan_for_tests(storage: &Arc<Mutex<Storage>>, threshold: f32) -> usize {
    resilience_scan_recent_window(storage, threshold)
}

fn resilience_scan_recent_window(storage: &Arc<Mutex<Storage>>, threshold: f32) -> usize {
    let rows = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match guard.vectors_for_model(crate::memory::embed::select_embedder().model_id()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "resilience_scan: vectors_for_model failed");
                return 0;
            }
        }
    };
    if rows.len() < 2 {
        return 0;
    }
    let mut sorted = rows;
    sorted.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    let window: Vec<(EpisodeId, Vec<f32>)> =
        sorted.into_iter().take(RESILIENCE_SCAN_WINDOW).collect();
    if window.len() < 2 {
        return 0;
    }

    // P2 fix (in-house ultrareview): O(N²) cosine scan no longer
    // re-acquires the storage mutex per-pair. The scan runs lock-free
    // over the in-memory `window` vector, accumulates pin candidates,
    // then commits them under a single lock acquisition. Removes the
    // 12h-cycle ingest stall caused by the earlier per-pair re-lock.
    let mut to_pin: Vec<(EpisodeId, EpisodeId, f32)> = Vec::new();
    let mut already_merged: std::collections::HashSet<EpisodeId> = Default::default();
    for i in 0..window.len() {
        if already_merged.contains(&window[i].0) {
            continue;
        }
        for j in (i + 1)..window.len() {
            if already_merged.contains(&window[j].0) {
                continue;
            }
            let sim = cosine_similarity(&window[i].1, &window[j].1);
            if sim < threshold {
                continue;
            }
            let (older, newer) = if window[i].0 < window[j].0 {
                (window[i].0, window[j].0)
            } else {
                (window[j].0, window[i].0)
            };
            to_pin.push((newer, older, sim));
            already_merged.insert(newer);
        }
    }

    let mut newly_merged = 0_usize;
    if !to_pin.is_empty() {
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        for (newer, older, sim) in to_pin {
            let reason = format!("merged-via-resilience-scan:{older}");
            if let Err(e) = guard.pin_episode(newer, &reason, sim) {
                tracing::warn!(error = %e, newer, older, "resilience_scan pin failed");
                continue;
            }
            newly_merged += 1;
        }
    }

    if newly_merged > 0 {
        tracing::info!(
            resilience_scan_merged = newly_merged,
            window_size = window.len(),
            "D133-cand resilience scan complete"
        );
    }
    newly_merged
}

/// On-demand narrative synthesis — the same body slow_loop uses
/// during its hourly cycle, exposed for `soma synthesize-narrative`
/// CLI so users can see the result without waiting for the
/// `delay_first` (5 min) + `interval` (1 hour) scheduling.
pub fn run_narrative_synthesis(storage: &Arc<Mutex<Storage>>) -> bool {
    synthesize_narrative(storage)
}

/// v1.2 chunk 1.4 (ADR 0008 §D3) — exposed for tests so they can
/// trigger one mLSTM training pass without spinning up the full
/// slow-loop interval.
pub fn run_train_mlstm(storage: &Arc<Mutex<Storage>>) -> usize {
    train_mlstm(storage)
}

/// v1.2 chunk 2.4 (ADR 0009 §D3) — explicit diagnostic entrypoint
/// for ANIL classifier training. ANIL project_attribution can select
/// default ContextEnvelope project scope, but `run_one_cycle` does
/// not call this trainable path by default.
pub fn run_train_anil_head(storage: &Arc<Mutex<Storage>>) -> usize {
    train_anil_head(storage)
}

/// v1.2 chunk 3.4 (ADR 0010) — explicit diagnostic entrypoint for
/// iPC predictor training. Ingest-time iPC free-energy can emit cited
/// `ContextEnvelope.open_decisions` anomalies, but this trainable
/// predictor path remains explicit diagnostics, so `run_one_cycle`
/// does not call it by default.
pub fn run_train_pc_predictor(storage: &Arc<Mutex<Storage>>) -> usize {
    train_pc_predictor(storage)
}

/// v1.2 chunk 4.4 (ADR 0011) — exposed for tests.
pub fn run_train_hopfield_head(storage: &Arc<Mutex<Storage>>) -> usize {
    train_hopfield_head(storage)
}

/// P1-A external-review fix — exposed for tests. D69 close
/// (2026-05-01) — now drains every active embedder (primary +
/// optional Studio secondary), not just the primary.
pub fn run_backfill_primary_model(storage: &Arc<Mutex<Storage>>) -> usize {
    backfill_active_models(storage)
}

/// P1-A backfill pass — for every active embedder (primary +
/// optional Studio secondary) and every episode whose
/// `episode_vectors` row lacks a parallel row for that embedder's
/// `model_id`, re-embed the episode_index_text and insert a new
/// row. Studio recall reads only the primary HNSW; Mini recall
/// reads only its own primary. The secondary row is purely
/// cross-profile backfill freight (plan §D3). Idempotent —
/// once a (episode, model_id) pair exists, subsequent cycles
/// skip it.
///
/// Cap at 64 episodes per active embedder per cycle so a 10K
/// backlog drains over hours instead of pinning a single
/// slow_loop tick. Caller may invoke repeatedly (the `soma backfill`
/// CLI does so until it returns 0).
fn backfill_active_models(storage: &Arc<Mutex<Storage>>) -> usize {
    let active = crate::memory::embed::select_active_embedders();
    let mut total = 0_usize;
    for embedder in &active {
        total += backfill_one_model(storage, embedder.as_ref());
    }
    total
}

fn backfill_one_model(
    storage: &Arc<Mutex<Storage>>,
    embedder: &dyn crate::memory::embed::Embedder,
) -> usize {
    let model_id = embedder.model_id();
    let dim = embedder.dim();

    const BACKFILL_CAP: usize = 64;

    // D159.5 close — single SQL anti-join replaces the two-call dance
    // (`vectors_for_model` HashSet build + `all_episodes` full dump).
    // The DB filters server-side, so we never load already-vectored
    // rows or vector blobs into memory just to discard them.
    let candidates: Vec<crate::storage::StoredEpisode> = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match guard.episodes_missing_vector_for(model_id, BACKFILL_CAP) {
            Ok(rows) => rows,
            Err(_) => return 0,
        }
    };

    let mut written = 0_usize;
    for ep in candidates {
        // Re-derive the same indexing text the ingest path used
        // (D90 §A invariant) so the re-embed represents the same
        // semantic content.
        let text = episode_index_text_for_backfill(&ep);
        if text.is_empty() {
            continue;
        }
        // D138 — backfill represents the *stored corpus* (re-emitting
        // ingest's vector for the same content), so this is the
        // passage-side embed. Non-e5 backends fall through to the
        // symmetric default.
        let v = embedder.embed_passage(&text);
        if v.len() != dim || !v.iter().all(|x| x.is_finite()) {
            continue;
        }
        // P2 fix (in-house ultrareview): double-check membership under
        // the same lock as the `put_vector`. Between the SQL snapshot
        // and now, a concurrent ingest may have already written this
        // episode's vector for `model_id`. Without the re-check, the
        // UNIQUE constraint absorbs the duplicate but we'd waste an
        // embed call + lock acquisition.
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        if let Ok(rows) = guard.vectors_for_model(model_id) {
            if rows.iter().any(|(id, _)| *id == ep.id) {
                continue;
            }
        }
        if guard.put_vector(ep.id, model_id, &v).is_ok() {
            written += 1;
        }
    }
    if written > 0 {
        tracing::info!(
            backfilled = written,
            target_model = model_id,
            target_dim = dim,
            "backfill_one_model: filled missing rows for upgrade/dual-store path"
        );
    }
    written
}

/// Mirror of `capture::ai_cli::episode_index_text` — the slow_loop
/// can't reach into the private fn there, so we replicate the
/// same join order so backfilled vectors are *bit-for-bit* the
/// same content the original ingest would have indexed. Codex 2차
/// review (2026-04-28) flagged that the first version pushed
/// empty-string parts where ingest skips them — fixed: the
/// `!s.is_empty()` guards mirror `ai_cli::episode_index_text`
/// exactly.
fn episode_index_text_for_backfill(ep: &crate::storage::StoredEpisode) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    if let Some(p) = ep.prompt_text.as_deref() {
        if !p.is_empty() {
            parts.push(p);
        }
    }
    if let Some(r) = ep.response_text.as_deref() {
        if !r.is_empty() {
            parts.push(r);
        }
    }
    if let Some(c) = ep.command.as_deref() {
        if !c.is_empty() {
            parts.push(c);
        }
    }
    parts.join("\n")
}

/// AdamW learning rate shared across all 4 trainable chunks
/// — mLSTM + ANIL + iPC + Hopfield. Hosted as a single const to
/// prevent drift between chunks. Chunk 1.2 reduced from 0.05 to
/// 0.01 after divergence; the value has held since. Future tuning
/// is tracked as a deferred config knob — Frobenius Δ plateau is
/// the trigger. `cognitive-train`-gated since the train paths
/// themselves are feature-conditional.
#[cfg(feature = "cognitive-train")]
const TRAIN_LR_ADAMW: f64 = 0.01;

/// Sub-unit-norm input scaling for training samples. v1 inputs are
/// L2-normalized embeddings (unit-norm); identity-init projections
/// produce output norm of about 1.0, which gives near-zero gradient
/// signal at the first step. Scaling to about 0.3 still injects
/// signal while keeping the input under one. Used by all 4 train
/// paths.
#[cfg(feature = "cognitive-train")]
const TRAIN_INPUT_SCALE: f32 = 0.3;

/// One slow-loop pass of mLSTM training. Loads stored weights (or
/// fresh identity init), pulls every available episode embedding
/// as a training batch, takes one SGD step per sample, persists
/// updated weights. Returns the number of steps actually taken.
///
/// `cognitive-train` feature off → always returns 0. `cognitive-
/// train` on but no embeddings → 0. Step count == sample count
/// taken from the embedding pool.
#[cfg(feature = "cognitive-train")]
fn train_mlstm(storage: &Arc<Mutex<Storage>>) -> usize {
    use crate::memory::cognitive::mlstm_trainable::TrainableMLstm;

    // Pull every (id, vec) under the active embedder's model_id.
    // We don't care about ids here — only the vector shape + count.
    let model_id = crate::memory::embed::select_embedder().model_id();
    let rows = match crate::util::mutex::lock_or_recover(storage).vectors_for_model(model_id) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "train_mlstm: vectors_for_model failed");
            return 0;
        }
    };
    if rows.is_empty() {
        return 0;
    }
    let dim = rows[0].1.len();
    if dim == 0 {
        return 0;
    }

    // Construct (or restore) the trainable cell.
    let cell = match TrainableMLstm::new_identity(dim) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "train_mlstm: identity init failed");
            return 0;
        }
    };
    if let Ok(Some((stored_dim, w_q, w_k, w_v, steps, _))) =
        crate::util::mutex::lock_or_recover(storage).get_working_memory_weights()
    {
        if stored_dim == dim && cell.import_weights(w_q, w_k, w_v) {
            cell.set_train_steps(steps);
        }
    }

    // Shuffle the batch — full-pool sequential ordering biases the
    // optimizer toward the last few samples (last-batch wins under
    // SGD-with-momentum). Deterministic seed = wall-clock-ns so each
    // cycle sees a different ordering, while a single cycle stays
    // reproducible for debugging.
    let mut shuffled: Vec<&(EpisodeId, Vec<f32>)> = rows.iter().collect();
    shuffle_in_place(&mut shuffled, now_ns_seed());

    // One AdamW pass over the batch. AdamW's adaptive moments give
    // ~1000× larger effective step on identity-near weights than
    // chunk 1.2's fixed-lr SGD did.
    let lr = TRAIN_LR_ADAMW;
    let mut taken = 0_usize;
    let mut first_loss: Option<f32> = None;
    let mut last_loss: f32 = 0.0;
    let mut diverged = 0_usize;
    for (_id, vec) in shuffled {
        if vec.len() != dim {
            continue;
        }
        // Scale into the sub-unit-norm regime so identity-near
        // weights have gradient signal (see mlstm_trainable.rs ::
        // tests::nontrivial_input docstring).
        let scaled: Vec<f32> = vec.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        match cell.train_step(&scaled, lr) {
            Ok(loss) => {
                if loss.is_finite() {
                    if first_loss.is_none() {
                        first_loss = Some(loss);
                    }
                    last_loss = loss;
                    taken += 1;
                } else {
                    // train_step layer-1 NaN guard already skipped the
                    // SGD step; we just track + log the divergent batch.
                    diverged += 1;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "train_mlstm: train_step failed mid-batch");
                break;
            }
        }
    }
    if taken == 0 {
        return 0;
    }
    tracing::info!(
        steps = taken,
        diverged = diverged,
        loss_first = first_loss.unwrap_or(f32::NAN),
        loss_last = last_loss,
        "train_mlstm: batch complete"
    );

    // Persist.
    let (q, k, v) = match cell.export_weights() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "train_mlstm: export_weights failed");
            return 0;
        }
    };
    // P2 fix (in-house ultrareview): if save fails, in-memory train_
    // steps moves forward but the persisted counter does not, so the
    // next cycle's AdamW LR schedule desyncs from the actual step
    // count. Abort the cycle (return 0) so the next cycle retries
    // cleanly from the prior persisted state.
    if let Err(e) = crate::util::mutex::lock_or_recover(storage).save_working_memory_weights(
        dim,
        &q,
        &k,
        &v,
        cell.train_steps(),
    ) {
        tracing::warn!(error = %e, "train_mlstm: save_working_memory_weights failed (cycle aborted)");
        return 0;
    }
    taken
}

/// Stub when `cognitive-train` feature is off — never trains.
#[cfg(not(feature = "cognitive-train"))]
fn train_mlstm(_storage: &Arc<Mutex<Storage>>) -> usize {
    0
}

/// One slow-loop pass of ANIL classifier training. Pulls every
/// labeled episode (project = Some) + its embedding, ensures the
/// project label exists in the head's row mapping, then runs
/// cross-entropy SGD over the (features, label) batch. Persists
/// `anil_head_weights` + the project-attribution self_state row.
/// ADR 0015 boundary: this does not choose `ContextEnvelope.scope`;
/// it is diagnostic until a future P4 slice wires scope selection.
///
/// Returns the number of SGD steps actually taken. `0` when:
///   * `cognitive-train` off
///   * no labeled episodes
///   * K = 1 (single project — gradient is zero by design)
#[cfg(feature = "cognitive-train")]
fn train_anil_head(storage: &Arc<Mutex<Storage>>) -> usize {
    use crate::memory::cognitive::anil_classifier::AnilClassifier;

    // Pull (id, project, vector) triples for every labeled episode.
    let model_id = crate::memory::embed::select_embedder().model_id();
    let dim = crate::memory::embed::select_embedder().dim();
    let labeled_pool = match collect_labeled_pool(storage, model_id, dim) {
        Some(p) => p,
        None => return 0,
    };
    if labeled_pool.is_empty() {
        return 0;
    }

    // Collect the unique project set in deterministic order so the
    // K-row layout is stable across cycles.
    let mut unique_projects: Vec<String> =
        labeled_pool.iter().map(|(_, project, _)| project.clone()).collect();
    unique_projects.sort();
    unique_projects.dedup();
    if unique_projects.is_empty() {
        return 0;
    }

    let cell = match AnilClassifier::new_seed(dim, &unique_projects[0]) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "train_anil_head: seed init failed");
            return 0;
        }
    };
    // Restore prior weights when persisted; fall back to fresh seed
    // on shape mismatch.
    if let Ok(Some((stored_dim, w, b, projects, steps, _))) =
        crate::util::mutex::lock_or_recover(storage).get_anil_head_weights()
    {
        if stored_dim == dim && cell.import(w, b, projects) {
            cell.set_train_steps(steps);
        }
    }

    // Add any new project rows after restore.
    for project in &unique_projects {
        if let Err(e) = cell.ensure_project(project) {
            tracing::warn!(error = %e, project = project.as_str(), "train_anil_head: ensure_project failed");
        }
    }

    // K = 1 → zero-init weights produce zero gradient (loss = 0
    // identically). Skip training but still write the row so the
    // persistence path stays primed.
    if cell.num_classes() <= 1 {
        let (w, b, projects) = match cell.export() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let _ = crate::util::mutex::lock_or_recover(storage).save_anil_head_weights(
            dim,
            &w,
            &b,
            &projects,
            cell.train_steps(),
        );
        return 0;
    }

    // Build a project-→-row-index lookup once (O(K)).
    let projects_now = cell.projects();
    let lr = TRAIN_LR_ADAMW;
    let mut taken = 0_usize;
    let mut diverged = 0_usize;
    let mut first_loss: Option<f32> = None;
    let mut last_loss = 0.0_f32;

    // Shuffle the labeled pool — same anti-bias trick as mLSTM.
    let mut shuffled: Vec<&(crate::storage::EpisodeId, String, Vec<f32>)> =
        labeled_pool.iter().collect();
    shuffle_in_place(&mut shuffled, now_ns_seed());

    for (_id, project, vec) in shuffled {
        let label_idx = match projects_now.iter().position(|p| p == project) {
            Some(i) => i,
            None => continue,
        };
        // Sub-unit-norm regime same as mLSTM — keeps cross-entropy
        // gradient finite even on identity-init.
        let scaled: Vec<f32> = vec.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        match cell.train_step(&scaled, label_idx, lr) {
            Ok(loss) => {
                if loss.is_finite() {
                    if first_loss.is_none() {
                        first_loss = Some(loss);
                    }
                    last_loss = loss;
                    taken += 1;
                } else {
                    diverged += 1;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "train_anil_head: train_step failed mid-batch");
                break;
            }
        }
    }

    if taken == 0 {
        return 0;
    }
    tracing::info!(
        steps = taken,
        diverged = diverged,
        loss_first = first_loss.unwrap_or(f32::NAN),
        loss_last = last_loss,
        num_classes = cell.num_classes(),
        "train_anil_head: batch complete"
    );

    // Persist updated weights.
    let (w, b, projects) = match cell.export() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "train_anil_head: export failed");
            return taken;
        }
    };
    if let Err(e) = crate::util::mutex::lock_or_recover(storage).save_anil_head_weights(
        dim,
        &w,
        &b,
        &projects,
        cell.train_steps(),
    ) {
        tracing::warn!(error = %e, "train_anil_head: save failed");
    }

    // chunk 2.5 — write the project-attribution self_state row.
    // ADR 0015 boundary: diagnostic only; the envelope compiler
    // does not consult this row for scope selection today.
    let _ = write_project_attribution(storage, &cell, &labeled_pool);

    taken
}

#[cfg(not(feature = "cognitive-train"))]
fn train_anil_head(_storage: &Arc<Mutex<Storage>>) -> usize {
    0
}

/// Optional iPC diagnostic training pass. Pulls every embedding for
/// the active model and decomposes each into a truncation hierarchy
/// (`PC_DIMS = [d, d/2, d/4, d/8]`). Each (latents, lr) tuple is one
/// SGD step; persistence per-layer at the end.
///
/// `cognitive-train` off → 0. No embeddings → 0. Anything finer
/// than truncation is outside the current ContextEnvelope path.
///
/// ADR 0015 boundary: this persists predictor diagnostics only. The
/// learned iPC signal does not feed pinning, `ContextEnvelope`, or
/// `open_decisions` until a future P4 slice wires that output.
#[cfg(feature = "cognitive-train")]
fn train_pc_predictor(storage: &Arc<Mutex<Storage>>) -> usize {
    use crate::memory::cognitive::ipc_trainable::TrainablePc;

    let model_id = crate::memory::embed::select_embedder().model_id();
    let dim = crate::memory::embed::select_embedder().dim();
    let rows = match crate::util::mutex::lock_or_recover(storage).vectors_for_model(model_id) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "train_pc_predictor: vectors_for_model failed");
            return 0;
        }
    };
    if rows.is_empty() || dim < 8 {
        return 0;
    }
    // iPC truncation hierarchy.
    let dims = vec![dim, dim / 2, dim / 4, dim / 8];
    let pc = match TrainablePc::new(dims.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "train_pc_predictor: new failed");
            return 0;
        }
    };
    // Restore prior layers when shape matches.
    let stored =
        crate::util::mutex::lock_or_recover(storage).get_pc_predictor_layers().unwrap_or_default();
    // P0-2 (audit fix) — TrainablePc has a single global
    // `train_steps` counter (ipc_trainable.rs:32) but persistence
    // writes the same value to every layer row (slow_loop persist
    // path below). Restore via max(layer_steps) so a stale layer-0
    // row from a divergent prior cycle doesn't undercount the real
    // counter; pre-fix the `lid == 0` guard misread the schema as
    // layer-keyed when it's actually a single counter mirrored.
    let mut max_steps: u64 = 0;
    for (lid, d_in, d_out, w, steps, _) in stored {
        if lid < dims.len() - 1
            && d_out == dims[lid]
            && d_in == dims[lid + 1]
            && pc.import_layer(lid, w)
        {
            max_steps = max_steps.max(steps);
        }
    }
    if max_steps > 0 {
        pc.set_train_steps(max_steps);
    }

    // Shuffle + train.
    let mut shuffled: Vec<&(crate::storage::EpisodeId, Vec<f32>)> = rows.iter().collect();
    shuffle_in_place(&mut shuffled, now_ns_seed());
    let lr = TRAIN_LR_ADAMW;
    let mut taken = 0_usize;
    for (_id, vec) in shuffled {
        if vec.len() != dim {
            continue;
        }
        // Hierarchical truncation — each layer = first dims[l] elements
        // of the previous (no PCA, just take). Sub-unit-norm via 0.3
        // scaling for stable gradient.
        let scaled: Vec<f32> = vec.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        let latents: Vec<Vec<f32>> =
            dims.iter().map(|&d| scaled.iter().take(d).copied().collect()).collect();
        if let Ok(loss) = pc.train_step(&latents, lr) {
            if loss.is_finite() {
                taken += 1;
            }
        }
    }
    if taken == 0 {
        return 0;
    }
    // Persist every layer.
    for l in 0..dims.len() - 1 {
        if let Ok(w) = pc.export_layer(l) {
            let _ = crate::util::mutex::lock_or_recover(storage).save_pc_predictor_layer(
                l,
                dims[l + 1],
                dims[l],
                &w,
                pc.train_steps(),
            );
        }
    }
    tracing::info!(steps = taken, num_layers = dims.len(), "train_pc_predictor: batch complete");
    taken
}

#[cfg(not(feature = "cognitive-train"))]
fn train_pc_predictor(_storage: &Arc<Mutex<Storage>>) -> usize {
    0
}

/// v1.2 chunk 4.4 (ADR 0011 §D3) — slow_loop pass for the
/// trainable Hopfield head. Pulls positive pairs from
/// `episode_edges` (D92 graph) — these are episodes the v1
/// kernel already flagged as semantically adjacent, so they make
/// strong contrastive ground truth.
///
/// Empty edge graph → 0. Edge-pool with no vector overlap → 0.
#[cfg(feature = "cognitive-train")]
fn train_hopfield_head(storage: &Arc<Mutex<Storage>>) -> usize {
    use crate::memory::cognitive::hopfield_trainable::TrainableHopfield;

    let model_id = crate::memory::embed::select_embedder().model_id();
    let dim = crate::memory::embed::select_embedder().dim();

    // Pull (left, right, sim) edges + a vector lookup.
    type EdgeList = Vec<(crate::storage::EpisodeId, crate::storage::EpisodeId, f32)>;
    type VecMap = std::collections::HashMap<crate::storage::EpisodeId, Vec<f32>>;
    let (edges, vec_map): (EdgeList, VecMap) = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        let edges = match guard.all_edges() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "train_hopfield_head: all_edges failed");
                return 0;
            }
        };
        let vectors = match guard.vectors_for_model(model_id) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let map: std::collections::HashMap<_, _> =
            vectors.into_iter().filter(|(_, v)| v.len() == dim).collect();
        (edges, map)
    };
    if edges.is_empty() || vec_map.is_empty() {
        return 0;
    }

    let cell = match TrainableHopfield::new_identity(dim) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "train_hopfield_head: new failed");
            return 0;
        }
    };
    if let Ok(Some((stored_dim, _heads, q, k, v, steps, _))) =
        crate::util::mutex::lock_or_recover(storage).get_hopfield_weights()
    {
        if stored_dim == dim && cell.import(q, k, v) {
            cell.set_train_steps(steps);
        }
    }

    // Train each (left, right) edge: query = left vector, ground_truth
    // = right vector. cos = identity init at start (since V = I). Loss
    // gradient pushes V toward 'right is the true read for left's
    // query'. Real signal builds across cycles.
    let lr = TRAIN_LR_ADAMW;
    let mut taken = 0_usize;
    let mut shuffled: Vec<&(crate::storage::EpisodeId, crate::storage::EpisodeId, f32)> =
        edges.iter().collect();
    shuffle_in_place(&mut shuffled, now_ns_seed());
    for (left, right, _sim) in shuffled {
        let lv = match vec_map.get(left) {
            Some(v) => v,
            None => continue,
        };
        let rv = match vec_map.get(right) {
            Some(v) => v,
            None => continue,
        };
        // Sub-unit-norm scaling for stable gradient.
        let q_scaled: Vec<f32> = lv.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        let gt_scaled: Vec<f32> = rv.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        if let Ok(loss) = cell.train_step(&q_scaled, &gt_scaled, lr) {
            if loss.is_finite() {
                taken += 1;
            }
        }
    }
    if taken == 0 {
        return 0;
    }
    let (q, k, v) = match cell.export() {
        Ok(t) => t,
        Err(_) => return taken,
    };
    let _ = crate::util::mutex::lock_or_recover(storage).save_hopfield_weights(
        dim,
        1,
        &q,
        &k,
        &v,
        cell.train_steps(),
    );
    tracing::info!(steps = taken, edges = edges.len(), "train_hopfield_head: batch complete");
    taken
}

#[cfg(not(feature = "cognitive-train"))]
fn train_hopfield_head(_storage: &Arc<Mutex<Storage>>) -> usize {
    0
}

/// Pull `(id, project, vector)` triples for every labeled episode
/// under `model_id`. `dim` filters out vectors of the wrong shape
/// (an old embedder's leftover row) so the train batch stays
/// homogeneous.
#[cfg(feature = "cognitive-train")]
#[allow(clippy::type_complexity)]
fn collect_labeled_pool(
    storage: &Arc<Mutex<Storage>>,
    model_id: &str,
    dim: usize,
) -> Option<Vec<(crate::storage::EpisodeId, String, Vec<f32>)>> {
    let guard = crate::util::mutex::lock_or_recover(storage);
    let vector_rows = guard.vectors_for_model(model_id).ok()?;
    let episodes = guard.all_episodes().ok()?;
    drop(guard);

    let id_to_project: std::collections::HashMap<crate::storage::EpisodeId, String> =
        episodes.into_iter().filter_map(|ep| ep.project.map(|p| (ep.id, p))).collect();

    let mut out = Vec::new();
    for (id, vec) in vector_rows {
        if vec.len() != dim {
            continue;
        }
        if let Some(project) = id_to_project.get(&id) {
            out.push((id, project.clone(), vec));
        }
    }
    Some(out)
}

/// chunk 2.5 — for each episode in `labeled_pool`, run the trained
/// head's forward pass and persist a `self_state` row keyed
/// `("anil", "project_attribution")` with the K-class probability
/// distribution averaged across episodes. `soma profile` rendering
/// picks this up so the user sees the trained head's view of their
/// project mix. ADR 0015 boundary: the ContextEnvelope scope adapter
/// may consume this row only when no explicit project/session filter
/// is present. Failure is advisory — log + continue.
#[cfg(feature = "cognitive-train")]
fn write_project_attribution(
    storage: &Arc<Mutex<Storage>>,
    cell: &crate::memory::cognitive::anil_classifier::AnilClassifier,
    pool: &[(crate::storage::EpisodeId, String, Vec<f32>)],
) -> bool {
    let projects = cell.projects();
    let k = projects.len();
    if k == 0 || pool.is_empty() {
        return false;
    }
    // Average softmax distribution over the labeled pool.
    let mut acc = vec![0.0_f32; k];
    let mut counted = 0_usize;
    for (_id, _label, vec) in pool {
        let scaled: Vec<f32> = vec.iter().map(|v| v * TRAIN_INPUT_SCALE).collect();
        match cell.forward(&scaled) {
            Ok(probs) if probs.len() == k => {
                for (i, p) in probs.iter().enumerate() {
                    acc[i] += p;
                }
                counted += 1;
            }
            _ => {}
        }
    }
    if counted == 0 {
        return false;
    }
    for slot in acc.iter_mut() {
        *slot /= counted as f32;
    }
    let mut entries: Vec<serde_json::Value> = Vec::with_capacity(k);
    for (project, prob) in projects.iter().zip(acc.iter()) {
        entries.push(serde_json::json!({ "project": project, "probability": prob }));
    }
    let value = serde_json::json!({
        "k": k,
        "episode_count": counted,
        "distribution": entries,
    });
    let res = crate::util::mutex::lock_or_recover(storage).upsert_self_fact(
        "anil",
        "project_attribution",
        &value.to_string(),
        &[],
    );
    res.is_ok()
}

/// Legacy off-hot-path narrative diagnostic.
///
/// 1. Compute the rule-based paragraph (always-on, frozen-weights).
/// 2. If `llm-summary` cargo feature is on AND `~/.soma/secrets.toml`
///    carries an Anthropic key, replace it with the historical
///    Anthropic-assisted paragraph. On *any* LLM failure (network,
///    no key, parse error), fall back to the rule-based result; the
///    slow_loop is advisory and should never abort.
///
/// Both paths write to `self_state.narrative.paragraph_md` with
/// `kind` = `"rule"` or `"llm"`, so downstream context consumers can
/// tell which diagnostic path they are seeing. This does not produce
/// ContextEnvelope `compiler_notes`, select envelope sections, or
/// define the cloud/local bridge.
fn synthesize_narrative(storage: &Arc<Mutex<Storage>>) -> bool {
    let rule_paragraph = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match crate::memory::narrative::synthesize_paragraph(&guard) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "narrative rule synthesis failed");
                return false;
            }
        }
    };
    if rule_paragraph.is_empty() {
        return false;
    }

    // The legacy Anthropic HTTP round-trip inside `synthesize_with_llm`
    // runs with the storage lock held. slow_loop's 1-hour cadence
    // makes that acceptable, and Haiku's 30s read timeout caps the
    // worst case. Any failure (feature off / no secret / network /
    // API) collapses to None and the rule paragraph wins.
    let llm_paragraph = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        crate::memory::narrative::synthesize_with_llm(&guard)
    };

    let (final_paragraph, kind) = match llm_paragraph {
        Some(p) => (p, "llm"),
        None => (rule_paragraph, "rule"),
    };

    let mut guard = crate::util::mutex::lock_or_recover(storage);
    guard.update_narrative(&final_paragraph, kind).is_ok()
}

/// D93 §E — repeated-pattern compression. For each
/// `summary_signature` group with ≥ 2 members and intra-group
/// cosine > 0.98, collapse to a single representative (oldest)
/// whose `summary_count` becomes the group size. The duplicates
/// are pinned with `reason='compressed-into:<rep>'` so context
/// rendering can dedupe (and forget audit retains the
/// trail). Returns the number of groups that produced a
/// compression.
fn compress_repeated_signatures(storage: &Arc<Mutex<Storage>>) -> usize {
    let groups = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        guard.episodes_by_signature().unwrap_or_default()
    };
    let mut compressed = 0usize;
    let model_id = crate::memory::embed::select_embedder().model_id();

    for (_sig, ids) in groups {
        if ids.len() < 2 {
            continue;
        }
        // Pull the (vector, id) pairs for this group.
        let vectors: Vec<(EpisodeId, Vec<f32>)> = {
            let guard = crate::util::mutex::lock_or_recover(storage);
            let all = guard.vectors_for_model(model_id).unwrap_or_default();
            let lookup: std::collections::HashMap<EpisodeId, Vec<f32>> = all.into_iter().collect();
            ids.iter().filter_map(|id| lookup.get(id).map(|v| (*id, v.clone()))).collect()
        };
        if vectors.len() < 2 {
            continue;
        }
        // Cluster check: require every member to be > 0.98 cos
        // against the oldest. Heterogeneous groups (same signature
        // but actually different content) are left alone.
        let rep = vectors[0].0;
        let rep_vec = &vectors[0].1;
        let all_close = vectors[1..].iter().all(|(_, v)| cosine_similarity(rep_vec, v) > 0.98);
        if !all_close {
            continue;
        }
        // Collapse — bump rep's summary_count (keep its signature
        // so future cycles still find it), strip duplicates'
        // signatures so they're invisible to the next compression
        // pass, pin them as compressed-into for audit.
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        let new_count = vectors.len() as u64;
        let rep_sig = guard.summary_metadata(rep).ok().flatten().and_then(|(_, s)| s);
        if guard.update_summary_metadata(rep, new_count, rep_sig.as_deref()).is_ok() {
            for (id, _) in vectors[1..].iter() {
                let reason = format!("compressed-into:{rep}");
                let _ = guard.pin_episode(*id, &reason, 1.0);
                let _ = guard.update_summary_metadata(*id, 1, None);
            }
            compressed += 1;
        }
    }
    compressed
}

/// Polish — wall-clock-ns seed for the per-cycle batch shuffle.
/// Distinct cycles see distinct orderings while one cycle stays
/// reproducible for debugging (same seed → same order). Falls back
/// to 1 on the (impossible) `SystemTime` error path.
#[cfg(feature = "cognitive-train")]
fn now_ns_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1 // never zero (SplitMix64 with seed 0 outputs all zeros)
}

/// Polish — Fisher-Yates shuffle with a SplitMix64 PRNG seeded
/// from `seed`. We don't pull `rand` into the dep tree just for
/// the slow_loop train batch; SplitMix64 fits in a few lines and
/// its statistical quality is fine for shuffle order.
#[cfg(feature = "cognitive-train")]
fn shuffle_in_place<T>(slice: &mut [T], seed: u64) {
    let mut state = seed;
    for i in (1..slice.len()).rev() {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        let j = (z as usize) % (i + 1);
        slice.swap(i, j);
    }
}

/// Pair-wise compare every (`hash-v1`) vector. For cosine >
/// `threshold`, treat as "similar enough to merge". v1.1 audits
/// via `note_pins.reason = 'merged-into:<older>'`. The merged
/// (newer) episode keeps its row but inherits a "merged" flag in
/// the pin so context rendering can dedupe.
///
/// O(N²) — fine for v1's ≤10K episode budget. A future incremental
/// path would use HNSW edges (D92).
/// P1-B external-review fix — pre-fix merge_similar_episodes
/// did O(N²) over every (id, vec) pair under the active model_id.
/// At 10K episodes that's ~50M pair-cosines × 384/1024 floats =
/// hours on Mini's CPU budget. Post-fix: gate by episode count.
/// Below `MERGE_FULL_SCAN_CAP` we keep the exhaustive scan (small
/// pools benefit from finding any duplicate pair); above it we
/// only consider pairs already adjacent in the D92 `episode_edges`
/// graph (which the ingest path itself populates with cosine >
/// 0.5 top-k neighbors). edge graph is O(E) and bounded by the
/// per-episode top-k cap.
const MERGE_FULL_SCAN_CAP: usize = 1024;

fn merge_similar_episodes(storage: &Arc<Mutex<Storage>>, threshold: f32) -> usize {
    let rows = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match guard.vectors_for_model(crate::memory::embed::select_embedder().model_id()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "merge: vectors_for_model failed");
                return 0;
            }
        }
    };
    if rows.len() < 2 {
        return 0;
    }

    // Pool small enough for the exhaustive O(N²) scan? Use it.
    if rows.len() <= MERGE_FULL_SCAN_CAP {
        return merge_full_scan(storage, rows, threshold);
    }
    // Large pool — restrict to D92 episode_edges candidates.
    merge_via_edges(storage, rows, threshold)
}

fn merge_full_scan(
    storage: &Arc<Mutex<Storage>>,
    rows: Vec<(EpisodeId, Vec<f32>)>,
    threshold: f32,
) -> usize {
    let mut sorted = rows;
    sorted.sort_by_key(|(id, _)| *id);

    let mut merged = 0usize;
    let mut already_merged: std::collections::HashSet<EpisodeId> = Default::default();

    for i in 0..sorted.len() {
        if already_merged.contains(&sorted[i].0) {
            continue;
        }
        for j in (i + 1)..sorted.len() {
            if already_merged.contains(&sorted[j].0) {
                continue;
            }
            let sim = cosine_similarity(&sorted[i].1, &sorted[j].1);
            if sim >= threshold {
                let older = sorted[i].0;
                let newer = sorted[j].0;
                let reason = crate::storage::AuditReason::MergedInto(older).to_wire();
                let mut guard = crate::util::mutex::lock_or_recover(storage);
                if let Err(e) = guard.pin_episode(newer, &reason, sim) {
                    tracing::warn!(error = %e, newer, older, "merge pin failed");
                    continue;
                }
                already_merged.insert(newer);
                merged += 1;
            }
        }
    }
    merged
}

/// P1-B large-pool path — opportunistic merge restricted to pairs
/// already present in the D92 `episode_edges` graph. **Contract is
/// approximate, not exhaustive.**
///
/// The ingest path inserts edges for the top `EDGE_K=8` cosine
/// neighbors of each episode (`capture::ai_cli::EDGE_K`). On a
/// large, semantically dense pool a duplicate above the merge
/// threshold can therefore land *outside* the top-8 of both
/// endpoints and never appear as an edge — `merge_via_edges`
/// will not consider it. This is a deliberate trade-off:
/// * resource-safe (O(E) where E ≤ N · EDGE_K vs O(N²) cosine).
/// * recall is not impacted — both episodes remain in
///   `episode_vectors` and are retrievable. Merge here is only
///   the dedup half (a `note_pins.reason='merged-into:<older>'`
///   marker so context rendering can suppress duplicates).
/// * full-scan path still covers small pools (≤ `MERGE_FULL_SCAN_CAP`),
///   which is where most users live.
///
/// Codex 2차 review (2026-04-28) flagged the original docstring
/// claim "necessarily already an edge" as semantically equivalent
/// to the previous exhaustive merge. It is not — softened above.
/// A future tightening could (a) raise EDGE_K (ingest-side cost)
/// or (b) add a periodic resilience scan that walks pairs not
/// covered by the edge graph at e.g. weekly cadence.
fn merge_via_edges(
    storage: &Arc<Mutex<Storage>>,
    rows: Vec<(EpisodeId, Vec<f32>)>,
    threshold: f32,
) -> usize {
    let edges = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match guard.all_edges() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "merge_via_edges: all_edges failed");
                return 0;
            }
        }
    };
    if edges.is_empty() {
        return 0;
    }
    let vec_map: std::collections::HashMap<EpisodeId, Vec<f32>> = rows.into_iter().collect();

    let mut merged = 0usize;
    let mut already_merged: std::collections::HashSet<EpisodeId> = Default::default();

    for (src, dst, edge_sim) in edges {
        if edge_sim < threshold {
            continue;
        }
        let (older, newer) = if src < dst { (src, dst) } else { (dst, src) };
        if already_merged.contains(&newer) || already_merged.contains(&older) {
            continue;
        }
        let (Some(va), Some(vb)) = (vec_map.get(&older), vec_map.get(&newer)) else {
            continue;
        };
        let sim = cosine_similarity(va, vb);
        if sim < threshold {
            continue;
        }
        // D157-final — wire format 그대로 유지, typed enum 가 SoT.
        let reason = crate::storage::AuditReason::MergedInto(older).to_wire();
        let mut guard = crate::util::mutex::lock_or_recover(storage);
        if let Err(e) = guard.pin_episode(newer, &reason, sim) {
            tracing::warn!(error = %e, newer, older, "merge pin failed");
            continue;
        }
        already_merged.insert(newer);
        merged += 1;
    }
    tracing::info!(
        path = "edges",
        merged,
        edge_count = "(see all_edges)",
        "merge_similar_episodes via edge graph (P1-B large-pool gate)"
    );
    merged
}

/// Walk every episode and count those whose Ebbinghaus decay has
/// fallen under the cold tier threshold. v1.1 logs the count;
/// D93's schema split adds the actual `tier='cold'` column so the
/// demotion is observable.
fn scan_cold_candidates(storage: &Arc<Mutex<Storage>>, cfg: &SlowLoopConfig, now_ns: i64) -> usize {
    let guard = crate::util::mutex::lock_or_recover(storage);
    let episodes = match guard.all_episodes() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "cold-scan: all_episodes failed");
            return 0;
        }
    };
    let mut cold = 0usize;
    for ep in &episodes {
        let pinned = guard.is_pinned(ep.id).unwrap_or(false);
        if pinned {
            continue;
        }
        let access = guard.access_metadata(ep.id).ok().flatten().map(|(a, _, _)| a).unwrap_or(0);
        let factor = decay_weight(now_ns, ep.ts_start_ns, access, cfg.lambda);
        if factor < cfg.cold_tier_threshold && access == 0 {
            cold += 1;
        }
    }
    cold
}

/// Recompute the user_profile_centroid as the EMA over the most
/// recent N episodes. Picks the same `α=0.1` the ingest path
/// uses — slow-loop's job is to undo any drift from advisory-skip
/// failures, not to override the live ingest signal.
fn recompute_centroid(storage: &Arc<Mutex<Storage>>) -> bool {
    let rows = {
        let guard = crate::util::mutex::lock_or_recover(storage);
        match guard.vectors_for_model(crate::memory::embed::select_embedder().model_id()) {
            Ok(r) => r,
            Err(_) => return false,
        }
    };
    if rows.is_empty() {
        return false;
    }
    let recent: Vec<(EpisodeId, Vec<f32>)> = {
        let mut owned = rows;
        owned.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
        owned.into_iter().take(64).collect()
    };
    let mut centroid = recent[0].1.clone();
    for (_, v) in recent.iter().skip(1) {
        centroid = salience::update_centroid(&centroid, v, 0.1);
    }
    let count = recent.len() as u64;
    let mut guard = crate::util::mutex::lock_or_recover(storage);
    guard.update_user_centroid(&centroid, count).is_ok()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Episode;
    use std::sync::OnceLock;

    fn term_episode(ts: i64, cmd: &str) -> Episode {
        use crate::storage::EpisodeSource;
        Episode {
            ts_start_ns: ts,
            ts_end_ns: ts,
            duration_ms: 0,
            source: EpisodeSource::Terminal,
            session_id: None,
            prompt_text: None,
            response_text: None,
            command: Some(cmd.into()),
            stdout: None,
            exit_code: Some(0),
            cwd: None,
            git_branch: Some("main".into()),
            project: Some("p".into()),
            digest: None,
        }
    }

    fn fresh() -> Arc<Mutex<Storage>> {
        Arc::new(Mutex::new(Storage::open_in_memory().unwrap()))
    }

    /// D124+D131 close — `LAST_EPISODE_COUNT` is process-global; lib
    /// tests in this `mod tests` run in parallel and would race on
    /// the static. Each test acquires this Mutex and resets the
    /// sentinel so the gate behaves as if from a cold start.
    fn test_serializer() -> &'static Mutex<()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
    }

    fn ingest_n(storage: &Arc<Mutex<Storage>>, prompts: &[&str]) -> Vec<EpisodeId> {
        // D70 — use the same factory the slow-loop uses for its
        // own queries so model_id agrees. Hard-coded HashEmbedder
        // here regressed under `--all-features` where the factory
        // returns OnnxEmbedder.
        let embedder = crate::memory::embed::select_embedder();
        let mut ids = Vec::new();
        let mut s = storage.lock().unwrap();
        for (i, p) in prompts.iter().enumerate() {
            let ep = term_episode(1_700_000_000_000_000_000 + i as i64 * 1_000_000_000, p);
            let v = embedder.embed(p);
            let id = s.append_episode_with_vector(&ep, embedder.model_id(), &v).unwrap();
            ids.push(id);
        }
        ids
    }

    #[test]
    fn cycle_with_no_episodes_is_noop() {
        let _g = test_serializer().lock().unwrap();
        reset_episode_delta_state_for_tests();
        let storage = fresh();
        let stats = run_one_cycle(&storage, &SlowLoopConfig::v1_default());
        assert_eq!(stats.merged_pairs, 0);
        assert_eq!(stats.cold_candidates, 0);
        assert!(!stats.centroid_updated);
    }

    #[test]
    fn cycle_recomputes_centroid_when_episodes_present() {
        let _g = test_serializer().lock().unwrap();
        reset_episode_delta_state_for_tests();
        let storage = fresh();
        ingest_n(&storage, &["alpha", "beta", "gamma"]);
        let stats = run_one_cycle(&storage, &SlowLoopConfig::v1_default());
        assert!(stats.centroid_updated);
    }

    #[test]
    fn cycle_writes_deterministic_user_policy_without_llm_summary() {
        let _g = test_serializer().lock().unwrap();
        reset_episode_delta_state_for_tests();
        let storage = fresh();
        ingest_n(&storage, &["cargo test --workspace", "cargo test --lib", "cargo fmt --check"]);

        let _stats = run_one_cycle(&storage, &SlowLoopConfig::v1_default());

        let guard = storage.lock().unwrap();
        let rules = crate::memory::policy::read_policy_set(&guard, Some("p")).expect("policy set");
        assert!(!rules.is_empty(), "slow loop should write deterministic policy rows");
        let cargo_rule = rules
            .iter()
            .find(|rule| rule.rule.contains("`cargo`"))
            .expect("cargo repetition policy");
        assert!(
            !cargo_rule.evidence_episode_ids.is_empty(),
            "policy rows must keep cited evidence"
        );
    }

    #[test]
    fn cycle_merges_near_duplicate_episodes() {
        let _g = test_serializer().lock().unwrap();
        reset_episode_delta_state_for_tests();
        let storage = fresh();
        // Two near-identical commands → cosine > 0.95 in HashEmbedder.
        ingest_n(
            &storage,
            &[
                "cargo test --workspace",
                "cargo test --workspace", // exact duplicate
            ],
        );
        let cfg = SlowLoopConfig { merge_similarity: 0.95, ..SlowLoopConfig::v1_default() };
        let stats = run_one_cycle(&storage, &cfg);
        assert!(stats.merged_pairs >= 1, "near-duplicates must merge, got {}", stats.merged_pairs);
        // Verify the audit pin landed.
        let guard = storage.lock().unwrap();
        let pinned = guard.pinned_episode_ids().unwrap();
        assert!(!pinned.is_empty(), "merge produces a pin audit entry");
    }
}
