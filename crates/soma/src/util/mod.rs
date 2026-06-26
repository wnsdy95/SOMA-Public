//! Cross-module utilities. Currently:
//!
//! * `mutex` — D155 close. Unified mutex-poison recovery helper
//!   (`lock_or_recover`) replacing the 50+ sites of
//!   `m.lock().expect("storage mutex")`.

pub mod mutex;
