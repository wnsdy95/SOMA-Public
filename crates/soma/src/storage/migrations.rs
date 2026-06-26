//! Migration runner — v1 simplified fork of WS 3 PR 3.1
//! (discussion 0024 §B).
//!
//! v1 reboot drops the 3-way backfill probe (F-fresh / F-v1-like /
//! F-v2-like) that WS 3 PR 3.1 carried for pre-runner legacy DBs.
//! In v1, **every** SOMA DB is created by this runner; pre-runner
//! shapes are structurally impossible. The runner:
//!
//! 1. Reads `schema_version` if it exists; current = 0 otherwise.
//! 2. Refuses with `NewerSchema` if current > registry target.
//! 3. Applies each pending migration inside its own transaction,
//!    inserting a `(version, applied_at)` row on commit.
//!
//! `MIGRATIONS` is append-only; never edit a landed file.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::error::StorageError;

/// A single numbered migration. `name` travels alongside `version`
/// so log lines carry a slug operators can grep for.
///
/// Invariants (checked at runtime by `run_migrations_with`):
///
/// * `version` is strictly monotonic across the registry.
/// * `version` begins at 1. Zero is reserved for "never applied".
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Authoritative migration registry. Append-only — never reorder,
/// never renumber. If a landed migration ships a bug, fix it
/// forward in a new numbered migration; never edit existing text.
pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "initial", sql: include_str!("migrations/0001_initial.sql") },
    Migration {
        version: 2,
        name: "runtime_jobs",
        sql: include_str!("migrations/0002_runtime_jobs.sql"),
    },
    Migration {
        version: 3,
        name: "episode_vectors",
        sql: include_str!("migrations/0003_episode_vectors.sql"),
    },
    Migration {
        version: 4,
        name: "self_state",
        sql: include_str!("migrations/0004_self_state.sql"),
    },
    Migration {
        version: 5,
        name: "user_centroid",
        sql: include_str!("migrations/0005_user_centroid.sql"),
    },
    Migration { version: 6, name: "note_pins", sql: include_str!("migrations/0006_note_pins.sql") },
    Migration {
        version: 7,
        name: "episode_edges",
        sql: include_str!("migrations/0007_episode_edges.sql"),
    },
    Migration {
        version: 8,
        name: "episode_summary",
        sql: include_str!("migrations/0008_episode_summary.sql"),
    },
    Migration {
        version: 9,
        name: "narrative_md",
        sql: include_str!("migrations/0009_narrative_md.sql"),
    },
    Migration {
        version: 10,
        name: "forgotten",
        sql: include_str!("migrations/0010_forgotten.sql"),
    },
    Migration {
        version: 11,
        name: "working_memory",
        sql: include_str!("migrations/0011_working_memory.sql"),
    },
    Migration {
        version: 12,
        name: "working_memory_weights",
        sql: include_str!("migrations/0012_working_memory_weights.sql"),
    },
    Migration {
        version: 13,
        name: "anil_head_weights",
        sql: include_str!("migrations/0013_anil_head_weights.sql"),
    },
    Migration {
        version: 14,
        name: "pc_predictor_weights",
        sql: include_str!("migrations/0014_pc_predictor_weights.sql"),
    },
    Migration {
        version: 15,
        name: "hopfield_weights",
        sql: include_str!("migrations/0015_hopfield_weights.sql"),
    },
    Migration {
        version: 16,
        name: "belief_candidates",
        sql: include_str!("migrations/0016_belief_candidates.sql"),
    },
    Migration {
        version: 17,
        name: "runtime_jobs_unique",
        sql: include_str!("migrations/0017_runtime_jobs_unique.sql"),
    },
    Migration {
        version: 18,
        name: "chat_recall_trace",
        sql: include_str!("migrations/0018_chat_recall_trace.sql"),
    },
    Migration {
        version: 19,
        name: "cleanup_capture_noise",
        sql: include_str!("migrations/0019_cleanup_capture_noise.sql"),
    },
    Migration {
        version: 20,
        name: "belief_candidate_resolution",
        sql: include_str!("migrations/0020_belief_candidate_resolution.sql"),
    },
    Migration {
        version: 21,
        name: "context_anomalies",
        sql: include_str!("migrations/0021_context_anomalies.sql"),
    },
    Migration {
        version: 22,
        name: "memory_lifecycle_proxies",
        sql: include_str!("migrations/0022_memory_lifecycle_proxies.sql"),
    },
    Migration {
        version: 23,
        name: "task_frames",
        sql: include_str!("migrations/0023_task_frames.sql"),
    },
    Migration {
        version: 24,
        name: "claim_verification",
        sql: include_str!("migrations/0024_claim_verification.sql"),
    },
    Migration {
        version: 25,
        name: "learning_critic_proposals",
        sql: include_str!("migrations/0025_learning_critic_proposals.sql"),
    },
    Migration {
        version: 26,
        name: "l2_candidate_projection_contract",
        sql: include_str!("migrations/0026_l2_candidate_projection_contract.sql"),
    },
    Migration {
        version: 27,
        name: "task_frame_projection_policy",
        sql: include_str!("migrations/0027_task_frame_projection_policy.sql"),
    },
    Migration {
        version: 28,
        name: "review_digest_notifications",
        sql: include_str!("migrations/0028_review_digest_notifications.sql"),
    },
    Migration {
        version: 29,
        name: "client_binding_proofs",
        sql: include_str!("migrations/0029_client_binding_proofs.sql"),
    },
    Migration {
        version: 30,
        name: "client_binding_installed_config",
        sql: include_str!("migrations/0030_client_binding_installed_config.sql"),
    },
    Migration {
        version: 31,
        name: "client_binding_render_evidence",
        sql: include_str!("migrations/0031_client_binding_render_evidence.sql"),
    },
    Migration {
        version: 32,
        name: "thread_identities",
        sql: include_str!("migrations/0032_thread_identities.sql"),
    },
    Migration {
        version: 33,
        name: "l3_proxy_access_decay",
        sql: include_str!("migrations/0033_l3_proxy_access_decay.sql"),
    },
    Migration {
        version: 34,
        name: "task_frame_outcomes",
        sql: include_str!("migrations/0034_task_frame_outcomes.sql"),
    },
    Migration {
        version: 35,
        name: "client_binding_review_action",
        sql: include_str!("migrations/0035_client_binding_review_action.sql"),
    },
];

