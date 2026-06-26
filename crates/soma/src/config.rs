//! v1 reboot config surface.
//!
//! The canonical shape matches the plan (`[runtime]` / `[capture]`
//! / `[memory]` / `[scheduler]` / `[injection]`). Phase 1 ships
//! only `[runtime]` resolution + profile detection; later phases
//! expand the struct as they land.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Hardware profile — picked by `profile::detect` at first launch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    #[default]
    Mini,
    Studio,
}

/// v1 language policy. `KoEn` is default; `En` available for
/// English-only users who want a tighter recall space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageProfile {
    #[default]
    KoEn,
    En,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// **Reserved for back-compat** — older `config.toml` files
    /// shipped with `[runtime] profile = "mini" | "studio"`. The
    /// effective override now lives in [`profile_override`]; this
    /// field is read-only kept for serde tolerance and ignored at
    /// runtime. Default = `Mini` (matches the historical default).
    #[serde(default)]
    pub profile: Profile,
    /// On-disk override of [`profile::detect`]. `None` (the default)
    /// = auto-detect from RAM. `Some(Profile::Studio)` forces the
    /// Studio code path on a Mini-class machine (e.g. CI runner)
    /// and vice versa. D94-cand external-review fix — pre-fix
    /// `Config::default_v1` returned `Config::default()` and
    /// ignored on-disk overrides entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_override: Option<Profile>,
    /// Recall language policy: `KoEn` (default, ko+en mixed) or
    /// `En` (English-only, tighter retrieval space for users who
    /// don't capture Korean episodes).
    #[serde(default)]
    pub language_profile: LanguageProfile,
    /// `true` (default) keeps a long-running resident process for
    /// hot-path latency. `false` runs every CLI subcommand cold —
    /// useful for CI runners that want zero-state semantics.
    #[serde(default = "default_resident")]
    pub resident: bool,
    /// D105-cand — graceful shutdown drain budget. The accept loop
    /// stops accepting new connections, then waits up to this many
    /// seconds for in-flight handlers to finish before forcing exit.
    /// Default 10 s matches the historical hard-coded value (was
    /// `runtime/resident.rs::SHUTDOWN_DRAIN`). Operators on slow
    /// disks or with long cleanup paths can raise it.
    #[serde(default = "default_shutdown_drain_secs")]
    pub shutdown_drain_secs: u64,
    /// D106-cand — `soma stop` request timeout. Default 5 s. Slow
    /// CI / heavily-loaded laptops occasionally need longer.
    #[serde(default = "default_cli_stop_timeout_secs")]
    pub cli_stop_timeout_secs: u64,
    /// D107-cand — `soma status` request timeout. Default 3 s. Same
    /// rationale as `cli_stop_timeout_secs`.
    #[serde(default = "default_cli_status_timeout_secs")]
    pub cli_status_timeout_secs: u64,
    /// D156-B close — Mini → Studio profile boundary in GiB. Default
    /// `60` (covers 24/36/48 GB Mini-class and splits at the 64 GB
    /// Studio line). Operators with non-Apple-Silicon hardware or
    /// custom hardware tiers (e.g. Mac Pro M2 Ultra at 192 GB) can
    /// shift the boundary without recompiling. Read by
    /// `profile::detect_from_bytes`.
    #[serde(default = "default_studio_threshold_gib")]
    pub studio_threshold_gib: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            profile_override: None,
            language_profile: LanguageProfile::default(),
            resident: default_resident(),
            shutdown_drain_secs: default_shutdown_drain_secs(),
            cli_stop_timeout_secs: default_cli_stop_timeout_secs(),
            cli_status_timeout_secs: default_cli_status_timeout_secs(),
            studio_threshold_gib: default_studio_threshold_gib(),
        }
    }
}

fn default_studio_threshold_gib() -> u32 {
    60
}

fn default_resident() -> bool {
    true
}

fn default_shutdown_drain_secs() -> u64 {
    10
}

fn default_cli_stop_timeout_secs() -> u64 {
    5
}

fn default_cli_status_timeout_secs() -> u64 {
    3
}

