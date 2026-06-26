//! OSC 133 escape-sequence parser — verbatim port from
//! `legacy/soma-terminal/src/osc133.rs` per discussion 0031 §B.
//!
//! Recognizes the shell-integration sequences emitted by bash /
//! zsh / fish / PowerShell after `soma install` injects the
//! integration script:
//!
//!   ESC ] 133 ; A ST        — prompt start
//!   ESC ] 133 ; B ST        — command start (user input begins)
//!   ESC ] 133 ; C ST        — pre-exec
//!   ESC ] 133 ; D [; N] ST  — post-exec with optional exit code
//!
//! ST is either BEL (0x07) or ESC \\ (0x1b 0x5c). Sequences may be
//! split across read() boundaries, so the parser is a byte-fed
//! state machine.
//!
//! v1 ships only the parser. The pty driver + episode builder
//! (legacy `src/pty/{mod,ready,session,shell}.rs`, ~1632 lines)
//! are deferred to v1.1 (D85-cand) — they require `portable-pty`
//! integration + shell RC injection that the v1 install path
//! does not yet wire.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133Event {
    PromptStart,
    CommandStart,
    PreExec,
    PostExec { exit_code: Option<i32> },
}

/// Per-byte classification used by the future EpisodeBuilder: each
/// input byte is either part of the passthrough stream (user-
/// visible output) or an OSC 133 event boundary. Yielding bytes
/// inline with events lets the builder route them into the correct
/// buffer without re-scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedItem {
    Byte(u8),
    Event(Osc133Event),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    SawEsc,
    InOsc,
    /// Inside OSC body, just consumed an ESC byte. Awaiting the
    /// next byte to disambiguate `ESC \` (terminator) vs anything
    /// else (cancel OSC). Held across `feed()` boundaries so a
    /// terminator split between two reads doesn't drop the payload.
    OscSawEsc,
}

pub struct Osc133Parser {
    state: State,
    payload: Vec<u8>,
}

impl Osc133Parser {
    pub fn new() -> Self {
        Self { state: State::Normal, payload: Vec::with_capacity(64) }
    }

    /// Variant of `feed()` that classifies every input byte as
    /// either passthrough (user content) or an OSC 133 event.
    /// Bytes belonging to OSC 133 escape sequences are consumed
    /// and do not appear in the output. Non-OSC-133 escape
    /// sequences (CSI colors, etc.) are emitted back as
    /// passthrough bytes so downstream consumers see intact SGR
    /// codes.
    pub fn feed_classified(&mut self, bytes: &[u8]) -> Vec<FeedItem> {
        let mut out: Vec<FeedItem> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.state {
                State::Normal => {
                    if b == 0x1b {
                        self.state = State::SawEsc;
                    } else {
                        out.push(FeedItem::Byte(b));
                    }
                }
                State::SawEsc => {
                    if b == b']' {
                        self.state = State::InOsc;
                        self.payload.clear();
                    } else if b == 0x1b {
                        // Not an OSC; the prior ESC belonged to some other
                        // sequence. Emit it as passthrough and restart.
                        out.push(FeedItem::Byte(0x1b));
                    } else {
                        // Non-OSC escape (e.g. CSI). Emit ESC + this byte.
                        out.push(FeedItem::Byte(0x1b));
                        out.push(FeedItem::Byte(b));
                        self.state = State::Normal;
                    }
                }
                State::InOsc => {
                    if b == 0x07 {
                        if let Some(ev) = parse_payload(&self.payload) {
                            out.push(FeedItem::Event(ev));
                        }
                        self.payload.clear();
                        self.state = State::Normal;
                    } else if b == 0x1b {
                        // Defer the lookahead across feed boundaries
                        // — a terminator split between two reads
                        // dropped the payload before the OscSawEsc
                        // state was added.
                        self.state = State::OscSawEsc;
                    } else if self.payload.len() < 256 {
                        self.payload.push(b);
                    } else {
                        self.payload.clear();
                        self.state = State::Normal;
                    }
                }
                State::OscSawEsc => {
                    if b == b'\\' {
                        if let Some(ev) = parse_payload(&self.payload) {
                            out.push(FeedItem::Event(ev));
                        }
                        self.payload.clear();
                        self.state = State::Normal;
                    } else if b == 0x1b {
                        // Bare ESC inside OSC body is malformed; drop
                        // accumulated payload but stay in OscSawEsc to
                        // give the new ESC a chance to terminate.
                        self.payload.clear();
                    } else {
                        self.payload.clear();
                        self.state = State::Normal;
                    }
                }
            }
            i += 1;
        }
        out
    }

    #[allow(dead_code)] // public raw-event API; used by tests
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Osc133Event> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.state {
                State::Normal => {
                    if b == 0x1b {
                        self.state = State::SawEsc;
                    }
                }
                State::SawEsc => {
                    if b == b']' {
                        self.state = State::InOsc;
                        self.payload.clear();
                    } else if b == 0x1b {
                        // stay in SawEsc
                    } else {
                        self.state = State::Normal;
                    }
                }
                State::InOsc => {
                    // Terminator: BEL (0x07) or ESC \
                    if b == 0x07 {
                        if let Some(ev) = parse_payload(&self.payload) {
                            out.push(ev);
                        }
                        self.payload.clear();
                        self.state = State::Normal;
                    } else if b == 0x1b {
                        // Defer lookahead across feed boundaries via
                        // OscSawEsc — see `feed_classified`.
                        self.state = State::OscSawEsc;
                    } else if self.payload.len() < 256 {
                        self.payload.push(b);
                    } else {
                        // runaway OSC — drop
                        self.payload.clear();
                        self.state = State::Normal;
                    }
                }
                State::OscSawEsc => {
                    if b == b'\\' {
                        if let Some(ev) = parse_payload(&self.payload) {
                            out.push(ev);
                        }
                        self.payload.clear();
                        self.state = State::Normal;
                    } else if b == 0x1b {
                        self.payload.clear();
                    } else {
                        self.payload.clear();
                        self.state = State::Normal;
                    }
                }
            }
            i += 1;
        }
        out
    }
}

