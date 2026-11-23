// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of ONE rule: a `maxitems`-paged listing reports in its
//! required `MaxItems` the page size the REQUEST asked for.
//!
//! The rule under test: `MaxItems` in a Route 53 listing's response is "the
//! value that you specified for the maxitems parameter" — so the caller's value
//! when it named one, and the service's default page size when it named none.
//! The ceiling an operation publishes for `maxitems` ("Amazon Route 53 returns a
//! maximum of 1000 items") bounds the page the service SERVES; it is not the
//! size an unsized request asks for, and reporting it makes a paging client that
//! derives its loop bound or its "was this a full page" test from the echoed
//! value mis-detect truncation.
//!
//! One listing asked for one page size is ONE row of that class, so the tables
//! below span it: every `maxitems`-paged listing this service serves, crossed
//! with page sizes that differ in magnitude — none at all, the smallest, one
//! inside the population, the listing's own published bound and one past it.
//!
//! Every test drives the service through the real router, using the SAME
//! harness the sibling listing suites use, so what is asserted is the
//! HTTP/restXml behaviour a Route 53 client sees.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::DEFAULT_PAGE_SIZE;

use crate::health_checks::create_health_check;
use crate::hosted_zone_records::{
    call, change_batch_body, create_zone_ok, router_holding_vpcs, Reply,
};
use crate::reusable_delegation_sets::create_set;
use crate::traffic_policies::{create_policy, document_with_start_endpoint};
use crate::traffic_policy_versions::create_version;
use crate::vpc_associations::{create_private_zone, HOME_REGION};

/// The CEILING `ListResourceRecordSets` bounds a page at, which this listing
/// publishes for itself: "ListResourceRecordSets returns up to 300 resource
/// record sets at a time in ASCII order"
/// (smithy_query(query="doc",shape="com.amazonaws.route53#ListResourceRecordSets")).
///
/// It caps what a request may ask for; the page a request naming no `maxitems`
/// is served is the service's [`DEFAULT_PAGE_SIZE`], like every other listing's.
const RECORD_SET_PAGE_BOUND: i32 = 300;

/// The VPC a private fixture zone is associated with, which the
/// `ListHostedZonesByVPC` row addresses.
const FIXTURE_VPC: &str = "vpc-0f1e2d3c4b5a69780";

/// A router whose account holds [`FIXTURE_VPC`], so the by-VPC listing below is
/// served rather than refused as another account's VPC.
fn router() -> MultiServiceRouter {
    router_holding_vpcs(&[(FIXTURE_VPC, HOME_REGION)])
}

/// One row of the listing table.
struct ListingRow {
    case: &'static str,
    /// The request path, including any query parameters the operation requires.
    uri: String,
    /// The page size the operation serves and reports for a request that names
    /// no `maxitems`.
    default_page_size: i32,
    /// The ceiling the operation publishes for `maxitems`, which bounds the
    /// page it serves but never the value it reports.
    page_bound: i32,}

/// `uri` with `maxitems` appended, joined with `?` or `&` as the path needs.
fn with_max_items(uri: &str, max_items: Option<i32>) -> String {
    match max_items {
        None => uri.to_string(),
        Some(max_items) => {
            let join = if uri.contains('?') { '&' } else { '?' };
            format!("{uri}{join}maxitems={max_items}")
        }
    }
}

