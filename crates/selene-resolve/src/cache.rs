//! The resolver's caches — `maps/resolution.md` §Caches, ported including its
//! sizing policy.
//!
//! Every cache is **per-resolver-instance** (the TS module-level
//! `cppIncludeDirCache` and the COBOL copybook `WeakMap` become plain fields —
//! §Rust port notes), and every one is an LRU with the TS `LRUCache`'s exact
//! semantics: `get` refreshes recency, `put` evicts the oldest. The `lru`
//! crate matches that behavior (pinned by a spike assertion,
//! `tests/spike_seam.rs` F6b).
//!
//! **Dead code, deliberately not ported:** `import-resolver.ts`'s module-level
//! `importMappingCache` (declared, cleared, never written — the real cache is
//! the resolver's LRU).
//!
//! # Sizing
//!
//! [`DEFAULT_CACHE_LIMIT`] entries, overridable by `SELENE_RESOLVER_CACHE_SIZE`.
//! **Content-bearing** caches (file text, split lines) get
//! [`content_cache_limit`] — `max(64, limit / 5)` — because they hold whole
//! files rather than small records, so they get a fifth of the entry budget.

use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

/// Entries per cache, unless `SELENE_RESOLVER_CACHE_SIZE` says otherwise
/// (`maps/resolution.md` §Caches: `DEFAULT_CACHE_LIMIT = 5000`).
pub const DEFAULT_CACHE_LIMIT: usize = 5_000;

/// The env override's name. A positive integer; **anything else falls back to
/// the default** — a garbage env var degrades performance, it never fails a
/// run (errors are collected, never thrown).
pub const CACHE_SIZE_ENV: &str = "SELENE_RESOLVER_CACHE_SIZE";

/// The configured entry limit: `SELENE_RESOLVER_CACHE_SIZE` when it parses to a
/// positive integer, else [`DEFAULT_CACHE_LIMIT`].
pub fn cache_limit() -> usize {
    std::env::var(CACHE_SIZE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_CACHE_LIMIT)
}

/// The limit for content-bearing caches (file text, split lines):
/// `max(64, limit / 5)`.
pub fn content_cache_limit(limit: usize) -> usize {
    (limit / 5).max(64)
}

/// An LRU behind a `Mutex`, because [`crate::ResolutionContext`] is
/// `Send + Sync` and its methods take `&self` (the strategies read through a
/// shared reference; the cache is the only interior mutability in the context).
///
/// Not a `RefCell`: the framework registry and the synth passes hold
/// `&dyn ResolutionContext` across threads.
#[derive(Debug)]
pub struct SyncLru<K: Hash + Eq, V> {
    inner: Mutex<LruCache<K, V>>,
}

