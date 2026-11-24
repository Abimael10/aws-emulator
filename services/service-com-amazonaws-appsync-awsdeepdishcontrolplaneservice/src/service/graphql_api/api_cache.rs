// SPDX-License-Identifier: CC0-1.0

//! The API's own SERVER-SIDE cache, as the GraphQL endpoint serves a request through it:
//! the key one answer is held under, the hit that replays it and the entry a miss writes.
//!
//! An `ApiCache` the service reports as available is not metadata. The three values
//! `ApiCachingBehavior` takes say what one entry IS and which requests are answered from
//! one, and the model documents each of them:
//!
//! * `FULL_REQUEST_CACHING` — "All requests from the same user are cached. Individual
//!   resolvers are automatically cached. All API calls will try to return responses from
//!   the cache." So the whole response is the unit, keyed by the request and the caller it
//!   came from, and every resolver of the API is cached as well, without any resolver
//!   declaring it;
//! * `OPERATION_LEVEL_CACHING` — "Full requests are cached together and returned without
//!   executing resolvers." So the whole response is the unit, keyed by the operation the
//!   request named, the document it ran and the values its variables took, and no entry of
//!   an individual resolver is written;
//! * `PER_RESOLVER_CACHING` — "Individual resolvers that you specify are cached." So the
//!   unit is one resolver's result, written only for a resolver that declares a
//!   `cachingConfig`, and keyed by exactly the `cachingKeys` it declares — "entries from
//!   the `$context.arguments`, `$context.source`, and `$context.identity` maps"
//!   (`CachingConfig$cachingKeys`) — so a request that changes an argument the resolver
//!   did NOT declare is answered from the entry written for the one it did.
//!
//! What a hit means is the same under all three: the resolution is not performed. No
//! resolver runs, no data source is called, and the value the entry holds is answered
//! again — a value the backing store may have moved on from — until `ApiCache$ttl` ("TTL in
//! seconds for cache entries") passes, the entry is evicted by
//! `extensions.evictFromApiCache` ([`evicted`]) or `FlushApiCache` discards the entries.
//!
//! Only a QUERY is answered from a cache and only a query's answer is held: a mutation
//! changes the state the API holds, so answering one from a stored response would report a
//! change nothing made, and a subscription is not executed on this plane at all.
//!
//! An answer that reports ERRORS is held by nothing. A cache entry exists to be served
//! instead of the resolution, and a resolution that did not produce a value has nothing to
//! serve: holding the failure would answer every later request for that key with a fault
//! that was one request's.
//!
//! The entries themselves belong to the cache instance
//! ([`crate::state::api_cache_entries`]); this module is what composes the keys and decides
//! which of them the behavior in force writes.

use ::std::time::{Duration, SystemTime};

use ::serde_json::Value as Json;
use ::sha2::{Digest, Sha256};

use super::execution::GraphqlRequest;
use super::PlaneContext;
use crate::ops::evaluate_code::appsync_js::{ExtensionCall, EVICT_FROM_API_CACHE};
use crate::ops::types::CachingConfig;

/// The `ApiCachingBehavior` values, as the model spells them
/// (`smithy_query(query="shape", shape="ApiCachingBehavior")`).
const FULL_REQUEST_CACHING: &str = "FULL_REQUEST_CACHING";
const OPERATION_LEVEL_CACHING: &str = "OPERATION_LEVEL_CACHING";
const PER_RESOLVER_CACHING: &str = "PER_RESOLVER_CACHING";

/// The root every caching key is rooted in, and the maps of it a key may name an entry of:
/// the three `CachingConfig$cachingKeys` documents, plus `$context.args` — VTL's short name
/// for the arguments map, which this crate already accepts on the write path
/// ([`crate::state::CACHING_KEY_ROOTS`]) and therefore resolves to the same map here.
const CONTEXT_ROOT: &str = "context";
const ARGUMENTS_MAP: &str = "arguments";
const ARGUMENTS_SHORT_MAP: &str = "args";
const SOURCE_MAP: &str = "source";
const IDENTITY_MAP: &str = "identity";

/// How deep a value is rendered when a key is composed from it. An emulator resource guard:
/// the value comes from the request, so the rendering is bounded in host code; past it the
/// subtree is rendered by the JSON writer in one go.
const MAX_KEY_DEPTH: usize = 32;

