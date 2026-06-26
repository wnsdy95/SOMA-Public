//! `soma ingest` handler — AIAdapter entry point (discussion 0023
//! D2 + discussion 0027).
//!
//! Given an `IngestArgs` (either flag-populated or JSON-populated)
//! and an `IngestContext` (db path), `run_ingest`:
//!
//! 1. Validates the input shape (flags-vs-JSON exclusivity, source-
//!    specific payload requirements).
//! 2. Builds an [`Episode`] by merging flag fields + optional JSON
//!    payload + on-the-fly timestamps.
//! 3. Opens `Storage` on the configured DB path (WAL mode — non-
//!    blocking vs any concurrent resident reader per discussion
//!    0027 §C).
//! 4. Appends the episode and returns the assigned `EpisodeId`.
//!
//! `run_ingest` is the boundary the `soma ingest` CLI dispatcher
//! calls, and it is what tests invoke directly — no child process
//! required.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::cli::IngestArgs;
use crate::memory::forgetting;
use crate::memory::salience::{
    self, SalienceContext, SalienceWeights, IPC_FREE_ENERGY_ANOMALY_KIND,
    IPC_FREE_ENERGY_ANOMALY_THRESHOLD,
};
use crate::storage::{Episode, EpisodeId, EpisodeSource, Storage, StorageError};

/// Runtime dependencies. Tests inject a tempdir DB path; production
/// resolves one via `resolve_db_path` (CLI override → `$SOMA_DB` →
/// `~/.soma/soma.db`) before constructing the context.
#[derive(Debug, Clone)]
pub struct IngestContext {
    /// Absolute path to the SQLite database. Tests use a tempdir;
    /// production resolves via `resolve_db_path`.
    pub db_path: PathBuf,
}

/// Successful outcome. Enum (not unit struct) so Phase 2+ can add
/// `Deferred` / `Queued` without breaking the match.
#[derive(Debug, Clone, Copy)]
pub enum IngestOutcome {
    Stored { episode_id: EpisodeId },
}

/// Defaults applied by `soma adapter-capture` before the normalized
/// payload is written through the same persistence path as `soma ingest`.
#[derive(Debug, Default, Clone)]
pub struct AdapterCaptureDefaults {
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub git_branch: Option<String>,
}

/// Typed failure legs mirroring the §D exit-code taxonomy
/// (discussion 0027). `MalformedInput` = exit 1, `Storage` = exit 2,
/// `Path` = exit 3, `PayloadTooLarge` = exit 4 (D112).
#[derive(Debug)]
#[non_exhaustive]
pub enum IngestError {
    /// User input failed validation (flag/JSON exclusivity, missing
    /// required field for the source, JSON parse failure, base64
    /// decode failure, stdout-file path outside tempdir).
    MalformedInput(String),
    /// SQLite open or write failure during episode persistence.
    Storage(StorageError),
    /// DB path resolution failure (no `$SOMA_DB`, no home directory,
    /// no `--db-path` override).
    Path(String),
    /// D112 — a capture-payload field exceeded the per-field byte
    /// cap. Distinct from `MalformedInput` because the cause is
    /// resource-pressure (OOM / disk-fill threat from a runaway or
    /// adversarial Stop-hook caller), not a shape error. Stop-hook
    /// callers can distinguish "your payload was too big" from
    /// "your payload was structurally wrong" via exit code 4 and
    /// the `payload_too_large` discriminator on the JSON envelope.
    PayloadTooLarge { field: &'static str, len: usize, limit: usize },
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::MalformedInput(m) => write!(f, "malformed input: {m}"),
            IngestError::Storage(e) => write!(f, "storage: {e}"),
            IngestError::Path(p) => write!(f, "path: {p}"),
            IngestError::PayloadTooLarge { field, len, limit } => {
                write!(f, "payload too large: field `{field}` is {len} bytes, limit is {limit}")
            }
        }
    }
}

impl std::error::Error for IngestError {}

impl From<StorageError> for IngestError {
    fn from(e: StorageError) -> Self {
        IngestError::Storage(e)
    }
}

/// §B JSON payload schema. Every field is optional at parse time;
/// source-specific validation runs in `run_ingest` after merging
/// JSON + args. Timestamps are optional — `run_ingest` fills them
/// with `SystemTime::now()` if absent.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonPayload {
    source: Option<String>,
    session_id: Option<String>,
    prompt_text: Option<String>,
    response_text: Option<String>,
    command: Option<String>,
    stdout_b64: Option<String>,
    exit_code: Option<i32>,
    cwd: Option<String>,
    git_branch: Option<String>,
    project: Option<String>,
    ts_start_ns: Option<i64>,
    ts_end_ns: Option<i64>,
    digest: Option<String>,
}

/// Run one `soma ingest` invocation. `args` is either flag-populated
/// or JSON-populated; `ctx` carries the DB path. See the module
/// docstring for the 4-step flow and the §D exit-code taxonomy.
///
/// D1 §A — `append_episode_with_vector` commits the row and the
/// default-embedder vector as one SQLite transaction. A failure
/// in either half rolls back, so the SOMA invariant — *every
/// episode is recallable under `model_id="hash-v1"`* — is
/// preserved against partial-write crashes.
pub fn run_ingest(args: &IngestArgs, ctx: &IngestContext) -> Result<IngestOutcome, IngestError> {
    enforce_mutex(args)?;
    let episode = build_episode(args)?;
    persist_episode(episode, ctx)
}