/// Production entry — applies the full registry.
pub fn run_migrations(conn: &mut Connection) -> Result<(), StorageError> {
    run_migrations_with(conn, MIGRATIONS)
}

/// Testable entry — applies an arbitrary slice. Test T.7 injects a
/// synthetic bad migration through this door to prove per-migration
/// atomicity without touching the production registry.
///
/// Validates `migrations` monotonicity + non-empty up front so
/// misuse surfaces before any I/O.
pub fn run_migrations_with(
    conn: &mut Connection,
    migrations: &[Migration],
) -> Result<(), StorageError> {
    assert_monotonic(migrations);

    // Pre-runner legacy DB guard (codex F4). v1 reboot's invariant
    // is that every SOMA DB is created by this runner, so
    // schema_version absence plus pre-existing SOMA tables is
    // structurally impossible. Refuse rather than silently record
    // version 1 over unknown-shape data.
    detect_pre_runner_legacy(conn)?;

    // Runner owns `schema_version` creation (discussion 0024 §B /
    // WS 3 PR 3.1 §C2). Migration SQL never creates it, and never
    // inserts into it. This CREATE is idempotent so re-opens are
    // safe.
    bootstrap_schema_version(conn)?;

    let current = probe_current_version(conn)?;
    let target = migrations.last().map(|m| m.version).unwrap_or(0);

    if current > target {
        return Err(StorageError::NewerSchema {
            db_version: current,
            binary_version: target,
            hint: format!(
                "binary supports up to v{target}; DB was written by a newer SOMA release. \
                 Upgrade the binary (recommended) or restore a v{target} backup."
            ),
        });
    }

    // D1 §D — only verify ledger contiguity after we've ruled out
    // a newer-schema DB. A future SOMA may use migration ids we
    // don't recognize, but that case is owned by NewerSchema; for
    // ledgers we *do* claim, the 1..=current contiguity invariant
    // protects against silent skips (e.g., {1, 3} treated as v3).
    verify_ledger_contiguous(conn, current)?;

    for m in migrations.iter().filter(|m| m.version > current) {
        apply_migration(conn, m)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// If a SOMA-owned table exists but the runner's `schema_version`
/// ledger doesn't, refuse the open — this is a pre-runner legacy
/// DB (or a corrupted one) that v1 never creates. Silently
/// recording a version-1 ledger over unknown-shape data would hide
/// the defect and risk later migrations running against wrong
/// assumptions. (codex F4.)
fn detect_pre_runner_legacy(conn: &Connection) -> Result<(), StorageError> {
    use rusqlite::OptionalExtension;
    let sv_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if sv_exists {
        return Ok(());
    }
    // Any of these names indicates a SOMA-shape DB. The runner's
    // own registry creates them, so their presence without
    // `schema_version` means somebody else did.
    for owned in ["episodes", "runtime_jobs", "episode_vectors", "self_state"] {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![owned],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "found SOMA-owned table `{owned}` without a `schema_version` ledger. \
                     This DB was not created by the v1 migration runner; refuse to proceed. \
                     Back it up, then remove it so SOMA can create a fresh store."
                ),
            });
        }
    }
    Ok(())
}

