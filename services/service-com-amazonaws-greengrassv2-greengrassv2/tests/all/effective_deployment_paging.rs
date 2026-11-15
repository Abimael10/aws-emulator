// SPDX-License-Identifier: CC0-1.0

//! `ListEffectiveDeployments`, end to end over the real `restJson1` front door: how the
//! deployments that reach one core device are PAGED, which page sizes are served, which
//! cursors are refused, and what the listing leaves behind.
//!
//! The rules under test:
//!
//! * a paginated reader reports every item that belongs to the collection exactly once
//!   across its pages, in one stable order a cursor can resume in — the order the opaque
//!   `nextToken` compares in, so the pages of any page size concatenate to the same
//!   sequence the whole-collection read answers, and the last page of a walk carries no
//!   `nextToken`;
//! * `maxResults` is a `DefaultMaxResults`, `range 1..100`
//!   (`smithy_query(query="operation",operation="ListEffectiveDeployments")`), so a value
//!   at either published bound is served and one outside it is refused with the modelled
//!   HTTP 400 `ValidationException` rather than answered with a page;
//! * a `nextToken` is this service's own opaque token, so one it did not issue names no
//!   page and is refused as bad input (HTTP 400 `ValidationException` naming the member),
//!   while a token it did issue is not consumed: resuming the same cursor twice answers
//!   the same page; and
//! * the listing REPORTS, so it changes nothing a later caller sees — however many times
//!   and through whichever page size it is read, every deployment's own reader, the
//!   deployment history and the core-device fleet answer afterwards exactly what they
//!   answered before, and reading the effective deployments of a thing name no device has
//!   reported under brings no device into being.
//!
//! The rows differ along what the rules generalise over: the NUMBER of deployments that
//! reach the device (none, one, several, more than any page holds), the page SIZE asked
//! for (none at all, one, a few, each published bound, one past each bound), the FORM of a
//! rejected cursor (not this encoding, an odd number of digits, an alphabet the encoding
//! does not use, one that decodes to no text), and the number of reads interleaved with
//! the writes (one, a handful, many).
//!
//! The harness — the router over one shared engine, the signed call, the body readers, the
//! routes and the core device's own status report — is the one `tests/all/core_device_fleet.rs`
//! owns; that sibling suite holds the core-device surface as a whole, while the axis here
//! is the paging of, and the purity of, the effective-deployment listing.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::core_device_fleet::{
    call, create_deployment, emulator, error_code, get_core_device, list, list_core_devices,
    list_effective_deployments, report, text, thing_arn, Report,
};

/// The `CoreDeviceStatus` the device in these fixtures reports
/// (`smithy_query(query="shape",shape="CoreDeviceStatus")`: `HEALTHY`, `UNHEALTHY`).
const HEALTHY: &str = "HEALTHY";

/// The code a request outside a modelled constraint is refused with
/// (`smithy_query(query="operation",operation="ListEffectiveDeployments")`: HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// The published bounds of `maxResults` (`DefaultMaxResults`, `range 1..100`).
const MIN_PAGE_SIZE: i64 = 1;
const MAX_PAGE_SIZE: i64 = 100;

/// The core device every row lists the effective deployments of.
const DEVICE: &str = "paged-core";

/// A device in the fleet with `deployments` deployments targeting its thing, and the ids
/// of those deployments in creation order.
async fn fixture(deployments: usize) -> (MultiServiceRouter, Vec<String>) {
    let (router, engine) = emulator();
    report(&engine, &Report::new(DEVICE, HEALTHY));
    let target = thing_arn(DEVICE);
    let mut ids = Vec::new();
    for revision in 1..=deployments {
        ids.push(create_deployment(&router, &target, Some(&format!("r{revision}"))).await);
    }
    (router, ids)
}

/// One `ListEffectiveDeployments` that must answer 200, as its body.
async fn listing(router: &MultiServiceRouter, case: &str, query: &str) -> Value {
    let (status, body) = list_effective_deployments(router, DEVICE, query).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListEffectiveDeployments {query:?} must answer 200, got {status} {body}"
    );
    body
}

/// The `deploymentId`s a listing body reports, in the order it reports them.
fn reported_ids(body: &Value) -> Vec<String> {
    list(body, "effectiveDeployments")
        .iter()
        .map(|deployment| text(deployment, "deploymentId"))
        .collect()
}

/// Walk every page of the device's effective deployments at the page size `max_results`,
/// and answer the ids in the order the pages reported them together with how many pages
/// the walk took.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<i64>,
) -> (Vec<String>, usize) {
    let mut walked: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let mut query = String::new();
        if let Some(size) = max_results {
            query.push_str(&format!("?maxResults={size}"));
        }
        if let Some(token) = &cursor {
            query.push(if query.is_empty() { '?' } else { '&' });
            query.push_str(&format!("nextToken={token}"));
        }
        let page = listing(router, case, &query).await;
        pages += 1;
        let ids = reported_ids(&page);
        if let Some(size) = max_results {
            assert!(
                i64::try_from(ids.len()).unwrap_or(i64::MAX) <= size,
                "case {case:?} page {pages}: a page of {size} must hold at most that many, \
                 got {page}"
            );
        }
        walked.extend(ids);
        let next = text(&page, "nextToken");
        if next.is_empty() {
            return (walked, pages);
        }
        cursor = Some(next);
        assert!(
            pages < 64,
            "case {case:?}: the walk must end, it took {pages} pages and still hands back a \
             cursor"
        );
    }
}

