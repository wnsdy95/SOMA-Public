//! axum router + lifecycle for `soma serve --gui`.
//!
//! v1.x dashboard surface (chunk 1.2):
//!
//! * `GET /` — multi-tab HTML shell (Quality / Recall / Memory /
//!   Architecture). Tab 1 shows optional quality-module diagnostics;
//!   tabs 2-4 are live recall, memory, and architecture views.
//! * `GET /health` — smoke check.
//! * `GET /assets/chart.umd.min.js` — vendored Chart.js 4.4.6 (no
//!   CDN — *digital sovereignty* invariant).
//! * `GET /api/quality/weights` — JSON snapshot of mLSTM / Hopfield /
//!   iPC / ANIL diagnostic rows. Mirrors `soma inspect weights`.
//!   Mock 0 / placebo 0 — every number is read from `self_state.weights_*`
//!   BLOBs at request time. ADR 0015 boundary: this is an operator
//!   diagnostic surface, not product acceptance for ContextEnvelope quality.
//! * `GET /api/training/weights` — historical alias for the quality endpoint.
//!
//! Shutdown:
//!
//! * `serve(...)` returns when the bound future completes — typically
//!   on Ctrl-C (SIGINT). Unix also wires SIGTERM.
//! * `serve_with_listener` accepts a pre-bound `TcpListener` and a
//!   user-owned shutdown future so integration tests can drive
//!   the lifecycle deterministically.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;

use super::config::DashboardConfig;
use super::{memory_state, operations, recall, training};

/// Static asset bundle — Chart.js 4.4.6 UMD minified, vendored
/// under `crates/soma/assets/`. Inlined into the binary so a
/// `cargo install --features dashboard` ships a self-contained
/// dashboard that works offline (ADR 0012 §A2 *build step 0 +
/// asset bundle inline*).
const CHART_JS: &[u8] = include_bytes!("../../../assets/chart.umd.min.js");

/// State shared across handlers — currently just the SQLite path.
/// Future tabs (recall stream / memory state) will add a broadcast
/// channel + cache here without changing the handler signatures.
#[derive(Clone)]
pub struct DashboardState {
    pub db_path: Arc<PathBuf>,
}

/// Spin up the dashboard on `cfg.bind:cfg.port`. Blocks until the
/// shutdown signal fires (Ctrl-C). `--open` causes the OS-native
/// `open` (macOS) / `xdg-open` (Linux) / `start` (Windows) launcher
/// to fire once the server has bound.
pub async fn serve(cfg: DashboardConfig) -> io::Result<()> {
    let db_path = crate::capture::ai_cli::resolve_db_path(None)
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, e.to_string()))?;
    let state = DashboardState { db_path: Arc::new(db_path) };

    let addr = SocketAddr::new(cfg.bind, cfg.port);
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(addr = %bound, "dashboard listening");
    if cfg.open_browser {
        open_browser_async(bound);
    }
    serve_with_state(listener, state, shutdown_signal()).await
}

/// Test-friendly entry — caller owns the listener and the state, and
/// supplies the shutdown future. Lets integration tests bind `:0`
/// for an ephemeral port and trigger shutdown via a `oneshot`.
pub async fn serve_with_state<F>(
    listener: TcpListener,
    state: DashboardState,
    shutdown: F,
) -> io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let app = router(state);
    axum::serve(listener, app.into_make_service()).with_graceful_shutdown(shutdown).await
}

/// Backward-compat wrapper for chunk 1.1's signature — used only
/// by the dashboard smoke test that doesn't need DB state.
pub async fn serve_with_listener<F>(listener: TcpListener, shutdown: F) -> io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let state = DashboardState { db_path: Arc::new(PathBuf::from("/tmp/soma-dashboard-smoke.db")) };
    serve_with_state(listener, state, shutdown).await
}

/// Router definition. Public so integration tests can `oneshot`
/// requests against it without a TCP bind.
pub fn router(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/assets/chart.umd.min.js", get(serve_chart_js))
        .route("/api/quality/weights", get(api_quality_weights))
        .route("/api/training/weights", get(api_quality_weights))
        .route("/api/recall/recent", get(api_recall_recent))
        .route("/api/memory/state", get(api_memory_state))
        .route("/api/memory/timeline", get(api_memory_timeline))
        .route("/api/operations/status", get(api_operations_status))
        .with_state(state)
}

/// State-less router for chunk 1.1's smoke test — only `/` and
/// `/health` survive when no `DashboardState` is wired.
pub fn router_minimal() -> Router {
    Router::new().route("/", get(index_minimal)).route("/health", get(health))
}

async fn health() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

async fn index() -> ([(header::HeaderName, &'static str); 1], Html<&'static str>) {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], Html(LANDING_HTML))
}

async fn index_minimal() -> ([(header::HeaderName, &'static str); 1], Html<&'static str>) {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], Html(LANDING_HTML))
}

async fn serve_chart_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        CHART_JS,
    )
}

async fn api_quality_weights(State(state): State<DashboardState>) -> impl IntoResponse {
    match training::weights_snapshot(&state.db_path) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "quality weights snapshot failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() })))
                .into_response()
        }
    }
}

async fn api_recall_recent(State(state): State<DashboardState>) -> impl IntoResponse {
    match recall::recent_recall_snapshot(&state.db_path) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "recall snapshot failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() })))
                .into_response()
        }
    }
}

async fn api_memory_state(State(state): State<DashboardState>) -> impl IntoResponse {
    match memory_state::memory_state_snapshot(&state.db_path) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "memory state snapshot failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() })))
                .into_response()
        }
    }
}

async fn api_memory_timeline(State(state): State<DashboardState>) -> impl IntoResponse {
    match memory_state::note_pin_timeline(&state.db_path) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "note-pin timeline failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": e.to_string() })))
                .into_response()
        }
    }
}

async fn api_operations_status(State(state): State<DashboardState>) -> impl IntoResponse {
    (StatusCode::OK, Json(operations::operations_snapshot(&state.db_path))).into_response()
}