/// Native adapter-capture entrypoint. Adapter wrappers pass one JSON object
/// using the `soma ingest --json` schema; this helper fills missing capture
/// metadata and then writes through the normal ingest pipeline.
pub fn run_adapter_capture_json(
    raw_json: &str,
    source_fallback: Option<&str>,
    defaults: AdapterCaptureDefaults,
    ctx: &IngestContext,
) -> Result<IngestOutcome, IngestError> {
    use std::str::FromStr;

    let mut payload = serde_json::from_str::<JsonPayload>(raw_json)
        .map_err(|e| IngestError::MalformedInput(format!("JSON parse: {e}")))?;
    if payload.source.is_none() {
        payload.source = source_fallback.map(str::to_string);
    }
    if payload.cwd.is_none() {
        payload.cwd = defaults.cwd;
    }
    if payload.project.is_none() {
        payload.project = defaults.project;
    }
    if payload.session_id.is_none() {
        payload.session_id = defaults.session_id;
    }
    if payload.git_branch.is_none() {
        payload.git_branch = defaults.git_branch;
    }

    let source_raw = payload.source.clone().ok_or_else(|| {
        IngestError::MalformedInput("JSON `source` or `--source` is required".into())
    })?;
    if source_raw.is_empty() {
        return Err(IngestError::MalformedInput("JSON `source` or `--source` is required".into()));
    }
    let source = EpisodeSource::from_str(&source_raw)
        .map_err(|e| IngestError::MalformedInput(e.to_string()))?;

    let stdout = match payload.stdout_b64.as_deref() {
        Some(b64) => {
            use base64::prelude::{Engine, BASE64_STANDARD};
            Some(
                BASE64_STANDARD
                    .decode(b64)
                    .map_err(|e| IngestError::MalformedInput(format!("stdout_b64 decode: {e}")))?,
            )
        }
        None => None,
    };

    validate_source_payload(
        &source,
        payload.prompt_text.as_deref(),
        payload.response_text.as_deref(),
        payload.command.as_deref(),
    )?;

    let now_ns = now_ns();
    let ts_start_ns = payload.ts_start_ns.unwrap_or(now_ns);
    let ts_end_ns = payload.ts_end_ns.unwrap_or(ts_start_ns);
    let duration_ms = duration_ms(ts_start_ns, ts_end_ns);

    let episode = Episode {
        ts_start_ns,
        ts_end_ns,
        duration_ms,
        source,
        session_id: payload.session_id,
        prompt_text: payload.prompt_text,
        response_text: payload.response_text,
        command: payload.command,
        stdout,
        exit_code: payload.exit_code,
        cwd: payload.cwd,
        git_branch: payload.git_branch,
        project: payload.project,
        digest: payload.digest,
    };

    persist_episode(episode, ctx)
}

fn persist_episode(episode: Episode, ctx: &IngestContext) -> Result<IngestOutcome, IngestError> {
    // D112 — reject oversized payloads BEFORE opening SQLite. A 2 GiB
    // prompt would otherwise be parsed, embedded, and committed before
    // the DB driver complains, by which time the host has already
    // burned the memory + disk budget. Validating here keeps the failure
    // cost O(input_size) and leaves no on-disk artifact behind.
    validate_payload_lengths(&episode)?;
    let mut store = Storage::open(&ctx.db_path)?;

    let text = episode_index_text(&episode);
    // D70 — factory picks OnnxEmbedder when `embed-onnx` feature is
    // on AND the model is downloaded. Else HashEmbedder. Same factory
    // is used by recall (`pack.rs::build_memory_pack`) so ingest +
    // recall always agree on `model_id` within one process.
    //
    // D138 — `embed_passage` adds the e5 `passage: ` prefix on the
    // ingest side; symmetric backends (Hash / MiniLM) fall through to
    // the default `embed` so they're unchanged.
    let embedder = crate::memory::embed::select_embedder();
    let (model_id, vector) = if text.is_empty() {
        (embedder.model_id(), Vec::new())
    } else {
        (embedder.model_id(), embedder.embed_passage(&text))
    };
    // D90 §A — score the episode against the *prior* centroid +
    // existing nearest neighbor, then commit the (episode, vector)
    // pair atomically (D1 §A invariant). Salience score is computed
    // pre-write so its `self_relevance` reflects the centroid before
    // the new episode pulls it.
    let salience_score = if !vector.is_empty() {
        compute_salience_pre_write(&store, &vector, episode.duration_ms)
    } else {
        None
    };

    let id = store.append_episode_with_vector(&episode, model_id, &vector)?;

    // D69 close (2026-05-01) — Studio dual-store. After the primary
    // (e5-large 1024d) row is committed, write a parallel MiniLM 384d
    // row so the episode stays recallable from a Mini binary (or a
    // Studio→Mini downgrade). R14 audit (2026-05-01) — extracted to
    // `write_secondary_vector` to drop nesting from depth-19 to
    // depth-2 + improve testability. Helper is a no-op when no
    // secondary embedder is configured.
    if !text.is_empty() {
        write_secondary_vector(&mut store, id, &text);
    }

    // D92 §C — wire ingest into the episode_edges graph. Compute
    // similarities against existing vectors (top-k by cosine) and
    // store every pair > EDGE_THRESHOLD as an undirected edge. The
    // edges back the multi-hop recall path. Failure here is
    // advisory — the episode + vector are already committed.
    //
    // D125 close (Batch 5, 2026-04-30) — `edge_k` is now config-
    // driven (`[memory] edge_k`, default 8, range [4, 32], clamped
    // by `Config::load_or_default`). We resolve home → config once
    // per ingest; on a missing home dir we fall back to defaults
    // so tests / non-resident invocations still work.
    if !vector.is_empty() {
        let cfg = match dirs::home_dir() {
            Some(home) => crate::config::Config::load_or_default(&home.join(".soma")),
            None => crate::config::Config::default_v1(),
        };
        let edge_k = cfg.memory.edge_k as usize;
        if let Err(e) = wire_edges_after_ingest(&mut store, id, &vector, edge_k) {
            tracing::warn!(error = %e, "edge wiring failed (advisory)");
        }
    }

    // D90 §A — update the user-profile centroid EMA so the next
    // `score_salience` call has a non-empty `self_relevance` axis.
    // Failures here log + continue: the episode is already persisted
    // and the centroid is advisory.
    if !vector.is_empty() {
        if let Err(e) = update_centroid_after_ingest(&mut store, &vector) {
            tracing::warn!(error = %e, "centroid EMA update failed (advisory)");
        }
    }

    // Layer 2 / iPC bridge: high predictive-coding free-energy is
    // persisted as a cited context anomaly. The ContextEnvelope
    // adapter reads these rows into `open_decisions`; pinning still
    // uses the scalar salience free_energy path below.
    if let Some(score) = &salience_score {
        if let Some(pc_free_energy) = score.pc_free_energy {
            if pc_free_energy >= IPC_FREE_ENERGY_ANOMALY_THRESHOLD {
                let evidence = format!(
                    "iPC pc_free_energy {:.3} exceeded anomaly threshold {:.3}",
                    pc_free_energy, IPC_FREE_ENERGY_ANOMALY_THRESHOLD
                );
                if let Err(e) = store.upsert_context_anomaly(
                    id,
                    IPC_FREE_ENERGY_ANOMALY_KIND,
                    pc_free_energy,
                    Some(&evidence),
                ) {
                    tracing::warn!(error = %e, "context anomaly write failed (advisory)");
                }
            }
        }
    }

    // D91 §B — pin to the Note Block when free_energy clears the
    // threshold. Pins are advisory; failures don't abort ingest.
    // D156-C close — threshold 가 [memory] pin_threshold 에서.
    if let Some(score) = &salience_score {
        let pin_threshold = match dirs::home_dir() {
            Some(home) => {
                crate::config::Config::load_or_default(&home.join(".soma")).memory.pin_threshold
            }
            None => forgetting::DEFAULT_PIN_THRESHOLD,
        };
        if forgetting::should_pin(score, pin_threshold) {
            // D157-final — wire format "salience" 그대로, typed
            // enum 의 `to_wire()` 가 SoT.
            let pin_reason = crate::storage::AuditReason::Salience.to_wire();
            if let Err(e) = store.pin_episode(id, &pin_reason, score.free_energy) {
                tracing::warn!(error = %e, "note-pin write failed (advisory)");
            }
        }
    }

    // D93 §E — stamp summary_signature so the slow_loop's
    // compression pass can later collapse repeated patterns.
    if let Some(sig) = compute_summary_signature(&episode) {
        if let Err(e) = store.update_summary_metadata(id, 1, Some(&sig)) {
            tracing::warn!(error = %e, "summary signature write failed (advisory)");
        }
    }

    Ok(IngestOutcome::Stored { episode_id: id })
}

