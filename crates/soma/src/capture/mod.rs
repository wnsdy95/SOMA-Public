//! Capture adapters. Phase 2 ships `TerminalAdapter` (pty + OSC
//! 133 port from `legacy/soma-terminal`) and `AIAdapter`
//! (`soma ingest` CLI handler). v2 adds editor / browser / system.

pub mod ai_cli;
pub mod terminal;
