//! Local executable identity diagnostics shared by read-only CLI reports.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinaryFileScan {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinaryIdentity {
    pub source: &'static str,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_exe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_soma: Option<String>,
    pub same_path: bool,
    pub same_fingerprint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_exe_scan: Option<BinaryFileScan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_soma_scan: Option<BinaryFileScan>,
    pub trust_boundary: &'static str,
}

impl BinaryIdentity {
    pub fn differs_from_path_soma(&self) -> bool {
        self.status == "path_soma_differs_from_current_exe"
    }

    pub fn resolved_soma_bin(&self) -> Option<&str> {
        self.differs_from_path_soma().then_some(self.current_exe.as_deref()).flatten()
    }
}

pub fn collect_binary_identity() -> (BinaryIdentity, Vec<String>) {
    let mut errors = Vec::new();
    let identity = collect_binary_identity_with_errors(&mut errors);
    (identity, errors)
}

pub fn resolved_soma_bin_for_operator_command() -> String {
    let (identity, _errors) = collect_binary_identity();
    identity.resolved_soma_bin().unwrap_or("soma").to_string()
}

pub fn command_with_current_binary_when_path_soma_differs(
    mut command: Vec<String>,
    binary_identity: &BinaryIdentity,
) -> Vec<String> {
    let Some(current_exe) = binary_identity.resolved_soma_bin() else {
        return command;
    };
    if command.first().is_some_and(|part| part == "soma") {
        command[0] = current_exe.to_string();
    }
    for idx in 0..command.len().saturating_sub(1) {
        if command[idx] == "--soma-bin" && command[idx + 1] == "soma" {
            command[idx + 1] = current_exe.to_string();
        }
    }
    command
}

pub fn collect_binary_identity_with_errors(errors: &mut Vec<String>) -> BinaryIdentity {
    let current_exe = match std::env::current_exe() {
        Ok(path) => Some(path),
        Err(err) => {
            errors.push(format!("current_exe: {err}"));
            None
        }
    };
    let path_soma = find_binary_on_path("soma");
    let current_exe_scan = current_exe.as_ref().map(|path| scan_binary_file(path, errors));
    let path_soma_scan = path_soma.as_ref().map(|path| scan_binary_file(path, errors));
    let current_exe_fingerprint =
        current_exe_scan.as_ref().and_then(|scan| scan.fingerprint.as_deref());
    let path_soma_fingerprint =
        path_soma_scan.as_ref().and_then(|scan| scan.fingerprint.as_deref());
    let same_path = match (current_exe.as_ref(), path_soma.as_ref()) {
        (Some(current), Some(path_soma)) => canonical_path_eq(current, path_soma),
        _ => false,
    };
    let same_fingerprint = match (current_exe_fingerprint, path_soma_fingerprint) {
        (Some(current), Some(path_soma)) => current == path_soma,
        _ => false,
    };
    let status = match (current_exe.as_ref(), path_soma.as_ref(), same_path, same_fingerprint) {
        (Some(_), Some(_), true, _) => "path_soma_matches_current_exe",
        (Some(_), Some(_), false, true) => "path_soma_same_bytes_different_path",
        (Some(_), Some(_), false, false) => "path_soma_differs_from_current_exe",
        (Some(_), None, _, _) => "path_soma_not_found",
        (None, Some(_), _, _) => "current_exe_unavailable",
        (None, None, _, _) => "binary_identity_unavailable",
    };

    BinaryIdentity {
        source: "soma_binary_identity.v1",
        status: status.to_string(),
        current_exe: current_exe.as_ref().map(|path| path.display().to_string()),
        path_soma: path_soma.as_ref().map(|path| path.display().to_string()),
        same_path,
        same_fingerprint,
        current_exe_scan,
        path_soma_scan,
        trust_boundary: "binary_identity_is_local_diagnostic_only: compares the running executable with the first soma on PATH by path and stable byte fingerprint; it installs no binary, starts no MCP server, records no proof row, creates no verification event, and does not prove client readiness",
    }
}

fn find_binary_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn scan_binary_file(path: &Path, errors: &mut Vec<String>) -> BinaryFileScan {
    match std::fs::read(path) {
        Ok(bytes) => BinaryFileScan {
            path: path.display().to_string(),
            byte_len: Some(bytes.len()),
            fingerprint: Some(stable_content_fingerprint(&bytes)),
            error: None,
        },
        Err(err) => {
            errors.push(format!("binary scan {}: {err}", path.display()));
            BinaryFileScan {
                path: path.display().to_string(),
                byte_len: None,
                fingerprint: None,
                error: Some(err.to_string()),
            }
        }
    }
}

fn canonical_path_eq(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("fnv1a64:{hash:016x}")
}
