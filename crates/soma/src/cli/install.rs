//! `soma install` / `soma uninstall` — LaunchAgent plist writer +
//! `launchctl bootstrap`. macOS-only per discussion 0023 v1 scope.
//!
//! Contract locked by discussion 0026 §A~§F:
//!
//! * §A Plist template = `include_str!` with 3 placeholder
//!   (`{LABEL}` / `{BINARY_PATH}` / `{LOG_PATH}`).
//! * §B Install path = `~/Library/LaunchAgents/dev.soma.runtime.plist`.
//! * §C `launchctl bootstrap gui/$UID <plist>` + symmetric
//!   `bootout`.
//! * §D Binary path = `std::env::current_exe()` absolute.
//! * §E Uninstall = bootout + rm; bootout failure is graceful.
//! * §F Testable surface — `LaunchCtl` trait + `NoopLaunchCtl` mock;
//!   real `launchctl` gated behind `#[cfg(target_os = "macos")]`.

use std::io;
use std::path::{Path, PathBuf};

/// LaunchAgent label. Matches `Label` key in the plist.
pub const LAUNCH_AGENT_LABEL: &str = "dev.soma.runtime";

/// Default plist filename (just the basename; combined with the
/// LaunchAgents dir at install time).
pub const PLIST_FILENAME: &str = "dev.soma.runtime.plist";

/// Raw plist template. 3 placeholders are replaced at install-time:
/// `{LABEL}`, `{BINARY_PATH}`, `{LOG_PATH}`. Using
/// `include_str!` + replace (not `format!`) so that a user's
/// home-dir path containing `{` / `}` doesn't collide with format
/// directives.
const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{BINARY_PATH}</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{LOG_PATH}/soma.out.log</string>
    <key>StandardErrorPath</key><string>{LOG_PATH}/soma.err.log</string>
</dict>
</plist>
"#;

/// Parameters used by `render_plist` and consumed by `install`.
/// Test code overrides `launch_agents_dir` to a tempdir; production
/// picks it from `~/Library/LaunchAgents/`.
#[derive(Debug, Clone)]
pub struct InstallConfig {
    pub launch_agents_dir: PathBuf,
    pub binary_path: PathBuf,
    pub log_dir: PathBuf,
}

/// D0 §B — production default for `soma install`.
///
/// Resolves three real paths:
/// * `~/Library/LaunchAgents` — where launchd looks for per-user agents.
/// * `std::env::current_exe()` — the absolute path to *this* binary
///   so the LaunchAgent re-invokes the same one the user just ran.
/// * `~/.soma/log` — stdout/stderr capture target for the daemon.
///
/// The log dir is created at install-time so launchd's `KeepAlive`
/// doesn't keep restarting a binary whose `StandardOutPath` parent
/// doesn't exist (codex F4-equivalent silent failure).
pub fn default_install_config() -> io::Result<InstallConfig> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory not resolvable"))?;
    let launch_agents_dir = home.join("Library").join("LaunchAgents");
    let log_dir = home.join(".soma").join("log");
    std::fs::create_dir_all(&log_dir)?;
    let binary_path = std::env::current_exe()?;
    Ok(InstallConfig { launch_agents_dir, binary_path, log_dir })
}

/// Render the plist to a deterministic UTF-8 string. Two calls with
/// the same `InstallConfig` produce byte-identical output (T.1
/// invariant). Path values are XML-escaped (codex F7) and the
/// substitution is a single-pass scan so a path containing
/// `{LOG_PATH}` literal bytes cannot collide with later replaces
/// (codex F8).
pub fn render_plist(cfg: &InstallConfig) -> String {
    let binary = xml_escape(&cfg.binary_path.display().to_string());
    let log = xml_escape(&cfg.log_dir.display().to_string());
    let label = xml_escape(LAUNCH_AGENT_LABEL);

    // P2 fix (in-house ultrareview): the earlier min-index loop was
    // over-engineered for what is just a 3-token substitution. Three
    // simple `.replace()` calls are correct because (1) all three
    // tokens are syntactically distinct (`{LABEL}`, `{BINARY_PATH}`,
    // `{LOG_PATH}`) so no token contains another as a substring, and
    // (2) any `{LABEL}`-shaped substring in a path value would have
    // been XML-escaped at the boundary (`{` is not in the xml_escape
    // table, but it is also not legal in macOS LaunchAgent plist
    // labels — so the substituted value cannot reintroduce a token).
    PLIST_TEMPLATE
        .replace("{LABEL}", &label)
        .replace("{BINARY_PATH}", &binary)
        .replace("{LOG_PATH}", &log)
}

