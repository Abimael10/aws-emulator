// SPDX-License-Identifier: CC0-1.0

//! The usage limit every entry of a `ListAccessGrants` page reports, driven end
//! to end over HTTP.
//!
//! The rule under test: a grant's usage limit is part of the grant, so EVERY
//! answer that reports a grant reports it — the `GetAccessGrant` of that grant
//! and each `<AccessGrant>` entry of a listing alike, with the same number on
//! both. A client reading the limit off a page therefore always finds the
//! element rather than having to tell "no limit configured" from "field not
//! returned", and can read a page without going back for each grant.
//!
//! The rows differ along what the rule generalises over: how many grants the
//! instance holds, how big a page is asked for (so an entry is read from a full
//! page, from a partial one and from a page that carries a continuation token),
//! which filter narrows the listing, the permission and the narrowing of each
//! grant, and the Region of the instance the grants belong to.
//!
//! The harness, the request builder and the grant-chain helpers are the sibling
//! suites': every assertion goes through the front door a client uses.

use crate::access_grants_data_plane::{
    create_grant, create_instance, create_location, get_grant, query_escape,
};
use crate::access_point_lifecycle::{call_in_region, Emulator, Reply, ACCOUNT, REGION};
use crate::storage_lens_configuration::leaves_of;

use service_com_amazonaws_s3control_awss3controlservicev20180820::state::{
    ACCESS_GRANT_UNCAPPED_USAGE_LIMIT, DEFAULT_LOCATION_SCOPE, PERMISSION_READ,
    PERMISSION_READWRITE, PERMISSION_WRITE,
};

/// A second Region, so the rows span the instances a listed grant can belong to.
const OTHER_REGION: &str = "eu-central-1";

/// The element the limit travels in.
const USAGE_LIMIT: &str = "UsageLimit";

/// The leaf path of one listed grant's id, and of its usage limit: an entry of
/// `AccessGrantsList` is written as `<AccessGrant>` (the member's `@xmlName`).
const LISTED_ID_PATH: &str = "AccessGrantsList/AccessGrant/AccessGrantId";
/// See [`LISTED_ID_PATH`].
const LISTED_LIMIT_PATH: &str = "AccessGrantsList/AccessGrant/UsageLimit";

/// One grant to create in an instance: how it narrows its location and the
/// permission it carries.
struct GrantCase {
    case: &'static str,
    location_scope: &'static str,
    sub_prefix: Option<&'static str>,
    permission: &'static str,
}

/// The grants the listings below are run over. No row is a renaming of another:
/// each varies the location scope, the narrowing or the permission.
fn grant_cases() -> Vec<GrantCase> {
    vec![
        GrantCase {
            case: "the default location narrowed to a bucket",
            location_scope: DEFAULT_LOCATION_SCOPE,
            sub_prefix: Some("reports/*"),
            permission: PERMISSION_READ,
        },
        GrantCase {
            case: "a bucket location narrowed to a prefix",
            location_scope: "s3://finance",
            sub_prefix: Some("quarterly/*"),
            permission: PERMISSION_WRITE,
        },
        GrantCase {
            case: "a bucket location the grant does not narrow",
            location_scope: "s3://archive/",
            sub_prefix: None,
            permission: PERMISSION_READWRITE,
        },
        GrantCase {
            case: "a location narrowed to a single object key",
            location_scope: "s3://ledger",
            sub_prefix: Some("2026/january.csv"),
            permission: PERMISSION_READ,
        },
    ]
}

/// `ListAccessGrants` in `region`, with `query` appended to its path.
async fn list(emulator: &Emulator, region: &str, query: &str) -> Reply {
    let uri = match query.is_empty() {
        true => "/v20180820/accessgrantsinstance/grants".to_string(),
        false => format!("/v20180820/accessgrantsinstance/grants?{query}"),
    };
    call_in_region(&emulator.router, region, "GET", &uri, ACCOUNT, "").await
}

/// The grants a listing reported, as (grant id, the text of the entry's usage
/// limit) in document order.
///
/// Read from the leaf PATHS rather than by scanning for an element name, so an
/// entry contributes exactly one pair and an entry that carries no limit at all
/// is reported as `None` rather than borrowing the next entry's.
fn listed_grants(reply: &Reply) -> Vec<(String, Option<String>)> {
    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    for (path, value) in leaves_of(&reply.body) {
        if path == LISTED_ID_PATH {
            entries.push((value, None));
        } else if path == LISTED_LIMIT_PATH {
            let entry = entries
                .last_mut()
                .unwrap_or_else(|| panic!("a <{USAGE_LIMIT}> outside an entry in {}", reply.body));
            entry.1 = Some(value);
        }
    }
    entries
}

/// The limit a `GetAccessGrant` reply reports, read off the one element it
/// travels in.
fn read_limit(reply: &Reply) -> Option<String> {
    reply.text(USAGE_LIMIT)
}