/// Seed one of every resource the listings below page over, and return the
/// table of listings addressed at them.
async fn seed(router: &MultiServiceRouter) -> Vec<ListingRow> {
    // Two public zones and a private one, so every zone listing has several
    // records to page and the by-VPC listing has one to find.
    let first = create_zone_ok(router, "paging-one.example.com", "paging-one").await;
    create_zone_ok(router, "paging-two.example.net", "paging-two").await;
    create_private_zone(
        router,
        "paging-private.example.org",
        "paging-private",
        FIXTURE_VPC,
    )
    .await;

    // Two health checks.
    for index in 0..2 {
        let reply = create_health_check(
            router,
            &format!("paging-check-{index}"),
            &format!("<Type>HTTP</Type><IPAddress>192.0.2.{}</IPAddress><Port>80</Port>", index + 1),
        )
        .await;
        assert_eq!(
            reply.status, 201,
            "seeding a health check: expected 201, got {} with body {}",
            reply.status, reply.body
        );
    }

    // A traffic policy with a second version, so the version listing pages over
    // more than one.
    let created = create_policy(
        router,
        "paging-policy",
        &document_with_start_endpoint("A", "192.0.2.10"),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "seeding a traffic policy: expected 201, got {} with body {}",
        created.status, created.body
    );
    let policy_id = created
        .first("TrafficPolicy/Id")
        .unwrap_or_else(|| {
            panic!(
                "the created policy must carry TrafficPolicy/Id; body {}",
                created.body
            )
        })
        .to_string();
    let version = create_version(
        router,
        &policy_id,
        &document_with_start_endpoint("A", "192.0.2.11"),
        None,
    )
    .await;
    assert_eq!(
        version.status, 201,
        "seeding a traffic policy version: expected 201, got {} with body {}",
        version.status, version.body
    );

    // A reusable delegation set.
    let set = create_set(router, "paging-set", None).await;
    assert_eq!(
        set.status, 201,
        "seeding a reusable delegation set: expected 201, got {} with body {}",
        set.status, set.body
    );

    vec![
        ListingRow {
            case: "ListHostedZones",
            uri: "/2013-04-01/hostedzone".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "Amazon Route 53 returns a maximum of 100 items in each response."
            page_bound: 100,
        },
        ListingRow {
            case: "ListHostedZonesByName",
            uri: "/2013-04-01/hostedzonesbyname".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "Route 53 returns up to 100 items in each response."
            page_bound: 100,
        },
        ListingRow {
            case: "ListHostedZonesByVPC",
            uri: format!(
                "/2013-04-01/hostedzonesbyvpc?vpcid={FIXTURE_VPC}&vpcregion={HOME_REGION}"
            ),
            default_page_size: DEFAULT_PAGE_SIZE,
            page_bound: 100,
        },
        ListingRow {
            case: "ListHealthChecks",
            uri: "/2013-04-01/healthcheck".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "Amazon Route 53 returns a maximum of 1000 items."
            page_bound: 1_000,
        },
        ListingRow {
            case: "ListReusableDelegationSets",
            uri: "/2013-04-01/delegationset".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "If you specify a value greater than 100, Route 53 returns only
            // the first 100 reusable delegation sets."
            page_bound: 100,
        },
        ListingRow {
            case: "ListTrafficPolicies",
            uri: "/2013-04-01/trafficpolicies".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // Bounded by the account's traffic policy quota, which no listing
            // can be longer than.
            page_bound: 50,
        },
        ListingRow {
            case: "ListTrafficPolicyInstances",
            uri: "/2013-04-01/trafficpolicyinstances".to_string(),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "Route 53 returns a maximum of 100 items in each response."
            page_bound: 100,
        },
        ListingRow {
            case: "ListTrafficPolicyVersions",
            uri: format!("/2013-04-01/trafficpolicies/{policy_id}/versions"),
            default_page_size: DEFAULT_PAGE_SIZE,
            // Bounded by `TrafficPolicyVersion`'s own `range 1..1000`.
            page_bound: 1_000,
        },
        ListingRow {
            case: "ListResourceRecordSets",
            uri: format!("/2013-04-01/hostedzone/{first}/rrset"),
            default_page_size: DEFAULT_PAGE_SIZE,
            // "ListResourceRecordSets returns up to 300 resource record sets at
            // a time."
            page_bound: RECORD_SET_PAGE_BOUND,
        },
    ]
}

/// Send one listing request and assert it was served.
async fn listing(router: &MultiServiceRouter, case: &str, uri: &str) -> Reply {
    let reply = call(router, "GET", uri, "").await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from {uri}, got {} with body {}",
        reply.status, reply.body
    );
    reply
}

