//! `soma recall` handler — top-k cosine recall over
//! `episode_vectors`. Discussion 0028 §§H-I + PR 6.9.
//!
//! Opens `Storage` at the configured DB path, builds a
//! `SemanticIndex` from the on-disk vectors for the configured
//! embedder's `model_id`, embeds the query, returns top-k. The
//! v1 embedder is always `HashEmbedder` (discussion 0028 §A); a
//! future `embed-onnx` build swaps in `OnnxEmbedder` behind the
//! same `Arc<dyn Embedder>`.
//!
//! `soma recall` is an operator/debug surface for ranked local
//! episodes. Cloud LLM clients should use `soma_recall` or
//! `soma://context/*`, which wrap the same retrieval substrate in
//! a cited ContextEnvelope. Markdown is human-readable; JSON is
//! for tooling.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::cli::RecallArgs;
use crate::memory::embed::Embedder;
use crate::memory::semantic::{SemanticError, SemanticIndex};
use crate::storage::{Storage, StorageError, StoredEpisode};

#[derive(Debug)]
pub enum RecallError {
    Storage(StorageError),
    Semantic(SemanticError),
    Path(String),
    BadFormat(String),
}

impl std::fmt::Display for RecallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecallError::Storage(e) => write!(f, "storage: {e}"),
            RecallError::Semantic(e) => write!(f, "semantic: {e}"),
            RecallError::Path(m) => write!(f, "path: {m}"),
            RecallError::BadFormat(m) => write!(f, "bad format: {m}"),
        }
    }
}

impl std::error::Error for RecallError {}

impl From<StorageError> for RecallError {
    fn from(e: StorageError) -> Self {
        RecallError::Storage(e)
    }
}

impl From<SemanticError> for RecallError {
    fn from(e: SemanticError) -> Self {
        RecallError::Semantic(e)
    }
}

/// Render mode. Kept as an enum (not a stringly-typed compare)
/// so tests and future additions (CSV? plist?) don't reach
/// into `args.format` text matching.
#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Markdown,
    Json,
}

impl OutputFormat {
    fn parse(s: &str) -> Result<Self, RecallError> {
        match s {
            "markdown" | "md" => Ok(OutputFormat::Markdown),
            "json" => Ok(OutputFormat::Json),
            other => Err(RecallError::BadFormat(format!(
                "unknown format `{other}`; expected `markdown` or `json`"
            ))),
        }
    }
}

/// Context for the handler — tests override `db_path` to a
/// tempdir; production path comes from `resolve_db_path`.
#[derive(Debug, Clone)]
pub struct RecallContext {
    pub db_path: PathBuf,
}

/// A single recall hit with enough episode metadata to render a
/// useful operator recall entry. Not a public wire schema — the
/// JSON output below serializes an explicit struct.
#[derive(Debug, Clone)]
pub struct RecallHit {
    pub episode: StoredEpisode,
    pub similarity: f32,
}

