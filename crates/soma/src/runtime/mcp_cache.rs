//! Process-life TTL cache for MemoryPack assemblies. Discussion
//! 0032 §A + §I.
//!
//! `mcp-serve` is a child process spawned per Claude session, so
//! a `Mutex<HashMap>` in process memory is the natural cache lifetime
//! — host shutdown, Claude session end, and new-session spawn each
//! produce a fresh empty cache. No persistent disk cache (rejected
//! per §D).
//!
//! D80 (warm-loop) will call `invalidate_all` over a separate
//! channel when `self_state` updates; this module exposes that hook
//! but never calls it from inside the v1.1 D74 chunk.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::context::pack::{MemoryPack, PackError};

/// Default TTL — discussion 0032 §B. Long enough to cover a single
/// Claude turn (typical 10-30 s) without bleeding stale data into
/// the user's next deliberate session.
pub const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Cache lookup key. `kind` discriminates the canonical URIs
/// (`current` vs `by-query` vs `project`) — the same query string
/// under different URIs is a distinct cache slot. `project` (D161)
/// scopes the pack to a single project; `None` is the historical
/// cross-project default.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct CacheKey {
    pub kind: &'static str,
    pub query: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub thread_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
}

#[derive(Clone)]
struct CacheEntry {
    pack: MemoryPack,
    fetched_at: Instant,
}

/// Process-life MemoryPack cache. `Send + Sync`; clone the
/// internal `Arc` to share across handler tasks.
///
/// D87 §B — `hits` + `misses` atomics surface in `soma status` for
/// observability without lock contention.
/// D88 §B — when `cache_ttl_secs == 0` the cache is fully bypassed
/// (every fetch goes straight to `builder`). Lock-held builder
/// invocation already serializes concurrent identical fetches
/// (§H first-wins-rest-wait), which is the v1 form of request
/// coalescing — no additional `tokio::sync::broadcast` plumbing
/// required for single-process MCP serve.
#[derive(Clone)]
pub struct MemoryPackCache {
    inner: Arc<Mutex<HashMap<CacheKey, CacheEntry>>>,
    ttl: Duration,
    builder_calls: Arc<AtomicUsize>,
    hits: Arc<AtomicUsize>,
    misses: Arc<AtomicUsize>,
}

