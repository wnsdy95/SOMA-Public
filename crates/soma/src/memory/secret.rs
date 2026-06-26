//! Secret management — `~/.soma/secrets.toml` reader.
//!
//! STAGE 2-C-full (D82) per ADR 0004 §F. The slow_loop's LLM
//! summary call needs an Anthropic API key; we read it from a
//! 0600-permissioned TOML file rather than prompting interactively
//! (resident is a daemon — no TTY).
//!
//! File shape:
//!
//! ```toml
//! [anthropic]
//! api_key = "sk-ant-..."
//! ```
//!
//! Permission enforcement — if the file exists with mode != 0600
//! we refuse to read it (avoids a half-locked secret leaking via a
//! cron job). v2 macOS Keychain integration is the natural follow-
//! up; v1.2 ships the file path only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug)]
pub enum SecretError {
    NotConfigured,
    PermissionsTooOpen { mode: u32, expected: u32 },
    ParseError(String),
    Io(io::Error),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::NotConfigured => {
                write!(f, "secrets file not present (~/.soma/secrets.toml)")
            }
            SecretError::PermissionsTooOpen { mode, expected } => {
                write!(f, "secrets file mode {mode:o} too open; expected {expected:o}")
            }
            SecretError::ParseError(m) => write!(f, "secrets parse: {m}"),
            SecretError::Io(e) => write!(f, "secrets io: {e}"),
        }
    }
}

impl std::error::Error for SecretError {}

impl From<io::Error> for SecretError {
    fn from(e: io::Error) -> Self {
        SecretError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct Secrets {
    pub anthropic_api_key: Option<String>,
}

impl Secrets {
    pub fn empty() -> Self {
        Self { anthropic_api_key: None }
    }
}

/// Default location: `~/.soma/secrets.toml`. Tests inject a tempdir
/// via `load_from`.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".soma").join("secrets.toml"))
}

/// Read the secrets file from a specific path. Tests / `inspect
/// secret` callers use this; the default `load` wraps with
/// `default_path`.
pub fn load_from(path: &Path) -> Result<Secrets, SecretError> {
    if !path.exists() {
        return Err(SecretError::NotConfigured);
    }
    enforce_permissions(path)?;
    let body = fs::read_to_string(path)?;
    parse(&body)
}

/// Read the secrets file from the default path. Returns
/// `Err(NotConfigured)` when the file is absent — callers should
/// treat that as "no LLM summary, fall back to rule-based".
pub fn load() -> Result<Secrets, SecretError> {
    let path =
        default_path().ok_or_else(|| SecretError::Io(io::Error::other("no home directory")))?;
    load_from(&path)
}

fn parse(body: &str) -> Result<Secrets, SecretError> {
    let v: toml::Value =
        toml::from_str(body).map_err(|e| SecretError::ParseError(e.to_string()))?;
    let key = v
        .get("anthropic")
        .and_then(|t| t.get("api_key"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    Ok(Secrets { anthropic_api_key: key })
}

#[cfg(unix)]
fn enforce_permissions(path: &Path) -> Result<(), SecretError> {
    let meta = fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(SecretError::PermissionsTooOpen { mode, expected: 0o600 });
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_permissions(_path: &Path) -> Result<(), SecretError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Test helper. Both callers are `#[cfg(unix)]` (the chmod path
    /// is meaningless on Windows), so the helper itself is unix-only
    /// — without the gate, Windows `-D warnings` flags the unused
    /// `mode` parameter and dead function.
    #[cfg(unix)]
    fn write_with_mode(path: &Path, body: &str, mode: u32) {
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn missing_file_is_not_configured() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.toml");
        match load_from(&p) {
            Err(SecretError::NotConfigured) => {}
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn parse_anthropic_key() {
        let s = parse("[anthropic]\napi_key = \"sk-ant-test\"\n").unwrap();
        assert_eq!(s.anthropic_api_key.as_deref(), Some("sk-ant-test"));
    }

    #[test]
    fn parse_missing_section_is_none() {
        let s = parse("# nothing here\n").unwrap();
        assert!(s.anthropic_api_key.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_rejected() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.toml");
        write_with_mode(&p, "[anthropic]\napi_key = \"k\"\n", 0o644);
        let err = load_from(&p).unwrap_err();
        assert!(matches!(err, SecretError::PermissionsTooOpen { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn strict_permissions_accepted() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.toml");
        write_with_mode(&p, "[anthropic]\napi_key = \"k1\"\n", 0o600);
        let secrets = load_from(&p).unwrap();
        assert_eq!(secrets.anthropic_api_key.as_deref(), Some("k1"));
    }
}
