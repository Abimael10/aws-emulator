// SPDX-License-Identifier: CC0-1.0

//! The caching configuration a resolver may be written with: the window its `ttl` lies
//! in, the values its `cachingKeys` may hold, and the refusals for a configuration
//! AppSync could not cache by.
//!
//! `CachingConfig` carries the two members that describe a resolver's cache
//! (`smithy_query(query="shape", shape="CachingConfig")`), and the model documents a
//! rule for each:
//!
//! * `ttl` is "the TTL in seconds for a resolver that has caching activated", and
//!   "Valid values are 1–3,600 seconds" — the same window the API cache's own `ttl` is
//!   documented with, which this crate already states
//!   ([`super::CACHE_TTL_MIN_SECONDS`]), so one window is enforced one way and worded one
//!   way wherever a caller sets a cache-entry TTL. The member targets an unconstrained
//!   `Long`, so the window is the service's to enforce rather than the request
//!   boundary's;
//! * `cachingKeys` holds the keys a cache entry is keyed on, and "Valid values are
//!   entries from the `$context.arguments`, `$context.source`, and `$context.identity`
//!   maps" — so a key is not free text: it names an entry of one of the request-context
//!   maps AppSync can key a cache entry on, and a key rooted anywhere else names nothing
//!   the cache could be keyed by. AppSync's own refusal for such a key names the roots it
//!   accepts, `$context.args` — VTL's short name for the arguments map — among them, so
//!   the accepted set below is the four roots that sentence names.
//!
//! Both rules are over the MEMBERS rather than over one request's values, so they are
//! the same on both calls that write a resolver's caching configuration
//! (`CreateResolver` and `UpdateResolver`), and a request that breaks either is refused
//! before anything is written — as the service's `BadRequestException`, the only client
//! error for a malformed value these operations model. A refused call therefore leaves
//! the field it named carrying no resolver, which is what a later `GetResolver` reports.

use crate::ops::types::CachingConfig;

/// The context maps a caching key may name an entry of: the three
/// `CachingConfig$cachingKeys` documents ("entries from the `$context.arguments`,
/// `$context.source`, and `$context.identity` maps") plus `$context.args`, the short
/// name for the arguments map that AppSync's own refusal for a key outside this set
/// names alongside them ([`INVALID_CACHING_KEYS_MESSAGE`]).
pub const CACHING_KEY_ROOTS: &[&str] = &[
    "$context.args",
    "$context.arguments",
    "$context.identity",
    "$context.source",
];

/// The text a `BadRequestException` (HTTP 400) carries for a caching key rooted outside
/// [`CACHING_KEY_ROOTS`]: AppSync's fixed sentence, which names the roots it accepts and
/// nothing of the request that broke the rule.
pub const INVALID_CACHING_KEYS_MESSAGE: &str =
    "Caching keys need to start with $context.args, $context.arguments, $context.identity, \
     or $context.source.";

/// Whether `key` names an entry of one of the maps a cache entry may be keyed on: the
/// rule AppSync's own sentence states is that the key STARTS WITH one of
/// [`CACHING_KEY_ROOTS`], so the test is that prefix and nothing more — the service says
/// nothing about what may follow the root, and a stricter test would refuse a key it
/// serves.
fn is_caching_key(key: &str) -> bool {
    CACHING_KEY_ROOTS.iter().any(|root| key.starts_with(root))
}

