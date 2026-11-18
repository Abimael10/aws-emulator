// SPDX-License-Identifier: CC0-1.0

//! Which of a viewer's COOKIES separate one cached object from another at the edge — end to
//! end over HTTP, with Amazon S3 mounted beside CloudFront on one engine so the origin is a
//! real object store and the bytes under test are ones an origin really answered with.
//!
//! RULE: a cache behaviour's cache policy decides which cookies of a viewer request take part
//! in the cache key, through `CachePolicyCookiesConfig$CookieBehavior` read over the
//! `CookieNames` list beside it — `none` keys on no cookie, `whitelist` on exactly the listed
//! ones, `allExcept` on every cookie but the listed ones, and `all` on every cookie the viewer
//! sent. So two requests for the same object that differ only in a cookie the behaviour keys
//! on are two cache entries, each fetched from the origin and then held on its own, while two
//! that differ only in a cookie it does not key on are ONE entry, and the second of them is
//! answered out of that entry's stored bytes however the origin has changed since.
//!
//! The rows span what the rule generalises over: every value of the behaviour enum
//! (`smithy_query(query="shape",shape="CachePolicyCookieBehavior")` — `none`, `whitelist`,
//! `allExcept`, `all`), and within each, the ways two requests can differ — the value of a
//! cookie the behaviour names, the presence of a cookie it does not name, the ORDER the viewer
//! sent its cookies in, and a request carrying no cookie at all. The second suite then reads
//! the SEPARATION itself: the origin object is rewritten between two views, so an entry that
//! was keyed by one cookie value must still answer with the bytes it holds while the other
//! value fetches the new ones.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_origin_response_headers::{
    create_distribution_over_bucket, create_origin_access_control, view, Emulator, Staged, HIT,
    MISS,
};

/// The object key every case fetches.
const KEY: &str = "cookie.txt";

/// Every TTL of the cache policies below, in seconds: an hour, chosen so that nothing a case
/// put in the edge cache can expire while that case is still running.
const TTL: u64 = 3600;

/// The object the first suite reads: it states nothing but its bytes, because what that suite
/// asserts is WHICH requests share an entry rather than what an entry holds.
const OBJECT: Staged = Staged::plain(KEY, "the origin's own bytes");

/// One row of the table: a cookie behaviour, the names its configuration lists, and the
/// successive viewer requests the rule is asserted over.
struct Row {
    case: &'static str,
    /// A value of `CachePolicyCookiesConfig$CookieBehavior`.
    behavior: &'static str,
    /// `CookieNames$Items`, empty where the behaviour names no list.
    names: &'static [&'static str],
    /// Each successive request: the `Cookie` header the viewer sends (`None` for a request
    /// carrying none at all) and the `X-Cache` the answer must carry.
    legs: &'static [(Option<&'static str>, &'static str)],
}

/// The table: one row per value of the cookie behaviour, each with the successive requests
/// that say which of them share a cache entry.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "whitelist, naming one cookie",
            behavior: "whitelist",
            names: &["sid"],
            legs: &[
                // The listed cookie's value separates the entries...
                (Some("sid=alpha"), MISS),
                (Some("sid=alpha"), HIT),
                // ... a cookie that is not listed does not, so this is the same entry ...
                (Some("sid=alpha; tracker=zzz"), HIT),
                // ... another value of the listed cookie is another entry of its own ...
                (Some("sid=beta"), MISS),
                (Some("sid=beta"), HIT),
                // ... and the first entry is still there, so this is separation, not eviction.
                (Some("sid=alpha"), HIT),
                // A request that sends the listed cookie is not one that omits it.
                (None, MISS),
                (None, HIT),
            ],
        },
        Row {
            case: "none, naming no cookie",
            behavior: "none",
            names: &[],
            legs: &[
                // No cookie takes part in the key, so every one of these is one entry.
                (Some("sid=alpha"), MISS),
                (Some("sid=beta"), HIT),
                (Some("sid=alpha; tracker=zzz"), HIT),
                (None, HIT),
            ],
        },
        Row {
            case: "allExcept, naming one cookie",
            behavior: "allExcept",
            names: &["tracker"],
            legs: &[
                // Every cookie but the named one keys, so the named one's value does not
                // separate anything ...
                (Some("sid=alpha; tracker=zzz"), MISS),
                (Some("sid=alpha; tracker=yyy"), HIT),
                (Some("sid=alpha"), HIT),
                // ... while every other cookie's value does.
                (Some("sid=beta; tracker=zzz"), MISS),
                (Some("sid=beta"), HIT),
            ],
        },
        Row {
            case: "all, naming every cookie",
            behavior: "all",
            names: &[],
            legs: &[
                (Some("sid=alpha"), MISS),
                (Some("sid=alpha"), HIT),
                // Every cookie keys, so one the other requests did not send separates this
                // one from them ...
                (Some("sid=alpha; tracker=zzz"), MISS),
                // ... and the ORDER the viewer sent them in does not: the edge keys on the
                // cookies it forwards, which it orders by name.
                (Some("tracker=zzz; sid=alpha"), HIT),
                (Some("sid=beta"), MISS),
                (Some("sid=alpha"), HIT),
            ],
        },
    ]
}

