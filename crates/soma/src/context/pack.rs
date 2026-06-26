//! MemoryPack — retrieval substrate and developer/debug view for
//! SOMA's assembled local context. Discussion 0029 §C + §G.
//!
//! The current cloud-LLM-facing contract is `ContextEnvelope`.
//! `MemoryPack` stays as the internal shape that gathers recent
//! and semantic episodes before envelope assembly, plus a direct
//! developer/debug rendering for inspecting raw retrieval.
//!
//! Assembly layers:
//!
//! 1. `recent` — last N episodes by `ts_start_ns`.
//! 2. `semantic` — top-K cosine hits for the query, empty when
//!    query is `None`.
//! 3. `project_state` — Phase 5 self-model snapshot
//!    (project_norms + computed_at_ns).
//! 4. `self_state` — Phase 5 self-model snapshot (narrative +
//!    tool_use + exit_success + computed_at_ns).
//!
//! Dedup: the same `episode_id` appearing in both `recent` and
//! `semantic` is kept once — in the `semantic` layer (its
//! similarity signal is the stronger one).
//!
//! Rendering: Markdown and JSON for developer/debug MCP
//! `resources/read` payloads. Both are lossless — structured fields
//! round-trip byte-for-byte from a `MemoryPack` instance.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::context::explain::attribute;
use crate::memory::embed::Embedder;
use crate::memory::forgetting::{decay_weight, DEFAULT_LAMBDA};
use crate::memory::semantic::{SemanticError, SemanticIndex};
use crate::storage::{EpisodeId, Storage, StorageError, StoredEpisode};

/// D91 §B — compute the Ebbinghaus decay factor for one episode at
/// the moment of recall. Pinned episodes skip decay (factor = 1).
/// Failures (missing access metadata) collapse to factor = 1 so a
/// transient read miss doesn't penalize a real episode.
///
/// D156-C.5 close — `lambda` 가 caller-supplied. build_memory_pack
/// 가 process-life cached value 한 번 read 후 모든 episode 의
/// decay 계산에 동일 lambda 사용. hot-path 매 recall 마다 config
/// 파일 read 하는 구조 회피.
fn decay_factor_for(store: &Storage, id: EpisodeId, now_ns: i64, lambda: f32) -> f32 {
    if matches!(store.is_pinned(id), Ok(true)) {
        return 1.0;
    }
    match store.access_metadata(id) {
        Ok(Some((access_count, _last_access, ts_start))) => {
            decay_weight(now_ns, ts_start, access_count, lambda)
        }
        _ => 1.0,
    }
}

/// D156-C.5 close — process-wide cached `[memory] decay_lambda`. The
/// first build_memory_pack call pays the config read; subsequent
/// recalls reuse the cached value. `OnceLock` guarantees both
/// thread safety and single-init.
fn cached_decay_lambda() -> f32 {
    static LAMBDA: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *LAMBDA.get_or_init(|| match dirs::home_dir() {
        Some(home) => {
            crate::config::Config::load_or_default(&home.join(".soma")).memory.decay_lambda
        }
        None => DEFAULT_LAMBDA,
    })
}

/// Protocol version for the MemoryPack JSON payload. Increment on
/// any field rename/removal; additive changes are version-stable.
pub const MEMORY_PACK_VERSION: u32 = 1;

/// Errors surfaced while building a MemoryPack.
#[derive(Debug)]
#[non_exhaustive]
pub enum PackError {
    /// SQLite read or write failure during pack assembly.
    Storage(StorageError),
    /// Embedder or HNSW index failure during semantic recall.
    Semantic(SemanticError),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Storage(e) => write!(f, "storage: {e}"),
            PackError::Semantic(e) => write!(f, "semantic: {e}"),
        }
    }
}

impl std::error::Error for PackError {}

impl From<StorageError> for PackError {
    fn from(e: StorageError) -> Self {
        PackError::Storage(e)
    }
}

impl From<SemanticError> for PackError {
    fn from(e: SemanticError) -> Self {
        PackError::Semantic(e)
    }
}