/// The refusal text for a caching configuration AppSync could not cache by, or `None`
/// for one it could — a request that carries no `cachingConfig`, or one whose `ttl` is
/// inside the documented window and whose `cachingKeys` are all rooted in a context map
/// ([`is_caching_key`]), the empty list of keys included.
///
/// One call states ONE refusal: the TTL, which every cached resolver carries, before the
/// keys, which are optional — the way the rest of this service's bad-request wording
/// names the first member the request got wrong.
pub fn caching_config_refusal(caching_config: Option<&CachingConfig>) -> Option<String> {
    let caching_config = caching_config?;
    if !super::is_cache_ttl(caching_config.ttl) {
        return Some(super::invalid_cache_ttl_message());
    }
    let keys = caching_config.caching_keys.as_ref()?;
    keys.iter()
        .any(|key| !is_caching_key(key))
        .then(|| INVALID_CACHING_KEYS_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{invalid_cache_ttl_message, CACHE_TTL_MAX_SECONDS, CACHE_TTL_MIN_SECONDS};

    /// A caching configuration over `keys`, at a TTL inside the documented window.
    fn config(keys: &[&str]) -> CachingConfig {
        CachingConfig {
            ttl: 60,
            caching_keys: Some(keys.iter().map(|key| (*key).to_string()).collect()),
        }
    }

    #[test]
    fn a_key_rooted_in_a_context_map_is_not_refused() {
        // One case per documented root, each selecting an entry a different way, plus
        // the root on its own and the configuration that lists no key at all.
        let cases: &[(&str, Vec<&str>)] = &[
            ("no caching keys", vec![]),
            ("the arguments map", vec!["$context.arguments.id"]),
            (
                "the arguments map by index",
                vec!["$context.arguments[\"id\"]"],
            ),
            ("the short arguments map", vec!["$context.args.id"]),
            ("the identity map", vec!["$context.identity.sub"]),
            ("the source map", vec!["$context.source.owner"]),
            ("a whole map", vec!["$context.identity"]),
            // The rule is the prefix the refusal names, so what follows a root is the
            // caller's business.
            ("a key that continues a root", vec!["$context.argumentsOfMine"]),
            (
                "several maps at once",
                vec![
                    "$context.arguments.id",
                    "$context.identity.sub",
                    "$context.source.parentId",
                ],
            ),
        ];
        for (case, keys) in cases {
            let refusal = caching_config_refusal(Some(&config(keys)));
            assert!(
                refusal.is_none(),
                "case {case:?}: expected no refusal, got {refusal:?}"
            );
        }
    }

    #[test]
    fn a_key_rooted_outside_the_context_maps_is_refused() {
        let cases: &[(&str, Vec<&str>)] = &[
            ("a map AppSync has none of", vec!["$context.nonsense.id"]),
            ("the result of the request", vec!["$context.result.id"]),
            ("a root that is not the context", vec!["$util.time"]),
            ("a bare argument name", vec!["id"]),
            ("an empty key", vec![""]),
            (
                "one bad key among valid ones",
                vec!["$context.arguments.id", "$context.stash.id"],
            ),
        ];
        for (case, keys) in cases {
            let refusal = caching_config_refusal(Some(&config(keys)));
            assert_eq!(
                refusal.as_deref(),
                Some(INVALID_CACHING_KEYS_MESSAGE),
                "case {case:?}: expected {INVALID_CACHING_KEYS_MESSAGE:?}, got {refusal:?}"
            );
        }
    }

    #[test]
    fn a_ttl_is_refused_outside_the_documented_window() {
        for ttl in [CACHE_TTL_MIN_SECONDS, 60, CACHE_TTL_MAX_SECONDS] {
            let inside = CachingConfig {
                ttl,
                caching_keys: Some(vec!["$context.arguments.id".to_string()]),
            };
            let refusal = caching_config_refusal(Some(&inside));
            assert!(
                refusal.is_none(),
                "case a TTL of {ttl} seconds: expected no refusal, got {refusal:?}"
            );
        }
        let expected = invalid_cache_ttl_message();
        for ttl in [CACHE_TTL_MIN_SECONDS - 1, CACHE_TTL_MAX_SECONDS + 1, -1] {
            let outside = CachingConfig {
                ttl,
                // The TTL is the first member refused, whatever the keys hold.
                caching_keys: Some(vec!["$context.nonsense.id".to_string()]),
            };
            let refusal = caching_config_refusal(Some(&outside));
            assert_eq!(
                refusal.as_deref(),
                Some(expected.as_str()),
                "case a TTL of {ttl} seconds: expected {expected:?}, got {refusal:?}"
            );
        }
    }

    #[test]
    fn no_caching_configuration_is_nothing_to_refuse() {
        let refusal = caching_config_refusal(None);
        assert!(
            refusal.is_none(),
            "case a request carrying no cachingConfig: expected no refusal, got {refusal:?}"
        );
    }
}