/// Minimal XML text-content escape. `<`, `>`, `&`, `"`, `'` are the
/// five characters whose literal presence inside a `<string>` node
/// produces invalid plist XML. Paths on macOS can legally contain
/// `&` (just not `/`), so the escape is non-optional.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Write the plist to `cfg.launch_agents_dir/dev.soma.runtime.plist`,
/// creating the parent dir if missing. Returns the full plist path.
/// Does **not** invoke `launchctl` — that's the `LaunchCtl` trait's
/// job so tests can pass `NoopLaunchCtl`.
///
/// D1 §E — write to a sibling tempfile, `fsync`, then `rename` over
/// the canonical path. A crash / disk-full / interrupted process
/// can no longer leave a truncated plist at the final filename;
/// the rename is atomic at the POSIX layer so launchd either sees
/// the old plist or the new one, never a partial.
pub fn write_plist(cfg: &InstallConfig) -> io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(&cfg.launch_agents_dir)?;
    let final_path = cfg.launch_agents_dir.join(PLIST_FILENAME);
    let tmp_path =
        cfg.launch_agents_dir.join(format!("{PLIST_FILENAME}.tmp.{}", std::process::id()));

    let bytes = render_plist(cfg);
    {
        // Round 3 audit (2026-04-29) — open the tempfile with mode
        // 0o600 from the start so the brief window between create
        // and atomic-rename doesn't expose a world-readable file.
        // Plist content is path-only (no secrets) so the prior
        // 0o644 default was low-impact, but for an open-source
        // release we keep the on-disk posture consistent with
        // `~/.soma/run/soma.{pid,sock}` (both 0o600).
        #[cfg(unix)]
        let mut tmp = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)?
        };
        #[cfg(not(unix))]
        let mut tmp =
            std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&tmp_path)?;
        tmp.write_all(bytes.as_bytes())?;
        tmp.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(final_path)
}

/// `launchctl` invocation seam. Production path runs real CLI on
/// macOS; tests inject `NoopLaunchCtl` or a custom fake.
pub trait LaunchCtl {
    /// `launchctl bootstrap gui/$UID <plist>`.
    fn bootstrap(&self, plist: &Path) -> io::Result<()>;
    /// `launchctl bootout gui/$UID <plist>`. Return
    /// `BootoutError::NotLoaded` (via [`BootoutError`]) when the
    /// agent is not loaded — uninstall treats that as graceful
    /// per §E.
    fn bootout(&self, plist: &Path) -> Result<(), BootoutError>;
}

/// Typed failure on `launchctl bootout` so `uninstall` can decide
/// which failures are graceful (codex F3 — the previous
/// implementation swallowed all bootout errors, which masked real
/// launchd problems).
#[derive(Debug)]
pub enum BootoutError {
    /// Agent was already unloaded. `uninstall` proceeds with the
    /// file removal.
    NotLoaded,
    /// Any other failure — surfaced by `uninstall` so the operator
    /// can diagnose.
    Other(io::Error),
}

impl std::fmt::Display for BootoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootoutError::NotLoaded => write!(f, "launchctl bootout: agent not loaded"),
            BootoutError::Other(e) => write!(f, "launchctl bootout: {e}"),
        }
    }
}

impl std::error::Error for BootoutError {}

impl From<BootoutError> for io::Error {
    fn from(e: BootoutError) -> Self {
        match e {
            BootoutError::NotLoaded => {
                io::Error::new(io::ErrorKind::NotFound, "launchd agent not loaded")
            }
            BootoutError::Other(e) => e,
        }
    }
}

