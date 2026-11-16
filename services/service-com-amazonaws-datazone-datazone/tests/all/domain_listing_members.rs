// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the members a `ListDomains` 200 body carries.
//!
//! The rule under test: the domain-summary list is ALWAYS on the wire — the model marks it
//! `@required` (`smithy_query(query="operation",operation="ListDomains")`: `items`, target
//! `DomainSummaries`, required) so an account holding no domain is reported with an EMPTY
//! array rather than with the member missing — and it is carried both under that modelled
//! member name and under the name the live service renders it, which this checkout's model
//! snapshot does not declare. The two names always report one and the same list, whatever
//! the listing holds and however it was filtered or paged.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: how many
//! domains the listing reports (none, one, several), whether the listing was narrowed by the
//! `status` filter, whether the filter matched anything, and whether the page carries a
//! continuation token. Every call goes over the wire through the shared [`crate::harness`]
//! emulator, so what is asserted is the HTTP body and not a handler's return value.

use serde_json::{json, Value};

use crate::harness::Emulator;

/// The member name the MODEL declares the domain summaries under.
const MODELLED: &str = "items";

/// The member name the live wire renders them under as well.
const SERVED: &str = "domains";

/// Assert that `body` carries the summaries under both names, with the same value, and
/// answer that list.
fn both_names(case: &str, body: &Value) -> Vec<Value> {
    let modelled = body.get(MODELLED).unwrap_or_else(|| {
        panic!("case {case:?}: the body must carry the {MODELLED:?} member, got {body}")
    });
    let served = body.get(SERVED).unwrap_or_else(|| {
        panic!("case {case:?}: the body must carry the {SERVED:?} member, got {body}")
    });
    assert_eq!(
        modelled, served,
        "case {case:?}: both names must report one list, got {body}"
    );
    let listed = modelled.as_array().unwrap_or_else(|| {
        panic!("case {case:?}: the summaries must be an array, got {modelled}")
    });
    listed.clone()
}

/// RULE: a listing of an account holding no domain reports the summaries as an empty array
/// under both names — not as a missing member.
#[tokio::test]
async fn an_empty_listing_reports_an_empty_array_under_both_names() {
    let dz = Emulator::new();

    // (case, the query the listing is narrowed by)
    for (case, query) in [
        ("no domains at all", ""),
        ("a status filter nothing matches", "?status=AVAILABLE"),
        ("a status no domain is in", "?status=DELETING"),
        ("a page size of one over an empty store", "?maxResults=1"),
    ] {
        let listed = dz.call("GET", &format!("/v2/domains{query}"), None).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            listed.status, listed.body
        );
        let summaries = both_names(case, &listed.body);
        assert!(
            summaries.is_empty(),
            "case {case:?}: expected no summaries, got {summaries:?}"
        );
    }
}

/// RULE: a listing that DOES report domains carries the same summaries under both names,
/// however many there are, however it was filtered and whether or not the page continues.
#[tokio::test]
async fn a_populated_listing_reports_the_same_summaries_under_both_names() {
    let dz = Emulator::new();
    let mut created: Vec<String> = Vec::new();
    for n in 0..3 {
        let answer = dz
            .create_domain(json!({ "name": format!("listing-domain-{n}") }))
            .await;
        assert_eq!(
            answer.status, 201,
            "creating domain {n}: expected 201, got {} {:?}",
            answer.status, answer.body
        );
        created.push(answer.s("id"));

        // (case, the query, how many summaries the page must report — `None` where the count
        // depends on which lifecycle step the clock has reached, which is not what this rule
        // is about: there the two names are still compared against each other)
        let rows: Vec<(String, String, Option<usize>)> = vec![
            (
                format!("{} stored, no filter", n + 1),
                String::new(),
                Some(n + 1),
            ),
            (
                // A status filter selects on the stored status, so how many it reports
                // depends on how many creations the clock has settled by now. The rule under
                // test is that both names report ONE list, which holds either way.
                format!("{} stored, filtered on CREATING", n + 1),
                "?status=CREATING".to_string(),
                None,
            ),
            (
                format!("{} stored, filtered on AVAILABLE", n + 1),
                "?status=AVAILABLE".to_string(),
                None,
            ),
            (
                // No operation of this service puts a domain in this status, so this one is
                // empty whatever the clock has done.
                format!("{} stored, filtered on a status none is in", n + 1),
                "?status=DELETION_FAILED".to_string(),
                Some(0),
            ),
            (
                format!("{} stored, one per page", n + 1),
                "?maxResults=1".to_string(),
                Some(1),
            ),
        ];

        for (case, query, expected) in rows {
            let listed = dz.call("GET", &format!("/v2/domains{query}"), None).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected 200, got {} {:?}",
                listed.status, listed.body
            );
            let summaries = both_names(&case, &listed.body);
            if let Some(expected) = expected {
                assert_eq!(
                    summaries.len(),
                    expected,
                    "case {case:?}: expected {expected} summaries, got {summaries:?}"
                );
            }
        }
    }

    // A page that continues: the token is reported beside both names, and the next page is
    // carried under both names too.
    let page = dz.call("GET", "/v2/domains?maxResults=1", None).await;
    let token = page.s("nextToken");
    assert_ne!(
        token, "<absent>",
        "a page with more to come must report a nextToken, got {:?}",
        page.body
    );
    let next = dz
        .call(
            "GET",
            &format!("/v2/domains?maxResults=1&nextToken={token}"),
            None,
        )
        .await;
    let summaries = both_names("the second page", &next.body);
    assert_eq!(
        summaries.len(),
        1,
        "the second page must report one summary, got {summaries:?}"
    );
}

/// The rule is the LISTING's: another operation's 200 body is unchanged, so a read of one
/// domain carries no such member.
#[tokio::test]
async fn another_operations_body_carries_no_listing_member() {
    let dz = Emulator::new();
    let id = dz.available_domain("listing-member-scope").await;

    for (case, uri) in [
        ("GetDomain", format!("/v2/domains/{id}")),
        (
            "ListProjects",
            format!("/v2/domains/{id}/projects?maxResults=1"),
        ),
    ] {
        let answer = dz.call("GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert!(
            answer.body.get(SERVED).is_none(),
            "case {case:?}: only the domain listing carries {SERVED:?}, got {:?}",
            answer.body
        );
    }
}
