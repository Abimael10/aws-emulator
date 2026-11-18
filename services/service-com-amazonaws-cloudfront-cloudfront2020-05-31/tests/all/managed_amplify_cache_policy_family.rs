// SPDX-License-Identifier: CC0-1.0

//! The AMPLIFY family of the published cache-policy catalogue, end to end over HTTP.
//!
//! The rule: the managed cache policies are AWS's own, the same in every account, so
//! `ListCachePolicies(Type=managed)` reports every policy the catalogue publishes and each one
//! is readable under the identifier it publishes — including the family Amplify attaches to
//! the behaviours of an application it hosts (`Managed-Amplify-*`), which a client, a template
//! or Amplify itself names by id. A family member missing from the catalogue is a not-found
//! refusal for a policy AWS always provides.
//!
//! The rows are the family's eight names, and for each the two things a caller does with a
//! published policy: find it in the managed listing, and read it by id. Each entry's
//! configuration is asserted to be a USABLE cache policy — the cache-key parameters present,
//! the three TTLs in the order the model documents them (`MinTTL` the floor, `MaxTTL` the
//! ceiling), and each behaviour a value of the model's own enum — rather than any one value,
//! which is the catalogue's to state.
//!
//! The wire harness is the lifecycle suites', reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::{
    CACHE_POLICY_COOKIE_BEHAVIOR_VALUES, CACHE_POLICY_HEADER_BEHAVIOR_VALUES,
    CACHE_POLICY_QUERY_STRING_BEHAVIOR_VALUES,
};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MANAGED_CACHE_POLICIES, CACHE_POLICY_TYPE_MANAGED,
};

use crate::cache_policy_lifecycle::get_policy;
use crate::cache_policy_list::list_policies;
use crate::distribution_lifecycle::emulator;

/// The Amplify family the catalogue publishes: the names a client attaches by, each in its
/// original and its revised (`-V2`) form.
const AMPLIFY_FAMILY: [&str; 8] = [
    "Managed-Amplify-Default",
    "Managed-Amplify-Default-V2",
    "Managed-Amplify-DefaultNoCookies",
    "Managed-Amplify-DefaultNoCookies-V2",
    "Managed-Amplify-ImageOptimization",
    "Managed-Amplify-ImageOptimization-V2",
    "Managed-Amplify-StaticContent",
    "Managed-Amplify-StaticContent-V2",
];

/// Every name of the family is in the published catalogue, is reported by the managed listing
/// and is readable by its own id, with a configuration a cache behaviour can be attached
/// under.
#[tokio::test]
async fn the_amplify_family_is_published_and_readable() {
    let router = emulator();

    let listed = list_policies(&router, &format!("?Type={CACHE_POLICY_TYPE_MANAGED}")).await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListCachePolicies, got {} with {}",
        listed.status, listed.raw
    );

    for case in AMPLIFY_FAMILY {
        // The catalogue the state module publishes carries the entry...
        let published = MANAGED_CACHE_POLICIES
            .iter()
            .find(|managed| managed.name == case)
            .unwrap_or_else(|| {
                panic!("case {case}: the published catalogue carries no such policy")
            });

        // ...the managed listing reports it, under that name and that id...
        let summary = listed
            .root()
            .descendant("Items")
            .map(|items| items.children_named("CachePolicySummary"))
            .unwrap_or_default()
            .into_iter()
            .find(|summary| summary.text_at("CachePolicy/CachePolicyConfig/Name") == Some(case))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the managed listing reports no such policy: {}",
                    listed.raw
                )
            });
        assert_eq!(
            summary.text_at("Type"),
            Some(CACHE_POLICY_TYPE_MANAGED),
            "case {case}: expected the managed type in the summary, got {:?}",
            summary.text_at("Type")
        );
        assert_eq!(
            summary.text_at("CachePolicy/Id"),
            Some(published.id),
            "case {case}: expected the published id {:?} in the listing, got {:?}",
            published.id,
            summary.text_at("CachePolicy/Id")
        );

        // ...and a read by that id answers from the same entry.
        let read = get_policy(&router, published.id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetCachePolicy, got {} with {}",
            read.status, read.raw
        );
        let config = read
            .root()
            .at("CachePolicyConfig")
            .unwrap_or_else(|| panic!("case {case}: the read reports no configuration"));
        assert_eq!(
            config.text_at("Name"),
            Some(case),
            "case {case}: expected the read to report the published name, got {:?}",
            config.text_at("Name")
        );

        // The configuration is a usable cache policy: the three TTLs in the documented order,
        // and the cache-key parameters a behaviour is keyed by.
        let ttl = |member: &str| -> i64 {
            config
                .text_at(member)
                .unwrap_or_else(|| panic!("case {case}: the read reports no {member}: {}", read.raw))
                .parse()
                .unwrap_or_else(|error| {
                    panic!("case {case}: expected a whole number of seconds for {member} ({error})")
                })
        };
        let (min_ttl, default_ttl, max_ttl) = (ttl("MinTTL"), ttl("DefaultTTL"), ttl("MaxTTL"));
        assert!(
            min_ttl <= default_ttl && default_ttl <= max_ttl,
            "case {case}: expected MinTTL <= DefaultTTL <= MaxTTL, got {min_ttl}, \
             {default_ttl}, {max_ttl}"
        );
        let parameters = config
            .at("ParametersInCacheKeyAndForwardedToOrigin")
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the read reports no cache-key parameters: {}",
                    read.raw
                )
            });
        // Each behaviour is a value of the model's own enum for it, so a behaviour the
        // catalogue states is one a caller could state too.
        for (path, values) in [
            (
                "HeadersConfig/HeaderBehavior",
                CACHE_POLICY_HEADER_BEHAVIOR_VALUES,
            ),
            (
                "CookiesConfig/CookieBehavior",
                CACHE_POLICY_COOKIE_BEHAVIOR_VALUES,
            ),
            (
                "QueryStringsConfig/QueryStringBehavior",
                CACHE_POLICY_QUERY_STRING_BEHAVIOR_VALUES,
            ),
        ] {
            let behavior = parameters.text_at(path).unwrap_or_else(|| {
                panic!("case {case}: the read reports no {path}: {}", read.raw)
            });
            assert!(
                values.contains(&behavior),
                "case {case}: expected {path} to be one of {values:?}, got {behavior:?}"
            );
        }
        assert!(
            parameters.text_at("EnableAcceptEncodingGzip").is_some(),
            "case {case}: the read reports no EnableAcceptEncodingGzip: {}",
            read.raw
        );
    }
}

/// A published id is unique in the catalogue: two entries never share one, so a client
/// attaching one of these policies by id reaches exactly one policy.
#[tokio::test]
async fn every_published_cache_policy_has_its_own_identity() {
    let mut ids: Vec<&str> = MANAGED_CACHE_POLICIES.iter().map(|one| one.id).collect();
    let published = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        published,
        "the published catalogue carries {} entries under {} identifiers",
        published,
        ids.len()
    );

    let mut names: Vec<&str> = MANAGED_CACHE_POLICIES.iter().map(|one| one.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        published,
        "the published catalogue carries {} entries under {} names",
        published,
        names.len()
    );
}