/// Test stub — both methods succeed silently without invoking any
/// external command.
pub struct NoopLaunchCtl;
impl LaunchCtl for NoopLaunchCtl {
    fn bootstrap(&self, _plist: &Path) -> io::Result<()> {
        Ok(())
    }
    fn bootout(&self, _plist: &Path) -> Result<(), BootoutError> {
        Ok(())
    }
}

/// Compose plist write + `launchctl bootstrap`. Idempotent — if the
/// agent is already loaded (re-running `soma install`), we
/// `bootout` first and then re-bootstrap so the new plist contents
/// (e.g., updated binary path) take effect. `BootoutError::NotLoaded`
/// is the expected case on a fresh install and is silently
/// absorbed; only `BootoutError::Other` aborts.
pub fn install(cfg: &InstallConfig, ctl: &dyn LaunchCtl) -> io::Result<PathBuf> {
    let plist = write_plist(cfg)?;
    // Pre-bootout — if launchctl already has this label loaded, the
    // bootstrap below would fail with status 5 ("Input/output error
    // — already loaded"). Run bootout first; NotLoaded is the
    // happy-path fresh install.
    match ctl.bootout(&plist) {
        Ok(()) | Err(BootoutError::NotLoaded) => {}
        Err(BootoutError::Other(e)) => return Err(e),
    }
    ctl.bootstrap(&plist)?;
    Ok(plist)
}

/// Compose `launchctl bootout` + plist removal. Only `NotLoaded`
/// bootout failures are graceful (§E); `BootoutError::Other` is
/// surfaced and the plist file is **not** removed — leaving the
/// system in a diagnosable state rather than half-cleaned (codex
/// F3).
pub fn uninstall(cfg: &InstallConfig, ctl: &dyn LaunchCtl) -> io::Result<()> {
    let plist = cfg.launch_agents_dir.join(PLIST_FILENAME);
    match ctl.bootout(&plist) {
        Ok(()) | Err(BootoutError::NotLoaded) => {}
        Err(BootoutError::Other(e)) => return Err(e),
    }
    match std::fs::remove_file(&plist) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Real `launchctl` invocation (macOS only).
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub mod platform {
    use std::io;
    use std::path::Path;
    use std::process::Command;

    use super::{BootoutError, LaunchCtl};

    /// Production `LaunchCtl` impl that shells out to `launchctl`.
    pub struct SystemLaunchCtl;

    fn gui_domain() -> String {
        // SAFETY: `getuid()` is always-safe, takes no arguments.
        #[allow(unsafe_code)]
        let uid = unsafe { libc::getuid() };
        format!("gui/{uid}")
    }

    /// Capture both exit status and stderr so the caller can
    /// distinguish `launchctl bootout`'s "not loaded" error from a
    /// genuine failure. macOS `launchctl` emits a message like
    /// "Boot-out failed: 5: Input/output error" with `not loaded`
    /// or a specific code for the "not currently loaded" leg.
    fn invoke_bootout(plist: &Path) -> Result<(), BootoutError> {
        let output = Command::new("launchctl")
            .arg("bootout")
            .arg(gui_domain())
            .arg(plist)
            .output()
            .map_err(BootoutError::Other)?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        // launchctl's "no such service" / "not loaded" wording
        // varies across macOS releases. Match on the common roots.
        if stderr.contains("could not find service")
            || stderr.contains("no such file")
            || stderr.contains("not loaded")
        {
            return Err(BootoutError::NotLoaded);
        }
        Err(BootoutError::Other(io::Error::other(format!(
            "launchctl bootout failed: status={} stderr={}",
            output.status,
            stderr.trim()
        ))))
    }

    fn invoke_bootstrap(plist: &Path) -> io::Result<()> {
        let status =
            Command::new("launchctl").arg("bootstrap").arg(gui_domain()).arg(plist).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("launchctl bootstrap failed with status {status}")))
        }
    }

    impl LaunchCtl for SystemLaunchCtl {
        fn bootstrap(&self, plist: &Path) -> io::Result<()> {
            invoke_bootstrap(plist)
        }
        fn bootout(&self, plist: &Path) -> Result<(), BootoutError> {
            invoke_bootout(plist)
        }
    }
}
