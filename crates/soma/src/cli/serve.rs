//! `soma serve --gui` — dashboard verb (D152 chunk 1.1).
//!
//! Translates the CLI flags into a `DashboardConfig` and hands off to
//! `runtime::dashboard::serve`. The verb itself is feature-gated by
//! `dashboard`; the CLI mod opts the variant in via `#[cfg]` so a
//! default-feature build doesn't even surface the verb in `--help`.

use std::io;

use crate::cli::ServeArgs;
use crate::runtime::dashboard::{serve, DashboardConfig};

#[derive(Debug)]
pub enum ServeError {
    NotEnabled,
    Runtime(io::Error),
    Server { source: io::Error, bind: std::net::IpAddr, port: u16 },
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServeError::NotEnabled => {
                write!(f, "soma serve currently requires --gui (no other mode wired)")
            }
            ServeError::Runtime(e) => write!(f, "dashboard runtime init failed: {e}"),
            ServeError::Server { source, bind, port } => {
                write!(
                    f,
                    "dashboard server failed for {bind}:{port}: {source}. {}",
                    dashboard_io_hint(source)
                )
            }
        }
    }
}

impl std::error::Error for ServeError {}

/// Returns the standard exit code for the given error variant.
/// Wired alongside the other `cli/*` verbs so `main.rs` can
/// translate uniformly.
pub fn exit_code_for(err: &ServeError) -> i32 {
    match err {
        ServeError::NotEnabled => 2,
        ServeError::Runtime(_) | ServeError::Server { .. } => 5,
    }
}

/// Synchronous entry — spins up a current-thread tokio runtime so
/// the verb is callable from the existing blocking `main.rs` match
/// arms without infecting the rest of the CLI with async.
pub fn run_blocking(args: ServeArgs) -> Result<(), ServeError> {
    if !args.gui {
        return Err(ServeError::NotEnabled);
    }
    let cfg = DashboardConfig { bind: args.bind, port: args.port, open_browser: args.open };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ServeError::Runtime)?;
    let bind = cfg.bind;
    let port = cfg.port;
    rt.block_on(serve(cfg)).map_err(|source| ServeError::Server { source, bind, port })
}

fn dashboard_io_hint(err: &io::Error) -> &'static str {
    match err.kind() {
        io::ErrorKind::PermissionDenied => {
            "If this is a sandboxed client, approve localhost binding and SOMA state access before retrying. Read-only dashboard fallbacks: `soma clients --brief`; `soma projects --brief`; `soma learning --brief`."
        }
        io::ErrorKind::AddrInUse => {
            "Another process already owns this port; retry with `soma serve --gui --port <free-port>`, or inspect the read-only status with `soma clients --brief`."
        }
        io::ErrorKind::AddrNotAvailable => {
            "The bind address is not available on this machine; retry with `--bind 127.0.0.1`, or inspect the read-only status with `soma clients --brief`."
        }
        _ => {
            "Read-only dashboard fallbacks: `soma clients --brief`; `soma projects --brief`; `soma learning --brief`."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn permission_denied_error_mentions_sandbox_and_read_only_fallbacks() {
        let err = ServeError::Server {
            source: io::Error::from(io::ErrorKind::PermissionDenied),
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 8765,
        };
        let rendered = err.to_string();

        assert!(rendered.contains("dashboard server failed for 127.0.0.1:8765"), "{rendered}");
        assert!(rendered.contains("approve localhost binding"), "{rendered}");
        assert!(rendered.contains("soma clients --brief"), "{rendered}");
        assert!(rendered.contains("soma projects --brief"), "{rendered}");
        assert!(rendered.contains("soma learning --brief"), "{rendered}");
    }

    #[test]
    fn addr_in_use_error_suggests_port_override() {
        let err = ServeError::Server {
            source: io::Error::from(io::ErrorKind::AddrInUse),
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 8765,
        };

        assert!(err.to_string().contains("soma serve --gui --port <free-port>"));
    }
}
