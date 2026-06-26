//! Rule-based self-model — discussion 0030.
//!
//! Three extractors (in `extractors::`) aggregate episodes into
//! structured `self_state` rows. LLM-free; deterministic; evidence-
//! backed. Every row carries an `evidence_ids` JSON array pointing
//! back at the episodes that produced the fact.
//!
//! Public surface:
//!
//! * [`SelfFact`] — one (key, value, evidence) triple an extractor
//!   emits.
//! * [`SelfExtractor`] — trait every extractor implements.
//! * [`run_all`] — production entry. Walks the built-in registry
//!   (tool_use / exit_success / project_norms) and writes the
//!   resulting facts into `self_state` via `Storage::upsert_self_fact`.
//! * [`SelfSnapshot`] — parsed read-side view of `self_state`.

pub mod extractors;

use serde::Serialize;

use crate::storage::{EpisodeId, SelfStateRow, Storage, StorageError};

/// One fact an extractor produces. The extractor picks the `value`
/// shape; the runner serializes it to JSON for storage.
#[derive(Debug, Clone)]
pub struct SelfFact {
    pub key: String,
    pub value: serde_json::Value,
    pub evidence_ids: Vec<EpisodeId>,
}

/// D157 close — typed enum for `self_state.kind`. EpisodeSource (D119)
/// pattern: wire format stays the kebab-case-stable string the SQL
/// column has carried since v0.1, but the Rust surface is a typed
/// enum so renames become compile-time errors instead of silent
/// drift. The 4 production extractors map 1:1 to the first 4
/// variants; `Profile` is the user-centroid row written by
/// `capture::ai_cli::update_centroid_after_ingest`. `Narrative`
/// is the slow_loop's narrative paragraph (`memory::narrative`).
/// Unknown wire values stay carried as `Other(String)` so a future
/// extractor (added in code but not yet known to this enum) round-
/// trips losslessly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfStateKind {
    /// `tool_use` — `extractors::tool_use::ToolUseExtractor`.
    ToolUse,
    /// `exit_success` — `extractors::exit_success::ExitSuccessExtractor`.
    ExitSuccess,
    /// `project_norms` — `extractors::project_norms::ProjectNormsExtractor`.
    ProjectNorms,
    /// `narrative` — slow_loop synthesized paragraph
    /// (`memory::narrative::synthesize_paragraph`).
    Narrative,
    /// `policy` — slow_loop extracted user/project policies
    /// (`memory::policy::Policy`).
    Policy,
    /// `profile` — user_centroid EMA (`capture::ai_cli::update_centroid_after_ingest`).
    Profile,
    /// Forward-compat: unknown wire string round-trips losslessly.
    Other(String),
}

impl SelfStateKind {
    /// Wire-format string the SQL `self_state.kind` column carries.
    /// Preserves the historical kebab-case for every variant; an
    /// `Other(s)` round-trips its raw string.
    pub fn as_str(&self) -> &str {
        match self {
            SelfStateKind::ToolUse => "tool_use",
            SelfStateKind::ExitSuccess => "exit_success",
            SelfStateKind::ProjectNorms => "project_norms",
            SelfStateKind::Narrative => "narrative",
            SelfStateKind::Policy => "policy",
            SelfStateKind::Profile => "profile",
            SelfStateKind::Other(s) => s.as_str(),
        }
    }
}