/// Which backend serves `MemoryPack::semantic` recall. STAGE 3-A
/// per ADR 0006 — `Hnsw` is the v1 path (default), `Hopfield`
/// activates when `--features cognitive` is on AND the operator
/// opts in via `PackConfig::backend`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackendKind {
    /// `instant-distance` HNSW + softmax-weighted reduction (D90).
    #[default]
    Hnsw,
    /// `cognitive::PaperHopfield` (Ramsauer 2020) — multi-head
    /// LayerNorm + 1/√d scaling + weighted-sum retrieval.
    /// Requires `--features cognitive`; if the binary was built
    /// without it, falls back to `Hnsw` at runtime.
    Hopfield,
}

/// Default assembly sizing.
pub const DEFAULT_RECENT_N: usize = 5;
pub const DEFAULT_SEMANTIC_K: usize = 5;
/// Discussion 0037 §D90 default — Hopfield β controls retrieval
/// sharpness. β = 8.0 produces a recall that tilts strongly toward
/// the top similarity hit while still admitting near-ties.
pub const DEFAULT_RETRIEVAL_BETA: f32 = 8.0;
/// Cumulative-mass cutoff for softmax-weighted retrieval. 0.95
/// matches the convention used in attention-based retrieval
/// systems — anything past 95% mass is statistical noise.
pub const DEFAULT_RETRIEVAL_MASS: f32 = 0.95;
const THREAD_STATE_SELECTION_LIMIT: usize = 3;

/// Assembly knobs.
#[derive(Debug, Clone)]
pub struct PackConfig {
    pub recent_n: usize,
    pub semantic_k: usize,
    /// Hopfield-attention inverse temperature for `semantic`
    /// section retrieval. Higher β = peakier retrieval (top hit
    /// dominates). Lower β = smoother mix of relevant memories.
    pub retrieval_beta: f32,
    /// Cumulative softmax weight after which `semantic` recall
    /// stops admitting more hits. Set lower (e.g., 0.8) to favor a
    /// shorter, sharper retrieval substrate; higher (0.99) to admit
    /// a long tail.
    pub retrieval_mass: f32,
    /// STAGE 3-A — `BackendKind::Hopfield` swaps the v1 HNSW retrieval
    /// for a multi-head Hopfield (Ramsauer 2020). Default = HNSW.
    pub backend: BackendKind,
    /// D161 close — narrow the pack to a single project. `None` (the
    /// historical behavior) admits every project's episodes;
    /// `Some("aenv")` filters both `recent` and `semantic` layers to
    /// rows whose `episodes.project` matches exactly. The MCP
    /// primary resource path `soma://context/project/<name>` exposes
    /// this knob to cloud-LLM clients. `soma://memory-pack/project/<name>`
    /// remains a developer/debug direct-read view of the same raw retrieval.
    pub project_filter: Option<String>,
    /// Narrow the pack to a single captured session. This is the first
    /// stable thread-like scope SOMA can expose without cross-client
    /// joins: exact `episodes.session_id` match only. When combined
    /// with `project_filter`, both filters must match.
    pub session_filter: Option<String>,
    /// Narrow the pack to an operator-confirmed set of captured
    /// sessions. This is used by `soma://context/thread/<thread_key>`;
    /// it is intentionally separate from `session_filter` so a
    /// durable thread identity is not disguised as one session.
    pub session_filters: Vec<String>,
}

impl Default for PackConfig {
    fn default() -> Self {
        Self {
            recent_n: DEFAULT_RECENT_N,
            semantic_k: DEFAULT_SEMANTIC_K,
            retrieval_beta: DEFAULT_RETRIEVAL_BETA,
            retrieval_mass: DEFAULT_RETRIEVAL_MASS,
            backend: BackendKind::default(),
            project_filter: None,
            session_filter: None,
            session_filters: Vec::new(),
        }
    }
}

/// One episode entry with attribution.
#[derive(Debug, Clone, Serialize)]
pub struct PackItem {
    /// Unique episode identifier in the local database.
    pub episode_id: EpisodeId,
    /// Origin tag (e.g. `terminal`, `claude-code`, `cursor`).
    pub source: String,
    /// First-line summary, truncated for UI display.
    pub preview: String,
    /// Cosine similarity to the query in [0, 1] for semantic hits;
    /// `None` for items in the recent layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// Project name extracted at ingest time, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Capture session identifier for grouping related episodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Episode start timestamp (nanoseconds since UNIX epoch).
    pub ts_start_ns: i64,
    /// Human-readable attribution: why this episode was included.
    pub why: String,
}

