// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests pinning `ListComponentVersions` as a pure REPORT of the versions this
//! account published, and pinning the page size it accepts to the modelled range.
//!
//! The rules under test:
//!
//! * listing the versions of a component is a report, so it changes nothing a later caller
//!   sees: however many listings are made, and whatever page size or cursor they use, every
//!   listing answers the same body, every listed version's own readers (`GetComponent` and
//!   `DescribeComponent`) still report exactly what they reported before, and the catalog
//!   `ListComponents` reports is unchanged. A cursor is a token, not a turn, so resuming
//!   the same `nextToken` twice answers the same page. Only a WRITE (publishing another
//!   version, deleting one) moves what a later listing reports, and the reads before it
//!   leave that write the state it would have found without them;
//! * `maxResults` is a `DefaultMaxResults`, whose modelled range is 1..100
//!   (`smithy_query(query="operation",operation="ListComponentVersions")`), so each
//!   published bound is served and a value one past either bound names no page and is
//!   refused as bad input — HTTP 400 `ValidationException` — rather than clamped into the
//!   range; and a refusal writes nothing, so the listing after it is the listing before it.
//!
//! The rows differ along what the rules generalise over: how many versions are published,
//! how many listings are interleaved at each point (none, one, several), the page size
//! those listings use (one entry, a partial last page, the whole collection, more than it
//! holds), and — for the range — each published bound and the value one past it on both
//! sides.
//!
//! Every assertion drives the real `restJson1` front door through the harness the sibling
//! suite `tests/all/component_versions_and_resolution.rs` owns, and is judged on the HTTP
//! status and the JSON body, so a listing served from anything but what
//! `CreateComponentVersion` persisted — or one that quietly disturbed the catalog — fails
//! here.

use serde_json::Value;
use service_com_amazonaws_greengrassv2_greengrassv2::state::MAX_PAGE_SIZE;
use smithy_server_runtime::MultiServiceRouter;

use crate::component_versions_and_resolution::{
    array, call, component_arn, emulator, error_code, get, label, listed_versions, publish, recipe,
    text, version_arn,
};

/// The code the modelled 400 carries
/// (`smithy_query(query="operation",operation="ListComponentVersions")`: `ValidationException`,
/// client, HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// The versions the fixtures below publish, greatest-version-first — the order the
/// operation documents ("Greater versions are listed first"). A row takes its first `n`
/// entries reversed, so a row of any size publishes a prefix of this set.
const VERSIONS_GREATEST_FIRST: &[&str] = &[
    "3.1.0",
    "2.0.0",
    "2.0.0-beta.2",
    "1.10.0",
    "1.2.3",
    "1.0.1",
    "1.0.0",
];

/// The path `ListComponentVersions` is bound to for one component
/// (`GET /greengrass/v2/components/{arn}/versions`).
fn versions_path(name: &str) -> String {
    format!(
        "/greengrass/v2/components/{}/versions",
        label(&component_arn(name))
    )
}

/// One `ListComponentVersions` that must answer 200, as its body.
async fn listing(router: &MultiServiceRouter, case: &str, name: &str, query: &str) -> Value {
    let path = versions_path(name);
    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    let (status, body) = get(router, &uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListComponentVersions {uri:?} must answer 200, got {status} {body}"
    );
    body
}

/// The queries one row lists through: the default page, one entry at a time, a page that
/// splits the collection, exactly the whole collection, and more than it holds. A listing
/// that disturbed the store through any of them shows up in the comparison the caller
/// makes.
fn queries(published: usize) -> Vec<(String, String)> {
    let mut rows = vec![("the default page".to_string(), String::new())];
    for page_size in [1, 2, published.max(1), published + 3] {
        rows.push((
            format!("pages of {page_size}"),
            format!("maxResults={page_size}"),
        ));
    }
    rows
}

/// Walk every page of one query and return the versions it reports, in order.
async fn walk(router: &MultiServiceRouter, case: &str, name: &str, query: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut page_query = query.to_string();
        if let Some(cursor) = &token {
            if !page_query.is_empty() {
                page_query.push('&');
            }
            page_query.push_str(&format!("nextToken={cursor}"));
        }
        let body = listing(router, case, name, &page_query).await;
        seen.extend(listed_versions(&body));
        pages += 1;
        assert!(
            pages <= VERSIONS_GREATEST_FIRST.len() + 2,
            "case {case:?}: paging {query:?} must terminate, got {seen:?}"
        );
        match body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    seen
}