fn bootstrap_schema_version(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        );",
    )?;
    Ok(())
}

fn probe_current_version(conn: &mut Connection) -> Result<u32, StorageError> {
    // `schema_version` is guaranteed to exist by `bootstrap_schema_
    // version` (called once at the top of `run_migrations_with`).
    // An empty ledger means current = 0 (fresh DB, apply loop fills
    // it row-by-row). Contiguity is verified separately by
    // `verify_ledger_contiguous` after the NewerSchema check, so
    // a future-binary DB whose ledger we don't recognize is
    // refused with NewerSchema rather than misclassified as
    // Corrupt.
    let max: Option<u32> =
        conn.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))?;
    Ok(max.unwrap_or(0))
}

/// D1 §D — make sure the applied ledger is exactly `1..=current`
/// (no holes). A ledger like `{1, 3}` would have `current = 3`
/// from MAX() but migration 2 was never applied — running
/// migration 4 against that DB would silently produce a wrong
/// schema. Refuse with `Corrupt` rather than continue.
fn verify_ledger_contiguous(conn: &Connection, current: u32) -> Result<(), StorageError> {
    if current == 0 {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT version FROM schema_version ORDER BY version ASC")?;
    let rows = stmt.query_map([], |r| r.get::<_, u32>(0))?;
    let mut applied: Vec<u32> = Vec::new();
    for r in rows {
        applied.push(r?);
    }
    for (i, v) in applied.iter().enumerate() {
        let expected = (i as u32) + 1;
        if *v != expected {
            return Err(StorageError::Corrupt {
                detail: format!(
                    "schema_version ledger non-contiguous: expected {expected} at index {i}, \
                     got {v} (full sequence: {applied:?}). Refusing to proceed."
                ),
            });
        }
    }
    Ok(())
}

fn apply_migration(conn: &mut Connection, m: &Migration) -> Result<(), StorageError> {
    // rusqlite's `Transaction` rolls back on drop unless `commit()`
    // is called. That's the auto-rollback leg of §B / WS 3 PR 3.1
    // §D.3.A (per-migration transaction).
    let tx = conn.transaction()?;

    if let Err(e) = tx.execute_batch(m.sql) {
        drop(tx); // explicit rollback before typed error surface
        return Err(StorageError::MigrationFailed { version: m.version, detail: format!("{e}") });
    }

    let now_secs =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);

    if let Err(e) = tx.execute(
        "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
        rusqlite::params![m.version, now_secs],
    ) {
        drop(tx);
        return Err(StorageError::MigrationFailed {
            version: m.version,
            detail: format!("schema_version INSERT failed: {e}"),
        });
    }

    tx.commit()?;

    tracing::info!(
        version = m.version,
        name = m.name,
        applied_at = now_secs,
        "schema migration applied"
    );

    Ok(())
}

fn assert_monotonic(migrations: &[Migration]) {
    for w in migrations.windows(2) {
        assert!(
            w[0].version < w[1].version,
            "MIGRATIONS slice must be strictly monotonic in version: \
             got {} then {}",
            w[0].version,
            w[1].version,
        );
    }
}