/// How many pages a walk of `total` items at `page_size` takes: one page per full-or-part
/// page of items, and a single empty page when there is nothing to report.
fn expected_pages(total: usize, page_size: usize) -> usize {
    total.div_ceil(page_size).max(1)
}

#[tokio::test]
async fn effective_deployments_are_paged_over_every_page_size_the_model_admits() {
    // The rows differ along the NUMBER of deployments that reach the device and the page
    // SIZE asked for — including the two published bounds of `maxResults`, and no
    // `maxResults` at all, which the model gives no default for.
    for total in [0usize, 1, 2, 5] {
        let (router, created) = fixture(total).await;

        // The whole-collection read is the order every page size must agree with, and the
        // order a lexicographic cursor can resume in: ascending by deployment id.
        let whole = listing(&router, "the whole collection", "").await;
        let order = reported_ids(&whole);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(
            order, sorted,
            "case {total} deployment(s): the listing must report one stable, ascending \
             order a cursor can resume in, got {whole}"
        );
        let mut expected_set = created.clone();
        expected_set.sort();
        assert_eq!(
            sorted, expected_set,
            "case {total} deployment(s): the listing must report exactly the deployments \
             that reach the device, got {whole}"
        );
        assert!(
            text(&whole, "nextToken").is_empty(),
            "case {total} deployment(s): a listing that reported everything must hand back \
             no cursor, got {whole}"
        );

        for max_results in [None, Some(MIN_PAGE_SIZE), Some(2), Some(3), Some(MAX_PAGE_SIZE)] {
            let case = format!("{total} deployment(s), maxResults={max_results:?}");
            let (walked, pages) = walk(&router, &case, max_results).await;
            assert_eq!(
                walked, order,
                "case {case}: the pages must concatenate to the whole collection, in the \
                 same order, got {walked:?}"
            );
            let page_size = usize::try_from(max_results.unwrap_or(MAX_PAGE_SIZE)).unwrap_or(1);
            assert_eq!(
                pages,
                expected_pages(total, page_size),
                "case {case}: expected {} page(s), got {pages}",
                expected_pages(total, page_size)
            );
        }
    }
}

#[tokio::test]
async fn a_page_size_outside_the_modelled_range_is_refused() {
    let (router, created) = fixture(3).await;

    // Both published bounds are served: the smallest page reports one item and hands back
    // a cursor, the largest reports the whole collection and hands back none.
    let smallest = listing(
        &router,
        "the smallest page the model admits",
        &format!("?maxResults={MIN_PAGE_SIZE}"),
    )
    .await;
    assert_eq!(
        reported_ids(&smallest).len(),
        1,
        "case maxResults={MIN_PAGE_SIZE}: expected one item, got {smallest}"
    );
    assert!(
        !text(&smallest, "nextToken").is_empty(),
        "case maxResults={MIN_PAGE_SIZE}: a page of one over {} deployments must hand back a \
         cursor, got {smallest}",
        created.len()
    );
    let largest = listing(
        &router,
        "the largest page the model admits",
        &format!("?maxResults={MAX_PAGE_SIZE}"),
    )
    .await;
    assert_eq!(
        (reported_ids(&largest).len(), text(&largest, "nextToken")),
        (created.len(), String::new()),
        "case maxResults={MAX_PAGE_SIZE}: expected the whole collection and no cursor, got \
         {largest}"
    );

    let before = listing(&router, "before the refusals", "").await;
    // One value past each published bound, and one far outside it on either side.
    for (case, value) in [
        ("one below the modelled minimum", MIN_PAGE_SIZE - 1),
        ("one above the modelled maximum", MAX_PAGE_SIZE + 1),
        ("a negative page size", -1),
        ("a page size far above the maximum", 1_000),
    ] {
        let (status, body) =
            list_effective_deployments(&router, DEVICE, &format!("?maxResults={value}")).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, VALIDATION_CODE.to_string()),
            "case {case:?} (maxResults={value}): expected a 400 {VALIDATION_CODE}, got \
             {status} {body}"
        );
        assert!(
            body.get("effectiveDeployments").is_none(),
            "case {case:?} (maxResults={value}): a refusal reports no page, got {body}"
        );
    }

    // A refusal writes nothing, so the listing after them is the listing before them.
    let after = listing(&router, "after the refusals", "").await;
    assert_eq!(
        after, before,
        "case the refused page sizes: the collection must be untouched, got {after}"
    );
}