/// D93 §E — `(command, project, exit_code)` triple SHA256.
/// Episodes that share a signature are *candidates* for compression
/// — the slow_loop pass also requires high cosine similarity
/// between their embeddings before collapsing. AI-source episodes
/// (no command) get a `None` signature and are never compressed
/// (their content varies too much across turns).
fn compute_summary_signature(ep: &Episode) -> Option<String> {
    let cmd = ep.command.as_deref()?;
    let project = ep.project.as_deref().unwrap_or("");
    let exit = ep.exit_code.unwrap_or(-1);
    let raw = format!("{cmd}\u{1f}{project}\u{1f}{exit}");
    let mut hasher = Sha256Like::new();
    hasher.update(raw.as_bytes());
    Some(hasher.finalize_hex())
}

/// Tiny SHA-256 impl wrapper. We use `serde_json::to_string` of a
/// stable shape elsewhere in the codebase, but signatures need to
/// be 64-character hex for the index. Implementation choice: a
/// minimal in-tree SHA256 to avoid pulling a new dep — discussion
/// 0037 §E acceptance "no new crate dep for D93".
struct Sha256Like {
    state: [u32; 8],
    buf: Vec<u8>,
    total: u64,
}

impl Sha256Like {
    fn new() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            buf: Vec::with_capacity(64),
            total: 0,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        self.buf.extend_from_slice(bytes);
        while self.buf.len() >= 64 {
            let block: [u8; 64] = self.buf[..64].try_into().unwrap();
            compress(&mut self.state, &block);
            self.buf.drain(..64);
        }
    }

    fn finalize_hex(mut self) -> String {
        // Padding per FIPS 180-4 §5.1.1.
        let bits = self.total * 8;
        self.buf.push(0x80);
        while self.buf.len() % 64 != 56 {
            self.buf.push(0);
        }
        self.buf.extend_from_slice(&bits.to_be_bytes());
        while self.buf.len() >= 64 {
            let block: [u8; 64] = self.buf[..64].try_into().unwrap();
            compress(&mut self.state, &block);
            self.buf.drain(..64);
        }
        let mut hex = String::with_capacity(64);
        for w in self.state {
            hex.push_str(&format!("{w:08x}"));
        }
        hex
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Build a `SalienceContext` from the live storage and score the
/// new episode's embedding. Returns `None` if storage probes fail
/// — caller treats that as "no score" and skips the pin decision.
fn compute_salience_pre_write(
    store: &Storage,
    embed: &[f32],
    duration_ms: i64,
) -> Option<crate::memory::salience::SalienceScore> {
    let centroid = store.get_user_centroid().ok().flatten().map(|(c, _)| c);
    // Nearest neighbor: re-rank against existing vectors for the
    // hash embedder. For ingest scale (k_pool ≤ ~10K) this is OK;
    // a future ANN refresh path can replace this without API churn.
    let model_id = crate::memory::embed::select_embedder().model_id();
    let rows = store.vectors_for_model(model_id).ok()?;
    let nearest = rows
        .iter()
        .map(|(_, v)| (v, dot(embed, v)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(v, _)| v.clone());

    // chunk 1.5 — when the trainable mLSTM has weights persisted,
    // compute the content-addressable read and feed it to the
    // novelty axis. Falls back to `nearest_neighbor` (raw cosine
    // top-1) when weights are absent or feature is off.
    //
    // ADR 0015 boundary: this is ingest-time salience only. It does
    // not compile or compress `ContextEnvelope.thread_state`.
    let wm_read = compute_working_memory_read(store, embed);

    // Optional iPC diagnostic decomposition. The decomposition
    // matches the slow_loop's iPC training schema (`[d, d/2, d/4,
    // d/8]`) so salience.pc_free_energy and the trained predictor
    // live in the same latent space.
    //
    // ADR 0015 boundary: this fills SalienceScore.pc_free_energy.
    // Ingest persists threshold-crossing values as `context_anomalies`;
    // the ContextEnvelope adapter is the only path that turns them
    // into cited `open_decisions`.
    let pc_latents = build_pc_latents(embed);

    let ctx = SalienceContext {
        recent_ema: centroid.as_deref(),
        user_profile_centroid: centroid.as_deref(),
        nearest_neighbor: nearest.as_deref(),
        working_memory_read: wm_read.as_deref(),
        context_embed: None,
        pc_latents: pc_latents.as_deref(),
    };
    Some(salience::score(embed, duration_ms, &ctx, &SalienceWeights::v1_default()))
}

/// Hierarchical truncation matching the slow_loop's iPC training
/// schema (`[d, d/2, d/4, d/8]`). Returns `None` when the embedding
/// is too small to support the iPC hierarchy (`d < 8`);
/// salience.pc_free_energy then stays None.
fn build_pc_latents(embed: &[f32]) -> Option<Vec<Vec<f32>>> {
    let d = embed.len();
    if d < 8 {
        return None;
    }
    let dims = [d, d / 2, d / 4, d / 8];
    Some(dims.iter().map(|&n| embed.iter().take(n).copied().collect()).collect())
}

/// chunk 1.5 — load the persisted `TrainableMLstm` weights and run
/// one forward pass against `embed`. Returns `None` when the
/// `cognitive-train` feature is off, when no weights row exists,
/// or when the cell init / forward fails.
///
/// ADR 0015 boundary: the resulting vector is consumed only by the
/// salience novelty axis. It is not a `ContextEnvelope.thread_state`
/// compressor until a future P4 slice wires and tests that output.
/// The cost on hot path is one DB read + one candle matmul of
/// `dim×dim` (≈ a few ms at d=384), within the ingest budget.
#[cfg(feature = "cognitive-train")]
fn compute_working_memory_read(store: &Storage, embed: &[f32]) -> Option<Vec<f32>> {
    use crate::memory::cognitive::mlstm_trainable::TrainableMLstm;

    let (dim, w_q, w_k, w_v, _steps, _ts) = store.get_working_memory_weights().ok().flatten()?;
    if dim != embed.len() {
        return None;
    }
    // Defense in depth — Storage's NaN guard *should* have caught
    // this before persist, but the cell's forward path is on the
    // ingest hot path; skip the wm_read entirely if something
    // slipped through. Caller falls back to nearest_neighbor.
    if w_q.iter().chain(w_k.iter()).chain(w_v.iter()).any(|v| !v.is_finite()) {
        tracing::warn!("compute_working_memory_read: persisted weights contain non-finite entries; falling back to nearest_neighbor");
        return None;
    }
    let cell = TrainableMLstm::new_identity(dim).ok()?;
    if !cell.import_weights(w_q, w_k, w_v) {
        return None;
    }
    let read = cell.forward(embed).ok()?;
    if read.iter().any(|v| !v.is_finite()) {
        tracing::warn!("compute_working_memory_read: forward result has non-finite entries; falling back to nearest_neighbor");
        return None;
    }
    Some(read)
}

#[cfg(not(feature = "cognitive-train"))]
fn compute_working_memory_read(_store: &Storage, _embed: &[f32]) -> Option<Vec<f32>> {
    None
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// D92 §C — top-k similar existing episodes get edges. The default
/// `edge_k = 8` matches the discussion's recommendation (high enough
/// to seed PPR, low enough that O(N) for N up to ~10K stays fast).
/// `EDGE_THRESHOLD = 0.5` filters out coincidental cosine matches
/// that would dilute the graph.
///
/// D125 close (Batch 5, 2026-04-30) — `edge_k` was promoted from
/// `const EDGE_K: usize = 8` to `[memory] edge_k` config knob; the
/// caller (`run_ingest`) now threads the post-clamp value here.
const EDGE_THRESHOLD: f32 = 0.5;

fn wire_edges_after_ingest(
    store: &mut Storage,
    new_id: EpisodeId,
    new_vec: &[f32],
    edge_k: usize,
) -> Result<(), StorageError> {
    let model_id = crate::memory::embed::select_embedder().model_id();
    let rows = store.vectors_for_model(model_id)?;
    let mut sims: Vec<(EpisodeId, f32)> = rows
        .iter()
        .filter(|(other_id, _)| *other_id != new_id)
        .map(|(other_id, v)| (*other_id, dot(new_vec, v)))
        .filter(|(_, sim)| *sim >= EDGE_THRESHOLD)
        .collect();
    sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sims.truncate(edge_k);
    for (other_id, sim) in sims {
        store.upsert_edge(new_id, other_id, sim)?;
    }
    Ok(())
}

/// EMA update of `self_state.user_profile_centroid` with α = 0.1.
/// `update_centroid` enforces L2 normalization on its return so the
/// stored centroid stays on the unit sphere (D90 §A invariant).
fn update_centroid_after_ingest(
    store: &mut Storage,
    new_embed: &[f32],
) -> Result<(), StorageError> {
    // Round 1 in-house ultrareview fix: pre-flight zero-norm guard.
    // If `new_embed` is empty or all-zero (pathological embedder output
    // when the indexable text degenerates to whitespace), `l2_normalize`
    // would divide by zero and produce a NaN centroid that poisons all
    // future Premakumar self-relevance computations. Skip the update
    // gracefully — the centroid stays on the prior state until a
    // well-formed embedding arrives.
    if new_embed.is_empty() || !new_embed.iter().any(|x| *x != 0.0 && x.is_finite()) {
        return Ok(());
    }
    let prior = store.get_user_centroid()?;
    let (existing, count) = match prior {
        Some((c, n)) => (c, n),
        None => (Vec::new(), 0),
    };
    let updated = if existing.is_empty() {
        salience::l2_normalize(new_embed)
    } else {
        salience::update_centroid(&existing, new_embed, 0.1)
    };
    store.update_user_centroid(&updated, count + 1)
}

/// Concatenate the indexable text fields of an episode in the same
/// preference order `cli::recall::preferred_title` uses for display:
/// prompt → response → command. Empty fields are skipped; the join
/// uses `\n` so the embedder sees a single document.
fn episode_index_text(ep: &Episode) -> String {
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

/// §I — flags vs `--json` mutual exclusion. If `--json` is present
/// AND any payload-bearing flag is also set, refuse. `--source` is
/// always allowed (it's the one CLI-level required field and the
/// handler uses it to pick source-specific validation rules even
/// when the JSON body carries its own `source`).
fn enforce_mutex(args: &IngestArgs) -> Result<(), IngestError> {
    if args.json.is_none() {
        return Ok(());
    }
    let any_flag = args.prompt.is_some()
        || args.response.is_some()
        || args.command.is_some()
        || args.stdout_file.is_some()
        || args.exit_code.is_some()
        || args.cwd.is_some()
        || args.git_branch.is_some()
        || args.project.is_some()
        || args.session.is_some()
        || args.digest.is_some();
    if any_flag {
        return Err(IngestError::MalformedInput(
            "`--json` cannot be combined with flag-based payload fields".into(),
        ));
    }
    Ok(())
}

/// Merge the flag-mode and JSON-mode inputs into a single Episode.
fn build_episode(args: &IngestArgs) -> Result<Episode, IngestError> {
    use std::str::FromStr;
    let (payload_source, payload) = match &args.json {
        Some(path) => {
            let p = read_json_source(path)?;
            (p.source.clone(), p)
        }
        None => (None, JsonPayload::default()),
    };

    // JSON `source` takes priority if present — it's the authoritative
    // description of the payload itself. The CLI `--source` is only a
    // fallback + convenience for hook authors who prefer explicit
    // routing.
    let source_raw = payload_source.unwrap_or_else(|| args.source.clone());
    // Keep an empty-input branch ahead of `FromStr` so the user sees
    // the dedicated "`--source` required" diagnostic instead of the
    // generic regex-fail message that an empty FromStr error would
    // produce. D119's typed error covers the regex-fail case below.
    if source_raw.is_empty() {
        return Err(IngestError::MalformedInput(
            "`--source` (or JSON `source`) is required".into(),
        ));
    }
    // D119 — lift the kebab-case shape gate (R5 regex
    // `^[a-z][a-z0-9-]*$`) into the type system. `FromStr` rejects
    // anything that fails the shape; `MalformedInput` exposes the
    // typed error's `Display` so the user sees the canonical
    // diagnostic without hand-rolled formatting.
    let source = EpisodeSource::from_str(&source_raw)
        .map_err(|e| IngestError::MalformedInput(e.to_string()))?;

    // Resolve `stdout` first because it may need to read
    // `payload.stdout_b64`; the rest are simple clones so moving
    // out of `payload` below is harmless.
    let stdout = read_stdout(args, &payload)?;
    let prompt_text = args.prompt.clone().or(payload.prompt_text);
    let response_text = args.response.clone().or(payload.response_text);
    let command = args.command.clone().or(payload.command);
    let exit_code = args.exit_code.or(payload.exit_code);
    let cwd = args.cwd.clone().or(payload.cwd);
    let git_branch = args.git_branch.clone().or(payload.git_branch);
    let project = args.project.clone().or(payload.project).or_else(|| env_nonempty("SOMA_PROJECT"));
    let session_id =
        args.session.clone().or(payload.session_id).or_else(|| env_nonempty("SOMA_SESSION_ID"));
    let digest = args.digest.clone().or(payload.digest);

    // §G source-specific validation.
    validate_source_payload(
        &source,
        prompt_text.as_deref(),
        response_text.as_deref(),
        command.as_deref(),
    )?;

    // §F timestamps. Absent inputs are filled from now.
    let now_ns = now_ns();
    let ts_start_ns = payload.ts_start_ns.unwrap_or(now_ns);
    let ts_end_ns = payload.ts_end_ns.unwrap_or(ts_start_ns);
    let duration_ms = duration_ms(ts_start_ns, ts_end_ns);

    Ok(Episode {
        ts_start_ns,
        ts_end_ns,
        duration_ms,
        source,
        session_id,
        prompt_text,
        response_text,
        command,
        stdout,
        exit_code,
        cwd,
        git_branch,
        project,
        digest,
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// D112 — per-field byte caps. Stop-hook callers (Claude Code /
/// Cursor / shell adapters) feed `soma ingest` arbitrary user
/// content; without these caps a runaway adapter or adversarial
/// caller can OOM the process or exhaust disk by piping a
/// gigabyte-sized prompt into one ingest. The check runs on the
/// merged Episode so JSON-mode and flag-mode payloads share one
/// enforcement point.
///
/// Why each value:
/// * 1 MiB prompt / response — covers the largest realistic
///   prompt-completion bundle (Claude Sonnet at a 200K-token
///   ceiling decodes to ~600 KiB UTF-8). Doubling it to 1 MiB
///   leaves headroom for embedded code blocks without admitting
///   pathological dumps.
/// * 64 KiB command — terminal commands are short by nature;
///   anything over 64 KiB is almost certainly a pasted log
///   accidentally redirected through `--command`.
/// * 16 MiB stdout — captured terminal stdout is `BLOB`-typed
///   and meant to fit a single command's output; 16 MiB matches
///   the SQLite default `SQLITE_MAX_LENGTH` page-friendly band
///   and keeps a single oversized stdout from blocking ingest.
fn validate_payload_lengths(ep: &Episode) -> Result<(), IngestError> {
    const MAX_PROMPT_BYTES: usize = 1 << 20; // 1 MiB
    const MAX_RESPONSE_BYTES: usize = 1 << 20; // 1 MiB
    const MAX_COMMAND_BYTES: usize = 64 << 10; // 64 KiB — terminal commands are short by nature
    const MAX_STDOUT_BYTES: usize = 16 << 20; // 16 MiB — captured terminal stdout cap

    if let Some(p) = ep.prompt_text.as_ref() {
        if p.len() > MAX_PROMPT_BYTES {
            return Err(IngestError::PayloadTooLarge {
                field: "prompt_text",
                len: p.len(),
                limit: MAX_PROMPT_BYTES,
            });
        }
    }
    if let Some(r) = ep.response_text.as_ref() {
        if r.len() > MAX_RESPONSE_BYTES {
            return Err(IngestError::PayloadTooLarge {
                field: "response_text",
                len: r.len(),
                limit: MAX_RESPONSE_BYTES,
            });
        }
    }
    if let Some(c) = ep.command.as_ref() {
        if c.len() > MAX_COMMAND_BYTES {
            return Err(IngestError::PayloadTooLarge {
                field: "command",
                len: c.len(),
                limit: MAX_COMMAND_BYTES,
            });
        }
    }
    if let Some(s) = ep.stdout.as_ref() {
        if s.len() > MAX_STDOUT_BYTES {
            return Err(IngestError::PayloadTooLarge {
                field: "stdout",
                len: s.len(),
                limit: MAX_STDOUT_BYTES,
            });
        }
    }
    Ok(())
}

/// D69 / R14 — write the secondary embedder's vector for an
/// already-persisted episode. No-op when no secondary embedder is
/// configured (Mini profile or `embed-onnx` feature off). All
/// failure modes are advisory: the primary vector is already on
/// disk, so the episode stays recallable on the primary path even
/// if the secondary write skips. Returns `true` only on a successful
/// write so callers can surface a metric if needed.
fn write_secondary_vector(store: &mut Storage, id: EpisodeId, text: &str) -> bool {
    let Some(sec) = crate::memory::embed::select_secondary_embedder() else {
        return false;
    };
    let sec_vec = sec.embed_passage(text);
    if sec_vec.len() != sec.dim() || !sec_vec.iter().all(|x| x.is_finite()) {
        return false;
    }
    match store.put_vector(id, sec.model_id(), &sec_vec) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                error = %e,
                model = sec.model_id(),
                "secondary vector write failed (advisory)"
            );
            false
        }
    }
}

fn read_json_source(path: &str) -> Result<JsonPayload, IngestError> {
    let raw = if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| IngestError::MalformedInput(format!("stdin read: {e}")))?;
        buf
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| IngestError::MalformedInput(format!("read `{path}`: {e}")))?
    };
    serde_json::from_str::<JsonPayload>(&raw)
        .map_err(|e| IngestError::MalformedInput(format!("JSON parse: {e}")))
}

fn read_stdout(args: &IngestArgs, payload: &JsonPayload) -> Result<Option<Vec<u8>>, IngestError> {
    if let Some(path) = &args.stdout_file {
        // Round 3 audit (2026-04-29) — `--stdout-file` is supposed
        // to receive a tempfile path written by the pty driver
        // (`/tmp/soma-pty-stdout-...`). An adversarial caller could
        // pass `../../../etc/passwd` and SOMA would happily ingest
        // it as "stdout". Reject any path whose canonical form
        // doesn't sit under the system tempdir. This is a defense-
        // in-depth gate — the LaunchAgent / shell-init paths never
        // pass a non-tempdir path, so legitimate users see no
        // change.
        let canon_path = validate_stdout_file_path(path)?;
        let bytes = read_file_bytes(&canon_path)?;
        // P2-A external-review fix — pty `write_temp_stdout` writes
        // `/tmp/soma-pty-stdout-<pid>-<ts>-<seq>.bin` per command;
        // pre-fix the file lingered after ingest, leaking /tmp on
        // long shell sessions. Best-effort delete after a successful
        // read; failure (file missing / permission) is logged at
        // debug and ignored — never fail ingest on cleanup.
        if let Err(e) = std::fs::remove_file(path) {
            tracing::debug!(
                error = %e,
                path = %path.display(),
                "stdout_file cleanup skipped (advisory)"
            );
        }
        return Ok(Some(bytes));
    }
    if let Some(b64) = &payload.stdout_b64 {
        use base64::prelude::{Engine, BASE64_STANDARD};
        let bytes = BASE64_STANDARD
            .decode(b64)
            .map_err(|e| IngestError::MalformedInput(format!("stdout_b64 decode: {e}")))?;
        return Ok(Some(bytes));
    }
    Ok(None)
}

fn read_file_bytes(path: &Path) -> Result<Vec<u8>, IngestError> {
    std::fs::read(path)
        .map_err(|e| IngestError::MalformedInput(format!("read `{}`: {e}", path.display())))
}

/// Round 3 audit (2026-04-29) — defense-in-depth check for the
/// `--stdout-file` argument. The pty driver always writes to
/// `std::env::temp_dir()/soma-pty-stdout-<pid>-<ts>-<seq>.bin`, so
/// any legitimate path canonicalizes inside `temp_dir()`. Reject
/// anything outside (path-traversal attempt) with a clear error so
/// the caller can't trick `soma ingest` into reading arbitrary
/// files as stdout.
fn validate_stdout_file_path(path: &Path) -> Result<PathBuf, IngestError> {
    let tmp = std::env::temp_dir();
    let canon_path = path.canonicalize().map_err(|e| {
        IngestError::MalformedInput(format!(
            "stdout_file `{}` does not exist or is unreadable: {e}",
            path.display()
        ))
    })?;
    let canon_tmp = tmp.canonicalize().unwrap_or(tmp);
    if !canon_path.starts_with(&canon_tmp) {
        return Err(IngestError::MalformedInput(format!(
            "stdout_file `{}` must live under the system tempdir `{}` \
             (resolved to `{}`)",
            path.display(),
            canon_tmp.display(),
            canon_path.display(),
        )));
    }
    Ok(canon_path)
}

fn validate_source_payload(
    source: &EpisodeSource,
    prompt: Option<&str>,
    response: Option<&str>,
    command: Option<&str>,
) -> Result<(), IngestError> {
    // D115-cand close (R5 audit, 2026-04-29) — kebab-case shape
    // (`^[a-z][a-z0-9-]*$`) is enforced at the `EpisodeSource::FromStr`
    // boundary (D119) so by the time `build_episode` reaches us the
    // source is already a typed enum. This function only enforces the
    // §G source-specific payload shape (which capture columns must be
    // populated for which source). Canonical variants
    // (`claude-code` / `codex-cli` / `codex-app` / `terminal` / `cursor` / `continue`) get
    // explicit branches; ad-hoc `Other(_)` falls through.
    match source {
        EpisodeSource::ClaudeCode
        | EpisodeSource::CodexCli
        | EpisodeSource::CodexApp
        | EpisodeSource::Cursor
        | EpisodeSource::Continue => {
            if prompt.is_none() && response.is_none() {
                return Err(IngestError::MalformedInput(format!(
                    "source=`{source}` requires at least one of prompt_text / response_text"
                )));
            }
        }
        EpisodeSource::Terminal => {
            if command.is_none() {
                return Err(IngestError::MalformedInput(
                    "source=`terminal` requires command".into(),
                ));
            }
        }
        // Ad-hoc kebab-case source — caller is responsible for shape.
        // Future-proof per §G + D119 forward-compat.
        EpisodeSource::Other(_) => {}
    }
    Ok(())
}

fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

fn duration_ms(start: i64, end: i64) -> i64 {
    if end < start {
        return 0;
    }
    let ns = end.saturating_sub(start);
    ns / 1_000_000
}

/// §E — resolve the on-disk DB path. Used by the CLI dispatcher in
/// `main.rs`; tests build an `IngestContext` directly and bypass
/// this.
pub fn resolve_db_path(cli_override: Option<&str>) -> Result<PathBuf, IngestError> {
    if let Some(p) = cli_override {
        return Ok(PathBuf::from(p));
    }
    if let Ok(env) = std::env::var("SOMA_DB") {
        if !env.is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| {
        IngestError::Path("home directory not resolvable; set --db-path or $SOMA_DB".into())
    })?;
    Ok(home.join(".soma").join("soma.db"))
}

/// Map `IngestError` to the §D exit-code taxonomy. CLI dispatcher
/// calls this to convert a handler result into the process exit.
///
/// D110-cand (R5 audit, 2026-04-29) — kept `pub` because
/// `crates/soma/src/main.rs` is a SEPARATE binary crate that
/// `extern crate soma` consumes the lib's public surface.
/// `pub(crate)` here would mean "lib-crate-private" and the
/// binary couldn't reach it. Open-source consumers see this
/// fn but the SomaError-shaped contract is stable enough for
/// that to be intentional.
pub fn exit_code_for(err: &IngestError) -> i32 {
    match err {
        IngestError::MalformedInput(_) => 1,
        IngestError::Storage(_) => 2,
        IngestError::Path(_) => 3,
        // D112 — `4` is the next free slot after the original §D
        // taxonomy (1/2/3). Stop-hook callers branch on this code
        // when they want to emit a quieter "your payload is too
        // large" UI rather than the generic "ingest failed" path.
        IngestError::PayloadTooLarge { .. } => 4,
    }
}

/// Emit the §D structured error line to stderr. Previously the
/// dispatcher assembled the JSON with `format!("{{\"error\":\"{e}\"}}")`
/// which produces invalid JSON the moment the error's `Display`
/// output contains `"`, `\`, or a newline. Serializing through
/// `serde_json` guarantees a valid single-line JSON envelope
/// regardless of the human message.
///
/// D110-cand (R5 audit, 2026-04-29) — kept `pub` for the same
/// reason as `exit_code_for`: `crates/soma/src/main.rs` is a
/// separate binary crate. The `eprintln!` here is intentional:
/// stop-hook callers (Claude Code / Cursor) parse this JSON line
/// out of the child stderr; routing through tracing would hide it
/// behind log subscribers.
pub fn emit_error_json(err: &IngestError) {
    let code = match err {
        IngestError::MalformedInput(_) => "malformed",
        IngestError::Storage(_) => "storage",
        IngestError::Path(_) => "path",
        IngestError::PayloadTooLarge { .. } => "payload_too_large",
    };
    let payload = serde_json::json!({
        "error": err.to_string(),
        "code": code,
    });
    // Even if serialization itself were to fail (it cannot for a
    // string+string map), we must still emit *something* on stderr
    // so the caller's stop-hook doesn't see a silent failure.
    let line = serde_json::to_string(&payload)
        .unwrap_or_else(|_| r#"{"error":"<internal>","code":"internal"}"#.to_string());
    eprintln!("{line}");
}

#[cfg(test)]
mod d0_vector_tests {
    use super::*;
    use crate::cli::IngestArgs;
    use tempfile::TempDir;

    fn args_claude(prompt: &str, response: &str) -> IngestArgs {
        IngestArgs {
            // `IngestArgs.source` is the user-facing CLI String; the
            // typed `EpisodeSource` enum lives at the storage edge.
            source: "claude-code".into(),
            session: Some("d0-test".into()),
            prompt: Some(prompt.into()),
            response: Some(response.into()),
            command: None,
            stdout_file: None,
            exit_code: None,
            cwd: None,
            git_branch: None,
            project: Some("soma".into()),
            digest: None,
            json: None,
            db_path: None,
        }
    }

    /// D0 §A — `run_ingest` populates `episode_vectors` with the
    /// HashEmbedder vector. Pre-fix the table was empty, so
    /// `vectors_for_model("hash-v1")` returned 0 rows after a
    /// successful ingest and `soma recall` had nothing to find.
    #[test]
    fn ingest_writes_vector_for_hash_model() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("soma.db");
        let ctx = IngestContext { db_path: db_path.clone() };

        let args = args_claude(
            "Help me refactor the auth middleware in myapp",
            "I suggest splitting into token validation and rate limit",
        );
        let outcome = run_ingest(&args, &ctx).expect("ingest must succeed");
        let IngestOutcome::Stored { episode_id } = outcome;

        let store = Storage::open(&db_path).unwrap();
        // D70 — read back under the *active* embedder's model_id so
        // the test passes regardless of `embed-onnx` feature state.
        let embedder = crate::memory::embed::select_embedder();
        let rows = store.vectors_for_model(embedder.model_id()).unwrap();
        assert_eq!(rows.len(), 1, "exactly one vector row");
        let (id, vec) = &rows[0];
        assert_eq!(*id, episode_id);
        assert_eq!(vec.len(), embedder.dim());
    }

    /// D0 §A — terminal-source episodes (no prompt/response) embed
    /// the command field instead. Without this branch the ingest
    /// path would silently skip `put_vector` for any terminal
    /// episode.
    #[test]
    fn ingest_terminal_episode_embeds_command() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("soma.db");
        let ctx = IngestContext { db_path: db_path.clone() };

        let mut args = args_claude("", "");
        args.source = "terminal".into();
        args.prompt = None;
        args.response = None;
        args.command = Some("cargo test --workspace".into());
        run_ingest(&args, &ctx).expect("terminal ingest must succeed");

        let store = Storage::open(&db_path).unwrap();
        let rows =
            store.vectors_for_model(crate::memory::embed::select_embedder().model_id()).unwrap();
        assert_eq!(rows.len(), 1, "command-only episode still embeds");
    }

    /// D120-cand (R10 audit, 2026-04-30) — exit code mapping for
    /// `IngestError::Path` must remain `3` (per §D taxonomy) so
    /// stop-hook callers can distinguish "DB path resolution failed"
    /// from "DB write failed" (`Storage` = 2) and "bad input"
    /// (`MalformedInput` = 1).
    #[test]
    fn exit_code_for_path_error_is_three() {
        let err = IngestError::Path("home directory not resolvable".into());
        assert_eq!(exit_code_for(&err), 3);
        // Display surface must mention "path" so operators can scan
        // logs for the failure mode.
        let msg = err.to_string();
        assert!(msg.contains("path"), "Display lost the kind tag: {msg}");
    }

    /// D0 §A helper — `episode_index_text` skips empty fields so the
    /// embedder doesn't see leading newlines or stray separators.
    #[test]
    fn episode_index_text_skips_empty_fields() {
        let mut ep = Episode {
            ts_start_ns: 0,
            ts_end_ns: 0,
            duration_ms: 0,
            source: EpisodeSource::Terminal,
            session_id: None,
            prompt_text: Some(String::new()),
            response_text: None,
            command: Some("ls".into()),
            stdout: None,
            exit_code: None,
            cwd: None,
            git_branch: None,
            project: None,
            digest: None,
        };
        assert_eq!(episode_index_text(&ep), "ls");

        ep.prompt_text = Some("hello".into());
        ep.response_text = Some("world".into());
        ep.command = None;
        assert_eq!(episode_index_text(&ep), "hello\nworld");
    }

    #[cfg(unix)]
    #[test]
    fn stdout_file_validation_returns_canonical_target_for_read() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("soma-pty-stdout-target.bin");
        let link = tmp.path().join("soma-pty-stdout-link.bin");
        std::fs::write(&target, b"captured stdout").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let resolved = validate_stdout_file_path(&link).expect("valid tempdir path");
        assert_eq!(resolved, target.canonicalize().expect("canonical target"));
        assert_eq!(read_file_bytes(&resolved).expect("read canonical"), b"captured stdout");
    }
}
