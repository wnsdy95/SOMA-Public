//! `soma persona` — legacy context/profile helper artifacts.
//!
//! These files predate the ContextEnvelope bridge. The command stays
//! only for disabled legacy prompt-injection installs and for user
//! inspection/debugging, but canonical cloud-LLM context delivery is MCP
//! `soma://context/*` plus active tools.
//!
//! Three sub-commands:
//!
//! * `soma persona read` — print `~/.soma/self/identity.md` to
//!   stdout. Long-form migration/debug diagnostic text.
//! * `soma persona regen` — force-rebuild the legacy context/profile artifacts
//!   *now*. Slow-loop no longer emits these artifacts automatically;
//!   ContextEnvelope/MCP is the canonical cloud-LLM path.
//! * `soma persona inject` — print `~/.soma/self/persona-card.md`
//!   to stdout for disabled legacy prompt-injection hook flows.
//!
//! This verb is retained only so old installs can migrate off prompt-prefix
//! injection. Under the context-layer reset, these artifacts are descriptive
//! context helpers, not a product claim that SOMA is a first-person companion.

use std::sync::{Arc, Mutex};

use crate::storage::{Storage, StorageError};

#[derive(Debug)]
pub enum PersonaError {
    Path(String),
    Storage(StorageError),
    Io(std::io::Error),
}

impl std::fmt::Display for PersonaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersonaError::Path(m) => write!(f, "path: {m}"),
            PersonaError::Storage(e) => write!(f, "storage: {e}"),
            PersonaError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for PersonaError {}

impl From<StorageError> for PersonaError {
    fn from(e: StorageError) -> Self {
        PersonaError::Storage(e)
    }
}

impl From<std::io::Error> for PersonaError {
    fn from(e: std::io::Error) -> Self {
        PersonaError::Io(e)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// Print legacy `identity.md` context/profile diagnostic text to stdout.
    /// Regenerate first if the file doesn't exist yet (cold-start friendly).
    Read,
    /// Force-rebuild legacy context/profile artifacts for migration/debug.
    Regen,
    /// Print `persona-card.md` to stdout for disabled legacy prompt-injection hooks.
    /// Regenerate first if absent (so the first hook fire never
    /// fails for a fresh install).
    Inject,
}

pub fn run_blocking(mode: Mode) -> Result<(), PersonaError> {
    let db_path = crate::capture::ai_cli::resolve_db_path(None)
        .map_err(|e| PersonaError::Path(e.to_string()))?;
    let storage = Arc::new(Mutex::new(Storage::open(&db_path)?));

    let identity_path = crate::memory::persona::identity_path()
        .ok_or_else(|| PersonaError::Path("home directory not resolvable".into()))?;
    let global_card_path = crate::memory::persona::persona_card_path()
        .ok_or_else(|| PersonaError::Path("home directory not resolvable".into()))?;

    // D153 phase 1 — pick the project card matching the cwd if one
    // exists; fall back to the global card otherwise. Computed
    // up-front so the existence probe drives `needs_regen` for the
    // cold-start ergonomic.
    let inject_path: std::path::PathBuf = {
        let project = crate::memory::persona::current_project_name();
        let project_card =
            crate::memory::persona::persona_card_path_for_project(project.as_deref());
        match project_card {
            Some(p) if p.exists() => p,
            _ => global_card_path.clone(),
        }
    };

    let needs_regen = matches!(mode, Mode::Regen)
        || (matches!(mode, Mode::Read) && !identity_path.exists())
        || (matches!(mode, Mode::Inject) && !inject_path.exists());

    let written = if needs_regen {
        let guard = crate::util::mutex::lock_or_recover(&storage);
        Some(crate::memory::persona::write_persona_artifacts(&guard)?)
    } else {
        None
    };

    match mode {
        Mode::Read => {
            let body = std::fs::read_to_string(&identity_path)?;
            println!("{body}");
        }
        Mode::Regen => {
            println!("soma: legacy context profile artifacts regenerated");
            println!("  identity: {}", identity_path.display());
            println!("  global legacy card: {}", global_card_path.display());
            if let Some(w) = written {
                for (project, path) in &w.project_cards {
                    println!("  project legacy card [{project}]: {}", path.display());
                }
            }
        }
        Mode::Inject => {
            // Re-resolve after a regen — the project card may have
            // appeared in this run.
            let final_path = if needs_regen {
                let project = crate::memory::persona::current_project_name();
                let project_card =
                    crate::memory::persona::persona_card_path_for_project(project.as_deref());
                match project_card {
                    Some(p) if p.exists() => p,
                    _ => global_card_path,
                }
            } else {
                inject_path
            };
            let body = std::fs::read_to_string(&final_path)?;
            // Print without trailing newline embellishment; legacy
            // callers own any final prompt formatting.
            print!("{body}");
        }
    }
    Ok(())
}

pub fn exit_code_for(err: &PersonaError) -> i32 {
    match err {
        PersonaError::Path(_) => 3,
        PersonaError::Storage(_) => 2,
        PersonaError::Io(_) => 5,
    }
}
