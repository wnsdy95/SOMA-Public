//! Dashboard runtime config — populated from `cli::serve::ServeArgs`.
//!
//! Defaults match ADR 0012 §A5:
//! * `port` 8765 — random user-friendly choice; +1 fallback on bind
//!   conflict left to the caller (operator picks a different
//!   `--port` rather than the server silently shifting).
//! * `bind` `127.0.0.1` — localhost only. External exposure is an
//!   explicit `--bind 0.0.0.0` opt-in (auth still no-op in v1.x;
//!   D154 candidates an `--auth-token` extension).
//! * `open_browser` false — operator must pass `--open`.

use std::net::{IpAddr, Ipv4Addr};

#[derive(Debug, Clone)]
pub struct DashboardConfig {
    pub bind: IpAddr,
    pub port: u16,
    pub open_browser: bool,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self { bind: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port: 8765, open_browser: false }
    }
}