/// The cache an API SERVES this request through: the behavior in force and the window an
/// entry it writes is served for.
#[derive(Debug, Clone)]
pub struct Serving {
    /// "Caching behavior" (`ApiCache$apiCachingBehavior`), which says what one entry is.
    behavior: String,
    /// "TTL in seconds for cache entries" (`ApiCache$ttl`), as the window itself.
    ttl: Duration,
}

/// The entry ONE resolver's result is held as: the keys it is composed from and the window
/// it is held for.
#[derive(Debug, Clone)]
pub struct ResolverEntry {
    /// The caching keys the entry is composed from, or `None` for a resolver cached without
    /// declaring any — where the whole of the context a cache may key on composes it.
    keys: Option<Vec<String>>,
    /// The window this entry is served for: the resolver's own
    /// `CachingConfig$ttl` ("the TTL in seconds for a resolver that has caching activated")
    /// where it declares one, else the cache's own.
    ttl: Duration,
}

impl Serving {
    /// Whether this behavior holds WHOLE RESPONSES: the two the model documents as caching
    /// full requests.
    pub fn caches_responses(&self) -> bool {
        self.behavior == FULL_REQUEST_CACHING || self.behavior == OPERATION_LEVEL_CACHING
    }

    /// The entry one resolver's result is held as under this behavior, or `None` where the
    /// behavior holds none for it — a resolver that declares no `cachingConfig` under
    /// `PER_RESOLVER_CACHING` ("individual resolvers that you specify are cached"), and
    /// every resolver under `OPERATION_LEVEL_CACHING`, which caches full requests only.
    pub fn resolver_entry(&self, caching: Option<&CachingConfig>) -> Option<ResolverEntry> {
        match self.behavior.as_str() {
            PER_RESOLVER_CACHING => {
                let caching = caching?;
                Some(ResolverEntry {
                    // Exactly the keys the resolver declares: the empty list declares none,
                    // which keys the entry on the resolver alone.
                    keys: Some(caching.caching_keys.clone().unwrap_or_default()),
                    ttl: seconds(caching.ttl),
                })
            }
            // "Individual resolvers are automatically cached": a resolver needs to declare
            // nothing, and one that declares a configuration anyway is cached by it.
            FULL_REQUEST_CACHING => Some(ResolverEntry {
                keys: caching.and_then(|caching| caching.caching_keys.clone()),
                ttl: caching.map_or(self.ttl, |caching| seconds(caching.ttl)),
            }),
            _ => None,
        }
    }
}

/// The cache `context`'s API serves requests through at `now`, or `None` where it serves
/// none — an API that holds no cache, one whose instance is not available yet, and one whose
/// instance is on its way out.
///
/// The transition the clock owes the instance is materialised as it is read
/// ([`crate::state::GraphqlApiRecord::serving_api_cache`]): it was owed at this instant
/// whether or not anybody read the cache, so a request arriving after the provisioning span
/// sees the same instance a reader of the control plane does.
pub fn serving(context: &PlaneContext, now: SystemTime) -> Option<Serving> {
    let mut store = context.state.lock();
    let cache = store
        .graphql_apis
        .get_mut(&context.api_id)?
        .serving_api_cache(now)?;
    Some(Serving {
        behavior: cache.api_caching_behavior.clone(),
        ttl: seconds(cache.ttl),
    })
}

/// The key the WHOLE RESPONSE of `request` is held under, or `None` where this behavior
/// holds no whole response.
///
/// What composes it is what the behavior says the unit is. Under `FULL_REQUEST_CACHING` the
/// REQUEST is the unit and the entry is one "from the same user", so the document, the
/// operation it named, its variables and the caller it presented itself as all compose the
/// key. Under `OPERATION_LEVEL_CACHING` the OPERATION is the unit, so the operation the
/// request named, the document it ran and the values its variables took compose it: two
/// requests that differ only by the operation they name address different entries, and one
/// that passes a new variable value misses.
pub fn response_key(serving: &Serving, request: &GraphqlRequest) -> Option<String> {
    if !serving.caches_responses() {
        return None;
    }
    let operation = request.operation_name.clone().unwrap_or_default();
    let mut pairs = vec![
        ("document".to_string(), Json::String(request.query.clone())),
        ("operation".to_string(), Json::String(operation)),
        (
            "variables".to_string(),
            Json::Object(request.variables.clone()),
        ),
    ];
    if serving.behavior == FULL_REQUEST_CACHING {
        pairs.push(("user".to_string(), request.identity.clone()));
    }
    Some(composed("response", &serving.behavior, "", "", &pairs))
}

