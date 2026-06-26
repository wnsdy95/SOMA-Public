//! Episode types — G6 canonical shape (discussion 0024 §D).
//!
//! `Episode` is the **write-side** shape: the caller supplies all
//! observational fields (temporal / capture payload / provenance),
//! but does **not** supply kernel-derived fields (memory_tier /
//! salience / digest) — those are filled by the warm loop. The
//! row's `id` is assigned by SQLite on insert.
//!
//! `StoredEpisode` is the **read-side** shape: everything the DB
//! row contains, including `id` + kernel-derived columns.
//!
//! `EpisodeId` is a thin newtype over i64 — SQLite ROWID. Chosen
//! over UUID/ULID because v1 is single-machine and opaque opaque
//! IDs in `soma forget --episode=<id>` hurt UX (discussion 0024 §G).
//!
//! `EpisodeSource` (D119) is the typed discriminator over the
//! capture-source taxonomy. Pre-D119 the field was `String`, which
//! let typos and arbitrary text reach the storage layer; the R5
//! `validate_source_payload` regex `^[a-z][a-z0-9-]*$` was a runtime
//! guard. D119 lifts that guard into the type system: every code
//! path now operates on a typed enum, and the regex check lives at
//! the FromStr boundary. Wire schema (`episodes.source` SQLite TEXT
//! column, MCP/ContextEnvelope JSON output, and legacy debug
//! payload `source` fields) stays kebab-case strings — the
//! boundary conversion happens at
//! `Display` / `Serialize` (write side) and `FromStr` / `map_row`
//! (read side).

use std::fmt;
use std::str::FromStr;

/// Row ID assigned by SQLite on insert.
pub type EpisodeId = i64;

/// Capture-source discriminator (D119). Canonical variants are the
/// upstream tools the SOMA stop-hook + pty-driver already wire
/// (`terminal`, `claude-code`, `codex-cli`, `codex-app`, `cursor`, `continue`);
/// `Other(String)` preserves forward-compat for ad-hoc sources whose strings still
/// match the kebab-case regex `^[a-z][a-z0-9-]*$` (R5 guard).
///
/// `Display` produces the kebab-case wire string; `FromStr` parses
/// the same shape and rejects anything that fails the regex. The
/// invariant is `from_str(x.to_string()) == Ok(x)` for every legal
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EpisodeSource {
    Terminal,
    ClaudeCode,
    CodexCli,
    CodexApp,
    Cursor,
    Continue,
    /// Forward-compat for ad-hoc sources.
    ///
    /// **Write-path contract** — when constructed via `FromStr`,
    /// the inner string is guaranteed to match the kebab-case regex
    /// `^[a-z][a-z0-9-]*$` (R5 guard at ingest boundary).
    ///
    /// **Read-path tolerance** — `map_row` (storage read) falls back
    /// to `Other(raw)` for legacy / unknown strings that fail the
    /// regex, so a corrupt or pre-D119 row never crashes
    /// `recall` / `inspect` / extractor scans. The wire-schema
    /// invariant (kebab-case strings) is enforced on writes only.
    Other(String),
}

impl EpisodeSource {
    /// Borrow the kebab-case wire string. Single source of truth for
    /// `Display`, `Serialize`, and `PartialEq<str>`; canonical
    /// variants resolve to `&'static str` and `Other` borrows its
    /// inner buffer (no allocation in either branch).
    fn as_str(&self) -> &str {
        match self {
            EpisodeSource::Terminal => "terminal",
            EpisodeSource::ClaudeCode => "claude-code",
            EpisodeSource::CodexCli => "codex-cli",
            EpisodeSource::CodexApp => "codex-app",
            EpisodeSource::Cursor => "cursor",
            EpisodeSource::Continue => "continue",
            EpisodeSource::Other(s) => s.as_str(),
        }
    }
}

impl fmt::Display for EpisodeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// FromStr error — single leg, since `^[a-z][a-z0-9-]*$` is the only
/// rejection condition. Empty / regex-fail collapse to one variant
/// because callers (ingest CLI, JSON payload parse) treat them
/// identically as `MalformedInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeSourceError {
    /// The input failed the kebab-case regex `^[a-z][a-z0-9-]*$`
    /// (or was empty). The contained `String` echoes the offending
    /// input so error messages can quote it.
    Invalid(String),
}

