//! D155 close — unified mutex-poison recovery helper.
//!
//! Pre-fix the codebase had 50+ sites of `m.lock().expect("storage
//! mutex")`. Each `expect` panics if a sibling thread panicked
//! while holding the same mutex — a strict cascading-failure
//! semantic that turns one bug into N. SOMA's runtime is mostly
//! advisory work (slow_loop cycles, capture ingest) where
//! continuing with a possibly-inconsistent inner state is *better*
//! than killing the resident: the next cycle re-derives the
//! affected facts, and the user keeps recall + chat.
//!
//! The trade-off is explicit: silent data corruption risk vs
//! resilience under partial failure. ADR-style note in v0.x — for
//! v1 the policy is *recover with a tracing::warn*. A future
//! audit / migration may reverse this for security-critical paths
//! (e.g. credential read).
//!
//! Use `lock_or_recover(&m)` instead of `m.lock().expect(...)` at
//! every site that fits the resilience-over-strict policy. The
//! helper logs once per recovery so the operator's daily log
//! captures every actual poison event.

use std::sync::{Mutex, MutexGuard};

/// Acquire `m`'s lock. On `Ok`, return the guard directly. On
/// `Err(PoisonError)`, log a tracing::warn and recover the inner
/// guard via `into_inner()`. The recovered guard sees whatever
/// state the panicking thread left behind — caller is responsible
/// for treating the inner data as possibly inconsistent (e.g.
/// re-read from SQLite if the cached snapshot is suspect).
pub fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            // Single-line tracing::warn so the operator can grep
            // the daily rolling log for "mutex poisoned" and tally
            // recovery counts. Backtrace deliberately omitted —
            // the panic site that poisoned the mutex already
            // emitted its own stack via Rust's default panic hook.
            tracing::warn!(target: "soma::mutex", "mutex poisoned — recovering inner state");
            poisoned.into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn recovers_inner_after_poison() {
        let m = Arc::new(Mutex::new(42_i32));
        let m2 = Arc::clone(&m);
        // Spawn a thread that panics while holding the lock —
        // this poisons it.
        let _ = thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("test poison");
        })
        .join();
        // Lock is poisoned now — strict expect would panic.
        let g = lock_or_recover(&m);
        assert_eq!(*g, 42, "inner state recovered");
    }

    #[test]
    fn happy_path_returns_guard() {
        let m = Mutex::new(100_u32);
        let g = lock_or_recover(&m);
        assert_eq!(*g, 100);
    }
}