/// Optional Layer 1 working-memory selector metadata.
///
/// The selector does not expose raw mLSTM state. It records the
/// episode IDs whose stored vectors are closest to the persisted
/// working-memory normalizer vector, so `ContextEnvelope.thread_state`
/// can prioritize task-continuity evidence under a small budget.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadStateSelection {
    pub strategy: String,
    pub selected_episode_ids: Vec<EpisodeId>,
    pub dim: usize,
    pub saved_at_ns: i64,
}

/// The top-level pack.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryPack {
    /// Schema version of this payload (see `MEMORY_PACK_VERSION`).
    pub version: u32,
    /// Wall-clock timestamp when this pack was built (ns since UNIX epoch).
    pub assembled_at_ns: i64,
    /// Query string that drove `semantic` recall, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Most recent episodes by `ts_start_ns` (chronological tail).
    pub recent: Vec<PackItem>,
    /// Top semantic hits for `query` (empty when `query` is None).
    pub semantic: Vec<PackItem>,
    /// Optional Layer 1 selector used only by the ContextEnvelope
    /// thread_state compiler. `relevant_memory` ordering remains the
    /// retrieval layer's contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_state_selection: Option<ThreadStateSelection>,
    /// Project-level state snapshot (norms, repo facts, computed_at_ns).
    pub project_state: serde_json::Value,
    /// User-level state snapshot (narrative, tool_use, exit_success).
    pub self_state: serde_json::Value,
}

