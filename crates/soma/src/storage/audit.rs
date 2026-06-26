//! D157.5 close — typed surface for `forgotten_reason` and
//! `note_pins.reason`. Both columns historically carried free-form
//! `prefix:payload` strings:
//!
//! * `"user-request"` — operator-issued forget.
//! * `"merged-into:<id>"` — slow_loop's similarity-merge dedup.
//! * `"forgotten:<reason>"` — note-pin audit emitted alongside
//!   forget(): pin "the older episode that consumed this one".
//! * `"salience"` — D91 high-salience auto-pin.
//! * `"cleanup:<tag>"` — onetime migration cleanups (D149 added
//!   `cleanup:capture-noise`).
//!
//! The wire format stays *byte-for-byte stable* — every existing
//! row in production / dogfooding DBs round-trips unchanged. The
//! Rust surface is a typed enum so renames / typos surface at
//! compile time instead of silently writing a new variant the
//! reader doesn't recognize. Unknown wire strings collapse into
//! `Other(String)` for forward-compat (a future variant added in
//! storage but not yet in this enum still parses + reserializes
//! losslessly).

use crate::storage::EpisodeId;

/// Cross-domain audit reason. The `forgotten_reason` and
/// `note_pins.reason` columns share one enum because slow_loop's
/// similarity merge writes `merged-into:<id>` to *both* (the
/// older episode gets a forgotten_reason; the merged episode gets
/// a note_pin with the same reason for the audit trail). A single
/// type captures the cross-column invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditReason {
    /// `"user-request"` — operator-issued forget via `soma forget`.
    UserRequest,
    /// `"merged-into:<id>"` — slow_loop's similarity-merge dedup.
    /// Payload is the *surviving* (older) episode id.
    MergedInto(EpisodeId),
    /// `"forgotten:<reason>"` — note_pins audit emitted by
    /// `Storage::forget_episode`. Payload is the user-supplied
    /// forget reason (typically `"user-request"` or a free-form
    /// string from `soma forget --reason`).
    Forgotten(String),
    /// `"salience"` — D91 high-salience auto-pin.
    Salience,
    /// `"cleanup:<tag>"` — onetime migration cleanups (D149).
    Cleanup(String),
    /// Forward-compat: unknown wire string round-trips losslessly.
    Other(String),
}

impl AuditReason {
    /// Wire format the SQL columns carry. Round-trips byte-for-byte
    /// with `from_wire`.
    pub fn to_wire(&self) -> String {
        match self {
            AuditReason::UserRequest => "user-request".to_string(),
            AuditReason::MergedInto(id) => format!("merged-into:{id}"),
            AuditReason::Forgotten(s) => format!("forgotten:{s}"),
            AuditReason::Salience => "salience".to_string(),
            AuditReason::Cleanup(s) => format!("cleanup:{s}"),
            AuditReason::Other(s) => s.clone(),
        }
    }

    /// Parse the historical wire format. Unknown values become
    /// `Other(s)` so the round-trip stays lossless.
    pub fn from_wire(s: &str) -> Self {
        if s == "user-request" {
            return AuditReason::UserRequest;
        }
        if s == "salience" {
            return AuditReason::Salience;
        }
        if let Some(payload) = s.strip_prefix("merged-into:") {
            if let Ok(id) = payload.parse::<EpisodeId>() {
                return AuditReason::MergedInto(id);
            }
            // Numeric parse failure → preserve as Other so the row
            // still round-trips (e.g. a legacy DB with a corrupt
            // payload doesn't crash the typed reader).
            return AuditReason::Other(s.to_string());
        }
        if let Some(payload) = s.strip_prefix("forgotten:") {
            return AuditReason::Forgotten(payload.to_string());
        }
        if let Some(payload) = s.strip_prefix("cleanup:") {
            return AuditReason::Cleanup(payload.to_string());
        }
        AuditReason::Other(s.to_string())
    }
}

impl std::fmt::Display for AuditReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl std::str::FromStr for AuditReason {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(AuditReason::from_wire(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_request_roundtrip() {
        assert_eq!(AuditReason::from_wire("user-request"), AuditReason::UserRequest);
        assert_eq!(AuditReason::UserRequest.to_wire(), "user-request");
    }

    #[test]
    fn salience_roundtrip() {
        assert_eq!(AuditReason::from_wire("salience"), AuditReason::Salience);
        assert_eq!(AuditReason::Salience.to_wire(), "salience");
    }

    #[test]
    fn merged_into_carries_episode_id() {
        let r = AuditReason::from_wire("merged-into:42");
        assert_eq!(r, AuditReason::MergedInto(42));
        assert_eq!(r.to_wire(), "merged-into:42");
    }

    #[test]
    fn merged_into_with_corrupt_payload_falls_to_other() {
        let r = AuditReason::from_wire("merged-into:not-a-number");
        match &r {
            AuditReason::Other(s) => assert_eq!(s, "merged-into:not-a-number"),
            _ => panic!("expected Other, got {r:?}"),
        }
        assert_eq!(r.to_wire(), "merged-into:not-a-number");
    }

    #[test]
    fn forgotten_carries_payload() {
        let r = AuditReason::from_wire("forgotten:user-request");
        assert_eq!(r, AuditReason::Forgotten("user-request".to_string()));
        assert_eq!(r.to_wire(), "forgotten:user-request");
    }

    #[test]
    fn cleanup_carries_tag() {
        let r = AuditReason::from_wire("cleanup:capture-noise");
        assert_eq!(r, AuditReason::Cleanup("capture-noise".to_string()));
        assert_eq!(r.to_wire(), "cleanup:capture-noise");
    }

    #[test]
    fn unknown_round_trips_as_other() {
        let r = AuditReason::from_wire("future-variant");
        match &r {
            AuditReason::Other(s) => assert_eq!(s, "future-variant"),
            _ => panic!("expected Other"),
        }
        assert_eq!(r.to_wire(), "future-variant");
    }
}