impl fmt::Display for EpisodeSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EpisodeSourceError::Invalid(raw) => write!(
                f,
                "source `{raw}` must match ^[a-z][a-z0-9-]*$ \
                 (lowercase ASCII letters, digits, or `-`; first char a letter)"
            ),
        }
    }
}

impl std::error::Error for EpisodeSourceError {}

impl FromStr for EpisodeSource {
    type Err = EpisodeSourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Match the canonical strings first — common case on the
        // read hot path (`map_row`) — so a successful parse skips
        // the regex pass entirely. Only ad-hoc / `Other` strings
        // fall through to the kebab-case shape gate.
        match s {
            "terminal" => Ok(EpisodeSource::Terminal),
            "claude-code" => Ok(EpisodeSource::ClaudeCode),
            "codex-cli" => Ok(EpisodeSource::CodexCli),
            "codex-app" => Ok(EpisodeSource::CodexApp),
            "cursor" => Ok(EpisodeSource::Cursor),
            "continue" => Ok(EpisodeSource::Continue),
            other if is_valid_source_shape(other) => Ok(EpisodeSource::Other(other.to_string())),
            other => Err(EpisodeSourceError::Invalid(other.to_string())),
        }
    }
}

/// Kebab-case shape gate: non-empty + first char ASCII lowercase
/// letter + rest ASCII lowercase / digit / `-`. The single source of
/// truth for the R5 regex `^[a-z][a-z0-9-]*$` (D115 audit close →
/// D119 type-system lift).
fn is_valid_source_shape(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    first_ok && rest_ok
}

// ---------------------------------------------------------------------------
// Equality helpers — let test assertions and runtime checks compare
// directly against the kebab-case wire string without round-tripping
// through Display. Reflects the type-level invariant
// `EpisodeSource::ClaudeCode == "claude-code"`.
// ---------------------------------------------------------------------------

impl PartialEq<str> for EpisodeSource {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for EpisodeSource {
    fn eq(&self, other: &&str) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

// ---------------------------------------------------------------------------
// Serde — write-side serialization to the kebab-case string. Reads
// go through `map_row`'s `FromStr`, not serde, so we don't need a
// `Deserialize` impl on the enum (the JSON ingest payload parses the
// string at the `JsonPayload` boundary).
// ---------------------------------------------------------------------------

impl serde::Serialize for EpisodeSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Borrow the canonical &str directly — avoids the
        // `collect_str` intermediate allocation that would re-run
        // `Display::fmt` for every JSON serialization.
        serializer.serialize_str(self.as_str())
    }
}

/// Observational episode — the payload the capture adapter writes.
///
/// All capture-payload columns (`prompt_text`, `response_text`,
/// `command`, `stdout`, `exit_code`) are `Option` because SOMA
/// stores AI episodes and terminal episodes in the same table
/// (§D canonical). The `source` discriminator picks which columns
/// are meaningful.
///
/// **D160 close — write-side vs read-side boundary**:
///
/// `Episode` is the *write-side* shape — what `soma ingest`
/// constructs and what `Storage::append_episode_with_vector`
/// consumes. Kernel-derived columns (`id`, `memory_tier`,
/// `salience`) are absent because the SQLite layer fills them on
/// insert. The mirror read-side type is [`StoredEpisode`] — same
/// payload columns plus the kernel-derived ones, returned by
/// `Storage::recent_episodes` / `get_live_episode`.
#[derive(Debug, Clone)]
pub struct Episode {
    /// Wall-clock at turn start (ns since UNIX epoch). Caller stamps
    /// `SystemTime::now()` if not already supplied via JSON payload
    /// (D27 §B). Storage preserves the ns precision; ms-truncation
    /// happens only inside `duration_ms`.
    pub ts_start_ns: i64,
    /// Wall-clock at turn end (ns since UNIX epoch). Capture path
    /// guarantees `ts_end_ns >= ts_start_ns`; bookkeeping that
    /// inverts (e.g. monotonic clock skew across reboots) is
    /// clamped at `duration_ms` derivation.
    pub ts_end_ns: i64,
    /// Pre-computed `(ts_end_ns - ts_start_ns) / 1_000_000`, clamped
    /// to non-negative. Stored alongside the timestamps for fast
    /// duration filters in `recent_episodes` queries without a
    /// derived-column trigger.
    pub duration_ms: i64,