/// Build a MemoryPack.
pub fn build_memory_pack(
    storage: Arc<Mutex<Storage>>,
    query: Option<&str>,
    cfg: PackConfig,
) -> Result<MemoryPack, PackError> {
    // D161 — project/session filters: when set, pull a wider window from
    // `recent_episodes` then narrow client-side. The wider pull is
    // needed because `recent_episodes(N)` orders globally by
    // `ts_start_ns`; a scoped recent N rows may straddle a much
    // longer window than the global last N.
    let scoped_recent = cfg.project_filter.is_some()
        || cfg.session_filter.is_some()
        || !cfg.session_filters.is_empty();
    let recent_window =
        if scoped_recent { cfg.recent_n.saturating_mul(20).max(200) } else { cfg.recent_n };
    let recent_episodes = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        let mut rows = guard.recent_episodes(recent_window)?;
        if scoped_recent {
            rows.retain(|ep| episode_matches_scope(ep, &cfg));
            rows.truncate(cfg.recent_n);
        }
        rows
    };

    let semantic_hits: Vec<(EpisodeId, f32)> = if let Some(q) = query {
        // D70 — factory picks OnnxEmbedder when `embed-onnx` feature
        // is on AND the model is downloaded. Else HashEmbedder (v1
        // default). Both produce 384d L2-norm vectors so HNSW /
        // Hopfield call sites are shape-stable.
        let embedder: Arc<dyn Embedder> = crate::memory::embed::select_embedder();
        // STAGE 3-A — backend selection. Hopfield path is feature-
        // gated; absent the feature we silently fall back to HNSW
        // so existing code paths keep working.
        let pool = match cfg.backend {
            #[cfg(feature = "cognitive")]
            BackendKind::Hopfield => {
                let backend =
                    crate::memory::cognitive::hopfield_backend::HopfieldBackend::open_with(
                        storage.clone(),
                        embedder.clone(),
                        crate::memory::cognitive::hopfield_backend::HopfieldBackend::DEFAULT_HEADS,
                        cfg.retrieval_beta,
                    )?;
                backend.recall(q, cfg.semantic_k.saturating_mul(4).max(cfg.semantic_k))?
            }
            _ => {
                let index = SemanticIndex::open(storage.clone(), embedder)?;
                // D90 §A — pull a wider candidate pool (k * 4) and
                // let the softmax-weighted reduction pick the top-
                // by-mass.
                index.recall(q, cfg.semantic_k.saturating_mul(4).max(cfg.semantic_k))?
            }
        };
        // D91 §B — apply Ebbinghaus decay before the softmax. Note
        // Block pins skip the decay so a high-salience old memory
        // still surfaces. `touch_episode` is called on the final
        // hits after the softmax reduction.
        let now_ns = now_ns();
        // D156-C.5 — cache the lambda once per process so the
        // hot-path doesn't re-read config.toml per recall.
        let lambda = cached_decay_lambda();
        let decayed: Vec<(EpisodeId, f32)> = {
            let guard = crate::util::mutex::lock_or_recover(&storage);
            pool.iter()
                .map(|(id, sim)| {
                    let factor = decay_factor_for(&guard, *id, now_ns, lambda);
                    (*id, sim * factor)
                })
                .collect()
        };
        let weighted = crate::memory::salience::softmax_weighted_recall(
            &decayed,
            cfg.retrieval_beta,
            cfg.retrieval_mass,
            cfg.semantic_k,
        );
        // D137 contract pin — `PackItem.similarity` carries the raw
        // decayed cosine, NOT the softmax attention weight. The pre-
        // fix bug surfaced softmax weights (typically 0.05~0.20) as
        // "semantic similarity X", causing downstream Claude sessions
        // to dismiss strong hits (raw cosine 0.93) as irrelevant.
        // `softmax_weighted_recall` returns the raw_sim alongside the
        // weight so this projection is a direct destructure.
        // D91 §B — bump access_count for hits that survive the
        // softmax. Failures are advisory (decay continues working
        // even if one update misses).
        {
            let mut guard = crate::util::mutex::lock_or_recover(&storage);
            for (id, _, _) in &weighted {
                let _ = guard.touch_episode(*id);
            }
        }
        weighted.into_iter().map(|(id, raw_sim, _w)| (id, raw_sim)).collect()
    } else {
        Vec::new()
    };

    // Semantic first so dedup removes duplicates from the `recent`
    // layer (semantic has stronger signal).
    //
    // R4 audit (2026-04-29) — `get_live_episode` filters out
    // forgotten rows. Belt-and-suspenders here: `vectors_for_model`
    // already JOINs on `forgotten_at_ns IS NULL`, but a race between
    // the vector lookup and the `get_episode` call could surface a
    // freshly-forgotten episode. The live filter closes that window.
    let mut seen: std::collections::HashSet<EpisodeId> = std::collections::HashSet::new();
    let mut semantic_items = Vec::with_capacity(semantic_hits.len());
    for (id, sim) in &semantic_hits {
        let ep = {
            let guard = crate::util::mutex::lock_or_recover(&storage);
            guard.get_live_episode(*id)?
        };
        if let Some(ep) = ep {
            // D161/D-context-session — scope filters narrow semantic
            // hits the same way they narrow the recent layer. The
            // rest fill the semantic vec naturally.
            if !episode_matches_scope(&ep, &cfg) {
                continue;
            }
            semantic_items.push(make_item(&ep, Some(*sim), query));
            seen.insert(*id);
        }
    }
    let mut recent_items = Vec::with_capacity(recent_episodes.len());
    for ep in recent_episodes {
        if seen.contains(&ep.id) {
            continue;
        }
        seen.insert(ep.id);
        recent_items.push(make_item(&ep, None, query));
    }
    let thread_state_selection =
        build_mlstm_thread_state_selection(&storage, &semantic_items, &recent_items);

    // Phase 5 — self_state + project_state fields now populated
    // from the `self_state` table (discussion 0030 §H). `project_
    // state` still materializes under `kind='project_norms'` rows,
    // so we surface both under one JSON payload shaped for MCP
    // clients to consume. Absent facts = empty object.
    let snapshot = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        crate::self_model::read_snapshot(&guard)?
    };
    // D90/G — slow_loop 가 합성 한 narrative paragraph 를 first
    // class field 로 노출. Context/debug profile consumers get a
    // semantic starting point instead of a raw fact list. 비어 있
    // 으면 (warming up) `narrative` 키 omit.
    //
    // D102-cand close (2026-04-29) — pre-fix `get_narrative().ok()
    // .flatten()` 가 read 실패 (DB lock contention / row corrupt /
    // schema drift) 와 "row 자체 없음" 을 같은 None 으로 collapse
    // — operator 가 "narrative 가 왜 비었지?" cause 추적 불가.
    // post-fix: read 실패 시 `_debug.narrative_status` 를 emit, row
    // 없음 / paragraph 빈 문자열 도 status 로 surface.
    let (narrative, narrative_status) = {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        match guard.get_narrative() {
            Ok(Some(row)) => (Some(row), "ready"),
            Ok(None) => (None, "no_row"),
            Err(e) => {
                tracing::warn!(error = %e, "narrative read failed; surfacing _debug.narrative_status");
                (None, "read_error")
            }
        }
    };
    let self_state_value = match narrative {
        Some((p, ts, kind)) if !p.is_empty() => serde_json::json!({
            "narrative": {
                "paragraph_md": p,
                "synthesized_at_ns": ts,
                "kind": kind,
            },
            "tool_use": snapshot.tool_use,
            "exit_success": snapshot.exit_success,
            "computed_at_ns": snapshot.computed_at_ns,
            "_debug": { "narrative_status": "ready" },
        }),
        Some(_) => serde_json::json!({
            "tool_use": snapshot.tool_use,
            "exit_success": snapshot.exit_success,
            "computed_at_ns": snapshot.computed_at_ns,
            "_debug": { "narrative_status": "empty_paragraph" },
        }),
        None => serde_json::json!({
            "tool_use": snapshot.tool_use,
            "exit_success": snapshot.exit_success,
            "computed_at_ns": snapshot.computed_at_ns,
            "_debug": { "narrative_status": narrative_status },
        }),
    };
    let project_state_value = serde_json::json!({
        "project_norms": snapshot.project_norms,
        "computed_at_ns": snapshot.computed_at_ns,
    });

    Ok(MemoryPack {
        version: MEMORY_PACK_VERSION,
        assembled_at_ns: now_ns(),
        query: query.map(str::to_string),
        recent: recent_items,
        semantic: semantic_items,
        thread_state_selection,
        project_state: project_state_value,
        self_state: self_state_value,
    })
}