/// List `reads` times through each query of [`queries`] and assert every listing answers
/// the body the first listing of that query answered, recording that body when there is
/// none yet.
///
/// That the last listing still agrees with the first is the report rule: a listing that
/// consumed, re-ordered or re-stamped anything would show up here.
async fn listings_agree(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    name: &str,
    published: usize,
    reads: usize,
    expected: &mut Vec<(String, Value)>,
) {
    for (query_case, query) in queries(published) {
        for read in 1..=reads.max(1) {
            let body = listing(router, case, name, &query).await;
            match expected.iter().find(|(stored, _)| stored == &query) {
                Some((_, first)) => assert_eq!(
                    &body, first,
                    "case {case:?} {stage} {query_case:?} read {read}: expected the body the \
                     first listing answered {first}, got {body}"
                ),
                None => expected.push((query.clone(), body)),
            }
            // Only the row's own read count is driven; the extra pass when a row asks for
            // no reads is what records the body the stage is compared against.
            if reads == 0 {
                break;
            }
        }
    }
}

/// Every published version's own readers plus the account's component catalog: what a
/// listing must leave untouched.
async fn stored(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    versions: &[String],
) -> Vec<(String, Value)> {
    let mut readings = Vec::new();
    for version in versions {
        let arn = label(&version_arn(name, version));
        for (reader, path) in [
            ("GetComponent", format!("/greengrass/v2/components/{arn}")),
            (
                "DescribeComponent",
                format!("/greengrass/v2/components/{arn}/metadata"),
            ),
        ] {
            let (status, body) = get(router, &path).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {reader} {version:?} must answer 200, got {status} {body}"
            );
            readings.push((format!("{reader} {version}"), body));
        }
    }
    let (status, catalog) = get(router, "/greengrass/v2/components").await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListComponents must answer 200, got {status} {catalog}"
    );
    readings.push(("ListComponents".to_string(), catalog));
    readings
}

/// Publish the first `count` versions of [`VERSIONS_GREATEST_FIRST`], least first, and
/// return them greatest-first — the order the listing must report them in.
async fn publish_versions(router: &MultiServiceRouter, name: &str, count: usize) -> Vec<String> {
    let greatest_first: Vec<String> = VERSIONS_GREATEST_FIRST
        .iter()
        .take(count)
        .map(|version| version.to_string())
        .collect();
    for version in greatest_first.iter().rev() {
        let (status, body) = publish(router, &recipe(name, version)).await;
        assert_eq!(
            status, 201,
            "case {version:?}: publishing must answer 201, got {status} {body}"
        );
    }
    greatest_first
}