#[tokio::test]
async fn a_listing_asked_for_no_page_size_reports_the_default_page_size() {
    // The rule for the absent `maxitems`: the response reports the default page
    // size such a request asks for, never the operation's own truncation
    // ceiling. The rows are every `maxitems`-paged listing this service serves.
    let router = router();
    for row in seed(&router).await {
        let uri = with_max_items(&row.uri, None);
        let reply = listing(&router, row.case, &uri).await;
        let reported = reply.first("MaxItems");
        assert_eq!(
            reported,
            Some(row.default_page_size.to_string().as_str()),
            "case {:?}: a request naming no maxitems must report the default page size {}, \
             got {reported:?}; body {}",
            row.case,
            row.default_page_size,
            reply.body
        );
        // The page itself is not truncated: the fixture holds far fewer records
        // than the default page, so nothing follows it. `IsTruncated` is absent
        // from the token-paged listing, which says the same by naming no token
        // to resume at.
        assert_ne!(
            reply.first("IsTruncated"),
            Some("true"),
            "case {:?}: the whole fixture fits one default page; body {}",
            row.case,
            reply.body
        );
        for resume in ["NextMarker", "NextToken", "NextDNSName", "NextRecordName"] {
            assert!(
                reply.first(resume).is_none(),
                "case {:?}: a page that carries the whole fixture names no {resume}, got {:?}; \
                 body {}",
                row.case,
                reply.first(resume),
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn a_listing_asked_for_a_page_size_reports_the_one_it_was_asked_for() {
    // The rule for a `maxitems` the request names: it is echoed whatever its
    // magnitude — the default is only what an unsized request is served — while
    // the page the response carries stays within the operation's own bound. The
    // sizes span the smallest, one inside the population, the published bound
    // and one past it.
    let router = router();
    for row in seed(&router).await {
        for requested in [1, 2, row.page_bound, row.page_bound + 1] {
            let case = format!("{} maxitems={requested}", row.case);
            let uri = with_max_items(&row.uri, Some(requested));
            let reply = listing(&router, &case, &uri).await;
            let reported = reply.first("MaxItems");
            assert_eq!(
                reported,
                Some(requested.to_string().as_str()),
                "case {case:?}: expected the requested maxitems echoed, got {reported:?}; \
                 body {}",
                reply.body
            );
        }
    }
}

/// How many record sets the paged zone below holds past one default page.
///
/// Enough that a default page leaves records over, so a page the service
/// truncated can be told from one that carried everything — and small enough
/// that one change batch seeds the zone.
const RECORDS_PAST_A_DEFAULT_PAGE: i32 = 5;

/// How many record sets `CreateHostedZone` gives a new public zone (its apex
/// `NS` and `SOA`), which count towards the page the listing serves.
const DEFAULT_APEX_RECORDS: i32 = 2;

/// Seed a zone holding more record sets than one default page, and return its
/// id and how many record sets it holds in all.
async fn zone_longer_than_a_default_page(router: &MultiServiceRouter) -> (String, i32) {
    let held = DEFAULT_PAGE_SIZE + RECORDS_PAST_A_DEFAULT_PAGE;
    let zone_id = create_zone_ok(router, "paged-records.example.com", "paged-records").await;
    let changes: String = (0..held - DEFAULT_APEX_RECORDS)
        .map(|index| {
            format!(
                "<Change><Action>CREATE</Action><ResourceRecordSet>\
                 <Name>r{index:04}.paged-records.example.com</Name><Type>A</Type>\
                 <TTL>60</TTL><ResourceRecords><ResourceRecord><Value>192.0.2.1</Value>\
                 </ResourceRecord></ResourceRecords></ResourceRecordSet></Change>"
            )
        })
        .collect();
    let applied = call(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        &change_batch_body(&changes, None),
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "seeding {held} record sets: expected 200, got {} with body {}",
        applied.status,
        applied.body.chars().take(400).collect::<String>()
    );
    (zone_id, held)
}

/// The record sets one `ListResourceRecordSets` page carries.
fn page_length(reply: &Reply) -> i32 {
    i32::try_from(reply.all("ResourceRecordSets/ResourceRecordSet/Name").len())
        .expect("a page carries far fewer record sets than i32 can count")
}

#[tokio::test]
async fn the_record_set_listing_serves_the_page_size_it_reports() {
    // The rule where a longer-than-one-page zone makes it observable: the
    // reported `MaxItems` is the page size the request asked for — its own
    // `maxitems`, or the default page size when it named none — and the page
    // actually served, together with `IsTruncated` and `NextRecordName`, agrees
    // with it. The rows span page sizes that differ in magnitude: none at all,
    // the smallest, the default, exactly the population, this listing's
    // published ceiling and one past it.
    let router = router();
    let (zone_id, held) = zone_longer_than_a_default_page(&router).await;
    let uri = format!("/2013-04-01/hostedzone/{zone_id}/rrset");

    for requested in [
        None,
        Some(1),
        Some(DEFAULT_PAGE_SIZE),
        Some(held),
        Some(RECORD_SET_PAGE_BOUND),
        Some(RECORD_SET_PAGE_BOUND + 1),
    ] {
        let case = format!("maxitems={requested:?}");
        let reply = listing(&router, &case, &with_max_items(&uri, requested)).await;
        // What the request asked for: its own value, or the default page size.
        let asked_for = requested.unwrap_or(DEFAULT_PAGE_SIZE);
        let reported = reply.first("MaxItems");
        assert_eq!(
            reported,
            Some(asked_for.to_string().as_str()),
            "case {case:?}: expected MaxItems {asked_for}, got {reported:?}; body {}",
            reply.body.chars().take(400).collect::<String>()
        );
        // The page it was served: what it asked for, bounded by the ceiling and
        // by the records the zone holds.
        let expected = asked_for.min(RECORD_SET_PAGE_BOUND).min(held);
        assert_eq!(
            page_length(&reply),
            expected,
            "case {case:?}: expected a page of {expected} record set(s), got {}",
            page_length(&reply)
        );
        let truncated = expected < held;
        assert_eq!(
            reply.first("IsTruncated") == Some("true"),
            truncated,
            "case {case:?}: expected IsTruncated {truncated}, got {:?}",
            reply.first("IsTruncated")
        );
        assert_eq!(
            reply.first("NextRecordName").is_some(),
            truncated,
            "case {case:?}: NextRecordName is present exactly when the page was \
             truncated ({truncated}), got {:?}",
            reply.first("NextRecordName")
        );
    }
}

#[tokio::test]
async fn paging_the_record_set_listing_at_the_default_page_size_covers_the_zone_once() {
    // The rule as an invariant: a client that pages with no `maxitems` at all,
    // resuming at the `NextRecordName`/`NextRecordType` each page names, is
    // served pages of the size reported in `MaxItems` and visits every record
    // set the zone holds exactly once.
    let router = router();
    let (zone_id, held) = zone_longer_than_a_default_page(&router).await;
    let uri = format!("/2013-04-01/hostedzone/{zone_id}/rrset");

    let mut seen: Vec<String> = Vec::new();
    let mut resume: Option<(String, String)> = None;
    // One page more than the zone can need, so a listing that never terminates
    // fails rather than spins.
    let pages = (held / DEFAULT_PAGE_SIZE) + 2;
    for page_number in 0..pages {
        let case = format!("page {page_number}");
        let next = match &resume {
            Some((name, rr_type)) => format!("{uri}?name={name}&type={rr_type}"),
            None => uri.clone(),
        };
        let reply = listing(&router, &case, &next).await;
        assert_eq!(
            reply.first("MaxItems"),
            Some(DEFAULT_PAGE_SIZE.to_string().as_str()),
            "case {case:?}: a request naming no maxitems reports the default page size; \
             body {}",
            reply.body.chars().take(400).collect::<String>()
        );
        assert!(
            page_length(&reply) <= DEFAULT_PAGE_SIZE,
            "case {case:?}: a page must hold at most the reported {DEFAULT_PAGE_SIZE} \
             record set(s), got {}",
            page_length(&reply)
        );
        for (name, rr_type) in reply
            .all("ResourceRecordSets/ResourceRecordSet/Name")
            .into_iter()
            .zip(reply.all("ResourceRecordSets/ResourceRecordSet/Type"))
        {
            // A name alone does not identify a record set — the apex holds both
            // an `NS` and an `SOA` — so the pair is what must be visited once.
            seen.push(format!("{name} {rr_type}"));
        }
        resume = match (reply.first("NextRecordName"), reply.first("NextRecordType")) {
            (Some(name), Some(rr_type)) => Some((name.to_string(), rr_type.to_string())),
            _ => None,
        };
        if resume.is_none() {
            break;
        }
    }
    assert!(
        resume.is_none(),
        "paging at the default page size did not terminate within {pages} page(s)"
    );
    let mut distinct = seen.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        (seen.len(), distinct.len()),
        (held as usize, held as usize),
        "paging must cover the zone's {held} record set(s) exactly once, got {} record set(s) \
         ({} distinct)",
        seen.len(),
        distinct.len()
    );
}