fn build_mlstm_thread_state_selection(
    storage: &Arc<Mutex<Storage>>,
    semantic_items: &[PackItem],
    recent_items: &[PackItem],
) -> Option<ThreadStateSelection> {
    let candidate_ids: HashSet<EpisodeId> =
        semantic_items.iter().chain(recent_items.iter()).map(|item| item.episode_id).collect();
    if candidate_ids.is_empty() {
        return None;
    }

    let (dim, _c, n, saved_at_ns) = match crate::util::mutex::lock_or_recover(storage)
        .get_working_memory_state()
    {
        Ok(Some(state)) => state,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "mLSTM thread_state selector skipped: state read failed");
            return None;
        }
    };
    if dim == 0 || n.len() != dim || !n.iter().all(|v| v.is_finite()) {
        return None;
    }

    let model_id = crate::memory::embed::select_embedder().model_id();
    let vector_rows = match crate::util::mutex::lock_or_recover(storage).vectors_for_model(model_id)
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "mLSTM thread_state selector skipped: vectors read failed");
            return None;
        }
    };
    let vectors_by_id: HashMap<EpisodeId, Vec<f32>> = vector_rows.into_iter().collect();
    let mut scored: Vec<(EpisodeId, f32)> = candidate_ids
        .into_iter()
        .filter_map(|id| {
            let vector = vectors_by_id.get(&id)?;
            let score = cosine_score(&n, vector)?;
            Some((id, score))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }

    scored.sort_by(|(id_a, score_a), (id_b, score_b)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| id_a.cmp(id_b))
    });
    let selected_episode_ids =
        scored.into_iter().take(THREAD_STATE_SELECTION_LIMIT).map(|(id, _)| id).collect();
    Some(ThreadStateSelection {
        strategy: "mlstm_working_memory_state".to_string(),
        selected_episode_ids,
        dim,
        saved_at_ns,
    })
}

