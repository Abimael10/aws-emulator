// SPDX-License-Identifier: CC0-1.0

//! The caching keys a resolver may be cached by, end to end over HTTP.
//!
//! A caching key names an entry of one of the request-context maps a cache entry may be
//! keyed on — "Valid values are entries from the `$context.arguments`,
//! `$context.source`, and `$context.identity` maps" (`CachingConfig$cachingKeys`), plus
//! `$context.args`, the short name for the arguments map AppSync's own refusal names
//! beside them — so a configuration whose keys are all rooted there is served 200 and
//! round-trips through `GetResolver`, and one carrying a key rooted anywhere else is
//! refused 400 `BadRequestException` with AppSync's fixed sentence, on BOTH calls that
//! write a resolver's caching configuration. A refused create leaves the field it named
//! carrying no resolver and a refused update leaves the resolver already there exactly as
//! it was, which the shared walk below asserts for every row.
//!
//! The rows differ along what the rule generalises over: which context map a key is
//! rooted in, how it selects an entry out of that map (by name, by index, the whole map,
//! or a longer name continuing the root), how many keys one configuration carries (none,
//! one, several), and where a refused key is rooted (another `$context` map, another
//! `$util` root, no root at all, an empty key, and one bad key among valid ones).
//!
//! The row type, the request helpers and the two walks are the sibling caching-TTL
//! suite's, which states the rule over the other member of the same configuration;
//! every assertion goes through the router the production emulator mounts.

use super::error_contract::router;
use super::resolver_caching_config::{
    assert_creates_follow_the_rule, assert_updates_follow_the_rule, Row,
};
use super::resolvers::api_with_dependencies;

/// AppSync's sentence for a caching key rooted outside the context maps a cache entry
/// may be keyed on.
///
/// Written out here rather than read off the service's own constant, so a change of
/// wording is caught rather than followed.
const INVALID_CACHING_KEYS: &str =
    "Caching keys need to start with $context.args, $context.arguments, $context.identity, \
     or $context.source.";

/// The caching-keys table, over the roots a key may name an entry of. Every row's TTL is
/// inside the documented window, so a row's outcome can only come from its keys.
///
/// Source: `smithy_query(query="shape", shape="CachingConfig")` — "Valid values are
/// entries from the `$context.arguments`, `$context.source`, and `$context.identity`
/// maps" — and the service's own refusal sentence, which names `$context.args` beside
/// those three.
const ROWS: &[Row] = &[
    Row {
        label: "an entry of the arguments map",
        ttl: 60,
        keys: Some(&["$context.arguments.id"]),
        refusal: None,
    },
    Row {
        label: "an entry of the arguments map selected by index",
        ttl: 60,
        keys: Some(&["$context.arguments[\"id\"]"]),
        refusal: None,
    },
    Row {
        label: "an entry of the arguments map under its short name",
        ttl: 60,
        keys: Some(&["$context.args.id"]),
        refusal: None,
    },
    Row {
        label: "an entry of the identity map",
        ttl: 60,
        keys: Some(&["$context.identity.sub"]),
        refusal: None,
    },
    Row {
        label: "an entry of the source map",
        ttl: 60,
        keys: Some(&["$context.source.parentId"]),
        refusal: None,
    },
    Row {
        label: "a whole context map",
        ttl: 60,
        keys: Some(&["$context.identity"]),
        refusal: None,
    },
    Row {
        label: "three keys over three maps at once",
        ttl: 300,
        keys: Some(&[
            "$context.arguments.id",
            "$context.identity.sub",
            "$context.source.parentId",
        ]),
        refusal: None,
    },
    Row {
        label: "a caching configuration listing no key",
        ttl: 60,
        keys: Some(&[]),
        refusal: None,
    },
    Row {
        label: "a caching configuration carrying no keys member",
        ttl: 60,
        keys: None,
        refusal: None,
    },
    Row {
        label: "a context map AppSync has none of",
        ttl: 60,
        keys: Some(&["$context.nonsense.id"]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
    Row {
        label: "the result of the request rather than its input",
        ttl: 60,
        keys: Some(&["$context.result.id"]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
    Row {
        label: "a root that is not the context at all",
        ttl: 60,
        keys: Some(&["$util.time.nowISO8601()"]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
    Row {
        label: "an argument named without its map",
        ttl: 60,
        keys: Some(&["id"]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
    Row {
        label: "an empty key",
        ttl: 60,
        keys: Some(&[""]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
    Row {
        label: "a key that continues one of the roots",
        ttl: 60,
        keys: Some(&["$context.argumentsOfMine"]),
        // The rule AppSync's sentence states is the prefix, so what follows a root is the
        // caller's business.
        refusal: None,
    },
    Row {
        label: "one key outside the maps among keys inside them",
        ttl: 60,
        keys: Some(&["$context.arguments.id", "$context.stash.id"]),
        refusal: Some(INVALID_CACHING_KEYS),
    },
];

#[tokio::test]
async fn a_create_writes_a_resolver_only_for_keys_a_cache_can_be_keyed_on() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_caching_keys_create").await;
    assert_creates_follow_the_rule(&router, &api_id, ROWS).await;
}

#[tokio::test]
async fn an_update_is_judged_by_the_same_roots_and_a_refusal_keeps_the_stored_keys() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_caching_keys_update").await;
    assert_updates_follow_the_rule(&router, &api_id, ROWS).await;
}
