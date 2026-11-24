// SPDX-License-Identifier: CC0-1.0

//! The ENTRIES an API's server-side cache holds: what the GraphQL endpoint already
//! answered, for as long as each answer may be answered again.
//!
//! An `ApiCache` is not a label. `ApiCache$ttl` is "the TTL in seconds for cache entries"
//! and `ApiCache$apiCachingBehavior` says what one entry IS — a whole request's response,
//! one operation's response, or one resolver's result — so a cache that is AVAILABLE holds
//! entries, and a request whose key already has a live entry is answered FROM it. That is
//! the only thing about a cache a client can observe: the resolver is not run, the data
//! source is not called, and the stored answer is served again until its TTL passes, it is
//! evicted, or `FlushApiCache` discards it.
//!
//! What is kept per entry is the VALUE that was served and the instant it stops being
//! served. The entries belong to the cache instance itself
//! ([`super::ApiCacheRecord::entries`]), so the teardown of the instance and the deletion
//! of the API take them with it, and no entry outlives the cache that holds it.
//!
//! The collection is bounded in host code, because a cache key is the CALLER's choice: a
//! request chooses its own document, variables and arguments, so the number of entries one
//! API can be made to hold and the bytes they retain — the keys they are held under as much
//! as the values they hold — are capped here rather than by anything a request carries
//! ([`ENTRIES_PER_CACHE`], [`MAX_ENTRY_BYTES`], [`MAX_RETAINED_BYTES`]). Past a cap the entry
//! whose TTL passes SOONEST makes room, so a long-lived entry is never dropped in favour of
//! one that is about to go.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::serde_json::Value as Json;

/// How many entries ONE cache instance may hold at a time.
const ENTRIES_PER_CACHE: usize = 1_024;

/// How large ONE entry may be, in the bytes its key and its value take together. An entry
/// past it is served to the caller that asked for it and held by nothing.
const MAX_ENTRY_BYTES: usize = 1024 * 1024;

/// How many bytes of entry one cache instance RETAINS in all, keys and values alike.
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;

/// One held entry: the value to answer with, and when it stops being answered with.
#[derive(Debug, Clone)]
struct HeldEntry {
    /// The value that was served, held exactly as it was served, so a hit answers what the
    /// miss answered rather than anything recomputed.
    value: Json,
    /// How many bytes of [`MAX_RETAINED_BYTES`] this entry takes: the value it holds AND the
    /// key it is held under, because a request chooses both.
    bytes: usize,
    /// The instant this entry's TTL passes, after which it is served to nobody.
    expires_at: SystemTime,
}

/// The entries one cache instance holds, keyed by the cache key the behavior in force
/// composed ([`super::ApiCacheRecord`] owns one of these).
#[derive(Debug, Clone, Default)]
pub struct CacheEntries {
    /// Keyed by the composed cache key.
    held: BTreeMap<String, HeldEntry>,
    /// The bytes the held values take between them, kept as they are inserted and removed
    /// so the byte cap costs no walk of the collection.
    retained: usize,
}

