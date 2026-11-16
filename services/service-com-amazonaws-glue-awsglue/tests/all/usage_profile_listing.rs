// SPDX-License-Identifier: CC0-1.0

//! How a `ListUsageProfiles` walk over the account's usage profile namespace behaves.
//!
//! RULE: a paged Glue listing reports the records the store holds NOW, in key order,
//! resuming strictly after the key its continuation token names and reporting a token only
//! while records remain after the page — so a page is computed from the store at the moment
//! it is asked for, and a record created or deleted between two pages changes what the rest
//! of the walk reports without ever repeating or skipping a record that stayed put.
//!
//! `tests/all/empty_collection_reads.rs` pins the page SIZES of this listing and the
//! projection each listed profile carries; this suite pins the walk itself — the cursor's
//! meaning across mutations, a page landing past the end of the listing, and the boundary at
//! which a token stops being reported.
//!
//! The members are the ones `ListUsageProfiles` models
//! (`smithy_query(query="operation", operation="ListUsageProfiles")`): `MaxResults`
//! (`OrchestrationPageSize200`, range 1..200) and `NextToken` (`OrchestrationToken`, length
//! 0..400000) in, `Profiles` and `NextToken` out.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, given_usage_profile, names_in, ok};

/// Delete one usage profile.
async fn delete_usage_profile(router: &MultiServiceRouter, name: &str) {
    ok(router, "DeleteUsageProfile", &json!({ "Name": name })).await;
}

/// The continuation token a page reports, if it reports one.
fn reported_token(page: &Value) -> Option<String> {
    page.get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Ask for one page, and answer the names it reports beside its continuation token. The
/// `Profiles` member must be PRESENT — the shared reader panics on an absent list rather than
/// reading absence as an empty page.
async fn page_of(
    router: &MultiServiceRouter,
    max_results: Option<i32>,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let mut request = json!({});
    if let Some(size) = max_results {
        request["MaxResults"] = json!(size);
    }
    if let Some(cursor) = token {
        request["NextToken"] = json!(cursor);
    }
    let page = ok(router, "ListUsageProfiles", &request).await;
    (names_in(&page, "Profiles"), reported_token(&page))
}

/// Walk the listing from `token` onwards in `max_results`-sized pages and answer every name
/// it reports, in order. The bound on the number of pages only stops a broken build from
/// looping forever.
async fn walk_from(
    case: &str,
    router: &MultiServiceRouter,
    max_results: i32,
    token: Option<&str>,
) -> Vec<String> {
    let mut cursor = token.map(str::to_string);
    let mut listed: Vec<String> = Vec::new();
    for _ in 0..64 {
        let (names, next) = page_of(router, Some(max_results), cursor.as_deref()).await;
        assert!(
            names.len() <= usize::try_from(max_results).unwrap_or(usize::MAX),
            "case {case}: a page must not exceed MaxResults {max_results}, got {names:?}"
        );
        listed.extend(names);
        match next {
            Some(next) => cursor = Some(next),
            None => return listed,
        }
    }
    panic!("case {case}: the walk must terminate, got {listed:?}")
}

/// The profiles every walk below starts from, in the order their names sort in.
const SEEDED: [&str; 4] = [
    "alpha-profile",
    "bravo-profile",
    "charlie-profile",
    "delta-profile",
];

/// One row of the walk below: the case name, the profiles deleted mid-walk, the profiles
/// created mid-walk, and the names the rest of the walk must report.
type MidWalkCase<'a> = (&'a str, Vec<&'a str>, Vec<&'a str>, Vec<&'a str>);

/// A walk resumes strictly after the profile its token names, against the store as it stands
/// when the next page is asked for: a profile deleted between two pages is gone from the
/// rest of the walk, one created after the cursor joins it, and one created before the cursor
/// — the cursor having already passed it — does not reappear.
///
/// The rows differ along the mutation applied mid-walk and where the mutated name falls
/// relative to the cursor: no mutation, a delete of the profile the cursor names, a delete
/// ahead of the cursor, a create ahead of the cursor, a create behind it, and a delete of
/// everything that remained.
#[tokio::test]
async fn a_usage_profile_walk_resumes_after_its_cursor_against_the_current_store() {
    let cases: Vec<MidWalkCase> = vec![
        (
            "nothing changes mid-walk",
            vec![],
            vec![],
            vec!["charlie-profile", "delta-profile"],
        ),
        (
            "the profile the cursor names is deleted",
            vec!["bravo-profile"],
            vec![],
            vec!["charlie-profile", "delta-profile"],
        ),
        (
            "a profile ahead of the cursor is deleted",
            vec!["charlie-profile"],
            vec![],
            vec!["delta-profile"],
        ),
        (
            "a profile ahead of the cursor is created",
            vec![],
            vec!["coda-profile"],
            vec!["charlie-profile", "coda-profile", "delta-profile"],
        ),
        (
            "a profile behind the cursor is created",
            vec![],
            vec!["aardvark-profile"],
            vec!["charlie-profile", "delta-profile"],
        ),
        (
            "everything ahead of the cursor is deleted",
            vec!["charlie-profile", "delta-profile"],
            vec![],
            vec![],
        ),
    ];

    for (case, deleted, created, rest) in cases {
        let router = emulator();
        for name in SEEDED {
            given_usage_profile(&router, name, None).await;
        }

        // The first page of the walk, taken before any mutation: two of the four profiles,
        // and a token naming the last of them.
        let (first, token) = page_of(&router, Some(2), None).await;
        assert_eq!(
            first,
            vec![SEEDED[0].to_string(), SEEDED[1].to_string()],
            "case {case}: the first page must report the first two stored profiles, got {first:?}"
        );
        let token = token.unwrap_or_else(|| {
            panic!("case {case}: profiles remain after the first page, so it must report a token")
        });

        for name in &deleted {
            delete_usage_profile(&router, name).await;
        }
        for name in &created {
            given_usage_profile(&router, name, None).await;
        }

        let listed = walk_from(case, &router, 2, Some(&token)).await;
        let expected: Vec<String> = rest.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            listed, expected,
            "case {case}: after deleting {deleted:?} and creating {created:?} the rest of the \
             walk must be {expected:?}, got {listed:?}"
        );
    }
}

