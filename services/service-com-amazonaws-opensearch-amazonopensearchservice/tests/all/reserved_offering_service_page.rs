// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the page a PUBLISHED offering catalog read answers when the
//! request names no page size of its own.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness and the two readers come from
//! [`super::reserved_instance_offerings`]; this suite adds no second copy of them.
//!
//! The rule under test:
//!
//! > `DescribeReservedInstanceOfferings` answers a request that names no `MaxResults` with
//! > the SERVICE's own page of the catalog it publishes and the token to resume from, the
//! > way the recorded answer of an unbounded call reports a first page beside a
//! > `NextToken`. The page is never larger than the largest one the member admits
//! > (`MaxResults` is `@range(max: 100)`,
//! > `smithy_query(query="operation",operation="DescribeReservedInstanceOfferings")`), the
//! > token names the first offering the answer did not report, and following it reaches
//! > every offering the catalog holds — so the bound hides nothing from a client that
//! > pages. An answer that reports the rest of the catalog carries no token at all.
//!
//! The rows differ along what the rule generalises over: how the absent bound is expressed
//! (the member left off, and the modelled default `maxResults=0` sent explicitly), where in
//! the catalog the unbounded answer is asked for (its start, and resumed from a token in
//! its middle), and a listing SHORTER than the service's own page (the
//! `ReservedInstanceOfferingId` filter, which selects one offering), which has no next page
//! to name.

use super::reserved_instance_offerings::{describe, offerings, whole_catalog};
use super::domain_lifecycle::Emulator;

/// The largest page `MaxResults` admits, which is the page the service answers a request
/// that names no bound of its own with.
const SERVICE_PAGE: usize = 100;

/// The ways a request expresses "no page size": the member left off, and the modelled
/// `@default(0)` sent explicitly.
const UNBOUNDED: [&str; 2] = ["", "maxResults=0"];

/// The id of one reported offering, with the case named on failure.
fn offering_id(offering: &serde_json::Value, case: &str) -> String {
    offering["ReservedInstanceOfferingId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected an offering id, got {offering:?}"))
        .to_owned()
}

#[tokio::test]
async fn an_unbounded_read_answers_the_services_own_page_and_where_to_resume() {
    let emulator = Emulator::new();
    // The catalog this Region publishes, walked through the tokens the answers hand out —
    // the suite's own measure of the set, rather than a count written into it.
    let catalog = whole_catalog(&emulator, "the published catalog").await;
    assert!(
        catalog.len() > SERVICE_PAGE,
        "the published catalog must be larger than one page for these rows to mean \
         anything, got {} offerings",
        catalog.len()
    );

    for unbounded in UNBOUNDED {
        let case = format!("no page size, as {unbounded:?}");
        let answer = describe(&emulator, unbounded).await;
        let page = offerings(&answer, &case);
        assert_eq!(
            page.len(),
            SERVICE_PAGE,
            "case {case}: expected the service's own page of {SERVICE_PAGE} offerings, got {}",
            page.len()
        );
        assert_eq!(
            page,
            catalog[..SERVICE_PAGE].to_vec(),
            "case {case}: expected the first {SERVICE_PAGE} offerings of the published \
             catalog, got {page:?}"
        );
        let token = answer.body["NextToken"].as_str().unwrap_or_default();
        assert_eq!(
            token,
            offering_id(&catalog[SERVICE_PAGE], &case),
            "case {case}: offerings remain, so the answer names the first one it did not \
             report, got {:?}",
            answer.body["NextToken"]
        );

        // Resuming from that token is an unbounded read too, so the same rule holds in the
        // MIDDLE of the catalog: one page from where the last answer stopped, and the next
        // token.
        let case = format!("{case}, resumed once");
        let resumed = describe(&emulator, &format!("nextToken={token}")).await;
        let next = offerings(&resumed, &case);
        let remaining = catalog.len() - SERVICE_PAGE;
        let expected = usize::min(remaining, SERVICE_PAGE);
        assert_eq!(
            next,
            catalog[SERVICE_PAGE..SERVICE_PAGE + expected].to_vec(),
            "case {case}: expected the {expected} offerings after the first page, got {}",
            next.len()
        );
        assert_eq!(
            resumed.body["NextToken"].as_str(),
            catalog
                .get(SERVICE_PAGE + expected)
                .map(|offering| offering_id(offering, &case))
                .as_deref(),
            "case {case}: expected the token to name the next offering, or none where the \
             answer reported the rest, got {:?}",
            resumed.body["NextToken"]
        );
    }
}

#[tokio::test]
async fn following_the_token_of_an_unbounded_read_reaches_the_whole_catalog() {
    let emulator = Emulator::new();
    let catalog = whole_catalog(&emulator, "the published catalog").await;

    // The walk a client makes when it names no page size at all: every answer is a page
    // and a token, the last answer carries none, and what the walk collects is the whole
    // catalog in the order it is published in — so the service's own bound hides nothing.
    let mut collected: Vec<serde_json::Value> = Vec::new();
    let mut pages = 0_usize;
    let mut token: Option<String> = None;
    loop {
        let case = format!("page {pages} of the unbounded walk");
        let query = match token.as_deref() {
            None => String::new(),
            Some(cursor) => format!("nextToken={cursor}"),
        };
        let answer = describe(&emulator, &query).await;
        let page = offerings(&answer, &case);
        assert!(
            page.len() <= SERVICE_PAGE,
            "case {case}: no answer is larger than the service's own page, got {}",
            page.len()
        );
        collected.extend(page);
        pages += 1;
        match answer.body["NextToken"].as_str() {
            None => break,
            Some(next) => token = Some(next.to_owned()),
        }
        assert!(
            pages <= catalog.len(),
            "case {case}: the walk terminates, got {pages} pages"
        );
    }
    assert_eq!(
        collected, catalog,
        "the unbounded walk must report the whole published catalog in order, got {} of {} \
         offerings over {pages} page(s)",
        collected.len(),
        catalog.len()
    );
    assert_eq!(
        pages,
        catalog.len().div_ceil(SERVICE_PAGE),
        "expected one page per {SERVICE_PAGE} offerings of the catalog, got {pages}"
    );
}

#[tokio::test]
async fn a_listing_shorter_than_the_page_names_nothing_to_resume_from() {
    let emulator = Emulator::new();
    let catalog = whole_catalog(&emulator, "the published catalog").await;

    // The filter selects ONE offering, so the listing is shorter than the service's own
    // page and the answer reports all of it: there is nothing left to resume from. Rows
    // from the catalog's start, middle and end, so the row is not a property of one
    // position in it.
    for index in [0, catalog.len() / 2, catalog.len() - 1] {
        let case = format!("the offering filter at {index} of {}", catalog.len());
        let id = offering_id(&catalog[index], &case);
        let answer = describe(&emulator, &format!("offeringId={id}")).await;
        let page = offerings(&answer, &case);
        assert_eq!(
            page.len(),
            1,
            "case {case}: the filter selects one offering, got {page:?}"
        );
        assert_eq!(
            offering_id(&page[0], &case),
            id,
            "case {case}: expected the offering the filter names, got {page:?}"
        );
        assert!(
            answer.body["NextToken"].is_null(),
            "case {case}: the answer reports the whole listing, so it names nothing to \
             resume from, got {:?}",
            answer.body["NextToken"]
        );
    }
}