impl CacheEntries {
    /// The value held under `key` where its TTL has not passed at `now`, or `None` where
    /// none is.
    ///
    /// An entry whose TTL HAS passed is dropped as it is found: the TTL passing is what the
    /// clock already owed that entry, so no later caller can be answered from it either.
    /// Reading an entry neither moves its expiry nor touches anything else in the
    /// collection, so a hit never extends the window the cache serves it for.
    pub fn held(&mut self, key: &str, now: SystemTime) -> Option<Json> {
        let entry = self.held.get(key)?;
        if now >= entry.expires_at {
            self.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    /// Hold `value` under `key` for `ttl` from `now`.
    ///
    /// What an entry COSTS is the value it holds and the key it is held under together: a
    /// request chooses both, so both are charged against the same budget and bounded by the
    /// same ceiling. An entry too large to retain ([`MAX_ENTRY_BYTES`]) is held by nothing,
    /// so the next request for that key is answered the same way this one was: by running
    /// the resolution again.
    pub fn hold(&mut self, key: String, value: Json, ttl: Duration, now: SystemTime) {
        let bytes = ::serde_json::to_string(&value)
            .map(|rendered| rendered.len())
            .unwrap_or(usize::MAX)
            .saturating_add(key.len());
        if bytes > MAX_ENTRY_BYTES {
            return;
        }
        let Some(expires_at) = now.checked_add(ttl) else {
            return;
        };
        // Every entry whose TTL has already passed goes first, which is what keeps the
        // collection to the entries that are live.
        self.retain_live(now);
        // Then, while this entry would not fit, the one that goes soonest makes room.
        self.replace(&key);
        while self.held.len() >= ENTRIES_PER_CACHE
            || self.retained.saturating_add(bytes) > MAX_RETAINED_BYTES
        {
            let Some(soonest) = self
                .held
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.remove(&soonest);
        }
        self.retained = self.retained.saturating_add(bytes);
        self.held.insert(
            key,
            HeldEntry {
                value,
                bytes,
                expires_at,
            },
        );
    }

    /// Discard the entry held under `key`, answering whether there was one.
    ///
    /// What ONE eviction removes: the entry of the key it names and no sibling of it.
    pub fn discard(&mut self, key: &str) -> bool {
        self.remove(key).is_some()
    }

    /// Discard every entry this cache holds — what a flush of the instance does to them.
    pub fn discard_all(&mut self) {
        self.held.clear();
        self.retained = 0;
    }

    /// How many entries are held, live or not. Diagnosable state rather than anything a
    /// response projects.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// How many bytes the entries held take between them, keys and values alike — the figure
    /// [`MAX_RETAINED_BYTES`] bounds.
    pub fn retained(&self) -> usize {
        self.retained
    }

    /// Whether the cache holds no entry at all.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Drop the entry under `key`, keeping the retained byte count with it.
    fn remove(&mut self, key: &str) -> Option<HeldEntry> {
        let entry = self.held.remove(key)?;
        self.retained = self.retained.saturating_sub(entry.bytes);
        Some(entry)
    }

    /// Drop the entry a fresh one under the same key replaces.
    fn replace(&mut self, key: &str) {
        self.remove(key);
    }

    /// Drop every entry whose TTL has passed at `now`.
    fn retain_live(&mut self, now: SystemTime) {
        let passed: Vec<String> = self
            .held
            .iter()
            .filter(|(_, entry)| now >= entry.expires_at)
            .map(|(key, _)| key.clone())
            .collect();
        for key in passed {
            self.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::serde_json::json;

    /// An entry is served for its own key while its TTL has not passed, is served to no
    /// other key, and is served to nobody once it has.
    #[test]
    fn an_entry_is_served_for_its_own_key_only_while_its_ttl_has_not_passed() {
        let mut entries = CacheEntries::default();
        let now = SystemTime::UNIX_EPOCH;
        entries.hold("one".to_string(), json!({"v": 1}), Duration::from_secs(300), now);
        // The rows differ along what is asked for and when.
        let cases: [(&str, &str, Duration, Option<Json>); 4] = [
            ("the same key, inside the ttl", "one", Duration::ZERO, Some(json!({"v": 1}))),
            (
                "the same key, one second before the ttl passes",
                "one",
                Duration::from_secs(299),
                Some(json!({"v": 1})),
            ),
            ("another key", "two", Duration::ZERO, None),
            (
                "the same key once the ttl has passed",
                "one",
                Duration::from_secs(300),
                None,
            ),
        ];
        for (case, key, elapsed, expected) in cases {
            let served = entries.held(key, now + elapsed);
            assert_eq!(
                served, expected,
                "case {case:?}: expected {expected:?}, got {served:?}"
            );
        }
    }

    /// A hit does not move the entry's expiry: the window is measured from the instant the
    /// entry was written, however often it is served inside it.
    #[test]
    fn serving_an_entry_does_not_extend_its_window() {
        let mut entries = CacheEntries::default();
        let now = SystemTime::UNIX_EPOCH;
        entries.hold("one".to_string(), json!(1), Duration::from_secs(10), now);
        for elapsed in [1u64, 5, 9] {
            let served = entries.held("one", now + Duration::from_secs(elapsed));
            assert_eq!(
                served,
                Some(json!(1)),
                "case a read {elapsed} seconds in: expected the held value, got {served:?}"
            );
        }
        let served = entries.held("one", now + Duration::from_secs(10));
        assert_eq!(
            served, None,
            "case a read once the ttl has passed: expected nothing, got {served:?}"
        );
    }

    /// An eviction removes the entry of the key it names and leaves every sibling; a flush
    /// removes them all.
    #[test]
    fn one_eviction_spares_the_sibling_keys_and_a_flush_spares_none() {
        let mut entries = CacheEntries::default();
        let now = SystemTime::UNIX_EPOCH;
        for key in ["a", "b", "c"] {
            entries.hold(key.to_string(), json!(key), Duration::from_secs(60), now);
        }
        assert!(
            entries.discard("b"),
            "case evicting a held key: expected it to have been held"
        );
        assert!(
            !entries.discard("missing"),
            "case evicting a key nothing holds: expected no entry to remove"
        );
        for (case, key, expected) in [
            ("the evicted key", "b", None),
            ("a sibling before it", "a", Some(json!("a"))),
            ("a sibling after it", "c", Some(json!("c"))),
        ] {
            let served = entries.held(key, now);
            assert_eq!(
                served, expected,
                "case {case:?}: expected {expected:?}, got {served:?}"
            );
        }
        entries.discard_all();
        assert!(
            entries.is_empty() && entries.held("a", now).is_none(),
            "case a flushed cache: expected no entry to remain, got {} entries",
            entries.len()
        );
    }

    /// The collection is bounded: a value too large for one entry is held by nothing, and
    /// past the entry count the entry whose TTL passes soonest makes room.
    #[test]
    fn what_one_cache_holds_is_bounded_in_host_code() {
        let mut entries = CacheEntries::default();
        let now = SystemTime::UNIX_EPOCH;
        let oversized = json!("x".repeat(MAX_ENTRY_BYTES + 1));
        entries.hold("big".to_string(), oversized, Duration::from_secs(60), now);
        assert!(
            entries.held("big", now).is_none(),
            "case a value past the entry ceiling: expected nothing to be held"
        );
        // The first entry goes soonest, so it is the one the cap drops.
        for index in 0..=ENTRIES_PER_CACHE {
            entries.hold(
                format!("key-{index}"),
                json!(index),
                Duration::from_secs(60 + index as u64),
                now,
            );
        }
        assert!(
            entries.len() <= ENTRIES_PER_CACHE,
            "case a cache filled past its entry cap: expected at most {ENTRIES_PER_CACHE} \
             entries, got {}",
            entries.len()
        );
        assert!(
            entries.held("key-0", now).is_none()
                && entries.held(&format!("key-{ENTRIES_PER_CACHE}"), now).is_some(),
            "case a cache filled past its entry cap: expected the soonest-expiring entry to \
             have made room for the newest"
        );
    }

    /// The bytes an entry's KEY takes are charged against the same budget as the value it
    /// holds, so a cache filled with long keys retains no more than the cap.
    #[test]
    fn the_bytes_a_key_takes_are_charged_against_the_retained_cap() {
        let mut entries = CacheEntries::default();
        let now = SystemTime::UNIX_EPOCH;
        // A key alone past the entry ceiling is held by nothing, whatever its value is.
        let long = "k".repeat(MAX_ENTRY_BYTES + 1);
        entries.hold(long.clone(), json!(1), Duration::from_secs(60), now);
        assert!(
            entries.held(&long, now).is_none() && entries.retained() == 0,
            "case a key past the entry ceiling: expected nothing held and nothing retained, \
             got {} bytes",
            entries.retained()
        );
        // And filling the cache with long keys retains no more than the cap allows: the
        // entries are the cap's own count, each as large as one entry may be.
        let key_bytes = MAX_ENTRY_BYTES / 2;
        for index in 0..=ENTRIES_PER_CACHE {
            let key = format!("{index:06}{}", "k".repeat(key_bytes));
            entries.hold(
                key,
                json!(index),
                Duration::from_secs(60 + index as u64),
                now,
            );
        }
        assert!(
            entries.retained() <= MAX_RETAINED_BYTES && entries.len() <= ENTRIES_PER_CACHE,
            "case a cache filled with long keys: expected at most {MAX_RETAINED_BYTES} bytes \
             over at most {ENTRIES_PER_CACHE} entries, got {} bytes over {} entries",
            entries.retained(),
            entries.len()
        );
        // The accounting is symmetric: discarding what is held leaves nothing retained.
        entries.discard_all();
        assert_eq!(
            entries.retained(),
            0,
            "case a flushed cache: expected nothing retained, got {} bytes",
            entries.retained()
        );
    }
}