impl Default for Osc133Parser {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_payload(payload: &[u8]) -> Option<Osc133Event> {
    if payload.len() < 5 || &payload[..4] != b"133;" {
        return None;
    }
    let rest = &payload[4..];
    match rest[0] {
        b'A' => Some(Osc133Event::PromptStart),
        b'B' => Some(Osc133Event::CommandStart),
        b'C' => Some(Osc133Event::PreExec),
        b'D' => {
            let exit = if rest.len() > 2 && rest[1] == b';' {
                std::str::from_utf8(&rest[2..]).ok().and_then(|s| s.trim().parse::<i32>().ok())
            } else {
                None
            };
            Some(Osc133Event::PostExec { exit_code: exit })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(bytes: &[u8]) -> Vec<Osc133Event> {
        let mut p = Osc133Parser::new();
        p.feed(bytes)
    }

    #[test]
    fn parses_bel_terminated() {
        let bytes = b"hello\x1b]133;A\x07world\x1b]133;B\x07";
        assert_eq!(feed_all(bytes), vec![Osc133Event::PromptStart, Osc133Event::CommandStart]);
    }

    #[test]
    fn parses_st_terminated() {
        let bytes = b"\x1b]133;C\x1b\\done";
        assert_eq!(feed_all(bytes), vec![Osc133Event::PreExec]);
    }

    #[test]
    fn parses_d_with_exit_code() {
        let bytes = b"\x1b]133;D;42\x07";
        assert_eq!(feed_all(bytes), vec![Osc133Event::PostExec { exit_code: Some(42) }]);
    }

    #[test]
    fn parses_d_without_exit_code() {
        let bytes = b"\x1b]133;D\x07";
        assert_eq!(feed_all(bytes), vec![Osc133Event::PostExec { exit_code: None }]);
    }

    #[test]
    fn handles_split_reads() {
        let mut p = Osc133Parser::new();
        assert_eq!(p.feed(b"\x1b]13"), vec![]);
        assert_eq!(p.feed(b"3;A\x07"), vec![Osc133Event::PromptStart]);
    }

    #[test]
    fn ignores_unrelated_osc() {
        let bytes = b"\x1b]0;my title\x07\x1b]133;A\x07";
        assert_eq!(feed_all(bytes), vec![Osc133Event::PromptStart]);
    }

    #[test]
    fn feed_classified_emits_events_and_passthrough() {
        let mut p = Osc133Parser::new();
        let out = p.feed_classified(b"hi\x1b]133;A\x07!");
        // 'h', 'i', PromptStart, '!'
        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], FeedItem::Byte(b'h')));
        assert!(matches!(out[1], FeedItem::Byte(b'i')));
        assert!(matches!(out[2], FeedItem::Event(Osc133Event::PromptStart)));
        assert!(matches!(out[3], FeedItem::Byte(b'!')));
    }

    /// D0 §C — buffer-boundary ESC \ split must terminate the OSC.
    /// Pre-fix this dropped the payload because the lookahead
    /// `i + 1 < bytes.len()` failed at the buffer end.
    #[test]
    fn handles_st_split_across_feed_boundary() {
        let mut p = Osc133Parser::new();
        // First feed ends with the ESC byte; second feed starts
        // with the backslash. Real-world equivalent: a pty read()
        // returns 5 bytes ending at the ESC.
        assert_eq!(p.feed(b"\x1b]133;D;0\x1b"), vec![]);
        assert_eq!(p.feed(b"\\next-prompt"), vec![Osc133Event::PostExec { exit_code: Some(0) }]);
    }

    /// D0 §C — same property for `feed_classified`. The split byte
    /// was not surfaced as passthrough either; the ESC + `\\` are
    /// purely OSC infrastructure.
    #[test]
    fn feed_classified_handles_st_split_across_boundary() {
        let mut p = Osc133Parser::new();
        let first = p.feed_classified(b"abc\x1b]133;A\x1b");
        // 'a', 'b', 'c' — the OSC is still pending.
        assert_eq!(first.len(), 3);
        assert!(matches!(first[0], FeedItem::Byte(b'a')));
        assert!(matches!(first[1], FeedItem::Byte(b'b')));
        assert!(matches!(first[2], FeedItem::Byte(b'c')));

        let second = p.feed_classified(b"\\xyz");
        // PromptStart, then 'x', 'y', 'z'.
        assert_eq!(second.len(), 4);
        assert!(matches!(second[0], FeedItem::Event(Osc133Event::PromptStart)));
        assert!(matches!(second[1], FeedItem::Byte(b'x')));
        assert!(matches!(second[2], FeedItem::Byte(b'y')));
        assert!(matches!(second[3], FeedItem::Byte(b'z')));
    }

    /// D0 §C — ESC inside OSC followed by a non-`\` byte cancels
    /// the OSC (matches pre-fix semantics; the new state machine
    /// still drops the malformed payload).
    #[test]
    fn malformed_esc_inside_osc_drops_payload() {
        let mut p = Osc133Parser::new();
        // ESC X (X != backslash) inside OSC body — payload is
        // dropped, parser returns to Normal.
        assert_eq!(p.feed(b"\x1b]133;A\x1bX\x1b]133;B\x07"), vec![Osc133Event::CommandStart]);
    }
}