/// MCP serve / ContextEnvelope cache knobs. STAGE 2 D86 — TTL
/// config knob, D87 — hit-ratio surfacing, D88 — request
/// coalescing thresholds. The cache still wraps the historical
/// MemoryPack substrate internally, but the operator-facing contract
/// is ContextEnvelope delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// ContextEnvelope cache lifetime in seconds. `0` disables the
    /// cache entirely (useful for debugging / freshness-sensitive
    /// workloads). Default = 30 s, matches `mcp_cache::DEFAULT_TTL`.
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    /// D156-E close — `mcp.rs::resident_preflight` 의 connect+
    /// hello timeout 의 second. default 2 (D158 의 hard-coded
    /// 와 동일). slow CI / heavy host 에서 raise — too low 면
    /// resident 가 정상 인데 fallback 으로 빠짐.
    #[serde(default = "default_preflight_timeout_secs")]
    pub preflight_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            cache_ttl_secs: default_cache_ttl_secs(),
            preflight_timeout_secs: default_preflight_timeout_secs(),
        }
    }
}

fn default_cache_ttl_secs() -> u64 {
    30
}

fn default_preflight_timeout_secs() -> u64 {
    2
}

/// `[memory]` section — knobs that govern the memory pipeline beyond
/// embedding model selection (which lives in the embed factory).
/// Phase 5 admits more knobs (lambda, merge threshold, cap budgets);
/// for now D125 is the first inhabitant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// D125 close (Batch 5, 2026-04-30) — top-K cap on the
    /// `wire_edges_after_ingest` neighbor pass. Default `8` matches
    /// the prior `EDGE_K` const (D92 §C). Out-of-range values clamp
    /// to `[4, 32]` with a `tracing::warn!` so a typo doesn't silently
    /// degrade recall (too low → graph too sparse for PPR seed) or
    /// inflate ingest cost (too high → linear scan on N=10K dwarfs
    /// the actual SQLite write).
    #[serde(default = "default_edge_k")]
    pub edge_k: u32,
    /// D156-A close — `slow_loop::seed_beliefs_pass` 의 episode
    /// window. Default `200` (마지막 200 episode 의 pairwise 검사
    /// 후 corroborate / contradict 후보 추출). Wider (500+) 는
    /// belief 그래프 가 더 dense, narrow (50) 는 sparse.
    #[serde(default = "default_belief_window")]
    pub belief_window: u32,
    /// D156-A close — belief 후보 의 cosine sim threshold.
    /// Default `0.85`. Higher (0.92+) 는 거의 동일 한 episode 만
    /// pair, lower (0.7) 는 약한 semantic match 도 belief 로.
    #[serde(default = "default_belief_threshold")]
    pub belief_threshold: f32,
    /// D156-C close — Ebbinghaus decay rate per day. Default
    /// `0.05` matches `memory::forgetting::DEFAULT_LAMBDA`. Higher
    /// (0.1) 는 더 빨리 잊고, lower (0.02) 는 long-tail 보존.
    #[serde(default = "default_decay_lambda")]
    pub decay_lambda: f32,
    /// D156-C close — note-pin auto-promote 의 salience score
    /// threshold. Default `0.70` (DEFAULT_PIN_THRESHOLD). 70%
    /// 이상 score 의 episode 가 자동 pin.
    #[serde(default = "default_pin_threshold")]
    pub pin_threshold: f32,
    /// D156-C close — slow_loop 의 episode merge cosine cutoff.
    /// Default `0.95` (DEFAULT_MERGE_SIMILARITY). 95% 이상 cosine
    /// 의 episode pair 는 동일 sample 로 통합.
    #[serde(default = "default_merge_similarity")]
    pub merge_similarity: f32,
    /// D156-C close — slow_loop 의 cold-tier (forget candidate)
    /// 의 decay weight cutoff. Default `0.05`
    /// (DEFAULT_COLD_TIER_THRESHOLD). 그 미만 weight 의 episode
    /// 가 forgotten 처리 candidate.
    #[serde(default = "default_cold_tier_threshold")]
    pub cold_tier_threshold: f32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            edge_k: default_edge_k(),
            belief_window: default_belief_window(),
            belief_threshold: default_belief_threshold(),
            decay_lambda: default_decay_lambda(),
            pin_threshold: default_pin_threshold(),
            merge_similarity: default_merge_similarity(),
            cold_tier_threshold: default_cold_tier_threshold(),
        }
    }
}