/// The key ONE resolver's result is held under, for the field `field_name` of `type_name`
/// resolved over this context.
///
/// Composed from exactly the caching keys the entry declares, each resolved against the
/// context map it names — so an argument the resolver did not declare a key for does not
/// take part, and a request that changes only such an argument is answered from the entry
/// already held. EVERY declared key takes part, including one that names no entry of a
/// context map: the write path admits any key rooted in one
/// ([`crate::state::caching_config_refusal`], which tests the root prefix and nothing more),
/// and such a key selects nothing, so it takes part under its own text with the empty value
/// every request presents for it — the keys a resolver declared can then never collapse to
/// none.
pub fn resolver_key(
    serving: &Serving,
    entry: &ResolverEntry,
    type_name: &str,
    field_name: &str,
    arguments: &Json,
    source: &Json,
    identity: &Json,
) -> String {
    let pairs = match &entry.keys {
        Some(declared) => declared
            .iter()
            .map(|key| match key_path(key) {
                Some(path) => {
                    let value = selected(&path, arguments, source, identity);
                    (path.join("."), value)
                }
                None => (key.trim().to_string(), Json::Null),
            })
            .collect::<Vec<(String, Json)>>(),
        // A resolver cached without declaring keys is cached over the WHOLE of the context
        // a cache may key on, which is the three maps `cachingKeys` draws from.
        None => vec![
            (
                format!("{CONTEXT_ROOT}.{ARGUMENTS_MAP}"),
                arguments.clone(),
            ),
            (format!("{CONTEXT_ROOT}.{SOURCE_MAP}"), source.clone()),
            (format!("{CONTEXT_ROOT}.{IDENTITY_MAP}"), identity.clone()),
        ],
    };
    composed(
        "resolver",
        &serving.behavior,
        type_name,
        field_name,
        &pairs,
    )
}

/// The window an entry under `entry` is served for.
pub fn entry_ttl(entry: &ResolverEntry) -> Duration {
    entry.ttl
}

/// The window a whole response is served for: the cache's own `ttl`.
pub fn response_ttl(serving: &Serving) -> Duration {
    serving.ttl
}

/// The value `context`'s API cache holds under `key` at `now`, or `None` where it holds
/// none — which is a MISS, and what makes the caller perform the resolution.
pub fn served(context: &PlaneContext, key: &str, now: SystemTime) -> Option<Json> {
    let mut store = context.state.lock();
    store
        .graphql_apis
        .get_mut(&context.api_id)?
        .serving_api_cache(now)?
        .entries
        .held(key, now)
}

/// Hold `value` under `key` for `ttl` from `now`, where the API still serves a cache.
pub fn hold(context: &PlaneContext, key: String, value: Json, ttl: Duration, now: SystemTime) {
    let mut store = context.state.lock();
    let Some(cache) = store
        .graphql_apis
        .get_mut(&context.api_id)
        .and_then(|api| api.serving_api_cache(now))
    else {
        return;
    };
    cache.entries.hold(key, value, ttl, now);
}

/// ONE eviction a run asked for: the field the entry belongs to, and the pair it was keyed
/// on, normalised ([`key_path`]).
#[derive(Debug, Clone)]
struct Eviction {
    type_name: String,
    field_name: String,
    pairs: Vec<(String, Json)>,
}

/// The eviction one `extensions.evictFromApiCache(typeName, fieldName, keyValuePair)` call
/// asks for, or `None` for a call that asks for none: one that did not name a type and a
/// field, and one whose pair names a key AppSync could not have keyed an entry on
/// ([`refused_evictions`] reports that one), which removes nothing rather than removing the
/// entry of the keys it did name.
fn eviction(call: &ExtensionCall) -> Option<Eviction> {
    let argument = call.argument();
    let named = argument.as_array()?;
    let type_name = named.first()?.as_str()?.to_string();
    let field_name = named.get(1)?.as_str()?.to_string();
    let mut pairs = Vec::new();
    for (key, value) in named.get(2).and_then(Json::as_object).into_iter().flatten() {
        pairs.push((key_path(key)?.join("."), value.clone()));
    }
    Some(Eviction {
        type_name,
        field_name,
        pairs,
    })
}