/// Multi-tab HTML shell — Quality diagnostics, Recall, Memory, and
/// Architecture views. Mock 0 / placebo 0; every displayed number comes from
/// the local DB or reports an explicit empty state.
///
/// All DOM construction in the inline script uses `createElement` +
/// `textContent` (no `innerHTML`) so the output is XSS-safe by
/// construction even if a future API leaks user-controlled bytes.
const LANDING_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>SOMA dashboard</title>
<style>
  :root {
    --fg: #1d1d1f;
    --muted: #6e6e73;
    --accent: #0066cc;
    --bg: #ffffff;
    --panel: #f5f5f7;
    --border: #d2d2d7;
  }
  body {
    font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    margin: 0; padding: 0; color: var(--fg); background: var(--bg);
  }
  header {
    padding: 1.25rem 1.5rem 0.5rem; border-bottom: 1px solid var(--border);
  }
  header h1 { margin: 0; font-size: 1.2rem; font-weight: 600; }
  header .sub { color: var(--muted); font-size: 0.85rem; margin-top: 0.15rem; }
  nav.tabs { display: flex; gap: 0.5rem; padding: 0.75rem 1.5rem 0;
    border-bottom: 1px solid var(--border); background: var(--bg); }
  nav.tabs button {
    background: transparent; border: none; padding: 0.5rem 0.85rem;
    font-size: 0.9rem; color: var(--muted); cursor: pointer;
    border-bottom: 2px solid transparent;
  }
  nav.tabs button.active { color: var(--fg); border-bottom-color: var(--accent); }
  nav.tabs button:hover { color: var(--fg); }
  main { padding: 1.5rem; max-width: 1100px; margin: 0 auto; }
  .panel { background: var(--panel); border: 1px solid var(--border);
    border-radius: 8px; padding: 1rem 1.25rem; margin-bottom: 1rem; }
  .panel h2 { margin: 0 0 0.5rem; font-size: 1rem; font-weight: 600; }
  .panel h3 { margin: 0 0 0.5rem; font-size: 0.95rem; font-weight: 600; }
  .panel .meta { color: var(--muted); font-size: 0.8rem; margin-bottom: 0.75rem; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
  .grid > .panel { margin: 0; }
  /* Chart.js 4 의 responsive + maintainAspectRatio:false 가 부모
     의 height 를 *명시적으로* 잡지 않으면 매 update 마다 canvas
     의 height 를 누적 grow 시키는 알려진 거동. 모든 canvas 를
     `.chart-wrap` 으로 감싸 height 를 고정. */
  .chart-wrap { position: relative; width: 100%; height: 240px; }
  .chart-wrap canvas { display: block; }
  canvas { max-width: 100%; }
  code { background: #eaeaea; padding: 0.1rem 0.35rem; border-radius: 3px;
    font-size: 0.85em; }
  .pending { color: var(--muted); font-style: italic; }
  .none { color: var(--muted); font-size: 0.9rem; padding: 1rem 0; }
  .source { font-size: 0.75rem; color: var(--muted); margin-top: 0.25rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  table th { text-align: left; color: var(--muted); font-weight: 500;
    padding: 0.35rem 0.5rem; border-bottom: 1px solid var(--border); }
  table td { padding: 0.35rem 0.5rem; border-bottom: 1px solid var(--border); }
  table td.num { text-align: right; font-variant-numeric: tabular-nums; }
  .stale { color: var(--muted); }
  .err { color: #b00020; }
  .recall-card { background: var(--panel); border: 1px solid var(--border);
    border-radius: 8px; padding: 0.85rem 1rem; margin-bottom: 0.85rem; }
  .recall-card .head { display: flex; justify-content: space-between;
    align-items: baseline; gap: 1rem; }
  .recall-card .query { font-weight: 600; color: var(--fg); word-break: break-word; }
  .recall-card .stamp { color: var(--muted); font-size: 0.75rem;
    font-variant-numeric: tabular-nums; flex-shrink: 0; }
  .recall-card .meta-line { color: var(--muted); font-size: 0.8rem;
    margin: 0.25rem 0 0.65rem; }
  .recall-card .chart-wrap { height: 160px; }
  .recall-card .resp { background: #fff; border: 1px solid var(--border);
    border-radius: 6px; padding: 0.6rem 0.75rem; margin-top: 0.65rem;
    font-size: 0.85rem; white-space: pre-wrap; word-break: break-word;
    max-height: 9rem; overflow: auto; }
  .recall-card.empty { color: var(--muted); font-size: 0.9rem; padding: 1rem;
    border-style: dashed; }
  /* legacy context artifact preview — D ultrareview 후속 redirect:
     이전 profile/helper artifact 카드가 오른쪽
     으로 계속 늘어나고, 아래는 hidden 처리됨".
     원인: grid 의 1fr 1fr 가 자식의 minimum-content size 받아
     늘어남 (CSS default), pre 의 long line 이 wrap 안 되어 panel
     width 를 push → 가로 overflow + 세로 scroll 차단.
     fix: minmax(0, 1fr) 로 track 이 자식 의 최소 width 강제 안
     받게, overflow-wrap: anywhere 로 long token 까지 break,
     pre 자체에 width: 100% + box-sizing border-box 박아서
     parent grid cell 안에 lock. CSS class names keep the legacy
     persona prefix to avoid dashboard churn. */
  .persona-grid {
    align-items: start;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  .persona-pre {
    display: block;
    box-sizing: border-box;
    width: 100%;
    max-height: 22rem;
    overflow: auto;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .arch-svg { width: 100%; overflow: hidden; padding: 0.5rem 0 0.25rem;
    user-select: none; }
  .arch-svg svg { display: block; width: 100%; height: auto; }
  .arch-node rect { fill: #ffffff; stroke: var(--border); stroke-width: 1.5; }
  .arch-node text { font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
    font-size: 12px; fill: var(--fg); text-anchor: middle; dominant-baseline: middle; }
  .arch-node text.sub { font-size: 10px; fill: var(--muted); }
  .arch-node.arch-tab { cursor: pointer; }
  .arch-node.arch-tab:hover rect { stroke: var(--accent); stroke-width: 2; }
  .arch-node.active rect { stroke: var(--accent); stroke-width: 2.5; }
  .arch-node.active text { fill: var(--accent); }
  .arch-edges path.active { stroke: var(--accent); stroke-width: 2.5; }
  .arch-meta { padding: 0.5rem 0.25rem 0; }
  .status-pill { display: inline-block; padding: 0.12rem 0.45rem;
    border: 1px solid var(--border); border-radius: 999px; background: #fff;
    font-size: 0.75rem; line-height: 1.35; }
  .status-pill.pass { border-color: #188038; color: #188038; }
  .status-pill.warn { border-color: #b06000; color: #8a4700; }
  .status-pill.fail { border-color: #b00020; color: #b00020; }
  .ops-card { background: #fff; border: 1px solid var(--border);
    border-radius: 8px; padding: 0.85rem 1rem; margin-top: 0.75rem; }
  .ops-card h3 { margin: 0 0 0.35rem; }
  .ops-line { font-size: 0.86rem; margin: 0.25rem 0; }
  .ops-list { margin: 0.45rem 0 0; padding-left: 1.1rem; color: var(--muted);
    font-size: 0.82rem; }
  .ops-list li { margin: 0.15rem 0; }
  .ops-command { display: block; margin-top: 0.45rem; white-space: pre-wrap;
    overflow-wrap: anywhere; }
  .ops-mini { margin-top: 0.2rem; color: var(--muted); font-size: 0.76rem;
    overflow-wrap: anywhere; }
  .ops-proof-ladder { display: flex; flex-wrap: wrap; gap: 0.25rem;
    margin-top: 0.3rem; }
  .ops-proof-step { display: inline-flex; align-items: center; gap: 0.25rem;
    border: 1px solid var(--border); border-radius: 999px; padding: 0.08rem 0.4rem;
    font-size: 0.72rem; line-height: 1.35; background: #fff; color: var(--muted); }
  .ops-proof-step.recorded { border-color: #188038; color: #188038; }
  .ops-proof-step.artifact_invalid { border-color: #b00020; color: #b00020; }
  .ops-proof-step.missing { border-color: #b06000; color: #8a4700; }
  .ops-review-list { margin-top: 0.75rem; display: grid; gap: 0.65rem; }
  .ops-review-item { border-top: 1px solid var(--border); padding-top: 0.6rem; }
  .ops-review-head { display: flex; flex-wrap: wrap; align-items: center;
    gap: 0.4rem; font-size: 0.82rem; }
  .ops-review-title { font-weight: 650; }
  .ops-review-summary { margin: 0.25rem 0 0; color: var(--fg);
    font-size: 0.84rem; }
  .ops-review-meta { margin: 0.25rem 0 0; color: var(--muted);
    font-size: 0.78rem; overflow-wrap: anywhere; }
  .ops-objective-list { margin-top: 0.75rem; display: grid; gap: 0.55rem; }
  .ops-objective-item { border-top: 1px solid var(--border); padding-top: 0.55rem; }
  .ops-objective-summary { margin: 0.25rem 0 0; color: var(--fg);
    font-size: 0.82rem; overflow-wrap: anywhere; }
</style>
</head>
<body>
<header>
  <h1>SOMA dashboard</h1>
  <div class="sub">local web transparency surface — <span id="status">connecting…</span></div>
</header>
<nav class="tabs">
  <button data-tab="ops" class="active">Operations</button>
  <button data-tab="quality">Quality</button>
  <button data-tab="recall">Recall</button>
  <button data-tab="memory">Memory</button>
  <button data-tab="arch">Architecture</button>
</nav>
<main>
  <section id="tab-ops">
    <div class="panel">
      <h2>Operations — readiness and review gates</h2>
      <div class="meta">source: <code>soma clients</code> +
        <code>soma projects</code> + <code>soma learning</code> via
        <code>GET /api/operations/status</code> · read-only · refreshed every 5s</div>
      <div id="ops-headline" class="source">loading…</div>
      <div id="ops-dogfood-card" class="ops-card">loading…</div>
      <div class="grid" style="margin-top:1rem">
        <div class="panel">
          <h3>Client binding readiness</h3>
          <table id="ops-clients">
            <thead>
              <tr><th>client</th><th>status</th><th>config</th>
                <th>dogfood</th><th>release</th><th>next action</th></tr>
            </thead>
            <tbody><tr><td colspan="6" class="none">loading…</td></tr></tbody>
          </table>
        </div>
        <div class="panel">
          <h3>Current scope</h3>
          <div id="ops-project-card" class="ops-card">loading…</div>
        </div>
      </div>
      <div class="panel" style="margin-top:1rem">
        <h3>Semantic learning review</h3>
        <div id="ops-learning-card" class="ops-card">loading…</div>
      </div>
    </div>
  </section>
  <section id="tab-quality" hidden>
    <div class="panel">
      <h2>Quality diagnostics — optional module rows</h2>
      <div class="meta">source: <code>self_state.weights_*</code> via
        <code>GET /api/quality/weights</code> · diagnostic only · refreshed every 10s</div>
      <div class="grid">
        <div class="panel">
          <h3>mLSTM working-memory (Q / K / V)</h3>
          <div class="chart-wrap"><canvas id="chart-mlstm"></canvas></div>
          <div id="meta-mlstm" class="source">loading…</div>
        </div>
        <div class="panel">
          <h3>Hopfield K/V (multi-head attention)</h3>
          <div class="chart-wrap"><canvas id="chart-hopfield"></canvas></div>
          <div id="meta-hopfield" class="source">loading…</div>
        </div>
      </div>
      <div class="grid" style="margin-top:1rem">
        <div class="panel">
          <h3>iPC predictor — per-layer weight norm</h3>
          <div class="chart-wrap"><canvas id="chart-pc"></canvas></div>
          <div id="meta-pc" class="source">loading…</div>
        </div>
        <div class="panel">
          <h3>ANIL classifier head</h3>
          <div class="chart-wrap"><canvas id="chart-anil"></canvas></div>
          <div id="meta-anil" class="source">loading…</div>
        </div>
      </div>
    </div>
    <div class="panel">
      <h2>Raw diagnostic rows</h2>
      <div class="meta">live mirror of <code>soma inspect weights</code></div>
      <table id="weights-table">
        <thead>
          <tr><th>Module</th><th>Dim</th><th class="num">steps</th>
            <th class="num">drift / norm</th><th>finite</th>
            <th class="num">saved (ago)</th></tr>
        </thead>
        <tbody><tr><td colspan="6" class="none">loading…</td></tr></tbody>
      </table>
    </div>
  </section>
  <section id="tab-recall" hidden>
    <div class="panel">
      <h2>Recent recall traces — debug only</h2>
      <div class="meta">source: <code>chat_recall_trace</code> table
        via <code>GET /api/recall/recent</code> · diagnostic trace for
        local <code>soma recall</code> and historical REPL debugging,
        not the cloud-LLM read path · refreshed every 5s</div>
      <div id="recall-list" class="recall-list">
        <div class="none">no local recall traces yet — MCP ContextEnvelope
          resources still work without this table</div>
      </div>
    </div>
  </section>
  <section id="tab-memory" hidden>
    <div class="panel">
      <h2>Memory state snapshot</h2>
      <div class="meta">source: <code>episodes</code> + <code>belief_candidates</code> +
        profile artifacts via <code>GET /api/memory/state</code> ·
        refreshed every 15s</div>
      <div class="grid">
        <div class="panel">
          <h3>Last 500 episodes — by source</h3>
          <div class="chart-wrap"><canvas id="chart-mem-source"></canvas></div>
          <div id="meta-mem-source" class="source">loading…</div>
        </div>
        <div class="panel">
          <h3>Last 500 episodes — by project</h3>
          <div class="chart-wrap"><canvas id="chart-mem-project"></canvas></div>
          <div id="meta-mem-project" class="source">loading…</div>
        </div>
      </div>
      <div class="panel" style="margin-top:1rem">
        <h3>Note-pin timeline (last 30 days)</h3>
        <div class="meta">source: <code>note_pins.pinned_at_ns</code>
          via <code>GET /api/memory/timeline</code>. Daily pin count
          — high-salience episode 가 자동 pin 된 시점.</div>
        <div class="chart-wrap"><canvas id="chart-mem-timeline"></canvas></div>
        <div id="meta-mem-timeline" class="source">loading…</div>
      </div>
      <div class="grid" style="margin-top:1rem">
        <div class="panel">
          <h3>Recent corroborations (belief candidates)</h3>
          <div class="meta">두 episode 가 의미적 으로 corroborate.
            cosine ≥ 0.85 + (same command + same exit_code) 또는
            (high cosine cross-command).</div>
          <table id="mem-corroborations">
            <thead>
              <tr><th class="num">a</th><th class="num">b</th>
                <th class="num">score</th><th>evidence</th>
                <th class="num">when</th></tr>
            </thead>
            <tbody><tr><td colspan="5" class="none">loading…</td></tr></tbody>
          </table>
        </div>
        <div class="panel">
          <h3>Recent contradictions (belief candidates)</h3>
          <div class="meta">command flap 또는 outcome mismatch 의
            episode pair. operator review 대상.</div>
          <table id="mem-contradictions">
            <thead>
              <tr><th class="num">a</th><th class="num">b</th>
                <th class="num">score</th><th>evidence</th>
                <th class="num">when</th></tr>
            </thead>
            <tbody><tr><td colspan="5" class="none">loading…</td></tr></tbody>
          </table>
        </div>
      </div>
      <div class="grid persona-grid" style="margin-top:1rem">
        <div class="panel">
          <h3>Legacy short context artifact</h3>
          <div id="mem-persona-meta" class="source">loading…</div>
          <pre id="mem-persona" class="resp persona-pre">loading…</pre>
        </div>
        <div class="panel">
          <h3>Legacy long context artifact</h3>
          <div id="mem-identity-meta" class="source">loading…</div>
          <pre id="mem-identity" class="resp persona-pre">loading…</pre>
        </div>
      </div>
    </div>
  </section>
  <section id="tab-arch" hidden>
    <div class="panel">
      <h2>Architecture — ContextEnvelope bridge</h2>
      <div class="meta">3 narrative diagram 으로 분해.
        context / memory / recall / quality node 클릭 시 해당 tab 으로
        이동. 두 번째 diagram 은 cloud LLM read/inspection/correction
        path 와 capture write path 를 분리해서 보여준다.</div>
    </div>

    <div class="panel">
      <h3>① Capture → Storage → Embedding</h3>
      <div class="meta">capture source 의 모든 episode 가 SQLite 의
        `episodes` + `episode_vectors` 로 적립. embedder factory 가
        profile 따라 Hash / MiniLM / e5-large 결정 (Studio 면 dual-
        store). 18 migration 의 schema 가 episode_edges (PPR) /
        note_pins (D91) / belief_candidates (D84) / debug recall
        trace (D152-1.3) 까지 cover.</div>
      <div class="arch-svg">
        <svg id="arch-graph-1" viewBox="0 0 980 380" xmlns="http://www.w3.org/2000/svg">
          <defs>
            <marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5"
              markerWidth="6" markerHeight="6" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" fill="#7a7a7d"/>
            </marker>
          </defs>
          <!-- Source column -->
          <g class="arch-node" transform="translate(20,20)">
            <rect width="180" height="40" rx="6"/>
            <text x="90" y="25">claude-code stop hook</text>
          </g>
          <g class="arch-node" transform="translate(20,70)">
            <rect width="180" height="40" rx="6"/>
            <text x="90" y="25">terminal shell-init (bash/zsh/fish)</text>
          </g>
          <g class="arch-node" transform="translate(20,120)">
            <rect width="180" height="40" rx="6"/>
            <text x="90" y="25">manual corrections</text>
          </g>
          <g class="arch-node" transform="translate(20,170)">
            <rect width="180" height="40" rx="6"/>
            <text x="90" y="25">cursor / continue</text>
          </g>
          <!-- Ingest validation -->
          <g class="arch-node" transform="translate(240,75)">
            <rect width="180" height="100" rx="6"/>
            <text x="90" y="28">soma ingest</text>
            <text x="90" y="48" class="sub">payload caps (D112)</text>
            <text x="90" y="64" class="sub">digest dedup</text>
            <text x="90" y="80" class="sub">source enum (D119)</text>
          </g>
          <!-- Embedder factory -->
          <g class="arch-node" transform="translate(460,40)">
            <rect width="200" height="80" rx="6"/>
            <text x="100" y="28">select_embedder</text>
            <text x="100" y="48" class="sub">Hash 384d (default)</text>
            <text x="100" y="64" class="sub">MiniLM ONNX 384d</text>
          </g>
          <g class="arch-node" transform="translate(460,140)">
            <rect width="200" height="60" rx="6"/>
            <text x="100" y="28">e5-large 1024d (Studio)</text>
            <text x="100" y="48" class="sub">passage prefix · D138</text>
          </g>
          <!-- Storage tables (right column) -->
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(700,10)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">episodes (18 migration · WAL)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(700,60)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">episode_vectors (Hash / MiniLM / e5)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(700,110)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">episode_edges (PPR · EDGE_K=8)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(700,160)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">note_pins (D91 high-salience)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(700,210)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">belief_candidates (D84)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="recall" transform="translate(700,260)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">debug recall trace (D152-1.3)</text>
          </g>
          <g class="arch-node" transform="translate(700,310)">
            <rect width="260" height="40" rx="6"/>
            <text x="130" y="25">self_state · narrative · weights</text>
          </g>
          <!-- Edges -->
          <g class="arch-edges" stroke="#7a7a7d" stroke-width="1.5" fill="none"
             marker-end="url(#arrow)">
            <path d="M200 40 C 220 40, 220 110, 240 110"/>
            <path d="M200 90 C 220 90, 220 115, 240 115"/>
            <path d="M200 140 C 220 140, 220 125, 240 125"/>
            <path d="M200 190 C 220 190, 220 135, 240 135"/>
            <path d="M420 110 C 440 110, 440 80, 460 80"/>
            <path d="M420 130 C 440 130, 440 170, 460 170"/>
            <path d="M660 80 L 700 30"/>
            <path d="M660 80 L 700 80"/>
            <path d="M660 170 L 700 80"/>
            <path d="M420 150 C 540 150, 660 150, 700 130"/>
            <path d="M420 160 C 540 160, 660 200, 700 180"/>
            <path d="M420 170 C 540 200, 660 230, 700 230"/>
            <path d="M420 180 C 540 230, 660 280, 700 280"/>
          </g>
        </svg>
      </div>
    </div>

    <div class="panel">
      <h3>② Cloud LLM (Claude Code) ContextEnvelope path</h3>
      <div class="meta">Cloud LLM 은 MCP resources/tools 로
        ContextEnvelope 를 읽고 검사한다. Stop hook 은 별도 write path 로
        turn 결과 를 episode 로 적립한다. Prompt-prefix injection 은
        current architecture path 가 아니다.</div>
      <div class="arch-svg">
        <svg id="arch-graph-2" viewBox="0 0 980 360" xmlns="http://www.w3.org/2000/svg">
          <defs>
            <marker id="arrow2" viewBox="0 0 10 10" refX="9" refY="5"
              markerWidth="6" markerHeight="6" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" fill="#7a7a7d"/>
            </marker>
          </defs>
          <!-- Claude Code top -->
          <g class="arch-node" transform="translate(360,10)">
            <rect width="260" height="46" rx="6"/>
            <text x="130" y="22">Claude Code</text>
            <text x="130" y="38" class="sub">cloud LLM session</text>
          </g>
          <!-- Channel A — Stop hook write path -->
          <g class="arch-node" data-node="capture-write" transform="translate(20,90)">
            <rect width="240" height="46" rx="6"/>
            <text x="120" y="22">A. Stop hook write path</text>
            <text x="120" y="38" class="sub">claude-code-stop-hook.sh</text>
          </g>
          <g class="arch-node" transform="translate(20,150)">
            <rect width="240" height="60" rx="6"/>
            <text x="120" y="24">soma ingest --source</text>
            <text x="120" y="42" class="sub">claude-code</text>
            <text x="120" y="56" class="sub">jq frame filter</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(20,225)">
            <rect width="240" height="40" rx="6"/>
            <text x="120" y="25">episode 적립 + embed</text>
          </g>
          <!-- Channel B — MCP resources -->
          <g class="arch-node" transform="translate(370,90)">
            <rect width="240" height="46" rx="6"/>
            <text x="120" y="22">B. MCP resources/read</text>
            <text x="120" y="38" class="sub">soma://context/*</text>
          </g>
          <g class="arch-node" transform="translate(370,150)">
            <rect width="240" height="60" rx="6"/>
            <text x="120" y="22">build_context_envelope</text>
            <text x="120" y="40" class="sub">current / query / project</text>
            <text x="120" y="54" class="sub">session scoped</text>
          </g>
          <g class="arch-node arch-tab" data-tab="recall" transform="translate(370,225)">
            <rect width="240" height="60" rx="6"/>
            <text x="120" y="22">ContextEnvelope</text>
            <text x="120" y="40" class="sub">XML + JSON render</text>
            <text x="120" y="54" class="sub">deterministic fallback</text>
          </g>
          <!-- Channel C — MCP active tools -->
          <g class="arch-node" transform="translate(720,90)">
            <rect width="240" height="46" rx="6"/>
            <text x="120" y="22">C. MCP tools/call</text>
            <text x="120" y="38" class="sub">active inspection + correction</text>
          </g>
          <g class="arch-node" transform="translate(720,150)">
            <rect width="240" height="60" rx="6"/>
            <text x="120" y="22">soma_recall</text>
            <text x="120" y="40" class="sub">soma_context_why</text>
            <text x="120" y="54" class="sub">soma_record_correction</text>
          </g>
          <g class="arch-node arch-tab" data-tab="recall" transform="translate(720,225)">
            <rect width="240" height="60" rx="6"/>
            <text x="120" y="22">auditable sections</text>
            <text x="120" y="40" class="sub">evidence + reasons</text>
            <text x="120" y="54" class="sub">corrections persist</text>
          </g>
          <!-- Bottom: resources/tools feed the cloud LLM working context -->
          <g class="arch-node" transform="translate(360,295)">
            <rect width="260" height="50" rx="6"/>
            <text x="130" y="22">Cloud LLM working context</text>
            <text x="130" y="40" class="sub">cited ContextEnvelope</text>
          </g>
          <g class="arch-edges" stroke="#7a7a7d" stroke-width="1.5" fill="none"
             marker-end="url(#arrow2)">
            <!-- Claude Code → context paths -->
            <path id="edge2-cc-a" d="M400 56 C 280 60, 180 70, 140 90"/>
            <path id="edge2-cc-b" d="M490 56 L 490 90"/>
            <path id="edge2-cc-c" d="M580 56 C 700 60, 800 70, 840 90"/>
            <!-- A → ingest → memory -->
            <path id="edge2-a1" d="M140 136 L 140 150"/>
            <path id="edge2-a2" d="M140 210 L 140 225"/>
            <!-- B → resource → envelope -->
            <path id="edge2-b1" d="M490 136 L 490 150"/>
            <path id="edge2-b2" d="M490 210 L 490 225"/>
            <!-- C → tools → auditable sections -->
            <path id="edge2-c1" d="M840 136 L 840 150"/>
            <path id="edge2-c2" d="M840 210 L 840 225"/>
            <!-- resources/tools merge into the cloud LLM context -->
            <path id="edge2-merge-a" d="M140 265 C 240 290, 360 295, 360 320" stroke-dasharray="4 3"/>
            <path id="edge2-merge-b" d="M490 285 L 490 295"/>
            <path id="edge2-merge-c" d="M840 285 C 720 290, 620 295, 620 310"/>
          </g>
        </svg>
      </div>
      <div class="arch-meta">
        <span id="arch-status" class="meta">resolving last turn…</span>
      </div>
    </div>

    <div class="panel">
      <h3>③ Context quality loop — evidence → optional candidates → proof gates</h3>
      <div class="meta">3-loop scheduler. episode 가 누적될 때마다
        SLOW 가 1h cycle 로 context evidence 와 candidate measurements 를
        정리. mLSTM / Hopfield / iPC / ANIL 은 typed adapter 가
        ContextEnvelope ranking, conflict detection, compression 을 바꿀
        때만 유지되는 optional quality modules.</div>
      <div class="arch-svg">
        <svg id="arch-graph-3" viewBox="0 0 980 380" xmlns="http://www.w3.org/2000/svg">
          <defs>
            <marker id="arrow3" viewBox="0 0 10 10" refX="9" refY="5"
              markerWidth="6" markerHeight="6" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" fill="#7a7a7d"/>
            </marker>
          </defs>
          <!-- Loops column -->
          <g class="arch-node" transform="translate(20,30)">
            <rect width="180" height="56" rx="6"/>
            <text x="90" y="24">FAST loop</text>
            <text x="90" y="42" class="sub">recall path · p95 &lt; 30ms</text>
          </g>
          <g class="arch-node" transform="translate(20,110)">
            <rect width="180" height="56" rx="6"/>
            <text x="90" y="24">WARM loop · 1m</text>
            <text x="90" y="42" class="sub">self_model + cache invalidate</text>
          </g>
          <g class="arch-node" transform="translate(20,190)">
            <rect width="180" height="80" rx="6"/>
            <text x="90" y="24">SLOW loop · 1h</text>
            <text x="90" y="42" class="sub">episode_delta &gt; 0 gate</text>
            <text x="90" y="58" class="sub">D124 + D131</text>
            <text x="90" y="72" class="sub">resilience scan</text>
          </g>
          <!-- Episode delta gate -->
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(240,200)">
            <rect width="160" height="56" rx="6"/>
            <text x="80" y="24">episodes 누적</text>
            <text x="80" y="42" class="sub">delta tracked</text>
          </g>
          <!-- optional context quality modules -->
          <g class="arch-node arch-tab" data-tab="quality" transform="translate(440,30)">
            <rect width="220" height="60" rx="6"/>
            <text x="110" y="24">mLSTM thread candidate</text>
            <text x="110" y="42" class="sub">diagnostic drift rows</text>
          </g>
          <g class="arch-node arch-tab" data-tab="quality" transform="translate(440,110)">
            <rect width="220" height="60" rx="6"/>
            <text x="110" y="24">Hopfield ranking candidate</text>
            <text x="110" y="42" class="sub">ranking corpus required</text>
          </g>
          <g class="arch-node arch-tab" data-tab="quality" transform="translate(440,190)">
            <rect width="220" height="60" rx="6"/>
            <text x="110" y="24">iPC anomaly candidate</text>
            <text x="110" y="42" class="sub">needs cited decision adapter</text>
          </g>
          <g class="arch-node arch-tab" data-tab="quality" transform="translate(440,270)">
            <rect width="220" height="60" rx="6"/>
            <text x="110" y="24">ANIL scope candidate</text>
            <text x="110" y="42" class="sub">diagnostic classifier rows</text>
          </g>
          <!-- Right-side outputs -->
          <g class="arch-node" transform="translate(720,30)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">diagnostic weight rows</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(720,90)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">belief_candidates seed</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(720,150)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">merge similar (≥0.95)</text>
          </g>
          <g class="arch-node arch-tab" data-tab="memory" transform="translate(720,210)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">forget cold (decay &lt;0.05)</text>
          </g>
          <g class="arch-node" transform="translate(720,270)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">legacy narrative (llm-summary)</text>
          </g>
          <g class="arch-node" transform="translate(720,330)">
            <rect width="240" height="44" rx="6"/>
            <text x="120" y="26">legacy profile artifacts</text>
          </g>
          <!-- Edges: episode → optional quality candidates → diagnostic outputs/proof gates -->
          <g class="arch-edges" stroke="#7a7a7d" stroke-width="1.5" fill="none"
             marker-end="url(#arrow3)">
            <path d="M200 230 L 240 228"/>
            <path d="M400 215 C 420 215, 420 60, 440 60"/>
            <path d="M400 220 C 420 220, 420 140, 440 140"/>
            <path d="M400 225 C 420 225, 420 220, 440 220"/>
            <path d="M400 235 C 420 235, 420 300, 440 300"/>
            <path d="M660 60 L 720 50"/>
            <path d="M660 140 L 720 50"/>
            <path d="M660 220 L 720 50"/>
            <path d="M660 300 L 720 50"/>
            <path d="M660 80 C 690 90, 700 100, 720 110"/>
            <path d="M660 150 L 720 170"/>
            <path d="M660 230 L 720 230"/>
            <path d="M660 310 L 720 290"/>
            <path d="M660 320 L 720 350"/>
          </g>
        </svg>
      </div>
      <div class="meta" style="margin-top:0.5rem">
        proof gate — trained weights 는 진단 및 opt-in quality module
        후보. ContextEnvelope 변경 은 연결된 adapter 의 field diff 에서만
        인정.
      </div>
    </div>
  </section>
</main>
<script src="/assets/chart.umd.min.js"></script>
<script>
(function(){
  const tabs = document.querySelectorAll('nav.tabs button');
  const sections = {
    ops:     document.getElementById('tab-ops'),
    quality: document.getElementById('tab-quality'),
    recall:   document.getElementById('tab-recall'),
    memory:   document.getElementById('tab-memory'),
    arch:     document.getElementById('tab-arch'),
  };
  tabs.forEach(b => b.addEventListener('click', () => {
    tabs.forEach(x => x.classList.toggle('active', x === b));
    Object.values(sections).forEach(s => s.hidden = true);
    sections[b.dataset.tab].hidden = false;
  }));

  const charts = {};
  function ensureChart(id, label) {
    if (charts[id]) return charts[id];
    const ctx = document.getElementById(id).getContext('2d');
    charts[id] = new Chart(ctx, {
      type: 'bar',
      data: { labels: [], datasets: [{ label, data: [],
        backgroundColor: '#0066cc' }] },
      options: { responsive: true, maintainAspectRatio: false,
        scales: { y: { beginAtZero: true } },
        plugins: { legend: { display: true } } }
    });
    return charts[id];
  }

  function fmtAgo(savedNs) {
    if (savedNs == null) return 'never';
    const ageS = Math.max(0, (Date.now() - Math.floor(savedNs / 1e6)) / 1000);
    if (ageS < 60) return ageS.toFixed(0) + 's';
    if (ageS < 3600) return (ageS/60).toFixed(1) + 'm';
    return (ageS/3600).toFixed(1) + 'h';
  }

  function setMeta(id, txt, stale) {
    const el = document.getElementById(id);
    if (!el) return;
    el.textContent = txt;
    el.classList.toggle('stale', !!stale);
  }

  function renderMlstm(d) {
    const c = ensureChart('chart-mlstm', 'Frobenius ΔF from identity');
    if (!d) {
      c.data.labels = []; c.data.datasets[0].data = []; c.update();
      setMeta('meta-mlstm', 'no diagnostic row yet — thread_state selector is idle until working_memory_state exists', true);
      return;
    }
    c.data.labels = ['W_q', 'W_k', 'W_v'];
    c.data.datasets[0].data = [d.frobenius_delta.w_q,
      d.frobenius_delta.w_k, d.frobenius_delta.w_v];
    c.update();
    setMeta('meta-mlstm',
      `dim ${d.dim} · ${d.train_steps} diagnostic steps · saved ${fmtAgo(d.saved_at_ns)} ago`,
      false);
  }
  function renderHopfield(d) {
    const c = ensureChart('chart-hopfield', 'Frobenius ΔF from identity');
    if (!d) {
      c.data.labels = []; c.data.datasets[0].data = []; c.update();
      setMeta('meta-hopfield', 'no diagnostic row yet — Hopfield promotion depends on ranking corpus proof', true);
      return;
    }
    c.data.labels = ['W_q', 'W_k', 'W_v'];
    c.data.datasets[0].data = [d.frobenius_delta.w_q,
      d.frobenius_delta.w_k, d.frobenius_delta.w_v];
    c.update();
    setMeta('meta-hopfield',
      `dim ${d.dim} · ${d.num_heads} heads · ${d.train_steps} diagnostic steps · saved ${fmtAgo(d.saved_at_ns)} ago`,
      false);
  }
  function renderPc(layers) {
    const c = ensureChart('chart-pc', '‖W‖ Frobenius');
    if (!layers || layers.length === 0) {
      c.data.labels = []; c.data.datasets[0].data = []; c.update();
      setMeta('meta-pc', 'no PC predictor rows yet', true);
      return;
    }
    c.data.labels = layers.map(l => 'L' + l.layer_idx);
    c.data.datasets[0].data = layers.map(l => l.w_norm);
    c.update();
    const last = layers[layers.length - 1];
    setMeta('meta-pc',
      `${layers.length} layers · ${last.train_steps} diagnostic steps · saved ${fmtAgo(last.saved_at_ns)} ago`,
      false);
  }
  function renderAnil(d) {
    const c = ensureChart('chart-anil', '‖W‖ vs ‖b‖');
    if (!d) {
      c.data.labels = []; c.data.datasets[0].data = []; c.update();
      setMeta('meta-anil', 'no ANIL head yet', true);
      return;
    }
    c.data.labels = ['‖W‖', '‖b‖'];
    c.data.datasets[0].data = [d.w_norm, d.b_norm];
    c.update();
    setMeta('meta-anil',
      `${d.num_classes} classes · ${d.train_steps} diagnostic steps · saved ${fmtAgo(d.saved_at_ns)} ago`,
      false);
  }

  function makeCell(text, opts) {
    const td = document.createElement('td');
    td.textContent = text;
    if (opts && opts.num) td.classList.add('num');
    if (opts && opts.cls) td.classList.add(opts.cls);
    return td;
  }

  // ---------- Operations (read-only goal surface) ----------
  function commandText(cmd) {
    return Array.isArray(cmd) && cmd.length > 0 ? cmd.join(' ') : '—';
  }
  function statusClass(status, ready) {
    const s = String(status || '').toLowerCase();
    if (ready === true || s === 'pass' || s.includes('ready')) return 'pass';
    if (s.includes('fail') || s.includes('blocked') || s.includes('missing')) return 'fail';
    return 'warn';
  }
  function statusPill(status, ready) {
    const span = document.createElement('span');
    span.className = 'status-pill ' + statusClass(status, ready);
    span.textContent = status || 'unknown';
    return span;
  }
  function appendOpsLine(parent, label, value) {
    const p = document.createElement('p');
    p.className = 'ops-line';
    const strong = document.createElement('strong');
    strong.textContent = label + ': ';
    p.appendChild(strong);
    p.appendChild(document.createTextNode(value == null || value === '' ? '—' : String(value)));
    parent.appendChild(p);
  }
  function appendOpsCommand(parent, command) {
    const code = document.createElement('code');
    code.className = 'ops-command';
    code.textContent = commandText(command);
    parent.appendChild(code);
  }
  function appendOpsMini(parent, label, value) {
    if (value == null || value === '' || (Array.isArray(value) && value.length === 0)) return;
    const div = document.createElement('div');
    div.className = 'ops-mini';
    const rendered = Array.isArray(value) ? value.join('; ') : String(value);
    div.textContent = `${label}: ${rendered}`;
    parent.appendChild(div);
  }
  function compactOpsText(value, maxChars) {
    const s = String(value || '');
    if (!maxChars || s.length <= maxChars) return s;
    return s.slice(0, maxChars - 1) + '…';
  }
  function proofLevelShortName(level) {
    const value = String(level || '');
    if (value === 'observed_app_hook') return 'app hook';
    if (value === 'observed_in_client_render') return 'render';
    if (value === 'observed_review_action') return 'review action';
    return value || 'proof';
  }
  function appendOpsProofLadder(parent, statuses) {
    if (!Array.isArray(statuses) || statuses.length === 0) return;
    const ladder = document.createElement('div');
    ladder.className = 'ops-proof-ladder';
    statuses.forEach(status => {
      const pill = document.createElement('span');
      const state = String(status.status || 'unknown');
      pill.className = 'ops-proof-step ' + state.replace(/[^a-z0-9_-]/gi, '_');
      pill.textContent = `${proofLevelShortName(status.proof_level)}: ${state}`;
      ladder.appendChild(pill);
    });
    parent.appendChild(ladder);
  }
  function appendOpsNamedCommand(parent, label, command) {
    if (!Array.isArray(command) || command.length === 0) return;
    const p = document.createElement('p');
    p.className = 'ops-line';
    const strong = document.createElement('strong');
    strong.textContent = label + ':';
    p.appendChild(strong);
    parent.appendChild(p);
    appendOpsCommand(parent, command);
  }
  function appendOpsList(parent, title, items) {
    if (!items || items.length === 0) return;
    const p = document.createElement('p');
    p.className = 'ops-line';
    const strong = document.createElement('strong');
    strong.textContent = title + ':';
    p.appendChild(strong);
    parent.appendChild(p);
    const ul = document.createElement('ul');
    ul.className = 'ops-list';
    items.slice(0, 4).forEach(item => {
      const li = document.createElement('li');
      li.textContent = String(item);
      ul.appendChild(li);
    });
    parent.appendChild(ul);
  }
  function appendOpsReviewCards(parent, cards) {
    if (!Array.isArray(cards) || cards.length === 0) return;
    const label = document.createElement('p');
    label.className = 'ops-line';
    const strong = document.createElement('strong');
    strong.textContent = 'review cards:';
    label.appendChild(strong);
    parent.appendChild(label);
    const list = document.createElement('div');
    list.className = 'ops-review-list';
    cards.slice(0, 5).forEach(card => {
      const item = document.createElement('div');
      item.className = 'ops-review-item';
      const head = document.createElement('div');
      head.className = 'ops-review-head';
      head.appendChild(statusPill(card.status, false));
      const title = document.createElement('span');
      title.className = 'ops-review-title';
      title.textContent = card.title || card.target || card.lane || 'review item';
      head.appendChild(title);
      const lane = document.createElement('span');
      lane.textContent = `${card.lane || 'lane'} · ${card.target || 'target'}`;
      head.appendChild(lane);
      item.appendChild(head);
      const summary = document.createElement('p');
      summary.className = 'ops-review-summary';
      summary.textContent = card.summary || 'No summary available.';
      item.appendChild(summary);
      const meta = document.createElement('p');
      meta.className = 'ops-review-meta';
      const evidence = Array.isArray(card.evidence_refs) ? card.evidence_refs.slice(0, 4).join(', ') : '—';
      meta.textContent = `evidence: ${evidence} · projection: ${card.projection_path || '—'}`;
      item.appendChild(meta);
      if (Array.isArray(card.primary_command) && card.primary_command.length > 0) {
        appendOpsCommand(item, card.primary_command);
      }
      list.appendChild(item);
    });
    parent.appendChild(list);
  }
  function appendOpsDogfoodObjectives(parent, objectives) {
    if (!Array.isArray(objectives) || objectives.length === 0) return;
    const label = document.createElement('p');
    label.className = 'ops-line';
    const strong = document.createElement('strong');
    strong.textContent = 'objective milestones:';
    label.appendChild(strong);
    parent.appendChild(label);
    const list = document.createElement('div');
    list.className = 'ops-objective-list';
    objectives.slice(0, 4).forEach(objective => {
      const item = document.createElement('div');
      item.className = 'ops-objective-item';
      const head = document.createElement('div');
      head.className = 'ops-review-head';
      head.appendChild(statusPill(objective.status, objective.status === 'pass'));
      const title = document.createElement('span');
      title.className = 'ops-review-title';
      title.textContent = objective.objective || 'dogfood objective';
      head.appendChild(title);
      item.appendChild(head);
      const summary = document.createElement('p');
      summary.className = 'ops-objective-summary';
      summary.textContent = compactOpsText(objective.summary, 260);
      item.appendChild(summary);
      appendOpsMini(item, 'evidence', objective.evidence_refs);
      appendOpsNamedCommand(item, 'next command', objective.next_command);
      list.appendChild(item);
    });
    parent.appendChild(list);
  }
  function renderOpsClients(clients) {
    const tbody = document.querySelector('#ops-clients tbody');
    tbody.replaceChildren();
    const rows = clients && Array.isArray(clients.clients) ? clients.clients : [];
    if (rows.length === 0) {
      const tr = document.createElement('tr');
      const td = document.createElement('td');
      td.colSpan = 6;
      td.className = 'none';
      td.textContent = clients && clients.error ? clients.error : 'no client readiness rows';
      tr.appendChild(td);
      tbody.appendChild(tr);
      return;
    }
    rows.forEach(row => {
      const tr = document.createElement('tr');
      tr.appendChild(makeCell(row.client || '—'));
      const statusTd = document.createElement('td');
      statusTd.appendChild(statusPill(row.status, row.ready));
      tr.appendChild(statusTd);
      tr.appendChild(makeCell(row.mcp_context_ready || row.ready ? 'yes' : 'no'));
      const dogfoodObserved =
        row.stored_local_capture_observed || row.observed_capture_dogfood || row.observed_capture_dogfood_evidence;
      tr.appendChild(makeCell(dogfoodObserved ? 'observed' : 'unproven'));
      tr.appendChild(makeCell(row.release_ready || row.ready_for_private_client_claim ? 'ready' : 'pending'));
      const nextTd = document.createElement('td');
      nextTd.textContent = row.operator_next_action_label || row.operator_next_action_id || '—';
      appendOpsProofLadder(nextTd, row.proof_level_statuses);
      appendOpsMini(nextTd, 'missing proof', row.missing_proof_levels);
      appendOpsMini(nextTd, 'ready to record', row.proof_session_ready_to_record_proof_levels);
      appendOpsMini(nextTd, 'real cli probe', row.real_cli_probe_status);
      appendOpsMini(nextTd, 'probe next', row.real_cli_probe_next_action);
      appendOpsMini(nextTd, 'proof step', row.proof_session_next_step_id);
      appendOpsMini(nextTd, 'proof blockers', row.proof_session_blocking_reasons);
      appendOpsMini(nextTd, 'artifact repair', row.artifact_repair_status);
      appendOpsMini(nextTd, 'artifact blockers', row.artifact_repair_blocked_claims);
      const renderScan = row.artifact_repair_render_evidence_scan || {};
      if (renderScan.status) {
        appendOpsMini(nextTd, 'render evidence',
          `${renderScan.status} · placeholders=${renderScan.placeholder_count || 0}`);
        appendOpsMini(nextTd, 'render missing', renderScan.missing_requirements);
      }
      appendOpsMini(nextTd, 'event source', row.expected_event_source);
      appendOpsMini(nextTd, 'binding nonce', row.binding_nonce);
      appendOpsMini(nextTd, 'event jsonl', row.event_jsonl_path);
      appendOpsMini(nextTd, 'event probe', row.event_jsonl_probe_status);
      const privateEvent = row.private_event_observation || {};
      if (privateEvent.status) {
        appendOpsMini(nextTd, 'private event',
          `${privateEvent.status} · release event=${privateEvent.matching_private_event_count || 0} · release nonce=${privateEvent.matching_private_binding_nonce_count || 0} · non-release test=${privateEvent.matching_private_non_release_test_event_count || 0} · manual=${privateEvent.matching_private_non_release_manual_event_count || 0}`);
        appendOpsMini(nextTd, 'event mismatches', privateEvent.latest_spool_mismatches);
      }
      const collector = row.continue_collector || {};
      if (collector.devdata_collector_status || row.continue_devdata_collector_status) {
        appendOpsMini(nextTd, 'Continue collector',
          `${collector.devdata_collector_status || row.continue_devdata_collector_status} · listening=${Boolean(collector.devdata_collector_listening ?? row.continue_devdata_collector_listening)} · devdata=${Boolean(collector.devdata_destination_visible ?? row.continue_devdata_destination_visible)}`);
      }
      appendOpsMini(nextTd, 'Continue config', row.continue_extension_config_status);
      const externalAction = row.proof_session_external_action || {};
      appendOpsMini(nextTd, 'external action', externalAction.action_label || externalAction.action_id);
      appendOpsMini(nextTd, 'minimal prompt', externalAction.suggested_minimal_test_prompt);
      appendOpsMini(nextTd, 'forbidden inputs', externalAction.forbidden_inputs);
      appendOpsNamedCommand(nextTd, 'wait hook',
        row.simple_private_event_wait_command || row.private_event_wait_command);
      const clientNextCommand =
        row.operator_next_command || row.proof_session_next_command || row.artifact_repair_next_command;
      if (Array.isArray(clientNextCommand) && clientNextCommand.length > 0) {
        appendOpsCommand(nextTd, clientNextCommand);
      }
      tr.appendChild(nextTd);
      tbody.appendChild(tr);
    });
  }
  function renderOpsDogfood(clients) {
    const card = document.getElementById('ops-dogfood-card');
    card.replaceChildren();
    if (!clients || clients.status === 'error') {
      card.textContent = clients && clients.error ? clients.error : 'dogfood status unavailable';
      return;
    }
    const dogfood = clients.dogfood_index || {};
    const release = clients.private_app_release_snapshot || {};
    const headline = document.createElement('div');
    headline.className = 'ops-review-head';
    const flowStatus = dogfood.evidence_report_flow_status || 'missing';
    headline.appendChild(statusPill(flowStatus, flowStatus === 'ready'));
    headline.appendChild(statusPill(release.status || dogfood.private_app_release_gate_status || 'pending',
      Boolean(release.ready || dogfood.private_app_release_gate_ready)));
    const title = document.createElement('span');
    title.className = 'ops-review-title';
    title.textContent = 'Dogfood / release gate';
    headline.appendChild(title);
    card.appendChild(headline);
    appendOpsLine(card, 'operator dogfood', flowStatus);
    appendOpsLine(card, 'dogfood objectives',
      `${dogfood.status || 'unknown'} (${dogfood.pass_count || 0} pass, ${dogfood.warning_count || 0} warn, ${dogfood.fail_count || 0} fail)`);
    appendOpsDogfoodObjectives(card, dogfood.objectives);
    appendOpsLine(card, 'release gate',
      `${release.status || dogfood.private_app_release_gate_status || 'unknown'} ready=${Boolean(release.ready || dogfood.private_app_release_gate_ready)}`);
    appendOpsLine(card, 'ready clients',
      (release.ready_clients || dogfood.private_app_release_gate_ready_clients || []).join(', '));
    appendOpsLine(card, 'pending clients',
      (release.pending_clients || dogfood.private_app_release_gate_pending_clients || []).join(', '));
    appendOpsLine(card, 'next', release.primary_next_step || clients.primary_next_step);
    appendOpsMini(card, 'operator artifact', dogfood.evidence_report_flow_summary);
    appendOpsMini(card, 'release proof gate', dogfood.private_app_release_gate_summary);
    appendOpsNamedCommand(card, 'next command',
      release.primary_next_command || dogfood.primary_next_command || clients.primary_next_command);
  }
  function renderOpsProject(projects) {
    const card = document.getElementById('ops-project-card');
    card.replaceChildren();
    if (!projects || projects.status === 'error') {
      card.textContent = projects && projects.error ? projects.error : 'project scope unavailable';
      return;
    }
    const scope = projects.current_terminal_scope || {};
    const op = projects.operator_card || {};
    const focus = projects.focus_project || {};
    const headline = document.createElement('div');
    headline.appendChild(statusPill(projects.status, scope.ready_for_project_scoped_capture));
    card.appendChild(headline);
    appendOpsLine(card, 'persona', scope.active_persona || projects.active_persona);
    appendOpsLine(card, 'project', scope.project || scope.suggested_project || focus.project);
    appendOpsLine(card, 'session', scope.session_id);
    appendOpsLine(card, 'scope', scope.capture_scope_status);
    appendOpsLine(card, 'scoped episodes', focus.episode_count);
    appendOpsLine(card, 'project sessions', focus.session_count);
    appendOpsLine(card, 'historical unscoped', projects.unscoped_episode_count);
    appendOpsLine(card, 'cross-project sessions', (projects.scope_integrity || {}).cross_project_session_count);
    appendOpsLine(card, 'recent sessions', (focus.recent_sessions || []).slice(0, 3).join(', '));
    appendOpsLine(card, 'latest scoped evidence',
      focus.latest_evidence_episode_id ? `episode:${focus.latest_evidence_episode_id} session:${focus.latest_session_id || '—'}` : '');
    appendOpsLine(card, 'missing envs', (projects.missing_scope_envs || scope.missing_scope_envs || []).join(', '));
    appendOpsNamedCommand(card, 'activate terminal', projects.activation_command);
    appendOpsLine(card, 'next', op.primary_next_step || projects.primary_next_step);
    appendOpsCommand(card, op.primary_next_command || projects.primary_next_command);
    appendOpsList(card, 'blocked', op.blocked_claims || projects.scope_warnings);
    appendOpsList(card, 'safe', op.safe_to_claim);
  }
  function renderOpsLearning(learning) {
    const card = document.getElementById('ops-learning-card');
    card.replaceChildren();
    if (!learning || learning.status === 'error') {
      card.textContent = learning && learning.error ? learning.error : 'learning status unavailable';
      return;
    }
    const op = learning.operator_card || {};
    const summary = learning.summary || {};
    const headline = document.createElement('div');
    headline.appendChild(statusPill(learning.status, summary.ready_proposal_count > 0));
    card.appendChild(headline);
    appendOpsLine(card, 'headline', learning.headline || op.headline);
    appendOpsLine(card, 'next', learning.primary_next_step || op.primary_next_step);
    appendOpsLine(card, 'review-only candidates', summary.review_only_candidate_count);
    appendOpsLine(card, 'pending review items', summary.pending_review_item_count);
    appendOpsLine(card, 'cloud draft blockers', summary.cloud_draft_blocked_count);
    appendOpsCommand(card, learning.primary_next_command || op.primary_next_command);
    appendOpsReviewCards(card, learning.review_cards);
    appendOpsList(card, 'blocked', op.blocked_claims);
    appendOpsList(card, 'safe', op.safe_to_claim);
  }
  async function refreshOps() {
    try {
      const r = await fetch('/api/operations/status', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const ops = await r.json();
      const clients = ops.clients || {};
      const projects = ops.projects || {};
      const learning = ops.learning || {};
      document.getElementById('ops-headline').textContent =
        `clients=${clients.status || 'unknown'} · scope=${projects.status || 'unknown'} · learning=${learning.status || 'unknown'} · ${new Date().toLocaleTimeString()}`;
      renderOpsDogfood(clients);
      renderOpsClients(clients);
      renderOpsProject(projects);
      renderOpsLearning(learning);
    } catch (e) {
      document.getElementById('ops-headline').textContent = 'operations poll error: ' + e.message;
    }
  }
  refreshOps();
  setInterval(refreshOps, 5_000);

  function appendRow(tbody, name, dim, steps, drift, finite, savedNs) {
    const tr = document.createElement('tr');
    tr.appendChild(makeCell(name));
    tr.appendChild(makeCell(dim == null ? '—' : String(dim), { num: true }));
    tr.appendChild(makeCell(steps == null ? '—' : String(steps), { num: true }));
    tr.appendChild(makeCell(drift == null ? '—' : drift, { num: true }));
    if (finite == null) {
      tr.appendChild(makeCell('—'));
    } else {
      const td = document.createElement('td');
      const span = document.createElement('span');
      if (finite) {
        span.textContent = 'non-finite';
        span.classList.add('err');
      } else {
        span.textContent = 'ok';
      }
      td.appendChild(span);
      tr.appendChild(td);
    }
    tr.appendChild(makeCell(savedNs == null ? '—' : fmtAgo(savedNs), { num: true }));
    tbody.appendChild(tr);
  }

  function renderTable(snap) {
    const tbody = document.querySelector('#weights-table tbody');
    tbody.replaceChildren();
    if (snap.mlstm) {
      const fd = snap.mlstm.frobenius_delta;
      appendRow(tbody, 'mLSTM', snap.mlstm.dim, snap.mlstm.train_steps,
        ((fd.w_q + fd.w_k + fd.w_v) / 3).toFixed(4),
        snap.mlstm.any_non_finite, snap.mlstm.saved_at_ns);
    } else { appendRow(tbody, 'mLSTM', null, null, null, null, null); }
    if (snap.hopfield) {
      const fd = snap.hopfield.frobenius_delta;
      appendRow(tbody, 'Hopfield (' + snap.hopfield.num_heads + 'h)',
        snap.hopfield.dim, snap.hopfield.train_steps,
        ((fd.w_q + fd.w_k + fd.w_v) / 3).toFixed(4),
        snap.hopfield.any_non_finite, snap.hopfield.saved_at_ns);
    } else { appendRow(tbody, 'Hopfield', null, null, null, null, null); }
    if (snap.anil) {
      appendRow(tbody, 'ANIL head', snap.anil.d_emb, snap.anil.train_steps,
        snap.anil.w_norm.toFixed(4),
        snap.anil.any_non_finite, snap.anil.saved_at_ns);
    } else { appendRow(tbody, 'ANIL head', null, null, null, null, null); }
    (snap.pc_layers || []).forEach(l => {
      appendRow(tbody, 'iPC L' + l.layer_idx, l.d_in + '→' + l.d_out,
        l.train_steps, l.w_norm.toFixed(4),
        l.any_non_finite, l.saved_at_ns);
    });
    if (tbody.children.length === 0) {
      const tr = document.createElement('tr');
      const td = document.createElement('td');
      td.colSpan = 6;
      td.className = 'none';
      td.textContent = 'no diagnostic rows yet';
      tr.appendChild(td);
      tbody.appendChild(tr);
    }
  }

  async function refreshQuality() {
    try {
      const r = await fetch('/api/quality/weights', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const snap = await r.json();
      renderMlstm(snap.mlstm);
      renderHopfield(snap.hopfield);
      renderPc(snap.pc_layers);
      renderAnil(snap.anil);
      renderTable(snap);
      document.getElementById('status').textContent = 'live · last poll ' +
        new Date().toLocaleTimeString();
    } catch (e) {
      document.getElementById('status').textContent = 'error: ' + e.message;
    }
  }
  refreshQuality();
  setInterval(refreshQuality, 10_000);

  // ---------- Recall (View 2) ----------
  const recallCharts = new Map(); // canvas-id → Chart
  function makeRecallCard(t) {
    const card = document.createElement('div');
    card.className = 'recall-card';

    const head = document.createElement('div');
    head.className = 'head';
    const q = document.createElement('span');
    q.className = 'query';
    q.textContent = t.query_text;
    const ts = document.createElement('span');
    ts.className = 'stamp';
    ts.textContent = new Date(Math.floor(t.created_at_ns / 1e6))
      .toLocaleString();
    head.appendChild(q); head.appendChild(ts);
    card.appendChild(head);

    const metaLine = document.createElement('div');
    metaLine.className = 'meta-line';
    const proj = t.project ? ('project: ' + t.project) : 'project: —';
    const dur = t.duration_ms != null ? (t.duration_ms + ' ms') : '—';
    const respLen = t.response_chars != null ? (t.response_chars + ' chars') : '—';
    metaLine.textContent = `${proj} · pack ${t.pack_count} ·
      ${(t.top_k || []).length} semantic matches · response ${respLen} · ${dur}`;
    card.appendChild(metaLine);

    const wrap = document.createElement('div');
    wrap.className = 'chart-wrap';
    const canvas = document.createElement('canvas');
    const cid = 'recall-canvas-' + t.id;
    canvas.id = cid;
    wrap.appendChild(canvas);
    card.appendChild(wrap);

    if (t.response_text) {
      const resp = document.createElement('div');
      resp.className = 'resp';
      resp.textContent = t.response_text;
      card.appendChild(resp);
    }

    return { card, cid, topK: t.top_k || [] };
  }
  function renderRecall(traces) {
    const list = document.getElementById('recall-list');
    // 매 render 마다 stale Chart instance 모두 destroy — 새 DOM
    // 의 canvas 와 cached chart 를 매칭 시키면 chart.update() 가
    // detached canvas 에 fire 해서 새 canvas 가 빈 채로 남고,
    // user 입장에서 "그래프가 잠깐 나타나고 없어지는" 거동이 됨.
    recallCharts.forEach(c => c.destroy());
    recallCharts.clear();
    list.replaceChildren();
    if (!traces || traces.length === 0) {
      const div = document.createElement('div');
      div.className = 'recall-card empty';
      div.textContent = 'no local recall traces yet — MCP ContextEnvelope resources still work without this table';
      list.appendChild(div);
      return;
    }
    traces.forEach(t => {
      const { card, cid, topK } = makeRecallCard(t);
      list.appendChild(card);
      const ctx = document.getElementById(cid).getContext('2d');
      const labels = topK.map(p => 'ep ' + p.episode_id);
      const data = topK.map(p => p.raw_sim);
      const chart = new Chart(ctx, {
        type: 'bar',
        data: { labels, datasets: [{ label: 'cosine sim',
          data, backgroundColor: '#0066cc' }] },
        options: { responsive: true, maintainAspectRatio: false,
          scales: { y: { beginAtZero: true, suggestedMax: 1 } },
          plugins: { legend: { display: false } } }
      });
      recallCharts.set(cid, chart);
    });
  }
  async function refreshRecall() {
    try {
      const r = await fetch('/api/recall/recent', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const j = await r.json();
      renderRecall(j.traces);
    } catch (e) {
      // swallow — keep the last successful render visible
    }
  }
  refreshRecall();
  setInterval(refreshRecall, 5_000);

  // ---------- Memory state (View 3) ----------
  function renderBarChart(canvasId, labels, data, label) {
    const ctx = document.getElementById(canvasId).getContext('2d');
    if (charts[canvasId]) {
      charts[canvasId].data.labels = labels;
      charts[canvasId].data.datasets[0].data = data;
      charts[canvasId].update();
      return;
    }
    charts[canvasId] = new Chart(ctx, {
      type: 'bar',
      data: { labels, datasets: [{ label, data, backgroundColor: '#0066cc' }] },
      options: {
        indexAxis: 'y',
        responsive: true, maintainAspectRatio: false,
        scales: { x: { beginAtZero: true } },
        plugins: { legend: { display: false } }
      }
    });
  }
  function renderBeliefTable(tbodySelector, rows, emptyText) {
    const tbody = document.querySelector(tbodySelector);
    tbody.replaceChildren();
    if (!rows || rows.length === 0) {
      const tr = document.createElement('tr');
      const td = document.createElement('td');
      td.colSpan = 5;
      td.className = 'none';
      td.textContent = emptyText;
      tr.appendChild(td);
      tbody.appendChild(tr);
      return;
    }
    rows.forEach(r => {
      const tr = document.createElement('tr');
      tr.appendChild(makeCell(String(r.episode_a_id), { num: true }));
      tr.appendChild(makeCell(String(r.episode_b_id), { num: true }));
      tr.appendChild(makeCell(r.score == null ? '—' : r.score.toFixed(3), { num: true }));
      tr.appendChild(makeCell(r.evidence ?? '—'));
      tr.appendChild(makeCell(fmtAgo(r.created_at_ns), { num: true }));
      tbody.appendChild(tr);
    });
  }
  function renderContradictions(rows) {
    renderBeliefTable('#mem-contradictions tbody', rows, 'no contradictions yet');
  }
  function renderCorroborations(rows) {
    renderBeliefTable('#mem-corroborations tbody', rows, 'no corroborations yet');
  }
  async function refreshMemory() {
    try {
      const r = await fetch('/api/memory/state', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const s = await r.json();
      const sourceLabels = (s.by_source || []).map(p => p.key);
      const sourceData   = (s.by_source || []).map(p => p.count);
      renderBarChart('chart-mem-source', sourceLabels, sourceData, 'episodes');
      const totalSource = sourceData.reduce((a,b)=>a+b, 0);
      setMeta('meta-mem-source',
        `${totalSource} of last ${s.window} episodes (totals: ${s.totals.episodes} ep / ${s.totals.vectors} vec)`,
        false);

      const projLabels = (s.by_project || []).map(p => p.key);
      const projData   = (s.by_project || []).map(p => p.count);
      renderBarChart('chart-mem-project', projLabels, projData, 'episodes');
      setMeta('meta-mem-project',
        `${projLabels.length} distinct projects in last ${s.window} episodes`,
        false);

      renderContradictions((s.beliefs && s.beliefs.contradictions_recent) || []);
      renderCorroborations((s.beliefs && s.beliefs.corroborations_recent) || []);

      const card = document.getElementById('mem-persona');
      const ident = document.getElementById('mem-identity');
      const profile = s.context_profile || s.persona || {};
      card.textContent = profile.card_preview || '(no short context artifact written yet)';
      ident.textContent = profile.identity_preview || '(no long context artifact written yet)';
      // D150 close — `<soma-context>` token cost 표시. profile/context
      // helper size 를 budget 추적 가시화에 사용.
      const cardMeta = document.getElementById('mem-persona-meta');
      const identMeta = document.getElementById('mem-identity-meta');
      if (profile.card_chars != null) {
        cardMeta.textContent =
          `${profile.card_chars.toLocaleString()} chars · ~${profile.card_est_tokens.toLocaleString()} tokens (context helper budget)`;
      }
      if (profile.identity_chars != null) {
        identMeta.textContent =
          `${profile.identity_chars.toLocaleString()} chars · ~${profile.identity_est_tokens.toLocaleString()} tokens`;
      }
    } catch (e) {
      // swallow — keep last good render
    }
  }
  refreshMemory();
  setInterval(refreshMemory, 15_000);

  // ---------- Memory note-pin timeline (D164) ----------
  function fmtDay(ts) {
    const d = new Date(ts * 1000);
    return (d.getMonth() + 1).toString() + '/' + d.getDate().toString();
  }
  async function refreshTimeline() {
    try {
      const r = await fetch('/api/memory/timeline', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const j = await r.json();
      const days = j.days || [];
      const labels = days.map(d => fmtDay(d.day_ts));
      const data = days.map(d => d.count);
      const ctx = document.getElementById('chart-mem-timeline').getContext('2d');
      if (charts['chart-mem-timeline']) {
        charts['chart-mem-timeline'].data.labels = labels;
        charts['chart-mem-timeline'].data.datasets[0].data = data;
        charts['chart-mem-timeline'].update();
      } else {
        charts['chart-mem-timeline'] = new Chart(ctx, {
          type: 'line',
          data: { labels, datasets: [{ label: 'pins', data,
            borderColor: '#0066cc', backgroundColor: 'rgba(0,102,204,0.15)',
            tension: 0.25, fill: true, pointRadius: 2 }] },
          options: { responsive: true, maintainAspectRatio: false,
            scales: { y: { beginAtZero: true, ticks: { precision: 0 } } },
            plugins: { legend: { display: false } } }
        });
      }
      const total = data.reduce((a,b)=>a+b, 0);
      setMeta('meta-mem-timeline',
        `${total} pins across last ${j.window_days || 30} days`,
        days.length === 0);
    } catch (e) {
      // swallow — keep last good render
    }
  }
  refreshTimeline();
  setInterval(refreshTimeline, 30_000);

  // ---------- Architecture (D168 — 3 narrative diagram) ----------
  // 모든 arch-tab 노드 click 시 해당 tab 으로 navigate. 3 svg
  // 모두 동일 handler.
  function activateTab(name) {
    tabs.forEach(x => x.classList.toggle('active', x.dataset.tab === name));
    Object.values(sections).forEach(s => s.hidden = true);
    if (sections[name]) sections[name].hidden = false;
  }
  document.querySelectorAll('#tab-arch .arch-node.arch-tab')
    .forEach(g => g.addEventListener('click', () => activateTab(g.dataset.tab)));

  // Active path highlight — debug recall traces only affect the MCP
  // resources/tools side. Capture/write remains separate.
  const D2_ACTIVE_EDGES = [
    'edge2-cc-b', 'edge2-cc-c',
    'edge2-b1', 'edge2-b2',
    'edge2-c1', 'edge2-c2',
    'edge2-merge-b', 'edge2-merge-c',
  ];
  function clearActivePath() {
    document.querySelectorAll('#arch-graph-2 .arch-edges path.active')
      .forEach(p => p.classList.remove('active'));
  }
  function activatePath() {
    D2_ACTIVE_EDGES.forEach(id => {
      const el = document.getElementById(id);
      if (el) el.classList.add('active');
    });
  }
  async function refreshArch() {
    const status = document.getElementById('arch-status');
    if (!status) return;
    try {
      const r = await fetch('/api/recall/recent?limit=1', { cache: 'no-store' });
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const j = await r.json();
      const t = (j.traces || [])[0];
      clearActivePath();
      if (!t) {
        status.textContent = 'no debug recall traces yet — MCP ContextEnvelope path still available';
        return;
      }
      const ageS = Math.max(0, (Date.now() - Math.floor(t.created_at_ns / 1e6)) / 1000);
      if (ageS <= 30) {
        activatePath();
        status.textContent = `active debug recall trace — ${ageS.toFixed(0)}s ago, query: ${t.query_text}`;
      } else {
        status.textContent = `idle — last debug recall trace ${fmtAgo(t.created_at_ns)} ago`;
      }
    } catch (e) {
      status.textContent = 'arch poll error: ' + e.message;
    }
  }
  refreshArch();
  setInterval(refreshArch, 5_000);
})();
</script>
</body>
</html>
"##;

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };

    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("dashboard shutdown signal received");
}

fn open_browser_async(addr: SocketAddr) {
    let url = format!("http://{addr}");
    tokio::spawn(async move {
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "start"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(cmd).arg(&url).spawn();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = router_minimal();
        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 64).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn assets_chart_js_is_served() {
        let state = DashboardState { db_path: Arc::new(PathBuf::from("/tmp/soma-test.db")) };
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder().uri("/assets/chart.umd.min.js").body(Body::empty()).unwrap(),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.starts_with("application/javascript"), "got {ct}");
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        assert!(body.len() > 50_000, "tiny body: {}", body.len());
        let head = String::from_utf8_lossy(&body[..200.min(body.len())]);
        assert!(head.contains("chart.js") || head.contains("Chart.js"), "head:\n{head}");
    }
}