/// A page that lands past the end of the listing is an EMPTY page, not a refusal and not a
/// missing member, and it ends the walk: nothing remains after it, so it reports no token.
///
/// The rows differ along how far past the end the cursor lands and how much the store holds:
/// the last stored name, a name sorting after every stored name of a four-profile listing, a
/// name sorting after the only stored name, and an account holding no profile at all.
#[tokio::test]
async fn a_usage_profile_page_past_the_end_of_the_listing_is_empty_and_final() {
    // (case, profiles to store, the cursor to ask from)
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "the cursor names the last stored profile",
            SEEDED.to_vec(),
            "delta-profile",
        ),
        (
            "the cursor sorts after every stored profile",
            SEEDED.to_vec(),
            "zeta-profile",
        ),
        (
            "the cursor sorts after the only stored profile",
            vec!["alpha-profile"],
            "bravo-profile",
        ),
        (
            "the account holds no profile at all",
            vec![],
            "alpha-profile",
        ),
    ];

    for (case, stored, cursor) in cases {
        let router = emulator();
        for name in &stored {
            given_usage_profile(&router, name, None).await;
        }

        let (status, page) = call(
            &router,
            "ListUsageProfiles",
            &json!({ "NextToken": cursor }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: a page past the end of the listing is served, got {status} with {page}"
        );
        assert_eq!(
            names_in(&page, "Profiles"),
            Vec::<String>::new(),
            "case {case}: nothing is stored after {cursor:?}, so the page must be empty, got \
             {page}"
        );
        assert_eq!(
            reported_token(&page),
            None,
            "case {case}: nothing remains after an empty page, so it must report no token, got \
             {page}"
        );
    }
}

/// A token is reported only while profiles remain after the page: a page that ends exactly on
/// the last stored profile reports none, and a shorter one reports a token the rest of the
/// listing follows from. The token's own spelling is the service's to choose — what is
/// asserted is that asking again with it reports exactly the profiles the page stopped short
/// of.
///
/// The rows differ along how the page size relates to the stored count: equal to it, one
/// below it, the smallest page the model admits against one stored profile, the same smallest
/// page against two, and the largest page the model admits, which is past the stored count.
#[tokio::test]
async fn a_usage_profile_page_reports_a_token_only_while_profiles_remain() {
    // (case, number of profiles stored, page size, the names the first page reports, whether
    // a token follows it)
    let cases: Vec<(&str, usize, i32, usize, bool)> = vec![
        ("the page ends on the last stored profile", 4, 4, 4, false),
        ("the page stops one short of the end", 4, 3, 3, true),
        ("the smallest page the model admits", 1, 1, 1, false),
        ("the smallest page over two profiles", 2, 1, 1, true),
        ("the largest page the model admits", 4, 200, 4, false),
    ];

    for (case, stored, page_size, expected_len, expects_token) in cases {
        let router = emulator();
        let names: Vec<String> = (0..stored).map(|index| format!("profile-{index}")).collect();
        for name in &names {
            given_usage_profile(&router, name, None).await;
        }

        let (listed, token) = page_of(&router, Some(page_size), None).await;
        assert_eq!(
            listed,
            names[..expected_len].to_vec(),
            "case {case}: a page of {page_size} over {stored} profiles must report the first \
             {expected_len} in name order, got {listed:?}"
        );
        assert_eq!(
            token.is_some(),
            expects_token,
            "case {case}: with {stored} profiles and a page of {page_size}, a token {} reported, \
             got {token:?}",
            if expects_token {
                "must be"
            } else {
                "must not be"
            }
        );
        if let Some(token) = &token {
            let rest = walk_from(case, &router, page_size, Some(token)).await;
            assert_eq!(
                rest,
                names[expected_len..].to_vec(),
                "case {case}: the rest of the listing must follow from the token, expected \
                 {:?}, got {rest:?}",
                &names[expected_len..]
            );
        }
    }
}