    /// Discriminator that picks which payload columns are meaningful
    /// (D119 typed enum). `ClaudeCode` / `CodexCli` / `CodexApp` /
    /// `Cursor` / `Continue` populate prompt+response; `Terminal` populates
    /// command+stdout+exit_code. Historical rows may still carry
    /// `Other("soma-chat")`.
    pub source: EpisodeSource,
    /// Coarse-grained grouping key — Claude Code / Codex CLI / Codex app session
    /// uuid or SOMA-managed id for AI sources, ad-hoc for terminal, and historical `chat-<ns>`
    /// rows from the removed local REPL.
    /// `None` for one-shot ingests that don't carry session
    /// affinity (e.g. external shell-init).
    pub session_id: Option<String>,

    /// AI source: the user prompt that triggered this turn. D112
    /// caps at 1 MiB.
    pub prompt_text: Option<String>,
    /// AI source: the assistant's response text. D112 caps at 1 MiB.
    pub response_text: Option<String>,
    /// Terminal source: the command line as executed. D112 caps at
    /// 64 KiB.
    pub command: Option<String>,
    /// Terminal source: captured stdout. D112 caps at 16 MiB.
    pub stdout: Option<Vec<u8>>,
    /// Terminal source: process exit code (`0` success). Used by
    /// `self_model::extractors::exit_success` to derive command-
    /// success rate over time.
    pub exit_code: Option<i32>,

    /// Working directory at capture time. Stored verbatim — the
    /// project-aware recall lens uses `basename(cwd)` as the
    /// partition key, but the full path is preserved for forensics.
    pub cwd: Option<String>,
    /// `git branch --show-current` at capture time, when the cwd
    /// is inside a git repo. `None` for non-git directories.
    pub git_branch: Option<String>,
    /// `basename(PWD)` at capture time. Used by project-scoped
    /// recall and ContextEnvelope compilation. Equality match
    /// against `current_project_name()` from `crate::project`.
    pub project: Option<String>,

    /// LLM-free short summary (first line + top 3 project nouns).
    /// Optional at write-time — capture adapters may supply a
    /// cheap digest, or leave `None` for the warm loop to fill.
    /// Used by slow_loop's similarity-merge as a deterministic
    /// dedup key (D ultrareview round 2).
    pub digest: Option<String>,
}

/// A row as stored. `id` + kernel-derived fields included.
///
/// **D160 close — read-side mirror of [`Episode`]**. Every payload
/// column round-trips byte-for-byte from `append_episode_with_vector`
/// → `recent_episodes` / `get_live_episode`. The three kernel-
/// derived columns (`id`, `memory_tier`, `salience`) are SQLite
/// triggers / runtime computations that the write-side `Episode`
/// can't supply; reading them back here closes the contract.
#[derive(Debug, Clone)]
pub struct StoredEpisode {
    /// Auto-increment primary key from `episodes.id`.
    pub id: EpisodeId,

    /// Wall-clock at turn start (ns since UNIX epoch).
    pub ts_start_ns: i64,
    /// Wall-clock at turn end (ns since UNIX epoch).
    pub ts_end_ns: i64,
    /// Pre-computed `(ts_end_ns - ts_start_ns) / 1_000_000`, clamped
    /// to non-negative.
    pub duration_ms: i64,

    /// Where this episode came from. The discriminator picks which
    /// of `prompt_text` / `response_text` / `command` / `stdout`
    /// will be `Some` (AI sources populate prompt+response;
    /// terminal sources populate command+stdout+exit_code).
    pub source: EpisodeSource,
    /// Coarse-grained grouping key — Claude Code / Codex CLI session uuid or
    /// SOMA-managed id for `claude-code` / `codex-cli` / `codex-app` / `cursor`
    /// / `continue`,
    /// ad-hoc for terminal,
    /// and historical `chat-<ns>` rows from the removed local REPL.
    pub session_id: Option<String>,

    /// AI sources only — the user prompt that triggered this turn.
    pub prompt_text: Option<String>,
    /// AI sources only — the assistant's response text.
    pub response_text: Option<String>,
    /// Terminal sources only — the command line as executed.
    pub command: Option<String>,
    /// Terminal sources only — captured stdout (bounded by D112
    /// 16 MiB cap).
    pub stdout: Option<Vec<u8>>,
    /// Terminal sources only — process exit code (`0` success).
    pub exit_code: Option<i32>,