fn cosine_score(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (av, bv) in a.iter().zip(b.iter()) {
        if !av.is_finite() || !bv.is_finite() {
            return None;
        }
        dot += av * bv;
        norm_a += av * av;
        norm_b += bv * bv;
    }
    if norm_a <= f32::EPSILON || norm_b <= f32::EPSILON {
        return None;
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

fn episode_matches_scope(ep: &StoredEpisode, cfg: &PackConfig) -> bool {
    if let Some(project) = cfg.project_filter.as_deref() {
        if ep.project.as_deref() != Some(project) {
            return false;
        }
    }
    if let Some(session_id) = cfg.session_filter.as_deref() {
        if ep.session_id.as_deref() != Some(session_id) {
            return false;
        }
    }
    if !cfg.session_filters.is_empty() {
        let Some(session_id) = ep.session_id.as_deref() else {
            return false;
        };
        if !cfg.session_filters.iter().any(|expected| expected == session_id) {
            return false;
        }
    }
    true
}

fn make_item(ep: &StoredEpisode, similarity: Option<f32>, query: Option<&str>) -> PackItem {
    let preview = preferred_preview(ep);
    let why = attribute(ep, similarity, query);
    PackItem {
        episode_id: ep.id,
        // D119 — `ep.source` is now `EpisodeSource`; `PackItem.source`
        // stays `String` because the MCP MemoryPack JSON wire schema
        // is the kebab-case string. Display does the boundary
        // conversion (matches `Serialize` shape).
        source: ep.source.to_string(),
        preview,
        similarity,
        project: ep.project.clone(),
        session_id: ep.session_id.clone(),
        ts_start_ns: ep.ts_start_ns,
        why,
    }
}

fn preferred_preview(ep: &StoredEpisode) -> String {
    if let Some(p) = ep.prompt_text.as_deref() {
        return first_line(p);
    }
    if let Some(c) = ep.command.as_deref() {
        return first_line(c);
    }
    String::new()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Markdown render — LLM-context-ready text.
///
/// D109-cand close (R4 audit, 2026-04-29) — user-controlled fields
/// (`query`, `preview`, `why`, narrative paragraph) flow into a
/// markdown blob returned through MCP `resources/read`. A captured prompt
/// containing ` ``` `, `[link](javascript:...)`, or header markers
/// could break the structure or re-inject confusing instructions into
/// the downstream LLM.
///
/// Strategy:
/// * `query` + `why`: short single-line strings → `inline_code`
///   (backtick-wrap) so any markdown metacharacter is rendered
///   literally.
/// * `preview` + narrative paragraph: multi-line user content →
///   `fence_user_content` (triple-backtick fence) so the entire
///   block is treated as code by markdown renderers and the LLM
///   sees the content as data, not as document structure.
///
/// The narrative paragraph is generated by the rule-based or LLM
/// synthesizer and the LLM path already wraps episode bytes in
/// `<episode>...</episode>`; fencing here is belt-and-suspenders.
pub fn render_markdown(pack: &MemoryPack) -> String {
    let mut out = String::new();
    out.push_str("# MemoryPack\n\n");
    if let Some(q) = &pack.query {
        out.push_str(&format!("**Query:** {}\n\n", inline_code(q)));
    }

    // 0037 §G — narrative paragraph (rule-based or LLM) leads the
    // pack so AI clients see the user-summary first.
    if let Some(narrative) = pack.self_state.get("narrative").and_then(|n| n.as_object()) {
        if let Some(paragraph) = narrative.get("paragraph_md").and_then(|s| s.as_str()) {
            if !paragraph.is_empty() {
                out.push_str("## About the user\n\n");
                out.push_str(&fence_user_content(paragraph));
                out.push_str("\n\n");
            }
        }
    }

    if !pack.semantic.is_empty() {
        out.push_str("## Semantically relevant\n\n");
        for (i, item) in pack.semantic.iter().enumerate() {
            out.push_str(&format_item(i + 1, item));
        }
    }

    if !pack.recent.is_empty() {
        out.push_str("## Recent\n\n");
        for (i, item) in pack.recent.iter().enumerate() {
            out.push_str(&format_item(i + 1, item));
        }
    }

    if pack.semantic.is_empty() && pack.recent.is_empty() {
        out.push_str("_No episodes to surface._\n");
    }
    out
}

/// Wrap a single-line string in backticks so any markdown
/// metacharacter is rendered literally. Internal backticks are
/// escaped by switching the wrapper count (CommonMark "delimiter
/// run" rule) — if the input contains backticks, we use a longer
/// fence than any contiguous backtick run inside.
fn inline_code(s: &str) -> String {
    // Find the longest run of backticks in the input; the wrapper
    // must be longer than that run.
    let mut max_run = 0usize;
    let mut cur_run = 0usize;
    for c in s.chars() {
        if c == '`' {
            cur_run += 1;
            if cur_run > max_run {
                max_run = cur_run;
            }
        } else {
            cur_run = 0;
        }
    }
    let fence: String = "`".repeat(max_run + 1);
    // Pad with a space so a leading/trailing backtick doesn't
    // confuse the wrapper boundary.
    format!("{fence} {s} {fence}")
}

/// Wrap multi-line user content in a triple-backtick fence with a
/// `text` language hint. Any internal triple-backtick is neutralised
/// by switching to a longer fence.
fn fence_user_content(s: &str) -> String {
    let mut max_run = 0usize;
    let mut cur_run = 0usize;
    for c in s.chars() {
        if c == '`' {
            cur_run += 1;
            if cur_run > max_run {
                max_run = cur_run;
            }
        } else {
            cur_run = 0;
        }
    }
    let len = std::cmp::max(3, max_run + 1);
    let fence: String = "`".repeat(len);
    format!("{fence}text\n{s}\n{fence}")
}

fn format_item(rank: usize, item: &PackItem) -> String {
    let sim = item.similarity.map(|s| format!(" (sim={s:.3})")).unwrap_or_default();
    let mut line = format!(
        "### {rank}. episode #{id} — {source}{sim}\n",
        id = item.episode_id,
        source = item.source
    );
    if !item.preview.is_empty() {
        // D109-cand — preview is captured user input, fence it so
        // markdown structure can't be hijacked by the content.
        line.push_str(&fence_user_content(&item.preview));
        line.push('\n');
    }
    line.push_str(&format!("_Why:_ {}\n\n", inline_code(&item.why)));
    line
}

/// JSON render — lossless, MCP `resources/read` compatible.
pub fn render_json(pack: &MemoryPack) -> String {
    serde_json::to_string(pack).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod markdown_escape_tests {
    use super::*;

    /// D120-cand (R10 audit, 2026-04-30) — `inline_code` must wrap
    /// any backtick run with a longer fence so user-controlled text
    /// can't break out of inline-code context. Property: the wrapper
    /// fence is strictly longer than every contiguous backtick run
    /// in the input.
    #[test]
    fn inline_code_escapes_backtick_runs() {
        let inputs = [
            "no ticks",
            "with ` one tick",
            "with `` two ticks",
            "with ``` three ticks",
            "leading ` and trailing `",
        ];
        for s in inputs {
            let wrapped = inline_code(s);
            // Find the longest backtick run in the input.
            let mut max_run = 0usize;
            let mut cur_run = 0usize;
            for c in s.chars() {
                if c == '`' {
                    cur_run += 1;
                    if cur_run > max_run {
                        max_run = cur_run;
                    }
                } else {
                    cur_run = 0;
                }
            }
            // The wrapper run on each side must be > max_run.
            let needed = max_run + 1;
            let leading: String = "`".repeat(needed);
            assert!(
                wrapped.starts_with(&leading) && wrapped.ends_with(&leading),
                "inline_code({s:?}) = {wrapped:?} must wrap with at least {needed} backticks"
            );
        }
    }

    /// D120-cand (R10 audit) — `fence_user_content` must wrap multi-
    /// line content in a triple-backtick fence (or longer if the
    /// content itself contains a triple-backtick run). The fence
    /// length must exceed any backtick run inside the content so a
    /// captured prompt containing ` ``` ` cannot break the fence.
    #[test]
    fn fence_user_content_neutralizes_internal_fences() {
        let payload = "user code:\n```rust\nfn evil() {}\n```\nend";
        let wrapped = fence_user_content(payload);
        // 4 ticks is the minimum that exceeds the internal triple-tick.
        assert!(
            wrapped.starts_with("````") && wrapped.contains("````text\n"),
            "fence_user_content must use ≥4 ticks when content has triple-tick: {wrapped}"
        );
        assert!(wrapped.ends_with("````"), "closing fence missing: {wrapped}");
    }
}