#[tokio::test]
async fn listing_the_versions_reports_them_without_changing_them() {
    // (case, versions published, listings interleaved at each point)
    for (case, count, reads) in [
        ("one version, no listings interleaved", 1usize, 0usize),
        ("two versions, one listing at each point", 2, 1),
        ("several versions, a handful of listings", 5, 3),
        (
            "every version, many listings",
            VERSIONS_GREATEST_FIRST.len(),
            5,
        ),
    ] {
        let router = emulator();
        let name = "com.example.Reported";

        // Nothing is published yet, so every listing answers the same empty list — and none
        // of those listings may bring a version into being.
        let mut empty: Vec<(String, Value)> = Vec::new();
        listings_agree(
            &router,
            case,
            "before any version",
            name,
            count,
            reads,
            &mut empty,
        )
        .await;
        for (query, body) in &empty {
            assert_eq!(
                listed_versions(body),
                Vec::<String>::new(),
                "case {case:?}: {query:?} on a component with no version must report an empty \
                 list, got {body}"
            );
        }

        let published = publish_versions(&router, name, count).await;
        let before = stored(&router, case, name, &published).await;

        let mut recorded: Vec<(String, Value)> = Vec::new();
        listings_agree(
            &router,
            case,
            "with the versions published",
            name,
            count,
            reads,
            &mut recorded,
        )
        .await;

        // Each listing reports exactly what the publishes persisted, greatest version first,
        // through every page size — so a page size can no more drop a version than it can
        // invent one — and every entry carries the component and the version ARN the publish
        // minted for it rather than a value of the listing's own.
        for (query_case, query) in queries(count) {
            let seen = walk(&router, case, name, &query).await;
            assert_eq!(
                seen, published,
                "case {case:?} {query_case:?}: expected every published version once, \
                 greatest first, got {seen:?}"
            );
        }
        for entry in array(&listing(&router, case, name, "").await, "componentVersions") {
            let version = text(&entry, "componentVersion");
            assert_eq!(
                text(&entry, "componentName"),
                name,
                "case {case:?} {version:?}: every entry names its component, got {entry}"
            );
            assert_eq!(
                text(&entry, "arn"),
                version_arn(name, &version),
                "case {case:?} {version:?}: every entry carries the version ARN the publish \
                 minted, got {entry}"
            );
        }

        // The listings consumed nothing: every version's own reader and the account catalog
        // still answer exactly what they answered before them.
        let after = stored(&router, case, name, &published).await;
        assert_eq!(
            after, before,
            "case {case:?}: listing must leave every version's readers answering what they \
             answered before"
        );

        // A cursor is a token, not a turn: resuming the same `nextToken` twice answers the
        // same page. Only a collection with more than one entry hands one back.
        if count > 1 {
            let first_page = listing(&router, case, name, "maxResults=1").await;
            let cursor = text(&first_page, "nextToken");
            assert!(
                !cursor.is_empty(),
                "case {case:?}: a page of one over {count} versions must hand back a cursor, \
                 got {first_page}"
            );
            let resumed = listing(
                &router,
                case,
                name,
                &format!("maxResults=1&nextToken={cursor}"),
            )
            .await;
            let resumed_again = listing(
                &router,
                case,
                name,
                &format!("maxResults=1&nextToken={cursor}"),
            )
            .await;
            assert_eq!(
                resumed, resumed_again,
                "case {case:?}: resuming the same cursor twice must answer the same page, got \
                 {resumed} then {resumed_again}"
            );
            assert_ne!(
                listed_versions(&resumed),
                listed_versions(&first_page),
                "case {case:?}: the resumed page must start after the first, got {resumed}"
            );
        }

        // Only a WRITE moves what a later listing reports — and the listings before it left
        // this delete the version it would have found without them.
        let removed = published
            .first()
            .expect("the fixture published a version")
            .clone();
        let (status, body) = call(
            &router,
            "DELETE",
            &format!(
                "/greengrass/v2/components/{}",
                label(&version_arn(name, &removed))
            ),
            Vec::new(),
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: deleting {removed:?} must answer 204, got {status} {body}"
        );

        let expected: Vec<String> = published.iter().skip(1).cloned().collect();
        for (query_case, query) in queries(count) {
            let seen = walk(&router, case, name, &query).await;
            assert_eq!(
                seen, expected,
                "case {case:?} {query_case:?}: after the delete the listing must report \
                 {expected:?}, got {seen:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_size_outside_the_modelled_range_is_refused() {
    let router = emulator();
    let name = "com.example.Paging";
    let published = publish_versions(&router, name, VERSIONS_GREATEST_FIRST.len()).await;

    // Both published bounds of the modelled 1..100 range are served, plus a value inside
    // it: the smallest page splits the collection, the largest holds all of it.
    for page_size in [1_i64, 2, i64::from(MAX_PAGE_SIZE)] {
        let case = format!("maxResults={page_size}");
        let body = listing(&router, &case, name, &case).await;
        let page = listed_versions(&body);
        assert!(
            i64::try_from(page.len()).unwrap_or(i64::MAX) <= page_size,
            "case {case:?}: a page carries at most that many entries, got {page:?}"
        );
        let seen = walk(&router, &case, name, &case).await;
        assert_eq!(
            seen, published,
            "case {case:?}: the pages together must be every published version, greatest \
             first, got {seen:?}"
        );
    }

    let before = listing(&router, "before the refusals", name, "").await;

    // One past each published bound, and further out on both sides: each names no page, so
    // each is bad input rather than a clamped listing.
    let refused = [
        ("one below the lower bound", "0".to_string()),
        ("well below the lower bound", "-7".to_string()),
        (
            "one past the upper bound",
            (i64::from(MAX_PAGE_SIZE) + 1).to_string(),
        ),
        (
            "well past the upper bound",
            (i64::from(MAX_PAGE_SIZE) * 10).to_string(),
        ),
    ];
    for (case, value) in refused {
        let (status, body) = get(
            &router,
            &format!("{}?maxResults={value}", versions_path(name)),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?} ({value:?}): expected 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_CODE,
            "case {case:?} ({value:?}): expected {VALIDATION_CODE}, got {body}"
        );
        assert!(
            !text(&body, "message").is_empty(),
            "case {case:?} ({value:?}): the refusal must say what was wrong with the \
             request, got {body}"
        );
        assert!(
            array(&body, "componentVersions").is_empty(),
            "case {case:?} ({value:?}): a refusal reports no list, got {body}"
        );
    }

    // A refusal writes nothing, so the listing after them is the listing before them.
    let after = listing(&router, "after the refusals", name, "").await;
    assert_eq!(
        after, before,
        "case the refused page sizes: the published versions must be untouched, got {after}"
    );
}