    /// Working directory at capture time.
    pub cwd: Option<String>,
    /// Result of `git branch --show-current` at capture time.
    pub git_branch: Option<String>,
    /// `basename(PWD)` at capture time. Used by project-scoped
    /// recall and ContextEnvelope compilation.
    pub project: Option<String>,

    /// Kernel-derived. `'short'` on fresh insert; warm loop may
    /// promote to `'mid'` / `'long'` based on salience + age.
    pub memory_tier: String,
    /// Kernel-derived. `None` until the warm loop scores the episode.
    pub salience: Option<f32>,
    pub digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D119 — kebab-case `Display` and `FromStr` are inverses for
    /// every canonical variant. The wire schema (`episodes.source`
    /// SQLite TEXT column, MCP JSON output) stays kebab-case
    /// strings, so this round-trip is the type-level invariant
    /// that protects the wire shape.
    #[test]
    fn episode_source_kebab_case_roundtrip() {
        let cases = [
            (EpisodeSource::Terminal, "terminal"),
            (EpisodeSource::ClaudeCode, "claude-code"),
            (EpisodeSource::CodexCli, "codex-cli"),
            (EpisodeSource::CodexApp, "codex-app"),
            (EpisodeSource::Cursor, "cursor"),
            (EpisodeSource::Continue, "continue"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.to_string(), expected, "Display variant={variant:?}");
            let parsed = EpisodeSource::from_str(expected).unwrap();
            assert_eq!(parsed, variant, "FromStr variant={variant:?}");
        }
    }

    /// D119 — a kebab-case string that doesn't match any canonical
    /// variant lands in `Other` (forward-compat for ad-hoc tools).
    /// `to_string()` must round-trip the original input verbatim.
    #[test]
    fn episode_source_unknown_kebab_becomes_other() {
        let parsed = EpisodeSource::from_str("my-tool").expect("kebab-case parses");
        assert_eq!(parsed, EpisodeSource::Other("my-tool".to_string()));
        assert_eq!(parsed.to_string(), "my-tool");
    }

    /// D119 — uppercase characters fail the regex even if otherwise
    /// well-formed. The R5 guard previously lived in
    /// `validate_source_payload` as a runtime check; D119 lifts it
    /// into `FromStr` so the type system rejects the input before
    /// it reaches the storage layer.
    #[test]
    fn episode_source_rejects_uppercase() {
        let err = EpisodeSource::from_str("Claude").expect_err("uppercase must fail");
        assert!(matches!(err, EpisodeSourceError::Invalid(ref s) if s == "Claude"));
    }

    /// D119 — empty input fails. Pre-D119 `build_episode` checked
    /// `source.is_empty()` separately; now it falls out of the same
    /// `FromStr` error leg so the validation surface is one place.
    #[test]
    fn episode_source_rejects_empty() {
        let err = EpisodeSource::from_str("").expect_err("empty must fail");
        assert!(matches!(err, EpisodeSourceError::Invalid(ref s) if s.is_empty()));
    }

    /// D119 — `PartialEq<&str>` lets call sites (test assertions,
    /// extractor `if ep.source != "terminal"` filters) compare
    /// against the wire string without `.to_string()` allocations.
    /// Pinned because the invariant is load-bearing for the
    /// extractor filter shape.
    #[test]
    fn episode_source_partial_eq_str_canonical_and_other() {
        assert_eq!(EpisodeSource::Terminal, "terminal");
        assert_eq!(EpisodeSource::ClaudeCode, "claude-code");
        assert!(EpisodeSource::Terminal != "claude-code");
        assert_eq!(EpisodeSource::Other("my-tool".to_string()), "my-tool");
        assert!(EpisodeSource::Other("my-tool".to_string()) != "terminal");
    }

    /// D119 — `Serialize` produces the kebab-case wire string; this
    /// is what the MCP `resources/read` JSON output and the recall
    /// `--format=json` consumer see. Without this, JSON output
    /// would emit `{"ClaudeCode"}` (default serde enum tag) and
    /// break every downstream consumer of the wire schema.
    #[test]
    fn episode_source_serialize_kebab_case() {
        let v = serde_json::to_value(EpisodeSource::ClaudeCode).unwrap();
        assert_eq!(v, serde_json::Value::String("claude-code".to_string()));
        let v = serde_json::to_value(EpisodeSource::Other("my-tool".to_string())).unwrap();
        assert_eq!(v, serde_json::Value::String("my-tool".to_string()));
    }
}