/// Create the instance, the locations and the grants `grant_cases` describes in
/// `region`, and answer the grant ids in creation order.
async fn provision(emulator: &Emulator, region: &str) -> Vec<String> {
    let instance = create_instance(emulator, ACCOUNT, region).await;
    assert!(
        instance.status == 200 || instance.status == 409,
        "the Access Grants instance must exist first, got {} with body {}",
        instance.status,
        instance.body
    );

    let mut ids = Vec::new();
    for case in grant_cases() {
        let location = create_location(emulator, ACCOUNT, region, case.location_scope).await;
        assert_eq!(
            location.status, 200,
            "case {:?}: the location must be registered first, got {} with body {}",
            case.case, location.status, location.body
        );
        let location_id = location
            .text("AccessGrantsLocationId")
            .unwrap_or_else(|| panic!("case {:?}: an AccessGrantsLocationId", case.case));

        let created = create_grant(
            emulator,
            ACCOUNT,
            region,
            &location_id,
            case.sub_prefix,
            case.permission,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected the grant to be created, got {} with body {}",
            case.case, created.status, created.body
        );
        ids.push(
            created
                .text("AccessGrantId")
                .unwrap_or_else(|| panic!("case {:?}: an AccessGrantId", case.case)),
        );
    }
    ids
}

/// Assert that every entry of `listing` carries the limit the grant's own read
/// reports, and that the page reported the grants `expected` names.
async fn assert_page_reports_every_limit(
    emulator: &Emulator,
    case: &str,
    region: &str,
    listing: &Reply,
    expected: usize,
) {
    assert_eq!(
        listing.status, 200,
        "case {case:?}: expected ListAccessGrants to answer 200, got {} with body {}",
        listing.status, listing.body
    );
    let entries = listed_grants(listing);
    assert_eq!(
        entries.len(),
        expected,
        "case {case:?}: expected {expected} entries on the page, got {entries:?} in body {}",
        listing.body
    );
    // One limit per entry and no more: a page of N grants carries exactly N of
    // the element, so no entry borrows another's.
    assert_eq!(
        listing.texts(USAGE_LIMIT).len(),
        expected,
        "case {case:?}: expected exactly {expected} <{USAGE_LIMIT}> elements, got body {}",
        listing.body
    );

    for (grant_id, listed_limit) in entries {
        let read = get_grant(emulator, ACCOUNT, region, &grant_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetAccessGrant({grant_id}) to answer 200, got {} with \
             body {}",
            read.status, read.body
        );
        assert_eq!(
            listed_limit,
            read_limit(&read),
            "case {case:?}: expected the entry for {grant_id} to report the limit the grant's \
             own read reports ({:?}), got {listed_limit:?} in body {}",
            read_limit(&read),
            listing.body
        );
        assert_eq!(
            listed_limit,
            Some(ACCESS_GRANT_UNCAPPED_USAGE_LIMIT.to_string()),
            "case {case:?}: expected the entry for {grant_id} to report the limit a grant \
             created without one carries, got {listed_limit:?} in body {}",
            listing.body
        );
    }
}

#[tokio::test]
async fn every_listed_grant_carries_the_limit_its_own_read_reports() {
    let emulator = Emulator::start();
    let ids = provision(&emulator, REGION).await;
    let held = ids.len();
    assert!(
        held >= 3,
        "the rows must span a page smaller than the instance's holdings, got {held} grants"
    );

    // A listing that asks for no page size reports every grant the instance
    // holds; each page size below cuts that set differently, and the filter row
    // narrows it to the grants of one permission.
    let pages: Vec<(&str, String, usize)> = vec![
        ("the whole instance, no page size asked for", String::new(), held),
        ("a page as big as the instance holds", format!("maxResults={held}"), held),
        ("a page smaller than the instance holds", "maxResults=2".to_string(), 2),
        ("a page of one, the smallest a page can be", "maxResults=1".to_string(), 1),
        ("a page bigger than the instance holds", format!("maxResults={}", held + 5), held),
        (
            "narrowed to the grants of one permission",
            format!("permission={PERMISSION_READ}"),
            grant_cases()
                .iter()
                .filter(|case| case.permission == PERMISSION_READ)
                .count(),
        ),
    ];

    for (case, query, expected) in pages {
        let listing = list(&emulator, REGION, &query).await;
        assert_page_reports_every_limit(&emulator, case, REGION, &listing, expected).await;

        // The page that stopped short hands back a token, and the entries of
        // the page it resumes carry the element just the same.
        if let Some(token) = listing.text("NextToken") {
            let resumed = match query.is_empty() {
                true => format!("nextToken={}", query_escape(&token)),
                false => format!("{query}&nextToken={}", query_escape(&token)),
            };
            let next = list(&emulator, REGION, &resumed).await;
            let remaining = held - expected;
            assert_page_reports_every_limit(
                &emulator,
                &format!("{case}: the page it resumes"),
                REGION,
                &next,
                remaining.min(expected),
            )
            .await;
        }
    }

    // Reading a listing changes nothing a later reader sees.
    let again = list(&emulator, REGION, "").await;
    assert_eq!(
        listed_grants(&again),
        listed_grants(&list(&emulator, REGION, "").await),
        "expected the listing to be stable across reads, got body {}",
        again.body
    );
}

#[tokio::test]
async fn a_listing_in_another_region_reports_its_own_grants_limits() {
    let emulator = Emulator::start();
    let ids = provision(&emulator, OTHER_REGION).await;

    let listing = list(&emulator, OTHER_REGION, "").await;
    assert_page_reports_every_limit(
        &emulator,
        "an instance in another Region",
        OTHER_REGION,
        &listing,
        ids.len(),
    )
    .await;
}