fn default_decay_lambda() -> f32 {
    crate::memory::forgetting::DEFAULT_LAMBDA
}

fn default_pin_threshold() -> f32 {
    crate::memory::forgetting::DEFAULT_PIN_THRESHOLD
}

fn default_merge_similarity() -> f32 {
    crate::memory::forgetting::DEFAULT_MERGE_SIMILARITY
}

fn default_cold_tier_threshold() -> f32 {
    crate::memory::forgetting::DEFAULT_COLD_TIER_THRESHOLD
}

fn default_edge_k() -> u32 {
    8
}

fn default_belief_window() -> u32 {
    200
}

fn default_belief_threshold() -> f32 {
    0.85
}

/// D125 close — `[memory] edge_k` valid range. Below 4 the graph is
/// too sparse to seed personalized PageRank (multi-hop recall
/// degenerates to single-hop); above 32 the per-ingest neighbor scan
/// dominates wall-clock cost without measurable recall gain.
const EDGE_K_MIN: u32 = 4;
const EDGE_K_MAX: u32 = 32;

/// `[local_compiler]` section — opt-in compiler-helper knobs.
/// SOMA's primary path remains deterministic ContextEnvelope delivery to
/// cloud LLMs; these values configure the optional user-owned Ollama helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCompilerConfig {
    /// ollama HTTP endpoint. Default: `http://localhost:11434`. Must
    /// include scheme (`http://` or `https://`); trailing slash is
    /// stripped automatically. The endpoint must respond to GET
    /// `/api/tags` and POST `/api/chat` per ollama's documented HTTP
    /// API. Connection failures surface as `LocalLlmError::Network`
    /// with a one-line `brew install ollama && ollama serve`
    /// remediation message.
    #[serde(default = "default_local_compiler_endpoint")]
    pub local_endpoint: String,
    /// Model name passed to ollama. Default: `gemma2:9b` (post-v1.0,
    /// see `memory::local_llm::DEFAULT_MODEL` for the swap rationale
    /// from `qwen2.5:7b`). The user must `ollama pull <model>` once
    /// before opt-in local compiler notes can use it.
    #[serde(default = "default_local_compiler_model")]
    pub local_model: String,
    /// Legacy parser-only field retained for old local preview configs.
    /// Current ContextEnvelope rendering uses `ContextRenderArgs` /
    /// `PackConfig` knobs; the local compiler does not own retrieval.
    #[serde(default = "default_local_compiler_recent_n")]
    pub recent_n: u32,
    /// Legacy parser-only field retained for old local preview configs.
    /// Ranking belongs to the ContextEnvelope pack path, not the optional
    /// local compiler helper.
    #[serde(default = "default_local_compiler_semantic_k")]
    pub semantic_k: u32,
    /// Legacy parser-only field retained for old local preview configs.
    /// It is not rendered in the operator-facing context-layer config.
    #[serde(default = "default_local_compiler_retrieval_mass")]
    pub retrieval_mass: f32,
}

impl Default for LocalCompilerConfig {
    fn default() -> Self {
        Self {
            local_endpoint: default_local_compiler_endpoint(),
            local_model: default_local_compiler_model(),
            recent_n: default_local_compiler_recent_n(),
            semantic_k: default_local_compiler_semantic_k(),
            retrieval_mass: default_local_compiler_retrieval_mass(),
        }
    }
}

/// Back-compat type alias for older internal callers. New code should use
/// [`LocalCompilerConfig`].
pub type ChatConfig = LocalCompilerConfig;

fn default_local_compiler_endpoint() -> String {
    crate::memory::local_llm::DEFAULT_ENDPOINT.to_string()
}

fn default_local_compiler_model() -> String {
    crate::memory::local_llm::DEFAULT_MODEL.to_string()
}

fn default_local_compiler_recent_n() -> u32 {
    12
}

fn default_local_compiler_semantic_k() -> u32 {
    15
}

