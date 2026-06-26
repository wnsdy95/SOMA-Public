//! Storage-layer error taxonomy (discussion 0024 §H).
//!
//! Four legs with distinct recovery semantics:
//!
//! * `MigrationFailed` — a specific migration's SQL blew up; caller
//!   should surface to the user and stop. Not retryable.
//! * `Corrupt` — schema_version table is in an impossible state
//!   (e.g. exists but empty). Reset the DB from backup.
//! * `NewerSchema` — DB was written by a binary newer than ours;
//!   downgrade refused. User must upgrade the binary.
//! * `Sqlite` — any other rusqlite transport-level error; typically
//!   transient (SQLITE_BUSY / lock contention) and retryable.
//!
//! `From<rusqlite::Error>` converts into `Sqlite`; callers inside
//! `storage::*` that know they are at migration boundaries build
//! `MigrationFailed` / `Corrupt` / `NewerSchema` explicitly.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    #[error("migration v{version} failed: {detail}")]
    MigrationFailed { version: u32, detail: String },

    #[error("corrupt schema state: {detail}")]
    Corrupt { detail: String },

    #[error("newer schema: db_version={db_version}, binary_version={binary_version}. {hint}")]
    NewerSchema { db_version: u32, binary_version: u32, hint: String },

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