/// Run a recall. Returns the serialized output string + the list
/// of hits (for tests that assert on the structured result
/// without re-parsing the rendered string).
pub fn run_recall(
    args: &RecallArgs,
    ctx: &RecallContext,
) -> Result<(String, Vec<RecallHit>), RecallError> {
    let fmt = OutputFormat::parse(&args.format)?;
    if args.query.is_empty() {
        return Err(RecallError::BadFormat("query cannot be empty".into()));
    }

    let storage = Arc::new(Mutex::new(Storage::open(&ctx.db_path)?));
    // D70 — factory picks OnnxEmbedder when `embed-onnx` is active +
    // model is downloaded. Else HashEmbedder. Same factory used by
    // ingest + pack so model_id discriminator is consistent across
    // hot paths.
    let embedder: Arc<dyn Embedder> = crate::memory::embed::select_embedder();
    let index = SemanticIndex::open(storage.clone(), embedder.clone())?;

    // D92 §C — single-hop = v1 semantic recall. `multi_hop > 0`
    // pulls neighbors via Personalized PageRank against the
    // episode_edges graph. `personalized_pagerank` returns
    // `(EpisodeId, score)` already ranked.
    let scoped = args.project.is_some() || args.session_id.is_some();
    let candidate_limit = if args.limit == 0 {
        0
    } else if scoped {
        args.limit.saturating_mul(20).max(200)
    } else {
        args.limit
    };
    let raw_hits: Vec<(crate::storage::EpisodeId, f32)> = if args.multi_hop == 0 {
        index.recall(&args.query, candidate_limit)?
    } else {
        let seed =
            index.recall(&args.query, candidate_limit.saturating_mul(2).max(candidate_limit))?;
        personalized_pagerank(&storage, &seed, args.multi_hop, candidate_limit)?
    };

    // Materialize StoredEpisode for each hit so the renderer has
    // all the fields it needs without another round-trip per entry.
    //
    // R4 audit (2026-04-29) — `get_live_episode` filters out
    // forgotten rows. Pre-fix the multi-hop PageRank traversal
    // walked `episode_edges` (no forgotten_at_ns column), then
    // `get_episode` returned the forgotten payload, leaking
    // soft-deleted content via 2nd-hop recall.
    // P3-nit fix (in-house ultrareview): single lock acquisition
    // around the materialization loop instead of one acquire/release
    // per hit. μsec-scale perf, user-visible 0, but matches the
    // single-lock pattern used elsewhere (e.g. slow_loop's
    // resilience_scan after the same audit).
    let mut hits = Vec::with_capacity(raw_hits.len());
    {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        for (id, sim) in raw_hits {
            if let Some(ep) = guard.get_live_episode(id)? {
                if !episode_matches_scope(&ep, args) {
                    continue;
                }
                hits.push(RecallHit { episode: ep, similarity: sim });
                if hits.len() >= args.limit {
                    break;
                }
            }
        }
    }

    let rendered = match fmt {
        OutputFormat::Markdown => {
            render_markdown(&args.query, args.project.as_deref(), args.session_id.as_deref(), &hits)
        }
        OutputFormat::Json => {
            render_json(&args.query, args.project.as_deref(), args.session_id.as_deref(), &hits)
        }
    };

    // ADR 0016 boundary — direct CLI recall persists only a
    // legacy-named local debug trace for dashboard/operator
    // inspection. It is not bridge completion evidence; canonical
    // cloud LLM reads use MCP ContextEnvelope resources/tools.
    // session_id prefix `cli-recall-{ns}` stays distinct from
    // historical `chat-{ns}` rows so dashboard/debug cohorts remain
    // separable without a DB rename.
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let session_id = crate::storage::session::cli_recall(now_ns);
    let project = args.project.clone().or_else(crate::project::current_name);
    let top_k_json: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "episode_id": h.episode.id,
                "raw_sim": h.similarity,
            })
        })
        .collect();
    let top_k_str = serde_json::to_string(&top_k_json).unwrap_or_else(|_| "[]".to_string());
    if let Ok(mut guard) = storage.lock() {
        let _ = guard.append_chat_recall_trace(
            now_ns,
            Some(&session_id),
            project.as_deref(),
            &args.query,
            hits.len() as i64,
            &top_k_str,
            None,
            0,
            None,
        );
    }

    Ok((rendered, hits))
}

fn episode_matches_scope(ep: &StoredEpisode, args: &RecallArgs) -> bool {
    if let Some(project) = args.project.as_deref() {
        if ep.project.as_deref() != Some(project) {
            return false;
        }
    }
    if let Some(session_id) = args.session_id.as_deref() {
        if ep.session_id.as_deref() != Some(session_id) {
            return false;
        }
    }
    true
}