#[tokio::test]
async fn a_cursor_this_service_did_not_issue_is_refused_and_one_it_issued_is_not_consumed() {
    let (router, _created) = fixture(3).await;

    // The forms differ along HOW the token misses this service's encoding.
    for (case, token) in [
        ("a token that is not this encoding", "not-a-cursor"),
        ("an odd number of digits", "abc"),
        ("an alphabet the encoding does not use", "zzzz"),
        ("a token that decodes to no text", "ff"),
    ] {
        let (status, body) =
            list_effective_deployments(&router, DEVICE, &format!("?nextToken={token}")).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, VALIDATION_CODE.to_string()),
            "case {case:?} ({token:?}): expected a 400 {VALIDATION_CODE}, got {status} {body}"
        );
        assert!(
            body.to_string().contains("nextToken"),
            "case {case:?} ({token:?}): the refusal must name the member that failed, got \
             {body}"
        );
        assert!(
            body.get("effectiveDeployments").is_none(),
            "case {case:?} ({token:?}): a refusal reports no page, got {body}"
        );
    }

    // A cursor is a token, not a turn: the one a page handed back resumes the same page
    // however often it is used.
    let first = listing(&router, "the first page of one", "?maxResults=1").await;
    let cursor = text(&first, "nextToken");
    assert!(
        !cursor.is_empty(),
        "case the first page of one: expected a cursor, got {first}"
    );
    let resumed = listing(
        &router,
        "resuming the cursor",
        &format!("?maxResults=1&nextToken={cursor}"),
    )
    .await;
    let resumed_again = listing(
        &router,
        "resuming the cursor again",
        &format!("?maxResults=1&nextToken={cursor}"),
    )
    .await;
    assert_eq!(
        resumed, resumed_again,
        "case resuming the same cursor twice: expected the same page, got {resumed} then \
         {resumed_again}"
    );
    assert_ne!(
        reported_ids(&resumed),
        reported_ids(&first),
        "case resuming the cursor: the resumed page must start after the first, got {resumed}"
    );
}

#[tokio::test]
async fn listing_a_devices_effective_deployments_changes_nothing_a_later_caller_sees() {
    // The rows differ along how many reads are interleaved with the state they report.
    for (case, reads) in [
        ("one read", 1usize),
        ("a handful of reads", 3),
        ("many reads", 7),
    ] {
        let (router, created) = fixture(3).await;

        // What a listing must leave untouched: the device itself, the fleet, the
        // deployment history and every deployment's own reader.
        let before = snapshot(&router, case, &created).await;
        let first = listing(&router, case, "").await;

        for read in 1..=reads {
            for query in ["", "?maxResults=1", "?maxResults=100"] {
                let body = listing(&router, case, query).await;
                if query.is_empty() {
                    assert_eq!(
                        body, first,
                        "case {case:?} read {read}: expected the body the first listing \
                         answered {first}, got {body}"
                    );
                }
                assert!(
                    !reported_ids(&body).is_empty(),
                    "case {case:?} read {read} ({query:?}): the deployments that reach the \
                     device must still be reported, got {body}"
                );
            }
            // A thing name no device has reported under is served an empty report, and
            // that read must not bring a device into being either.
            let (status, body) =
                list_effective_deployments(&router, "never-reported-core", "").await;
            assert_eq!(
                (status, body.get("effectiveDeployments").is_none()),
                (200, true),
                "case {case:?} read {read}: a thing name with no deployment must be served \
                 an empty report, got {status} {body}"
            );
        }

        let after = snapshot(&router, case, &created).await;
        assert_eq!(
            after, before,
            "case {case:?}: {reads} listing(s) must leave every reader answering what it \
             answered before, got {after:?}"
        );
    }
}

/// Everything a listing must leave untouched: the device's own reader, the fleet listing,
/// the deployment history and each deployment's own reader.
async fn snapshot(
    router: &MultiServiceRouter,
    case: &str,
    ids: &[String],
) -> Vec<(String, u16, Value)> {
    let mut readings = Vec::new();
    let (status, body) = get_core_device(router, DEVICE).await;
    readings.push(("GetCoreDevice".to_string(), status, body));
    let (status, body) = list_core_devices(router, "").await;
    readings.push(("ListCoreDevices".to_string(), status, body));
    let (status, body) = call(
        router,
        "GET",
        "/greengrass/v2/deployments?historyFilter=ALL",
        Vec::new(),
    )
    .await;
    readings.push(("ListDeployments".to_string(), status, body));
    // A thing name no device reported under must stay unknown to the device's own reader.
    let (status, body) = get_core_device(router, "never-reported-core").await;
    readings.push(("GetCoreDevice(unreported)".to_string(), status, body));
    for id in ids {
        let (status, body) = call(
            router,
            "GET",
            &format!("/greengrass/v2/deployments/{id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDeployment {id:?} must answer 200, got {status} {body}"
        );
        readings.push((format!("GetDeployment({id})"), status, body));
    }
    readings
}
