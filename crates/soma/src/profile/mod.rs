//! Mini vs Studio hardware profile detection + budget.
//!
//! Phase 1 default: probe `sysctl hw.memsize` on Apple Silicon;
//! return `Mini` under 60GB, `Studio` at or above. User can
//! override via `[runtime] profile_override = "studio"` in
//! `~/.soma/config.toml` (D94-cand external-review fix).
//!
//! Resolution order (`effective` entry point):
//! 1. on-disk `profile_override` if set.
//! 2. RAM-based `detect_from_bytes`.
//!
//! [`detect`] (no-arg) returns the cached effective profile —
//! callers like `select_embedder` use this.

use crate::config::{Config, Profile, RuntimeConfig};
use std::sync::OnceLock;

/// Process-wide cache of the resolved profile. First hot-path
/// caller pays the disk read; subsequent calls reuse the cached
/// `Profile`. `select_embedder` (memory/embed/mod.rs) is the
/// hottest consumer.
static EFFECTIVE: OnceLock<Profile> = OnceLock::new();

/// Public entry point — RAM-based detection + on-disk override
/// resolution, cached for the process lifetime.
pub fn detect() -> Profile {
    *EFFECTIVE.get_or_init(compute_effective)
}

/// Pure resolver — explicit override beats RAM detection.
/// No global state, no I/O — safe to call from tests.
pub fn resolve(override_opt: Option<Profile>) -> Profile {
    override_opt.unwrap_or_else(|| detect_from_bytes(total_memory_bytes()))
}

/// D156-B close — config-aware variant. Reads
/// `[runtime] studio_threshold_gib` for the Mini/Studio boundary
/// instead of the hard-coded 60 GiB.
pub fn resolve_with_threshold(override_opt: Option<Profile>, threshold_gib: u32) -> Profile {
    override_opt.unwrap_or_else(|| {
        let threshold_bytes = u64::from(threshold_gib) * 1024 * 1024 * 1024;
        detect_from_bytes_with(total_memory_bytes(), threshold_bytes)
    })
}

/// Pure resolver from a [`RuntimeConfig`]. Sugar over [`resolve`].
pub fn resolve_from_runtime(rt: &RuntimeConfig) -> Profile {
    resolve(rt.profile_override)
}

fn compute_effective() -> Profile {
    // Try ~/.soma/config.toml first; on any failure we fall through
    // to bare RAM-detection (the v0.2.0 historical path).
    let cfg = match dirs::home_dir() {
        Some(home) => Config::load_or_default(&home.join(".soma")),
        None => Config::default_v1(),
    };
    // D156-B — boundary 가 config 에서 와야 하므로 resolve_with_
    // threshold 로 라우팅. resolve_from_runtime 은 historical
    // hard-coded 60 GiB 의 default path 유지 (테스트 + 외부
    // caller 호환).
    resolve_with_threshold(cfg.runtime.profile_override, cfg.runtime.studio_threshold_gib)
}

fn detect_from_bytes(bytes: u64) -> Profile {
    // D156-B — boundary 가 [runtime] studio_threshold_gib 에서.
    // default 60 GiB (24/36/48 GB Mini-class 와 64 GB Studio 사이
    // 의 split). detect_from_bytes 자체 는 pure (테스트 친화적)
    // 라 config 호출 안 하고 기본값 inline; runtime path 의
    // detect_from_bytes_with 가 config 거쳐 호출.
    detect_from_bytes_with(bytes, default_studio_threshold_bytes())
}

/// D156-B close — config-driven boundary variant. Production calls
/// from `compute_effective` pass the user-tunable threshold; the
/// pure `detect_from_bytes` above keeps the historical default for
/// test fixtures.
fn detect_from_bytes_with(bytes: u64, threshold_bytes: u64) -> Profile {
    if bytes >= threshold_bytes {
        Profile::Studio
    } else {
        Profile::Mini
    }
}

fn default_studio_threshold_bytes() -> u64 {
    60 * 1024 * 1024 * 1024
}

#[cfg(target_os = "macos")]
fn total_memory_bytes() -> u64 {
    // `sysctl` via libc. sysctlbyname("hw.memsize", ...).
    use std::mem;
    let mut size: u64 = 0;
    let mut len = mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    // SAFETY: `sysctlbyname` writes `size_of::<u64>()` bytes into
    // `&mut size` and updates `len`. We give it the exact buffer
    // size and a valid C-string name.
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr() as *const libc::c_char,
            &mut size as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        size
    } else {
        0
    }
}

#[cfg(not(target_os = "macos"))]
fn total_memory_bytes() -> u64 {
    // v1 is Apple-Silicon only. Non-macOS builds default to Mini.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_ram_is_mini() {
        assert_eq!(detect_from_bytes(24 * 1024 * 1024 * 1024), Profile::Mini);
        assert_eq!(detect_from_bytes(48 * 1024 * 1024 * 1024), Profile::Mini);
    }

    #[test]
    fn studio_threshold_at_64gb() {
        assert_eq!(detect_from_bytes(60 * 1024 * 1024 * 1024), Profile::Studio);
        assert_eq!(detect_from_bytes(64 * 1024 * 1024 * 1024), Profile::Studio);
        assert_eq!(detect_from_bytes(128 * 1024 * 1024 * 1024), Profile::Studio);
    }

    /// D94-cand — explicit override beats RAM detection regardless
    /// of host hardware. Critical for CI runners (small RAM but
    /// want to exercise the Studio code path) and the inverse
    /// (force Mini on a Mac Studio for testing).
    #[test]
    fn resolve_with_studio_override_returns_studio() {
        assert_eq!(resolve(Some(Profile::Studio)), Profile::Studio);
    }

    #[test]
    fn resolve_with_mini_override_returns_mini() {
        assert_eq!(resolve(Some(Profile::Mini)), Profile::Mini);
    }

    #[test]
    fn resolve_with_no_override_falls_through_to_detect() {
        // No override → RAM-based result. We can't assert the
        // host's profile (varies by runner), but the result must
        // be one of the two variants.
        let p = resolve(None);
        assert!(matches!(p, Profile::Mini | Profile::Studio));
    }

    #[test]
    fn resolve_from_runtime_honors_override_field() {
        let rt =
            RuntimeConfig { profile_override: Some(Profile::Studio), ..RuntimeConfig::default() };
        assert_eq!(resolve_from_runtime(&rt), Profile::Studio);
    }
}