/// D92 §C — Personalized PageRank over `episode_edges`.
///
/// `seed` = single-hop semantic hits (`(id, similarity)`). Each
/// iteration:
///   1. For every node, distribute `damping · score` along its
///      edges, weighted by edge similarity.
///   2. Add `(1 - damping) · seed_mass` back to the seeded nodes
///      (the "restart" probability).
///
/// `damping = 0.85` matches the original PageRank constant; for a
/// HippoRAG-style multi-hop QA traversal this gives a useful mix of
/// "near the seed" + "structurally important" nodes. Returns
/// `(EpisodeId, score)` sorted by score DESC, capped at `limit`.
fn personalized_pagerank(
    storage: &Arc<Mutex<Storage>>,
    seed: &[(crate::storage::EpisodeId, f32)],
    hops: usize,
    limit: usize,
) -> Result<Vec<(crate::storage::EpisodeId, f32)>, RecallError> {
    use std::collections::HashMap;
    if seed.is_empty() {
        return Ok(Vec::new());
    }
    const DAMPING: f32 = 0.85;
    const NEIGHBOR_LIMIT: usize = 16;

    // Normalize seed similarities into a probability distribution.
    let total: f32 = seed.iter().map(|(_, s)| s.max(0.0)).sum();
    let seed_dist: HashMap<crate::storage::EpisodeId, f32> = if total > 0.0 {
        seed.iter().map(|(id, s)| (*id, s.max(0.0) / total)).collect()
    } else {
        seed.iter().map(|(id, _)| (*id, 1.0 / seed.len() as f32)).collect()
    };

    let mut score: HashMap<crate::storage::EpisodeId, f32> = seed_dist.clone();
    for _ in 0..hops {
        let mut next: HashMap<crate::storage::EpisodeId, f32> = HashMap::new();
        for (id, mass) in &score {
            let neighbors = {
                let guard = crate::util::mutex::lock_or_recover(storage);
                guard.edges_for(*id, NEIGHBOR_LIMIT)?
            };
            let neighbor_total: f32 = neighbors.iter().map(|(_, s)| *s).sum();
            for (other, sim) in &neighbors {
                let frac = if neighbor_total > 0.0 { sim / neighbor_total } else { 0.0 };
                *next.entry(*other).or_insert(0.0) += mass * DAMPING * frac;
            }
        }
        // Restart probability into the seed.
        for (id, dist) in &seed_dist {
            *next.entry(*id).or_insert(0.0) += (1.0 - DAMPING) * dist;
        }
        score = next;
    }

    let mut ranked: Vec<(crate::storage::EpisodeId, f32)> = score.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    Ok(ranked)
}

fn render_markdown(
    query: &str,
    project: Option<&str>,
    session_id: Option<&str>,
    hits: &[RecallHit],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Recall: {query}\n\n"));
    if project.is_some() || session_id.is_some() {
        out.push_str("Scope:");
        if let Some(project) = project {
            out.push_str(&format!(" project={project}"));
        }
        if let Some(session_id) = session_id {
            out.push_str(&format!(" session_id={session_id}"));
        }
        out.push_str("\n\n");
    }
    if hits.is_empty() {
        out.push_str("_No matching episodes._\n");
        return out;
    }
    for (rank, h) in hits.iter().enumerate() {
        let title = preferred_title(&h.episode);
        out.push_str(&format!(
            "## {n}. episode #{id} ({source}, sim={sim:.3})\n",
            n = rank + 1,
            id = h.episode.id,
            source = h.episode.source,
            sim = h.similarity
        ));
        out.push_str(&format!("> {title}\n\n"));
    }
    out
}

fn render_json(
    query: &str,
    project: Option<&str>,
    session_id: Option<&str>,
    hits: &[RecallHit],
) -> String {
    let items: Vec<_> = hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "episode_id": h.episode.id,
                "source": h.episode.source,
                "similarity": h.similarity,
                "preview": preferred_title(&h.episode),
                "project": h.episode.project,
                "session_id": h.episode.session_id,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "query": query,
        "project": project,
        "session_id": session_id,
        "hits": items,
    });
    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into())
}

/// Pick the best one-line preview of an episode. AI episodes show
/// the prompt; terminal episodes show the command. Fallback is an
/// empty string (caller's rendering tolerates it).
fn preferred_title(ep: &StoredEpisode) -> String {
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

/// Exit-code mapping for the CLI dispatcher.
pub fn exit_code_for(err: &RecallError) -> i32 {
    match err {
        RecallError::BadFormat(_) => 1,
        RecallError::Storage(_) | RecallError::Semantic(_) => 2,
        RecallError::Path(_) => 3,
    }
}

/// Reuse `ai_cli::resolve_db_path` policy — `--db-path` override,
/// then `$SOMA_DB`, then `~/.soma/soma.db`.
pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, RecallError> {
    crate::capture::ai_cli::resolve_db_path(cli_override).map_err(|e| {
        // ai_cli::IngestError::Path → RecallError::Path. The other
        // variants can't surface from resolve_db_path but we map
        // defensively.
        use crate::capture::ai_cli::IngestError;
        match e {
            IngestError::Path(m) => RecallError::Path(m),
            other => RecallError::Path(other.to_string()),
        }
    })
}