/// The `CookiesConfig` element of a cache policy configuration: the behaviour, and the
/// `CookieNames` list where it names one.
fn cookies_config(behavior: &str, names: &[&str]) -> String {
    let mut xml = format!("<CookiesConfig><CookieBehavior>{behavior}</CookieBehavior>");
    if !names.is_empty() {
        xml.push_str(&format!(
            "<Cookies><Quantity>{}</Quantity><Items>",
            names.len()
        ));
        for name in names {
            xml.push_str(&format!("<Name>{name}</Name>"));
        }
        xml.push_str("</Items></Cookies>");
    }
    xml.push_str("</CookiesConfig>");
    xml
}

/// `CreateCachePolicy` for a policy that keys on the cookies `behavior` names over `names` and
/// on nothing else — no header and no query string — answering the policy's id.
async fn create_cache_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    behavior: &str,
    names: &[&str],
) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/cache-policy"),
        Some(&format!(
            "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name><DefaultTTL>{TTL}</DefaultTTL><MaxTTL>{TTL}</MaxTTL>\
             <MinTTL>{TTL}</MinTTL><ParametersInCacheKeyAndForwardedToOrigin>\
             <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
             <EnableAcceptEncodingBrotli>false</EnableAcceptEncodingBrotli>\
             <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
             {}\
             <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
             </QueryStringsConfig>\
             </ParametersInCacheKeyAndForwardedToOrigin></CachePolicyConfig>",
            cookies_config(behavior, names)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateCachePolicy answered {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {case:?}: the created cache policy reports no id"))
        .to_string()
}

/// A deployed distribution over its own origin bucket holding `object`, whose default cache
/// behaviour keys on the cookies `behavior` names: the bucket's name and the distribution's
/// domain name.
///
/// `label` names this case's bucket, origin access control, cache policy and caller
/// reference, so every case stands on its own resources.
async fn served(
    emulator: &Emulator,
    case: &str,
    label: &str,
    object: &Staged,
    behavior: &str,
    names: &[&str],
) -> (String, String) {
    let bucket = format!("edge-cookie-{label}");
    emulator.staged_bucket(case, &bucket).await;
    emulator.staged_object(case, &bucket, object).await;
    let control = create_origin_access_control(&emulator.router, case, label).await;
    let policy = create_cache_policy(&emulator.router, case, label, behavior, names).await;
    let (id, domain, arn) = create_distribution_over_bucket(
        &emulator.router,
        case,
        label,
        &bucket,
        &control,
        &format!("<CachePolicyId>{policy}</CachePolicyId>"),
    )
    .await;
    emulator.granted_to_cloudfront(case, &bucket, &arn).await;
    awaited_deployment(&emulator.router, case, &id).await;
    (bucket, domain)
}

/// One viewer request for the object, carrying `cookie` where the leg sends one: the answer
/// must be a `200` whose `X-Cache` is `x_cache` and whose bytes are `body`.
async fn assert_view(
    emulator: &Emulator,
    domain: &str,
    case: &str,
    cookie: Option<&str>,
    x_cache: &str,
    body: &str,
) {
    let headers: Vec<(&str, &str)> = cookie
        .map(|value| vec![("cookie", value)])
        .unwrap_or_default();
    let viewed = view(&emulator.router, "GET", domain, &format!("/{KEY}"), &headers).await;
    assert_eq!(
        (viewed.status, viewed.x_cache(), viewed.body.as_str()),
        (200, x_cache, body),
        "case {case:?} (cookie {cookie:?}): expected 200 with X-Cache {x_cache:?} and body \
         {body:?}, got {} with {:?} and {:?}",
        viewed.status,
        viewed.x_cache(),
        viewed.body
    );
}

/// Which requests share a cache entry is decided by the cookie behaviour the cache policy
/// states: over every value of that behaviour, the successive requests of a row are answered
/// from the origin exactly when the behaviour says they are different objects.
#[tokio::test]
async fn the_cache_key_separates_requests_by_the_cookies_the_policy_keys_on() {
    let emulator = Emulator::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let label = format!("key-{index}");
        let (_bucket, domain) =
            served(&emulator, case, &label, &OBJECT, row.behavior, row.names).await;
        for (leg, (cookie, expected)) in row.legs.iter().enumerate() {
            let leg_case = format!("{case} / request {}", leg + 1);
            assert_view(&emulator, &domain, &leg_case, *cookie, expected, OBJECT.body).await;
        }
    }
}

/// The separation an allowlisted cookie makes is between ENTRIES that each hold their own
/// bytes: with the origin object rewritten between the views, a request differing only in a
/// cookie the policy does not key on is still answered out of the entry the keyed cookie made
/// — the bytes the origin held when that entry was filled — while a different value of the
/// keyed cookie fetches the origin's current bytes and becomes an entry of its own, leaving
/// the first one intact.
#[tokio::test]
async fn a_keyed_cookies_entries_each_hold_the_bytes_they_were_filled_with() {
    let case = "a cache policy that keys on one cookie";
    /// What the origin held when the first entry was filled.
    const BEFORE: Staged = Staged::plain(KEY, "the bytes the origin held first");
    /// What the origin holds after it is rewritten.
    const AFTER: Staged = Staged::plain(KEY, "the bytes the origin holds now");

    let emulator = Emulator::new();
    let (bucket, domain) =
        served(&emulator, case, "separation", &BEFORE, "whitelist", &["sid"]).await;

    // One value of the keyed cookie fills an entry from the origin, and the next request for
    // it is answered out of that entry.
    assert_view(
        &emulator,
        &domain,
        "the cold request for one value of the keyed cookie",
        Some("sid=alpha"),
        MISS,
        BEFORE.body,
    )
    .await;
    assert_view(
        &emulator,
        &domain,
        "the same value again",
        Some("sid=alpha"),
        HIT,
        BEFORE.body,
    )
    .await;

    // The origin object is rewritten. Nothing about the entries changes: the edge holds each
    // of them for the TTL the policy states.
    emulator.staged_object(case, &bucket, &AFTER).await;

    // A cookie the policy does not key on is the SAME object, so this is answered out of the
    // entry that was filled before the rewrite.
    assert_view(
        &emulator,
        &domain,
        "a cookie the policy does not key on, after the rewrite",
        Some("sid=alpha; tracker=zzz"),
        HIT,
        BEFORE.body,
    )
    .await;

    // Another value of the keyed cookie is another object, so it goes to the origin and sees
    // what the origin holds NOW, then becomes an entry of its own.
    assert_view(
        &emulator,
        &domain,
        "another value of the keyed cookie, after the rewrite",
        Some("sid=beta"),
        MISS,
        AFTER.body,
    )
    .await;
    assert_view(
        &emulator,
        &domain,
        "that other value again",
        Some("sid=beta"),
        HIT,
        AFTER.body,
    )
    .await;

    // And the first value's entry is still intact, still holding what it was filled with.
    assert_view(
        &emulator,
        &domain,
        "the first value once more",
        Some("sid=alpha"),
        HIT,
        BEFORE.body,
    )
    .await;
}