fn default_local_compiler_retrieval_mass() -> f32 {
    0.95
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// `[runtime]` section — profile, language policy, resident
    /// toggle, drain/timeout budgets.
    #[serde(default)]
    pub runtime: RuntimeConfig,
    /// `[mcp]` section — ContextEnvelope cache and stdio knobs.
    #[serde(default)]
    pub mcp: McpConfig,
    /// `[memory]` section — graph + recall knobs. D125 added
    /// `edge_k`; later phases add merge thresholds, lambda, etc.
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Primary `[local_compiler]` section — user-owned local compiler
    /// endpoint for opt-in `compiler_notes`. When absent, the legacy
    /// `[chat]` section below is used as a compatibility fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_compiler: Option<LocalCompilerConfig>,
    /// Legacy `[chat]` section — accepted only as a compatibility
    /// fallback for old config files.
    #[serde(default)]
    pub chat: LocalCompilerConfig,
}

impl Config {
    /// v1 default — no on-disk config needed.
    pub fn default_v1() -> Self {
        Config::default()
    }

    pub fn effective_local_compiler(&self) -> &LocalCompilerConfig {
        self.local_compiler.as_ref().unwrap_or(&self.chat)
    }

    /// Render the resolved operator-facing config without re-advertising
    /// historical `chat` terminology. The on-disk `[local_compiler]` key is
    /// primary; legacy `[chat]` remains a fallback for older config files.
    pub fn render_context_layer_json(&self) -> String {
        let local_compiler = self.effective_local_compiler();
        let value = serde_json::json!({
            "runtime": &self.runtime,
            "mcp": &self.mcp,
            "memory": &self.memory,
            "local_compiler": {
                "local_endpoint": &local_compiler.local_endpoint,
                "local_model": &local_compiler.local_model
            }
        });
        serde_json::to_string_pretty(&value).expect("config JSON serialization is infallible")
    }