impl MemoryPackCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
            builder_calls: Arc::new(AtomicUsize::new(0)),
            hits: Arc::new(AtomicUsize::new(0)),
            misses: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(DEFAULT_TTL)
    }

    /// D86 §B — construct from a `Duration` parsed from `[mcp]
    /// cache_ttl_secs`. `Duration::ZERO` produces a cache whose
    /// `get_or_build` always misses (full bypass).
    pub fn from_ttl_secs(secs: u64) -> Self {
        Self::new(Duration::from_secs(secs))
    }

    /// Look up `key`. Hit (and not expired) returns the cached
    /// `MemoryPack` clone + `CacheStatus::Hit`. Miss / expired
    /// invokes `builder` once, caches its result, and returns
    /// `CacheStatus::Miss`. The builder is **not** retried on
    /// error — a failed build leaves the cache slot empty.
    pub fn get_or_build<F>(
        &self,
        key: CacheKey,
        builder: F,
    ) -> Result<(MemoryPack, CacheStatus), PackError>
    where
        F: FnOnce() -> Result<MemoryPack, PackError>,
    {
        // D86 §B — TTL of 0 means "no cache". Skip the read +
        // skip the write so each fetch is a pure pass-through.
        if self.ttl.is_zero() {
            self.misses.fetch_add(1, Ordering::Relaxed);
            self.builder_calls.fetch_add(1, Ordering::Relaxed);
            let pack = builder()?;
            return Ok((pack, CacheStatus::Miss));
        }
        // Hold the lock across read + miss-build so concurrent
        // identical requests serialize (§H first-wins-rest-wait).
        let mut guard = self.inner.lock().expect("cache mutex");
        if let Some(entry) = guard.get(&key) {
            if entry.fetched_at.elapsed() <= self.ttl {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok((entry.pack.clone(), CacheStatus::Hit));
            }
            // Expired — fall through to rebuild.
            guard.remove(&key);
        }

        // Miss path — invoke builder while still holding the lock.
        // Lock contention is low (single-Claude single-user) and
        // the assemble cost (storage open + HNSW rebuild) is the
        // dominant factor, not the lock.
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.builder_calls.fetch_add(1, Ordering::Relaxed);
        let pack = builder()?;
        let entry = CacheEntry { pack: pack.clone(), fetched_at: Instant::now() };
        guard.insert(key, entry);
        Ok((pack, CacheStatus::Miss))
    }

    /// Drop every cached entry. Call site = D80 warm-loop. v1.1
    /// D74 chunk wires the method but never invokes it.
    pub fn invalidate_all(&self) {
        let mut guard = self.inner.lock().expect("cache mutex");
        guard.clear();
    }

    /// Test-visible counter of `builder` invocations across the
    /// cache's lifetime. The cache's `Hit` rate = 1 - calls / fetches.
    pub fn builder_calls(&self) -> usize {
        self.builder_calls.load(Ordering::Relaxed)
    }

    /// D87 §B — total successful cache hits.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }

    /// D87 §B — total cache misses (includes TTL-bypass mode).
    pub fn misses(&self) -> usize {
        self.misses.load(Ordering::Relaxed)
    }

    /// D87 §B — hit-ratio rendered for `soma status` consumers.
    /// Returns `None` until at least one fetch has been served so
    /// the ratio doesn't display `0.0%` in a brand-new resident.
    ///
    /// Round 3 audit (2026-04-29) — the snapshot is intentionally
    /// taken under the inner mutex so concurrent `get_or_build`
    /// can't increment one counter between our two reads. Pre-fix
    /// the two `fetch_add(Relaxed)` reads (still atomic, but
    /// independent) could yield a transient ratio outside `[0, 1]`
    /// when the second counter incremented between reads. Visible
    /// in `soma status` only as a sub-percentage cosmetic glitch
    /// — but for an open-source release we want the visible
    /// counters consistent with each other.
    pub fn hit_ratio(&self) -> Option<f32> {
        // Holding the inner lock for the duration of two atomic
        // loads is cheap (no DB / IO under it) and pairs the
        // counters with whatever the get_or_build caller already
        // committed. An advanced atomics-only fix would use a
        // single packed u64 (hits<<32 | misses) but the lock path
        // is already shared with `get_or_build` so this avoids a
        // second synchronization primitive.
        let _guard = self.inner.lock().expect("cache mutex");
        let h = self.hits();
        let m = self.misses();
        let total = h + m;
        if total == 0 {
            None
        } else {
            Some(h as f32 / total as f32)
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

impl Default for MemoryPackCache {
    fn default() -> Self {
        Self::with_default_ttl()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::pack::{MemoryPack, MEMORY_PACK_VERSION};

    fn empty_pack(query: Option<&str>) -> MemoryPack {
        MemoryPack {
            version: MEMORY_PACK_VERSION,
            assembled_at_ns: 0,
            query: query.map(str::to_string),
            recent: vec![],
            semantic: vec![],
            thread_state_selection: None,
            project_state: serde_json::json!({}),
            self_state: serde_json::json!({}),
        }
    }

    #[test]
    fn test_cache_hit_skips_builder() {
        let cache = MemoryPackCache::new(Duration::from_secs(60));
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };

        let (pack1, status1) = cache.get_or_build(key.clone(), || Ok(empty_pack(None))).unwrap();
        assert!(matches!(status1, CacheStatus::Miss));

        let (pack2, status2) = cache
            .get_or_build(key.clone(), || panic!("builder must not run on cache hit"))
            .unwrap();
        assert!(matches!(status2, CacheStatus::Hit));
        assert_eq!(pack1.version, pack2.version);
        assert_eq!(cache.builder_calls(), 1);
    }

    #[test]
    fn test_cache_miss_after_ttl_expiry() {
        let cache = MemoryPackCache::new(Duration::from_millis(50));
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };

        cache.get_or_build(key.clone(), || Ok(empty_pack(None))).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        let (_, status) = cache.get_or_build(key, || Ok(empty_pack(None))).unwrap();
        assert!(matches!(status, CacheStatus::Miss));
        assert_eq!(cache.builder_calls(), 2);
    }

    #[test]
    fn test_different_keys_dont_share() {
        let cache = MemoryPackCache::new(Duration::from_secs(60));
        let k_current = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };
        let k_query = CacheKey {
            kind: "by-query",
            query: Some("debug ci".into()),
            project: None,
            session_id: None,
            thread_key: None,
        };

        cache.get_or_build(k_current.clone(), || Ok(empty_pack(None))).unwrap();
        let (_, status) = cache.get_or_build(k_query, || Ok(empty_pack(Some("debug ci")))).unwrap();
        assert!(matches!(status, CacheStatus::Miss));
        assert_eq!(cache.builder_calls(), 2);

        // Re-query the original key — still cached.
        let (_, status) = cache.get_or_build(k_current, || panic!("hit")).unwrap();
        assert!(matches!(status, CacheStatus::Hit));
    }

    #[test]
    fn test_invalidate_all_drops_entries() {
        let cache = MemoryPackCache::new(Duration::from_secs(60));
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };
        cache.get_or_build(key.clone(), || Ok(empty_pack(None))).unwrap();

        cache.invalidate_all();

        let (_, status) = cache.get_or_build(key, || Ok(empty_pack(None))).unwrap();
        assert!(matches!(status, CacheStatus::Miss));
        assert_eq!(cache.builder_calls(), 2);
    }

    /// D86 §B — TTL = 0 fully bypasses the cache.
    #[test]
    fn test_zero_ttl_bypasses_cache() {
        let cache = MemoryPackCache::from_ttl_secs(0);
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };

        cache.get_or_build(key.clone(), || Ok(empty_pack(None))).unwrap();
        let (_, status) = cache.get_or_build(key, || Ok(empty_pack(None))).unwrap();
        assert!(matches!(status, CacheStatus::Miss), "TTL=0 → every fetch misses");
        assert_eq!(cache.builder_calls(), 2);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 2);
    }

    /// D87 §B — hit / miss / hit_ratio counters track every fetch.
    #[test]
    fn test_hit_ratio_tracks_fetches() {
        let cache = MemoryPackCache::new(Duration::from_secs(60));
        let key = CacheKey {
            kind: "current",
            query: None,
            project: None,
            session_id: None,
            thread_key: None,
        };

        // No fetches → ratio is None (fresh resident).
        assert_eq!(cache.hit_ratio(), None);

        // 1 miss + 3 hits = 75 % hit ratio.
        cache.get_or_build(key.clone(), || Ok(empty_pack(None))).unwrap();
        for _ in 0..3 {
            cache.get_or_build(key.clone(), || panic!("must hit")).unwrap();
        }
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 3);
        let ratio = cache.hit_ratio().unwrap();
        assert!((ratio - 0.75).abs() < 1e-3, "hit ratio = 0.75, got {ratio}");
    }
}