/// The caching keys the evictions among `calls` named that name no entry of a context map at
/// all, each as the sentence the call is refused with ([`invalid_caching_key_message`]), in
/// the order the calls stated them.
///
/// A caching key is not free text on either side of the cache: "valid values are entries
/// from the `$context.arguments`, `$context.source`, and `$context.identity` maps"
/// (`CachingConfig$cachingKeys`) is the rule the write path enforces over a resolver's
/// declared keys ([`crate::state::caching_config_refusal`]), and an eviction names those same
/// keys — so one rooted anywhere else names nothing an entry could have been keyed on and the
/// call is reported as naming an invalid key rather than silently removing another entry.
pub fn refused_evictions(calls: &[ExtensionCall]) -> Vec<String> {
    calls
        .iter()
        .filter(|call| call.name == EVICT_FROM_API_CACHE)
        .flat_map(|call| {
            let argument = call.argument();
            let named = argument
                .as_array()
                .and_then(|named| named.get(2))
                .and_then(Json::as_object)
                .map(|pair| pair.keys().cloned().collect::<Vec<String>>())
                .unwrap_or_default();
            named
        })
        .filter(|key| key_path(key).is_none())
        .map(|key| invalid_caching_key_message(&key))
        .collect()
}

/// The sentence an eviction naming `key` is refused with: AppSync's own, which names the key
/// the call stated and nothing else of the request.
pub fn invalid_caching_key_message(key: &str) -> String {
    format!("{INVALID_CACHING_KEY} {key}")
}

/// The fixed half of that sentence.
const INVALID_CACHING_KEY: &str = "Invalid caching key";

/// Remove the entries this request's runs asked to be removed:
/// `extensions.evictFromApiCache(typeName, fieldName, keyValuePair)`, which "evicts an item
/// from the AppSync server-side cache".
///
/// ONE entry per call: the one the named type, the named field and the key-value pair
/// compose, which is the same key the resolution of that field wrote
/// ([`resolver_key`]) — so the sibling entries of other key values are left exactly as they
/// were. A call naming a pair no entry was keyed on removes nothing.
///
/// The pair's own names are the caching keys WITHOUT the `$` a resolver declares them with
/// (AppSync's own example evicts `{'context.arguments.id': …}` against a resolver declaring
/// `$context.arguments.id`), which is why both sides are normalised the same way
/// ([`key_path`]).
///
/// `None` where the runs asked for no eviction at all; else HOW MANY entries the evictions
/// removed, which is what the response reports back about them
/// ([`entries_deleted`]) — and the only thing a caller can observe of an eviction, since the
/// entries themselves are never projected.
pub fn evicted(context: &PlaneContext, calls: &[ExtensionCall], now: SystemTime) -> usize {
    let asked: Vec<Eviction> = calls
        .iter()
        .filter(|call| call.name == EVICT_FROM_API_CACHE)
        .filter_map(eviction)
        .collect();
    if asked.is_empty() {
        return 0;
    }
    let mut store = context.state.lock();
    let Some(cache) = store
        .graphql_apis
        .get_mut(&context.api_id)
        .and_then(|api| api.serving_api_cache(now))
    else {
        return 0;
    };
    let behavior = cache.api_caching_behavior.clone();
    let mut deleted = 0usize;
    for eviction in asked {
        let key = composed(
            "resolver",
            &behavior,
            &eviction.type_name,
            &eviction.field_name,
            &eviction.pairs,
        );
        let removed = cache.entries.discard(&key);
        deleted = deleted.saturating_add(usize::from(removed));
        ::tracing::debug!(
            type_name = %eviction.type_name,
            field_name = %eviction.field_name,
            removed,
            "a resolver asked for an entry of this API's cache to be evicted"
        );
    }
    deleted
}

/// What a response reports about the evictions it performed: `apiCacheEntriesDeleted`, the
/// number of entries the `extensions.evictFromApiCache` calls of the request REMOVED, which
/// AppSync answers under the response's own `extensions`.
///
/// Reported only where an eviction removed something ([`reported_eviction`]): a call that
/// named a field, a key or a value no entry was held under deleted nothing, and a response
/// that deleted nothing carries no such member at all.
pub fn entries_deleted(deleted: usize) -> Json {
    ::serde_json::json!({ENTRIES_DELETED_MEMBER: deleted})
}

/// The `extensions` a response carries about `deleted` entries, or `None` where it carries
/// none.
pub fn reported_eviction(deleted: usize) -> Option<Json> {
    (deleted > 0).then(|| entries_deleted(deleted))
}

/// The member that count is reported under.
const ENTRIES_DELETED_MEMBER: &str = "apiCacheEntriesDeleted";

