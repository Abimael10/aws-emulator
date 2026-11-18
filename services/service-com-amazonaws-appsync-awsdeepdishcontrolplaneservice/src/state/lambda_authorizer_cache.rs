// SPDX-License-Identifier: CC0-1.0

//! The decisions a GraphQL API's Lambda authorizer has taken, for as long as each may be
//! REUSED.
//!
//! `LambdaAuthorizerConfig$authorizerResultTtlInSeconds` is "the number of seconds a response
//! should be cached for. The default is 0 seconds, which disables caching… The Lambda function
//! can override this by returning a `ttlOverride` key in its response" — so a decision taken
//! for one token is the decision of every further request presenting that token until its
//! window closes, and the authorizer is invoked exactly once inside it.
//!
//! What is kept is the ANSWER the authorizer returned and the instant the window closes.
//! Nothing of the token itself is kept: an entry is held under the DIGEST of the token
//! ([`token_key`]), which is what makes two requests presenting one token one cache entry
//! without this store retaining the material a caller presented.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ::sha2::{Digest, Sha256};

/// How many decisions ONE API may hold at a time.
///
/// An emulator resource guard, not an AWS quota: the key is a token a CALLER chose, so the
/// number of entries one API can be made to hold is the callers' choice rather than the
/// account's. Past it the oldest-expiring entry makes room, so a live decision is never
/// dropped in favour of nothing.
const DECISIONS_PER_API: usize = 1_024;

/// One decision, for as long as it may be reused.
#[derive(Debug, Clone)]
struct CachedDecision {
    /// The answer the authorizer returned, exactly as it returned it.
    answer: ::serde_json::Value,
    /// When the window this decision may be reused in closes.
    until: Instant,
}

/// The decisions each GraphQL API's Lambda authorizer has taken.
#[derive(Debug, Default)]
pub struct LambdaAuthorizerCache {
    /// Keyed by the API and the digest of the token the decision was taken for.
    decisions: BTreeMap<(String, String), CachedDecision>,
}

impl LambdaAuthorizerCache {
    /// The decision held for `token` on `api_id` where its window is still open at `now`, or
    /// `None` where none is.
    ///
    /// An entry whose window has CLOSED is dropped as it is found: the window closing is what
    /// the clock already owed this entry, so a later caller cannot be answered from it either.
    pub fn cached(
        &mut self,
        api_id: &str,
        token: &str,
        now: Instant,
    ) -> Option<::serde_json::Value> {
        let key = (api_id.to_string(), token_key(token));
        let held = self.decisions.get(&key)?;
        if held.until <= now {
            self.decisions.remove(&key);
            return None;
        }
        Some(held.answer.clone())
    }

    /// Hold `answer` as the decision for `token` on `api_id` for `window` from `now`.
    pub fn remember(
        &mut self,
        api_id: &str,
        token: &str,
        answer: &::serde_json::Value,
        window: Duration,
        now: Instant,
    ) {
        // Every window that has already closed is dropped first, which is what keeps the
        // collection to the decisions that are live.
        self.decisions.retain(|_, held| held.until > now);
        if self
            .decisions
            .keys()
            .filter(|(held, _)| held == api_id)
            .count()
            >= DECISIONS_PER_API
        {
            let soonest = self
                .decisions
                .iter()
                .filter(|((held, _), _)| held == api_id)
                .min_by_key(|(_, held)| held.until)
                .map(|(key, _)| key.clone());
            if let Some(soonest) = soonest {
                self.decisions.remove(&soonest);
            }
        }
        self.decisions.insert(
            (api_id.to_string(), token_key(token)),
            CachedDecision {
                answer: answer.clone(),
                until: now.checked_add(window).unwrap_or(now),
            },
        );
    }

    /// Forget every decision taken for `api_id` — what an API being deleted does to them.
    pub fn forget_api(&mut self, api_id: &str) {
        self.decisions.retain(|(held, _), _| held != api_id);
    }
}

/// The key one token's decision is held under: the SHA-256 digest of the token, in hex.
///
/// A digest rather than the token, so two requests presenting one token share one entry while
/// nothing of the material a caller presented is retained in this store. Written out through
/// the tree's one hex encoder.
fn token_key(token: &str) -> String {
    ::hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::serde_json::json;

    /// A decision is the decision of every request presenting the same token while its window
    /// is open, is nobody else's, and is gone once the window closes.
    #[test]
    fn a_decision_is_reused_only_for_its_own_token_and_only_while_its_window_is_open() {
        let mut cache = LambdaAuthorizerCache::default();
        let now = Instant::now();
        let answer = json!({"isAuthorized": true});
        cache.remember("api-one", "a-token", &answer, Duration::from_secs(300), now);
        // The rows differ along what is asked for: the same token of the same API, another
        // token of it, the same token of another API, and the same token after the window.
        let cases: [(&str, &str, &str, Duration, bool); 4] = [
            ("the same token, inside the window", "api-one", "a-token", Duration::from_secs(1), true),
            ("another token of the same API", "api-one", "other-token", Duration::ZERO, false),
            ("the same token of another API", "api-two", "a-token", Duration::ZERO, false),
            (
                "the same token once the window has closed",
                "api-one",
                "a-token",
                Duration::from_secs(301),
                false,
            ),
        ];
        for (case, api_id, token, elapsed, expected) in cases {
            let held = cache.cached(api_id, token, now + elapsed);
            assert_eq!(
                held.is_some(),
                expected,
                "case {case:?}: expected a held decision to be {expected}, got {held:?}"
            );
        }
    }

    /// Nothing of the token itself is retained: the key one decision is held under carries no
    /// part of the material presented.
    #[test]
    fn a_decision_is_keyed_by_a_digest_rather_than_the_token() {
        let token = "a-caller-chosen-token-value";
        let key = token_key(token);
        assert!(
            !key.contains(token) && key.len() == 64,
            "case the key of one token: expected a 64-character digest carrying no part of \
             the token, got {key:?}"
        );
        assert_ne!(
            key,
            token_key("another-token"),
            "case two tokens: expected different keys, got {key:?} for both"
        );
    }

    /// An API being deleted takes its decisions with it, and no other API's.
    #[test]
    fn deleting_an_api_forgets_its_own_decisions_only() {
        let mut cache = LambdaAuthorizerCache::default();
        let now = Instant::now();
        let answer = json!({"isAuthorized": true});
        for api_id in ["api-one", "api-two"] {
            cache.remember(api_id, "a-token", &answer, Duration::from_secs(300), now);
        }
        cache.forget_api("api-one");
        for (case, api_id, expected) in [
            ("the deleted API", "api-one", false),
            ("the API beside it", "api-two", true),
        ] {
            let held = cache.cached(api_id, "a-token", now);
            assert_eq!(
                held.is_some(),
                expected,
                "case {case:?}: expected a held decision to be {expected}, got {held:?}"
            );
        }
    }
}