impl From<&str> for SelfStateKind {
    fn from(s: &str) -> Self {
        match s {
            "tool_use" => SelfStateKind::ToolUse,
            "exit_success" => SelfStateKind::ExitSuccess,
            "project_norms" => SelfStateKind::ProjectNorms,
            "narrative" => SelfStateKind::Narrative,
            "policy" => SelfStateKind::Policy,
            "profile" => SelfStateKind::Profile,
            other => SelfStateKind::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for SelfStateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SelfStateKind {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(SelfStateKind::from(s))
    }
}

/// Trait implemented by each extractor. `kind` is the stable tag
/// written to `self_state.kind` — renaming is a breaking migration.
pub trait SelfExtractor: Send + Sync {
    fn kind(&self) -> &'static str;
    fn extract(&self, storage: &Storage) -> Result<Vec<SelfFact>, StorageError>;
}

/// Walk the default extractor registry + write every emitted fact
/// into `self_state`. Returns the number of facts written.
pub fn run_all(storage: &mut Storage) -> Result<usize, StorageError> {
    let registry: Vec<Box<dyn SelfExtractor>> = vec![
        Box::new(extractors::tool_use::ToolUseExtractor),
        Box::new(extractors::exit_success::ExitSuccessExtractor),
        Box::new(extractors::project_norms::ProjectNormsExtractor),
    ];

    let mut count = 0;
    for ex in &registry {
        let facts = ex.extract(storage)?;
        for fact in facts {
            let value = serde_json::to_string(&fact.value).unwrap_or_else(|_| "{}".to_string());
            storage.upsert_self_fact(ex.kind(), &fact.key, &value, &fact.evidence_ids)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Read-side view of the current `self_state` table. Grouped by
/// `kind` for rendering convenience.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SelfSnapshot {
    pub tool_use: Vec<SelfSnapshotEntry>,
    pub exit_success: Vec<SelfSnapshotEntry>,
    pub project_norms: Vec<SelfSnapshotEntry>,
    pub other: Vec<SelfSnapshotEntry>,
    pub computed_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelfSnapshotEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub evidence_ids: Vec<EpisodeId>,
}

impl SelfSnapshot {
    pub fn from_rows(rows: Vec<SelfStateRow>) -> Self {
        let mut snap = SelfSnapshot::default();
        for row in rows {
            let value: serde_json::Value =
                serde_json::from_str(&row.value_json).unwrap_or(serde_json::Value::Null);
            let evidence_ids: Vec<EpisodeId> =
                serde_json::from_str(&row.evidence_ids_json).unwrap_or_default();
            snap.computed_at_ns = snap.computed_at_ns.max(row.computed_at_ns);
            let entry = SelfSnapshotEntry { key: row.key, value, evidence_ids };
            // D157 close — match arm 이 typed enum 으로. 새 wire
            // string 가 등장 하면 `SelfStateKind::Other(s)` 로
            // round-trip + `_other` bucket. compile-time exhaustive
            // 가 5 known kind 의 drift 차단.
            match SelfStateKind::from(row.kind.as_str()) {
                SelfStateKind::ToolUse => snap.tool_use.push(entry),
                SelfStateKind::ExitSuccess => snap.exit_success.push(entry),
                SelfStateKind::ProjectNorms => snap.project_norms.push(entry),
                SelfStateKind::Narrative
                | SelfStateKind::Policy
                | SelfStateKind::Profile
                | SelfStateKind::Other(_) => snap.other.push(entry),
            }
        }
        snap
    }

    pub fn is_empty(&self) -> bool {
        self.tool_use.is_empty()
            && self.exit_success.is_empty()
            && self.project_norms.is_empty()
            && self.other.is_empty()
    }
}

/// Convenience: read storage → parse rows → `SelfSnapshot`.
pub fn read_snapshot(storage: &Storage) -> Result<SelfSnapshot, StorageError> {
    let rows = storage.read_all_self_facts()?;
    Ok(SelfSnapshot::from_rows(rows))
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn roundtrip_known_variants() {
        for s in ["tool_use", "exit_success", "project_norms", "narrative", "policy", "profile"] {
            let k = SelfStateKind::from(s);
            assert_eq!(k.as_str(), s, "round-trip for {s}");
        }
    }

    #[test]
    fn unknown_variant_routes_to_other() {
        let k = SelfStateKind::from("future_extractor");
        match &k {
            SelfStateKind::Other(s) => assert_eq!(s, "future_extractor"),
            _ => panic!("expected Other"),
        }
        // Round-trips losslessly.
        assert_eq!(k.as_str(), "future_extractor");
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(SelfStateKind::ToolUse.to_string(), "tool_use");
        assert_eq!(SelfStateKind::Other("x".into()).to_string(), "x");
    }
}