/// The ONE key composition every entry of a cache goes under: the kind of unit, the
/// behavior that wrote it, the field it belongs to where it belongs to one, and the pairs
/// that key it, digested.
///
/// Digested, so a key is bounded whatever the request carried, and the pairs are SORTED by
/// name and rendered canonically ([`canonical`]), so two requests that present the same keys
/// in a different order address one entry. The behavior takes part because it says what the
/// entry IS: an instance whose behavior has since been updated serves none of the entries
/// written under the old one.
fn composed(
    kind: &str,
    behavior: &str,
    type_name: &str,
    field_name: &str,
    pairs: &[(String, Json)],
) -> String {
    let mut sorted: Vec<&(String, Json)> = pairs.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut text = format!("{kind}\u{1f}{behavior}\u{1f}{type_name}\u{1f}{field_name}");
    for (name, value) in sorted {
        text.push('\u{1f}');
        text.push_str(name);
        text.push('\u{1e}');
        text.push_str(&canonical(value, 0));
    }
    format!("{kind}:{}", ::hex::encode(Sha256::digest(text.as_bytes())))
}

/// `value` rendered so that two equal values render alike: an object by its members in NAME
/// order, a list in its own order, and everything else as JSON writes it.
///
/// Needed because the values a key is composed from come from a request body, whose member
/// order is the client's choice while the value it states is not.
fn canonical(value: &Json, depth: usize) -> String {
    if depth >= MAX_KEY_DEPTH {
        return ::serde_json::to_string(value).unwrap_or_default();
    }
    match value {
        Json::Object(members) => {
            let mut sorted: Vec<(&String, &Json)> = members.iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(right.0));
            let rendered: Vec<String> = sorted
                .into_iter()
                .map(|(name, member)| {
                    format!(
                        "{}:{}",
                        Json::String(name.clone()),
                        canonical(member, depth.saturating_add(1))
                    )
                })
                .collect();
            format!("{{{}}}", rendered.join(","))
        }
        Json::Array(elements) => {
            let rendered: Vec<String> = elements
                .iter()
                .map(|element| canonical(element, depth.saturating_add(1)))
                .collect();
            format!("[{}]", rendered.join(","))
        }
        other => other.to_string(),
    }
}

/// The path one caching key names, as the segments that select the value, or `None` for a
/// key that names no entry of a context map at all.
///
/// The one reading of a caching key, used for the keys a resolver DECLARES (written with the
/// `$` of a VTL reference, and possibly indexing a map rather than selecting a member of it)
/// and for the names an eviction's key-value pair carries (written without it). Both are
/// normalised to the same segments, so the two sides of one key agree: the `$` is dropped,
/// the two index forms are read as a member each, and `args` — the short name for the
/// arguments map — is read as the arguments map itself.
fn key_path(key: &str) -> Option<Vec<String>> {
    let flattened = key
        .trim()
        .trim_start_matches('$')
        .replace(['[', ']'], ".")
        .replace(['"', '\''], "");
    let mut segments = flattened
        .split('.')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<String>>();
    let [root, map, ..] = segments.as_slice() else {
        return None;
    };
    if root != CONTEXT_ROOT {
        return None;
    }
    let map = match map.as_str() {
        ARGUMENTS_SHORT_MAP => ARGUMENTS_MAP,
        ARGUMENTS_MAP | SOURCE_MAP | IDENTITY_MAP => map,
        _ => return None,
    }
    .to_string();
    segments[1] = map;
    Some(segments)
}

/// The value `path` selects out of the context a cache may key on, or `Json::Null` where it
/// selects nothing — a member no value carries is not a key value, and keys the resolution
/// cannot fill are what two requests that present neither have in common.
fn selected(path: &[String], arguments: &Json, source: &Json, identity: &Json) -> Json {
    let [_, map, members @ ..] = path else {
        return Json::Null;
    };
    let mut value = match map.as_str() {
        ARGUMENTS_MAP => arguments,
        SOURCE_MAP => source,
        IDENTITY_MAP => identity,
        _ => return Json::Null,
    }
    .clone();
    for member in members {
        value = match &value {
            Json::Object(held) => held.get(member).cloned().unwrap_or(Json::Null),
            Json::Array(elements) => member
                .parse::<usize>()
                .ok()
                .and_then(|index| elements.get(index).cloned())
                .unwrap_or(Json::Null),
            _ => Json::Null,
        };
    }
    value
}