impl<K: Hash + Eq, V: Clone> SyncLru<K, V> {
    /// A cache holding at most `capacity` entries (clamped to ≥ 1).
    pub fn new(capacity: usize) -> Self {
        // `max(1)`: NonZeroUsize is a type-level guarantee, and a zero-capacity
        // cache would be a silent "cache nothing" — the clamp is honest.
        #[allow(clippy::unwrap_used)] // max(1) makes this infallible
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Look `key` up, refreshing its recency on a hit.
    pub fn get(&self, key: &K) -> Option<V> {
        let mut guard = self.inner.lock().ok()?;
        guard.get(key).cloned()
    }

    /// Insert `value`, evicting the least-recently-used entry when full.
    pub fn put(&self, key: K, value: V) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.put(key, value);
        }
    }

    /// Cached read-through: `get(key)`, or compute with `f`, store, and return.
    ///
    /// `f` runs **outside** the lock — it performs a store query or a file
    /// read, and holding the cache mutex across that would serialize the whole
    /// resolver on one lock. A benign consequence: two threads can compute the
    /// same key concurrently; the second `put` simply wins. Resolution is
    /// deterministic regardless (the computed value is a pure function of the
    /// key and the immutable graph).
    pub fn get_or_insert_with<F: FnOnce() -> V>(&self, key: K, f: F) -> V
    where
        K: Clone,
    {
        if let Some(hit) = self.get(&key) {
            return hit;
        }
        let value = f();
        self.put(key, value.clone());
        value
    }

    /// Drop every entry (`clear_caches()` — the resolver clears before and
    /// after `run_post_extract`, which mutates nodes).
    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.clear();
        }
    }

    /// Entries currently held (tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Whether the cache holds nothing (tests; paired with [`Self::len`]).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_refreshes_recency_and_put_evicts_the_oldest() {
        // The TS LRUCache's exact semantics (maps/resolution.md §Caches).
        let cache: SyncLru<&str, u32> = SyncLru::new(2);
        cache.put("a", 1);
        cache.put("b", 2);
        assert_eq!(cache.get(&"a"), Some(1)); // refreshes `a` → `b` is now LRU
        cache.put("c", 3);
        assert_eq!(cache.get(&"b"), None, "`b` was the LRU and got evicted");
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"c"), Some(3));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn read_through_computes_once_per_key() {
        let cache: SyncLru<String, u32> = SyncLru::new(8);
        let mut calls = 0;
        let mut load = |n: u32| {
            calls += 1;
            n * 10
        };
        assert_eq!(cache.get_or_insert_with("k".to_string(), || load(4)), 40);
        assert_eq!(cache.get_or_insert_with("k".to_string(), || load(4)), 40);
        assert_eq!(calls, 1, "the second call is a cache hit");
    }

    #[test]
    fn zero_capacity_is_clamped_not_a_panic() {
        let cache: SyncLru<&str, u32> = SyncLru::new(0);
        cache.put("a", 1);
        assert_eq!(cache.get(&"a"), Some(1));
    }

    #[test]
    fn clear_drops_everything() {
        let cache: SyncLru<&str, u32> = SyncLru::new(4);
        cache.put("a", 1);
        cache.clear();
        assert_eq!(cache.get(&"a"), None);
    }

    #[test]
    fn content_cache_limit_is_a_fifth_with_a_floor() {
        assert_eq!(content_cache_limit(5_000), 1_000);
        assert_eq!(content_cache_limit(100), 64, "the floor is 64, not 20");
        assert_eq!(content_cache_limit(1), 64);
    }

    /// The env override parses positive integers and **falls back silently**
    /// on anything else — a bad env var must never fail a run.
    ///
    /// Single test, serial by construction: `set_var`/`remove_var` mutate
    /// process-global state, and `cargo test` runs tests in one process on
    /// multiple threads, so splitting this across tests would flake.
    #[test]
    // `std::env::set_var`/`remove_var` are `unsafe` in edition 2024 (they race with a
    // concurrent `getenv` in another thread). The workspace lints `unsafe_code`, and
    // rightly so — this is the one place in the crate that needs it, it is test-only,
    // and the safety argument is in the block comment below.
    #[allow(unsafe_code)]
    fn cache_size_env_parses_or_falls_back() {
        // SAFETY: `set_var`/`remove_var` are unsafe in edition 2024 (they race
        // with concurrent getenv in other threads). This is the ONLY test in
        // the crate that touches this variable, and it restores it before
        // returning, so no other test observes the mutation.
        unsafe {
            std::env::remove_var(CACHE_SIZE_ENV);
            assert_eq!(cache_limit(), DEFAULT_CACHE_LIMIT);

            std::env::set_var(CACHE_SIZE_ENV, "128");
            assert_eq!(cache_limit(), 128);

            std::env::set_var(CACHE_SIZE_ENV, " 256 ");
            assert_eq!(cache_limit(), 256, "surrounding whitespace is tolerated");

            std::env::set_var(CACHE_SIZE_ENV, "0");
            assert_eq!(cache_limit(), DEFAULT_CACHE_LIMIT, "zero is not positive");

            std::env::set_var(CACHE_SIZE_ENV, "-5");
            assert_eq!(cache_limit(), DEFAULT_CACHE_LIMIT);

            std::env::set_var(CACHE_SIZE_ENV, "banana");
            assert_eq!(cache_limit(), DEFAULT_CACHE_LIMIT, "garbage never errors");

            std::env::remove_var(CACHE_SIZE_ENV);
        }
        assert_eq!(cache_limit(), DEFAULT_CACHE_LIMIT);
    }
}