    /// D94-cand external-review fix — load `<root>/config.toml`
    /// if present, fall back to defaults on missing file or parse
    /// error. Parse errors emit a `tracing::warn!` and degrade
    /// gracefully (callers should not fail to start because the
    /// user wrote a typo).
    ///
    /// `root` is normally `~/.soma`. Tests inject a tempdir.
    pub fn load_or_default(root: &Path) -> Self {
        let path = root.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Config::default_v1();
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "config.toml read failed; using v1 defaults"
                );
                return Config::default_v1();
            }
        };
        match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                // D125 close — clamp out-of-range `edge_k` after parse.
                // We don't surface a hard error because (a) the rest
                // of the config is fine and (b) the warn surfaces in
                // `RUST_LOG=warn` traces so an operator can fix the
                // typo without losing the resident.
                let raw = cfg.memory.edge_k;
                let clamped = raw.clamp(EDGE_K_MIN, EDGE_K_MAX);
                if clamped != raw {
                    tracing::warn!(
                        path = %path.display(),
                        requested = raw,
                        clamped = clamped,
                        min = EDGE_K_MIN,
                        max = EDGE_K_MAX,
                        "[memory] edge_k out of range; clamping"
                    );
                    cfg.memory.edge_k = clamped;
                }
                cfg
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "config.toml parse failed; using v1 defaults"
                );
                Config::default_v1()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_or_default_returns_default_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = Config::load_or_default(dir.path());
        assert_eq!(cfg.runtime.profile_override, None);
        assert_eq!(cfg.runtime.profile, Profile::Mini);
        // D105/D106/D107-cand defaults match the historical hard-
        // coded values the close PR replaced.
        assert_eq!(cfg.runtime.shutdown_drain_secs, 10);
        assert_eq!(cfg.runtime.cli_stop_timeout_secs, 5);
        assert_eq!(cfg.runtime.cli_status_timeout_secs, 3);
    }

    #[test]
    fn load_or_default_reads_timeout_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[runtime]\n\
             shutdown_drain_secs = 30\n\
             cli_stop_timeout_secs = 15\n\
             cli_status_timeout_secs = 7\n",
        )
        .unwrap();
        let cfg = Config::load_or_default(dir.path());
        assert_eq!(cfg.runtime.shutdown_drain_secs, 30);
        assert_eq!(cfg.runtime.cli_stop_timeout_secs, 15);
        assert_eq!(cfg.runtime.cli_status_timeout_secs, 7);
    }

    #[test]
    fn load_or_default_reads_profile_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, "[runtime]").unwrap();
        writeln!(f, "profile_override = \"studio\"").unwrap();
        drop(f);
        let cfg = Config::load_or_default(dir.path());
        assert_eq!(cfg.runtime.profile_override, Some(Profile::Studio));
    }

    #[test]
    fn load_or_default_handles_parse_error_gracefully() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml = {{").unwrap();
        let cfg = Config::load_or_default(dir.path());
        // Falls back to defaults instead of panicking.
        assert_eq!(cfg.runtime.profile_override, None);
    }

    #[test]
    fn load_or_default_legacy_profile_field_tolerated() {
        // Older config.toml may have `[runtime] profile = "studio"`
        // (the back-compat field). Ensure it parses without error
        // even though the runtime ignores it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[runtime]\nprofile = \"studio\"\n").unwrap();
        let cfg = Config::load_or_default(dir.path());
        assert_eq!(cfg.runtime.profile, Profile::Studio);
        assert_eq!(cfg.runtime.profile_override, None);
    }

    #[test]
    fn load_or_default_reads_primary_local_compiler_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[local_compiler]\n\
             local_endpoint = \"http://127.0.0.1:11435\"\n\
             local_model = \"llama3.1:8b\"\n\
             recent_n = 4\n\
             semantic_k = 5\n\
             retrieval_mass = 0.8\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(dir.path());
        let local = cfg.effective_local_compiler();

        assert_eq!(local.local_endpoint, "http://127.0.0.1:11435");
        assert_eq!(local.local_model, "llama3.1:8b");
        assert_eq!(local.recent_n, 4);
        assert_eq!(local.semantic_k, 5);
        assert!((local.retrieval_mass - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn load_or_default_accepts_legacy_chat_section_as_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[chat]\n\
             local_endpoint = \"http://127.0.0.1:11436\"\n\
             local_model = \"legacy-model\"\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(dir.path());
        let local = cfg.effective_local_compiler();

        assert_eq!(local.local_endpoint, "http://127.0.0.1:11436");
        assert_eq!(local.local_model, "legacy-model");
    }

    #[test]
    fn primary_local_compiler_section_overrides_legacy_chat_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[chat]\n\
             local_endpoint = \"http://legacy:11434\"\n\
             local_model = \"legacy-model\"\n\
             \n\
             [local_compiler]\n\
             local_endpoint = \"http://primary:11434\"\n\
             local_model = \"primary-model\"\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(dir.path());
        let local = cfg.effective_local_compiler();

        assert_eq!(local.local_endpoint, "http://primary:11434");
        assert_eq!(local.local_model, "primary-model");
    }

    #[test]
    fn rendered_config_hides_legacy_chat_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[local_compiler]\n\
             local_endpoint = \"http://primary:11434\"\n\
             local_model = \"primary-model\"\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(dir.path());
        let rendered = cfg.render_context_layer_json();

        assert!(rendered.contains("\"local_compiler\""), "{rendered}");
        assert!(rendered.contains("http://primary:11434"), "{rendered}");
        assert!(rendered.contains("primary-model"), "{rendered}");
        assert!(!rendered.contains("\"chat\""), "{rendered}");
    }

    #[test]
    fn rendered_config_hides_legacy_local_compiler_retrieval_knobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[local_compiler]\n\
             local_endpoint = \"http://primary:11434\"\n\
             local_model = \"primary-model\"\n\
             recent_n = 4\n\
             semantic_k = 5\n\
             retrieval_mass = 0.8\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(dir.path());
        let rendered = cfg.render_context_layer_json();

        assert!(rendered.contains("\"local_compiler\""), "{rendered}");
        assert!(rendered.contains("\"local_endpoint\""), "{rendered}");
        assert!(rendered.contains("\"local_model\""), "{rendered}");
        assert!(!rendered.contains("\"recent_n\""), "{rendered}");
        assert!(!rendered.contains("\"semantic_k\""), "{rendered}");
        assert!(!rendered.contains("\"retrieval_mass\""), "{rendered}");
    }
}