/// `seconds` as a window. The member is a `Long` the service's own window check bounds to
/// 1–3,600 ([`crate::state::is_cache_ttl`]), so a negative value reaches no stored record.
fn seconds(seconds: i64) -> Duration {
    Duration::from_secs(u64::try_from(seconds).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::serde_json::json;

    /// A serving cache of `behavior`, at a one-hour window.
    fn serving_cache(behavior: &str) -> Serving {
        Serving {
            behavior: behavior.to_string(),
            ttl: Duration::from_secs(3_600),
        }
    }

    /// A caching configuration over `keys`.
    fn caching(keys: &[&str]) -> CachingConfig {
        CachingConfig {
            ttl: 60,
            caching_keys: Some(keys.iter().map(|key| (*key).to_string()).collect()),
        }
    }

    /// Which unit each behavior holds, as the model documents them.
    #[test]
    fn each_behavior_holds_the_unit_the_model_documents_for_it() {
        // One row per value of `ApiCachingBehavior`, against a resolver that declares a
        // caching configuration and one that declares none.
        let rows: [(&str, bool, bool, bool); 3] = [
            (FULL_REQUEST_CACHING, true, true, true),
            (OPERATION_LEVEL_CACHING, true, false, false),
            (PER_RESOLVER_CACHING, false, true, false),
        ];
        for (behavior, responses, declared, undeclared) in rows {
            let serving = serving_cache(behavior);
            assert_eq!(
                serving.caches_responses(),
                responses,
                "case {behavior:?}: expected whole responses to be held to be {responses}"
            );
            let config = caching(&["$context.arguments.id"]);
            assert_eq!(
                serving.resolver_entry(Some(&config)).is_some(),
                declared,
                "case {behavior:?}: expected a declared resolver to be held to be {declared}"
            );
            assert_eq!(
                serving.resolver_entry(None).is_some(),
                undeclared,
                "case {behavior:?}: expected an undeclared resolver to be held to be \
                 {undeclared}"
            );
        }
    }

    /// The resolver key is composed of exactly the declared caching keys: a value one of
    /// them selects changes it, and anything else the context carries does not.
    #[test]
    fn a_resolver_key_is_composed_of_exactly_the_declared_caching_keys() {
        let serving = serving_cache(PER_RESOLVER_CACHING);
        let config = caching(&["$context.arguments.id"]);
        let entry = serving
            .resolver_entry(Some(&config))
            .expect("a declared resolver is cached under this behavior");
        let key = |arguments: Json, source: Json, identity: Json| {
            resolver_key(
                &serving,
                &entry,
                "Query",
                "counted",
                &arguments,
                &source,
                &identity,
            )
        };
        let first = key(json!({"id": "a", "noise": "one"}), Json::Null, Json::Null);
        // The rows differ along which part of the context changes.
        let rows: [(&str, Json, Json, Json, bool); 5] = [
            (
                "the same declared value",
                json!({"id": "a", "noise": "one"}),
                Json::Null,
                Json::Null,
                true,
            ),
            (
                "an argument that is not a declared key",
                json!({"id": "a", "noise": "two"}),
                Json::Null,
                Json::Null,
                true,
            ),
            (
                "the arguments in another order",
                json!({"noise": "one", "id": "a"}),
                Json::Null,
                Json::Null,
                true,
            ),
            (
                "the declared key's own value",
                json!({"id": "b", "noise": "one"}),
                Json::Null,
                Json::Null,
                false,
            ),
            (
                "a source and an identity nothing declares",
                json!({"id": "a", "noise": "one"}),
                json!({"owner": "someone"}),
                json!({"sub": "else"}),
                true,
            ),
        ];
        for (case, arguments, source, identity, shared) in rows {
            let other = key(arguments, source, identity);
            assert_eq!(
                other == first,
                shared,
                "case {case:?}: expected the key to be shared with the first to be {shared}"
            );
        }
        // And the field the entry belongs to takes part in it.
        let sibling = resolver_key(
            &serving,
            &entry,
            "Query",
            "uncounted",
            &json!({"id": "a"}),
            &Json::Null,
            &Json::Null,
        );
        assert_ne!(
            sibling, first,
            "case another field of the same type: expected a key of its own"
        );
    }

    /// A declared key the write path admits but that names no entry of a context map — the
    /// write path tests the root PREFIX and nothing more — still takes part in the
    /// composition, so one resolver's keys never collapse to the key of a resolver that
    /// declared none.
    #[test]
    fn a_declared_key_that_selects_nothing_still_takes_part_in_the_key() {
        let serving = serving_cache(PER_RESOLVER_CACHING);
        let keyed = |declared: &[&str]| {
            let config = caching(declared);
            let entry = serving
                .resolver_entry(Some(&config))
                .expect("a declared resolver is cached under this behavior");
            resolver_key(
                &serving,
                &entry,
                "Query",
                "counted",
                &json!({"id": "a"}),
                &Json::Null,
                &Json::Null,
            )
        };
        // The rows differ along what the resolver declared: a key the write path admits whose
        // root merely PREFIXES a context map, two such keys that differ, the readable key
        // beside one of them, and the resolver that declares no key at all.
        let prefix_only = keyed(&["$context.argumentsOfMine"]);
        let rows: [(&str, Vec<&str>); 3] = [
            ("another unreadable key", vec!["$context.sourceOfMine"]),
            (
                "a readable key beside it",
                vec!["$context.arguments.id", "$context.argumentsOfMine"],
            ),
            ("no declared key at all", vec![]),
        ];
        for (case, declared) in rows {
            let other = keyed(&declared);
            assert_ne!(
                other, prefix_only,
                "case {case:?}: expected a key of its own rather than the unreadable key's"
            );
        }
        // And the unreadable key composes the SAME key for two requests, since it selects
        // nothing from either: it is the readable keys that separate them.
        let config = caching(&["$context.argumentsOfMine"]);
        let entry = serving
            .resolver_entry(Some(&config))
            .expect("a declared resolver is cached under this behavior");
        let other_request = resolver_key(
            &serving,
            &entry,
            "Query",
            "counted",
            &json!({"id": "b"}),
            &Json::Null,
            &Json::Null,
        );
        assert_eq!(
            other_request, prefix_only,
            "case another request under an unreadable key: expected the one entry it keys"
        );
    }

    /// An eviction's key-value pair composes the SAME key the resolution wrote, whichever
    /// way each side spells the caching key.
    #[test]
    fn an_eviction_composes_the_key_the_resolution_wrote() {
        let serving = serving_cache(PER_RESOLVER_CACHING);
        // The rows differ along how the resolver declared the key and how the eviction
        // names it: the `$` prefix, the short name of the arguments map, and the index form.
        let rows: [(&str, &str, &str); 4] = [
            (
                "the documented spelling on both sides",
                "$context.arguments.id",
                "context.arguments.id",
            ),
            (
                "an eviction that keeps the dollar",
                "$context.arguments.id",
                "$context.arguments.id",
            ),
            (
                "a resolver declaring the short arguments map",
                "$context.args.id",
                "context.arguments.id",
            ),
            (
                "a resolver declaring the index form",
                "$context.arguments[\"id\"]",
                "context.arguments.id",
            ),
        ];
        for (case, declared, named) in rows {
            let config = caching(&[declared]);
            let entry = serving
                .resolver_entry(Some(&config))
                .expect("a declared resolver is cached under this behavior");
            let written = resolver_key(
                &serving,
                &entry,
                "Query",
                "counted",
                &json!({"id": "a"}),
                &Json::Null,
                &Json::Null,
            );
            let evicted = composed(
                "resolver",
                PER_RESOLVER_CACHING,
                "Query",
                "counted",
                &[(
                    key_path(named).expect("the pair names a context map").join("."),
                    json!("a"),
                )],
            );
            assert_eq!(
                written, evicted,
                "case {case:?}: expected the eviction to compose the key the resolution wrote"
            );
        }
    }

    /// The whole-response key is the request's: the document, the operation it names and
    /// its variable values all take part.
    #[test]
    fn a_response_key_separates_operations_and_variable_values() {
        for behavior in [FULL_REQUEST_CACHING, OPERATION_LEVEL_CACHING] {
            let serving = serving_cache(behavior);
            let keyed = |query: &str, operation: Option<&str>, variables: Json| {
                let request = GraphqlRequest {
                    query: query.to_string(),
                    operation_name: operation.map(str::to_string),
                    variables: variables.as_object().cloned().unwrap_or_default(),
                    ..GraphqlRequest::default()
                };
                response_key(&serving, &request).expect("this behavior holds whole responses")
            };
            let first = keyed("query Alpha { plain(id: \"k\") }", Some("Alpha"), json!({}));
            // The rows differ along what the request changed.
            let rows: [(&str, &str, Option<&str>, Json, bool); 4] = [
                (
                    "the identical request",
                    "query Alpha { plain(id: \"k\") }",
                    Some("Alpha"),
                    json!({}),
                    true,
                ),
                (
                    "a document differing only by operation name",
                    "query Beta { plain(id: \"k\") }",
                    Some("Beta"),
                    json!({}),
                    false,
                ),
                (
                    "the same document, another operation named",
                    "query Alpha { plain(id: \"k\") }",
                    Some("Beta"),
                    json!({}),
                    false,
                ),
                (
                    "the same operation, a variable value",
                    "query Alpha { plain(id: \"k\") }",
                    Some("Alpha"),
                    json!({"i": "k2"}),
                    false,
                ),
            ];
            for (case, query, operation, variables, shared) in rows {
                let other = keyed(query, operation, variables);
                assert_eq!(
                    other == first,
                    shared,
                    "case {behavior} / {case:?}: expected the key to be shared to be {shared}"
                );
            }
        }
        // And a behavior that holds no whole response composes no such key.
        let request = GraphqlRequest::default();
        let none = response_key(&serving_cache(PER_RESOLVER_CACHING), &request);
        assert!(
            none.is_none(),
            "case PER_RESOLVER_CACHING: expected no whole-response key, got {none:?}"
        );
    }

    /// Every root the WRITE path admits a caching key under
    /// ([`crate::state::CACHING_KEY_ROOTS`], the set `CreateResolver` and `UpdateResolver`
    /// enforce) is a root this module reads a key under, so a key a resolver may be stored
    /// with is never one the cache cannot key by.
    #[test]
    fn every_root_the_write_path_admits_is_one_this_module_reads() {
        for root in crate::state::CACHING_KEY_ROOTS {
            let key = format!("{root}.member");
            let path = key_path(&key);
            assert!(
                path.is_some(),
                "case the root {root:?}: expected the read path to resolve a key under it, \
                 got {path:?}"
            );
        }
    }

    /// A key rooted outside the context maps names nothing a cache is keyed by, so an
    /// eviction naming one asks for no removal and is refused with the key it named.
    #[test]
    fn a_key_outside_the_context_maps_names_no_cache_key() {
        for key in [
            "$context.result.id",
            "$util.time",
            "id",
            "",
            "$context",
            "$ctx.args.id",
        ] {
            let path = key_path(key);
            assert!(
                path.is_none(),
                "case {key:?}: expected no caching path, got {path:?}"
            );
            let call = ExtensionCall {
                name: EVICT_FROM_API_CACHE.to_string(),
                argument: ::serde_json::to_string(&json!(["Query", "counted", {key: "a"}]))
                    .expect("the call renders"),
            };
            let refused = refused_evictions(::std::slice::from_ref(&call));
            assert_eq!(
                refused,
                vec![format!("{INVALID_CACHING_KEY} {key}")],
                "case {key:?}: expected the eviction to be refused naming the key"
            );
            assert!(
                eviction(&call).is_none(),
                "case {key:?}: expected a refused eviction to remove nothing"
            );
        }
        // And one whose pair names a context map is asked for and refused with nothing.
        let call = ExtensionCall {
            name: EVICT_FROM_API_CACHE.to_string(),
            argument: ::serde_json::to_string(&json!([
                "Query",
                "counted",
                {"context.arguments.id": "a"}
            ]))
            .expect("the call renders"),
        };
        let refused = refused_evictions(::std::slice::from_ref(&call));
        assert!(
            refused.is_empty() && eviction(&call).is_some(),
            "case a pair naming the arguments map: expected one eviction and no refusal, got \
             {refused:?}"
        );
    }

    /// What a declared key selects out of the context, over each map and each form.
    #[test]
    fn a_declared_key_selects_the_value_of_the_map_it_names() {
        let arguments = json!({"id": "a", "nested": {"deep": 7}, "list": ["x", "y"]});
        let source = json!({"owner": "someone"});
        let identity = json!({"sub": "else"});
        // The rows span the maps and the selection forms.
        let rows: [(&str, &str, Json); 7] = [
            ("an argument", "$context.arguments.id", json!("a")),
            ("the short arguments map", "$context.args.id", json!("a")),
            (
                "a nested member",
                "$context.arguments.nested.deep",
                json!(7),
            ),
            (
                "an element of a list",
                "$context.arguments.list[1]",
                json!("y"),
            ),
            ("the source map", "$context.source.owner", json!("someone")),
            ("the identity map", "$context.identity.sub", json!("else")),
            (
                "a member nothing carries",
                "$context.arguments.missing",
                Json::Null,
            ),
        ];
        for (case, key, expected) in rows {
            let path = key_path(key).expect("the key names a context map");
            let value = selected(&path, &arguments, &source, &identity);
            assert_eq!(
                value, expected,
                "case {case:?}: expected {expected}, got {value}"
            );
        }
    }
}
