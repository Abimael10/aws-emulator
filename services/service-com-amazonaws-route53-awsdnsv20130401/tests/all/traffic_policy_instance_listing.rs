// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of `ListTrafficPolicyInstances` and of the SAME listing
//! held to one hosted zone, `ListTrafficPolicyInstancesByHostedZone`, or to one
//! version of one traffic policy, `ListTrafficPolicyInstancesByPolicy`.
//!
//! The rule under test: the operation REPORTS the traffic policy instances the
//! account holds at the moment of the call, read from the service's state in the
//! order given by the hosted zone each instance's record sets live in, then the
//! DNS name they answer for, then their record type — the triple the listing's
//! markers carry — as a page that starts at the instance
//! `(HostedZoneIdMarker, TrafficPolicyInstanceNameMarker,
//! TrafficPolicyInstanceTypeMarker)` name, carries at most `MaxItems`
//! instances, echoes that `maxitems` back and names in those three markers the
//! first instance of the next page. So the pages concatenate to the whole
//! population exactly once, every listed instance reports what
//! `GetTrafficPolicyInstance` reports for it, and nothing a caller reads changes
//! what a later caller sees.
//!
//! The tables therefore vary the axes the rule generalises over rather than
//! renaming an instance: the POPULATION (none, then one more instance at a
//! time), the HOSTED ZONE the instance lives in (two of them, so the listing is
//! account-wide), the DNS NAME and its depth, the RECORD TYPE (including two
//! instances that differ only in it, which the type marker alone tells apart),
//! the TTL, the PAGE SIZE (every size from one to one past the population, plus
//! the published page bound and one past it) and the MARKERS (every position in
//! the listing, each component of the triple on its own, and markers the account
//! holds no instance under).
//!
//! `ListTrafficPolicyInstancesByHostedZone` reports that same listing RESTRICTED
//! to the hosted zone its `id` names, so its page starts at the
//! `(TrafficPolicyInstanceNameMarker, TrafficPolicyInstanceTypeMarker)` pair —
//! the zone is already fixed, so there is no zone marker — and a zone the
//! account does not hold is refused rather than reported as empty. Its tables
//! vary the ZONE listed (each zone of the population, a zone holding no
//! instance at all, and a zone the account does not hold), the SPELLING of the
//! zone id, the page size and the markers.
//!
//! `ListTrafficPolicyInstancesByPolicy` reports that same listing RESTRICTED to
//! the instances created from the ONE traffic policy version its
//! `(TrafficPolicyId, TrafficPolicyVersion)` pair names, so it keeps all three
//! markers and a pair the account holds no traffic policy version under is
//! refused rather than reported as empty. Its tables vary the PAIR listed (two
//! versions of one policy, a policy of another record type, and a version no
//! instance was created from), the population, the page size and the markers —
//! including markers carried by an instance of ANOTHER pair.
//!
//! Every test drives the service through the real router, using the SAME
//! harness the other traffic-policy suites use, so what is asserted is the
//! HTTP/restXml behaviour a Route 53 client sees.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::ops::types::R_R_TYPE_VALUES;
use service_com_amazonaws_route53_awsdnsv20130401::state::{
    DEFAULT_PAGE_SIZE, NON_POSITIVE_PAGE_SIZE_MESSAGE, TRAFFIC_POLICY_INSTANCE_APPLIED,
    TRAFFIC_POLICY_INSTANCE_CREATING,
};

use crate::hosted_zone_records::{call, create_zone_ok, router, Reply};
use crate::traffic_policies::{create_policy, document_with_start_endpoint, policy_of};
use crate::traffic_policy_deletion::delete_policy;
use crate::traffic_policy_instances::{create_instance, get_instance, instance_count};
use crate::traffic_policy_not_found_envelope::{
    assert_identity_only, assert_no_such_traffic_policy,
};
use crate::traffic_policy_versions::create_version;

/// The page this listing publishes for itself: "Route 53 returns a maximum of
/// 100 items in each response. If you have a lot of traffic policy instances,
/// you can use the MaxItems parameter to list them in groups of up to 100"
/// (smithy_query(query="doc",shape="com.amazonaws.route53#ListTrafficPolicyInstances")).
const PAGE_BOUND: i32 = 100;

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Send one `ListTrafficPolicyInstances` request, with `query` as its query
/// string.
async fn list_instances(router: &MultiServiceRouter, query: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/2013-04-01/trafficpolicyinstances{query}"),
        "",
    )
    .await
}

/// List with `query`, asserting the call succeeded.
async fn listing(router: &MultiServiceRouter, case: &str, query: &str) -> Reply {
    let reply = list_instances(router, query).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ListTrafficPolicyInstances, got {} with body {}",
        reply.status, reply.body
    );
    reply
}

// ---------------------------------------------------------------------------
// What a reply reports
// ---------------------------------------------------------------------------

/// One `TrafficPolicyInstance` as the wire reports it, apart from its `State`:
/// the state follows the clock, so it is asserted on its own rather than
/// compared for equality.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceView {
    id: String,
    hosted_zone_id: String,
    name: String,
    ttl: String,
    traffic_policy_id: String,
    traffic_policy_version: String,
    traffic_policy_type: String,
}

/// Every instance a reply reports, in the order it reports them.
///
/// Each member read here is required of `TrafficPolicyInstance`, so a reply
/// that reports one instance whole reports as many of each member as it has
/// instances; anything else is a malformed listing rather than a different
/// order.
fn instances_of(reply: &Reply, case: &str) -> Vec<InstanceView> {
    let pick = |member: &str| reply.all(&format!("TrafficPolicyInstance/{member}"));
    let ids = pick("Id");
    let zones = pick("HostedZoneId");
    let names = pick("Name");
    let ttls = pick("TTL");
    let policies = pick("TrafficPolicyId");
    let versions = pick("TrafficPolicyVersion");
    let types = pick("TrafficPolicyType");
    let widths = (
        zones.len(),
        names.len(),
        ttls.len(),
        policies.len(),
        versions.len(),
        types.len(),
    );
    let wanted = (
        ids.len(),
        ids.len(),
        ids.len(),
        ids.len(),
        ids.len(),
        ids.len(),
    );
    assert_eq!(
        widths, wanted,
        "case {case:?}: every listed instance must be reported whole, got {widths:?} beside \
         {} id(s); body {}",
        ids.len(),
        reply.body
    );
    (0..ids.len())
        .map(|at| InstanceView {
            id: ids[at].to_string(),
            hosted_zone_id: zones[at].to_string(),
            name: names[at].to_string(),
            ttl: ttls[at].to_string(),
            traffic_policy_id: policies[at].to_string(),
            traffic_policy_version: versions[at].to_string(),
            traffic_policy_type: types[at].to_string(),
        })
        .collect()
}

/// The markers of a page: `None` once the listing has ended, where there is no
/// instance left to resume at.
fn next_markers(reply: &Reply) -> Option<(String, String, String)> {
    let zone = reply.first("HostedZoneIdMarker")?;
    let name = reply.first("TrafficPolicyInstanceNameMarker")?;
    let rr_type = reply.first("TrafficPolicyInstanceTypeMarker")?;
    Some((zone.to_string(), name.to_string(), rr_type.to_string()))
}

/// The markers that resume a listing at `instance`, as a query string.
fn marker_query(instance: &InstanceView, max_items: Option<usize>) -> String {
    let size = max_items.map_or_else(String::new, |size| format!("&maxitems={size}"));
    format!(
        "?hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype={}{size}",
        instance.hosted_zone_id, instance.name, instance.traffic_policy_type
    )
}

/// The place an instance takes in the listing: the hosted zone it is in, the
/// DNS name with its labels REVERSED (the order Route 53 reports DNS names in),
/// then the record type.
fn listing_key(instance: &InstanceView) -> (String, String, String) {
    let labels = instance.name.trim_end_matches('.').to_ascii_lowercase();
    let reversed: Vec<&str> = labels.split('.').rev().collect();
    (
        instance.hosted_zone_id.clone(),
        format!("{}.", reversed.join(".")),
        instance.traffic_policy_type.clone(),
    )
}

/// `created` in the order the listing has to report them in.
fn in_listing_order(created: &[InstanceView]) -> Vec<InstanceView> {
    let mut ordered = created.to_vec();
    ordered.sort_by_key(listing_key);
    ordered
}

// ---------------------------------------------------------------------------
// The population
// ---------------------------------------------------------------------------

/// One row of the population: which of the test's zones the instance lives in,
/// the DNS name its record sets answer for, their record type and TTL.
struct InstanceRow {
    case: &'static str,
    /// Index into the zones the test created.
    zone: usize,
    name: &'static str,
    /// The canonical form of that name, which is what a response reports.
    expect_name: &'static str,
    rr_type: &'static str,
    ttl: i64,
}

/// The zone names the population lives across.
const ZONE_NAMES: [&str; 2] = ["example.com", "example.net"];

/// The population every test in this suite lists: instances across two hosted
/// zones, at names of differing depth and spelling, of differing record types —
/// including two that differ ONLY in that type — and with differing TTLs.
fn population() -> Vec<InstanceRow> {
    vec![
        InstanceRow {
            case: "an A instance at a subdomain of the first zone",
            zone: 0,
            name: "www.example.com",
            expect_name: "www.example.com.",
            rr_type: "A",
            ttl: 60,
        },
        InstanceRow {
            case: "an AAAA instance at that same name, told apart by its type",
            zone: 0,
            name: "WWW.Example.com",
            expect_name: "www.example.com.",
            rr_type: "AAAA",
            ttl: 0,
        },
        InstanceRow {
            case: "a CNAME instance earlier in the zone's DNS order",
            zone: 0,
            name: "alpha.example.com.",
            expect_name: "alpha.example.com.",
            rr_type: "CNAME",
            ttl: 86_400,
        },
        InstanceRow {
            case: "a TXT instance two labels deep",
            zone: 0,
            name: "deep.sub.example.com",
            expect_name: "deep.sub.example.com.",
            rr_type: "TXT",
            ttl: 300,
        },
        InstanceRow {
            case: "an A instance in the second zone",
            zone: 1,
            name: "www.example.net",
            expect_name: "www.example.net.",
            rr_type: "A",
            ttl: 120,
        },
        InstanceRow {
            case: "an MX instance in the second zone",
            zone: 1,
            name: "mail.example.net",
            expect_name: "mail.example.net.",
            rr_type: "MX",
            ttl: 900,
        },
    ]
}

/// A value one record set of `rr_type` can carry, so each row's policy document
/// is one the service accepts.
fn value_for(rr_type: &str) -> &'static str {
    match rr_type {
        "A" => "192.0.2.1",
        "AAAA" => "2001:db8::1",
        "CNAME" => "target.example.org",
        "MX" => "10 mail.example.org",
        "TXT" => "v=spf1 ~all",
        _ => "192.0.2.1",
    }
}

/// Create the two zones every test lists across, and return their bare ids.
async fn create_zones(router: &MultiServiceRouter, suffix: &str) -> Vec<String> {
    let mut zones = Vec::new();
    for (index, name) in ZONE_NAMES.iter().enumerate() {
        zones.push(create_zone_ok(router, name, &format!("{suffix}-{index}")).await);
    }
    zones
}

/// Create the instance `row` describes and return what every listing of the
/// account must then report for it.
async fn create_row(
    router: &MultiServiceRouter,
    zones: &[String],
    row: &InstanceRow,
    nonce: usize,
) -> InstanceView {
    let created = create_policy(
        router,
        &format!("instance-listing-policy-{nonce}"),
        &document_with_start_endpoint(row.rr_type, value_for(row.rr_type)),
        Some(row.case),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {:?}: expected 201 from CreateTrafficPolicy, got {} with body {}",
        row.case, created.status, created.body
    );
    let policy = policy_of(&created, row.case).id;

    let zone = &zones[row.zone];
    let instance = create_instance(router, zone, row.name, row.ttl, &policy, 1).await;
    assert_eq!(
        instance.status, 201,
        "case {:?}: expected 201 from CreateTrafficPolicyInstance, got {} with body {}",
        row.case, instance.status, instance.body
    );
    let answered = instances_of(&instance, row.case);
    assert_eq!(
        answered.len(),
        1,
        "case {:?}: a create answers with exactly one instance; body {}",
        row.case,
        instance.body
    );
    let held = answered[0].clone();
    assert_eq!(
        (held.hosted_zone_id.as_str(), held.name.as_str()),
        (zone.as_str(), row.expect_name),
        "case {:?}: the create must persist the instance in the zone and at the canonical \
         name it was asked for, got {held:?}; body {}",
        row.case,
        instance.body
    );
    held
}

/// Create every row of [`population`], in order, and return what the listing
/// must report for them, in the order the listing reports them in.
async fn populate(router: &MultiServiceRouter, zones: &[String]) -> Vec<InstanceView> {
    let mut created = Vec::new();
    for (nonce, row) in population().iter().enumerate() {
        created.push(create_row(router, zones, row, nonce).await);
    }
    in_listing_order(&created)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_reports_every_instance_the_account_holds() {
    // The population is the varying axis: none, then one more instance after
    // every row — so the listing is asserted against a set that grows by an
    // instance in another zone, at another name or of another type each time.
    // Every row's record type is one the model's own `RRType` enum names.
    for row in &population() {
        assert!(
            R_R_TYPE_VALUES.contains(&row.rr_type),
            "case {:?}: {} is not a modelled RRType",
            row.case,
            row.rr_type
        );
    }

    let router = router();
    let zones = create_zones(&router, "instance-listing").await;
    let mut created: Vec<InstanceView> = Vec::new();

    for (nonce, row) in population().iter().enumerate() {
        let case = format!(
            "{} instance(s), before creating {}",
            created.len(),
            row.case
        );
        let reply = listing(&router, &case, "").await;
        let listed = instances_of(&reply, &case);
        assert_eq!(
            listed,
            in_listing_order(&created),
            "case {case:?}: the listing must report every instance the account holds, by \
             hosted zone, then DNS name, then record type; body {}",
            reply.body
        );
        assert_ne!(
            reply.first("IsTruncated"),
            Some("true"),
            "case {case:?}: a listing that fits one page must not be truncated; body {}",
            reply.body
        );
        assert_eq!(
            next_markers(&reply),
            None,
            "case {case:?}: an untruncated listing names no instance to resume at; body {}",
            reply.body
        );
        assert_eq!(
            reply.first("MaxItems"),
            Some(DEFAULT_PAGE_SIZE.to_string()).as_deref(),
            "case {case:?}: a request naming no maxitems asks for the default page; body {}",
            reply.body
        );

        // Every listed instance is cross-checked against what
        // GetTrafficPolicyInstance reports for the same id, so the listing is
        // proved against another reader of the same state.
        for (at, instance) in listed.iter().enumerate() {
            let read = get_instance(&router, &instance.id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the listed instance {} must be readable, got {} with body {}",
                instance.id, read.status, read.body
            );
            let reported = instances_of(&read, &case);
            assert_eq!(
                reported.first(),
                Some(instance),
                "case {case:?}: instance {at} must be listed as GetTrafficPolicyInstance \
                 reports it; body {}",
                read.body
            );
            let state = read.first("TrafficPolicyInstance/State").unwrap_or("");
            assert!(
                [
                    TRAFFIC_POLICY_INSTANCE_CREATING,
                    TRAFFIC_POLICY_INSTANCE_APPLIED
                ]
                .contains(&state),
                "case {case:?}: instance {at} reported the state {state:?}; body {}",
                read.body
            );
        }

        created.push(create_row(&router, &zones, row, nonce).await);
    }
}

#[tokio::test]
async fn traffic_policy_instance_paging_covers_the_listing_exactly_once() {
    // The rule as an invariant: a page holds at most MaxItems instances and the
    // three markers name where the next page starts, so the pages concatenate
    // to the full listing for EVERY page size. The page size is the varying
    // axis.
    let router = router();
    let zones = create_zones(&router, "instance-listing-paging").await;
    let all = populate(&router, &zones).await;

    let unpaged = listing(&router, "the whole listing", "").await;
    assert_eq!(
        instances_of(&unpaged, "the whole listing"),
        all,
        "the whole listing must report every created instance in listing order; body {}",
        unpaged.body
    );

    for page_size in 1..=all.len() + 1 {
        let mut walked: Vec<InstanceView> = Vec::new();
        let mut resume: Option<InstanceView> = None;
        let mut pages = 0;
        loop {
            let case = format!("maxitems={page_size}, resuming at {resume:?}");
            let query = match &resume {
                Some(at) => marker_query(at, Some(page_size)),
                None => format!("?maxitems={page_size}"),
            };
            let page = listing(&router, &case, &query).await;
            assert_eq!(
                page.first("MaxItems"),
                Some(page_size.to_string()).as_deref(),
                "case {case:?}: the response must echo the request's maxitems; body {}",
                page.body
            );
            let listed = instances_of(&page, &case);
            assert!(
                listed.len() <= page_size,
                "case {case:?}: a page held {} instance(s); body {}",
                listed.len(),
                page.body
            );
            walked.extend(listed);
            pages += 1;
            assert!(
                pages <= all.len() + 2,
                "case {case:?}: paging did not terminate"
            );
            if page.first("IsTruncated") != Some("true") {
                assert_eq!(
                    next_markers(&page),
                    None,
                    "case {case:?}: a final page names no instance to resume at; body {}",
                    page.body
                );
                break;
            }
            let markers = next_markers(&page).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a truncated page must name the instance the next page \
                     starts at; body {}",
                    page.body
                )
            });
            let next = all.get(walked.len()).unwrap_or_else(|| {
                panic!("case {case:?}: a truncated page must leave an instance to resume at")
            });
            assert_eq!(
                markers,
                (
                    next.hosted_zone_id.clone(),
                    next.name.clone(),
                    next.traffic_policy_type.clone()
                ),
                "case {case:?}: the markers must name the first instance of the next page; \
                 body {}",
                page.body
            );
            resume = Some(next.clone());
        }
        assert_eq!(
            walked, all,
            "case maxitems={page_size}: paging must cover the listing exactly once"
        );
    }
}

#[tokio::test]
async fn the_markers_resume_the_listing_at_the_instance_they_name() {
    // The markers name "the first traffic policy instance in the next group", so
    // a listing resumed at any position reports exactly the tail from that
    // position. Every position is a row; so is each component of the triple on
    // its own, which positions the first instance carrying it.
    let router = router();
    let zones = create_zones(&router, "instance-listing-markers").await;
    let all = populate(&router, &zones).await;
    assert!(
        all.len() > 1,
        "the suite needs several instances to resume at, got {}",
        all.len()
    );

    for (at, instance) in all.iter().enumerate() {
        let case = format!("the full marker triple of instance {at}");
        let page = listing(&router, &case, &marker_query(instance, None)).await;
        assert_eq!(
            instances_of(&page, &case),
            all[at..].to_vec(),
            "case {case:?}: the listing must resume at the instance the markers name; body {}",
            page.body
        );
    }

    // One component of the triple on its own names the FIRST instance carrying
    // it, which is where the listing then resumes.
    let partial = [
        ("the hosted zone alone", "hostedzoneid", 0),
        ("the DNS name alone", "trafficpolicyinstancename", 1),
        ("the record type alone", "trafficpolicyinstancetype", 2),
    ];
    for (case, parameter, component) in partial {
        for instance in &all {
            let value = match component {
                0 => instance.hosted_zone_id.clone(),
                1 => instance.name.clone(),
                _ => instance.traffic_policy_type.clone(),
            };
            let case = format!("{case}: {parameter}={value}");
            let page = listing(&router, &case, &format!("?{parameter}={value}")).await;
            let expected_at = match component {
                0 => all
                    .iter()
                    .position(|held| held.hosted_zone_id == value)
                    .expect("the value came from an instance of the listing"),
                1 => all
                    .iter()
                    .position(|held| held.name == value)
                    .expect("the value came from an instance of the listing"),
                _ => all
                    .iter()
                    .position(|held| held.traffic_policy_type == value)
                    .expect("the value came from an instance of the listing"),
            };
            assert_eq!(
                instances_of(&page, &case),
                all[expected_at..].to_vec(),
                "case {case:?}: the listing must resume at the first instance the marker \
                 names; body {}",
                page.body
            );
        }
    }
}

#[tokio::test]
async fn markers_naming_no_instance_of_the_account_are_refused() {
    // Markers position the listing AT an instance, so markers the account holds
    // no instance under name no position in the listing: the request is refused
    // rather than answered from somewhere else. The rows are a marker triple
    // whose zone, whose name and whose type is one no instance carries, plus
    // three parts that each name an instance but never the same one.
    let router = router();
    let zones = create_zones(&router, "instance-listing-unknown").await;
    let all = populate(&router, &zones).await;
    // An instance picked by WHAT IT IS rather than by its place in the listing,
    // which depends on the ids the zones were minted with: the A instance at
    // `www.example.com.`, the name a second instance carries at another type and
    // a third in another zone.
    let held = all
        .iter()
        .find(|instance| instance.name == "www.example.com." && instance.traffic_policy_type == "A")
        .expect("the population holds an A instance at www.example.com.")
        .clone();

    let rows = [
        (
            "a hosted zone the account holds no instance in",
            format!(
                "?hostedzoneid=Z0000000000000&trafficpolicyinstancename={}\
                 &trafficpolicyinstancetype={}",
                held.name, held.traffic_policy_type
            ),
        ),
        (
            "a DNS name no instance answers for",
            format!(
                "?hostedzoneid={}&trafficpolicyinstancename=nobody.example.com.\
                 &trafficpolicyinstancetype={}",
                held.hosted_zone_id, held.traffic_policy_type
            ),
        ),
        (
            "a record type no instance of that name carries",
            format!(
                "?hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype=SRV",
                held.hosted_zone_id, held.name
            ),
        ),
        (
            "a name and a type that belong to different instances",
            format!(
                "?hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype=MX",
                held.hosted_zone_id, held.name
            ),
        ),
        (
            "a name alone that no instance answers for",
            "?trafficpolicyinstancename=nobody.example.org.".to_string(),
        ),
    ];

    for (case, query) in &rows {
        let reply = list_instances(&router, query).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "NoSuchTrafficPolicyInstance",
            "case {case:?}: expected the modelled NoSuchTrafficPolicyInstance, got {:?}; \
             body {}",
            reply.error_code(),
            reply.body
        );
    }

    // The refusals left the listing exactly as it was.
    let after = listing(&router, "after the refusals", "").await;
    assert_eq!(
        instances_of(&after, "after the refusals"),
        all,
        "a refused listing must leave the account's instances alone; body {}",
        after.body
    );
}

#[tokio::test]
async fn the_listing_echoes_the_page_size_it_was_asked_for() {
    // `MaxItems` in the response is "the value that you specified for the
    // MaxItems parameter", so it is echoed whatever the caller sent — while the
    // page itself never exceeds the 100 items this listing publishes. The rows
    // are sizes below the population, the published bound and one past it.
    let router = router();
    let zones = create_zones(&router, "instance-listing-page-size").await;
    let all = populate(&router, &zones).await;

    for requested in [1, 2, PAGE_BOUND, PAGE_BOUND + 1] {
        let case = format!("maxitems={requested}");
        let page = listing(&router, &case, &format!("?maxitems={requested}")).await;
        assert_eq!(
            page.first("MaxItems"),
            Some(requested.to_string()).as_deref(),
            "case {case:?}: expected the requested maxitems echoed, got {:?}; body {}",
            page.first("MaxItems"),
            page.body
        );
        let listed = instances_of(&page, &case);
        let wanted = (requested as usize).min(PAGE_BOUND as usize).min(all.len());
        assert_eq!(
            listed.len(),
            wanted,
            "case {case:?}: expected {wanted} instance(s), got {}; body {}",
            listed.len(),
            page.body
        );
        assert_eq!(
            page.first("IsTruncated") == Some("true"),
            listed.len() < all.len(),
            "case {case:?}: IsTruncated must say whether more instances remain; body {}",
            page.body
        );
    }
}

#[tokio::test]
async fn listing_the_instances_leaves_the_account_exactly_as_it_was() {
    // The rule is a report: repeating it answers the same listing, creates and
    // consumes nothing, and the population it reports still moves with a later
    // create.
    let router = router();
    let zones = create_zones(&router, "instance-listing-reads").await;
    let all = populate(&router, &zones).await;
    let before = instance_count(&router).await;

    for attempt in 1..=3 {
        let case = format!("listing {attempt}");
        assert_eq!(
            instances_of(&listing(&router, &case, "").await, &case),
            all,
            "case {case:?}: a read must report what the first read did"
        );
        assert_eq!(
            instance_count(&router).await,
            before,
            "case {case:?}: listing must not change how many instances the account holds"
        );
    }

    // A paged walk is a read as well: paging through the listing changes
    // neither the population nor what an unpaged read reports.
    let mut resume: Option<InstanceView> = None;
    loop {
        let case = format!("a paged read resuming at {resume:?}");
        let query = match &resume {
            Some(at) => marker_query(at, Some(1)),
            None => "?maxitems=1".to_string(),
        };
        let page = listing(&router, &case, &query).await;
        if page.first("IsTruncated") != Some("true") {
            break;
        }
        let markers = next_markers(&page).expect("a truncated page names the next instance");
        resume = all
            .iter()
            .find(|held| {
                (
                    held.hosted_zone_id.as_str(),
                    held.name.as_str(),
                    held.traffic_policy_type.as_str(),
                ) == (markers.0.as_str(), markers.1.as_str(), markers.2.as_str())
            })
            .cloned();
        assert!(
            resume.is_some(),
            "case {case:?}: the markers must name an instance the account holds; body {}",
            page.body
        );
    }
    assert_eq!(
        instance_count(&router).await,
        before,
        "paging through the listing must not change how many instances the account holds"
    );

    let added = create_row(
        &router,
        &zones,
        &InstanceRow {
            case: "an instance created after the listing",
            zone: 1,
            name: "late.example.net",
            expect_name: "late.example.net.",
            rr_type: "A",
            ttl: 45,
        },
        population().len(),
    )
    .await;
    let mut expected = all.clone();
    expected.push(added);
    let expected = in_listing_order(&expected);
    let case = "after a later create";
    assert_eq!(
        instances_of(&listing(&router, case, "").await, case),
        expected,
        "case {case:?}: the listing must follow the state the account is in"
    );
}

// ---------------------------------------------------------------------------
// ListTrafficPolicyInstancesByHostedZone: the same listing, held to one zone
// ---------------------------------------------------------------------------

/// Send one `ListTrafficPolicyInstancesByHostedZone` request for the hosted zone
/// `zone`, with `query` appended to its query string.
async fn list_zone_instances(router: &MultiServiceRouter, zone: &str, query: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/2013-04-01/trafficpolicyinstances/hostedzone?id={zone}{query}"),
        "",
    )
    .await
}

/// List one zone's traffic policy instances, asserting the call succeeded.
async fn zone_listing(router: &MultiServiceRouter, case: &str, zone: &str, query: &str) -> Reply {
    let reply = list_zone_instances(router, zone, query).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ListTrafficPolicyInstancesByHostedZone, got {} with \
         body {}",
        reply.status, reply.body
    );
    reply
}

/// The markers of a per-zone page: `None` once the listing has ended, where
/// there is no instance left to resume at. The zone is fixed by the request, so
/// the pair of the name and the type is the whole of what a per-zone page names.
fn next_zone_markers(reply: &Reply) -> Option<(String, String)> {
    let name = reply.first("TrafficPolicyInstanceNameMarker")?;
    let rr_type = reply.first("TrafficPolicyInstanceTypeMarker")?;
    Some((name.to_string(), rr_type.to_string()))
}

/// The markers that resume a per-zone listing at `instance`, as a query string.
fn zone_marker_query(instance: &InstanceView, max_items: Option<usize>) -> String {
    let size = max_items.map_or_else(String::new, |size| format!("&maxitems={size}"));
    format!(
        "&trafficpolicyinstancename={}&trafficpolicyinstancetype={}{size}",
        instance.name, instance.traffic_policy_type
    )
}

/// The instances of `listed` whose record sets live in the hosted zone `zone`,
/// in the order they are listed in.
fn in_zone(listed: &[InstanceView], zone: &str) -> Vec<InstanceView> {
    listed
        .iter()
        .filter(|instance| instance.hosted_zone_id == zone)
        .cloned()
        .collect()
}

/// The fixed form this service words a missing hosted zone in, with the bare id
/// the request asked about filled in (`crate::state::no_such_hosted_zone`).
fn missing_zone_message(bare_id: &str) -> String {
    format!("No hosted zone found with ID: {bare_id}")
}

#[tokio::test]
async fn the_per_zone_listing_reports_the_instances_of_the_zone_it_names() {
    // The varying axis is the ZONE listed: each zone of the population, a zone
    // of the same account holding no instance at all, and each of those in both
    // spellings a zone id is accepted in.
    let router = router();
    let zones = create_zones(&router, "zone-instance-listing").await;
    let all = populate(&router, &zones).await;
    let empty = create_zone_ok(&router, "empty.example.org", "zone-instance-listing-empty").await;

    let mut listed_zones: Vec<&String> = zones.iter().chain([&empty]).collect();
    // The account-wide listing orders by hosted zone id first, so the per-zone
    // listings concatenate to it when they are taken in that order.
    listed_zones.sort();

    let mut concatenated: Vec<InstanceView> = Vec::new();
    for zone in &listed_zones {
        for spelling in [zone.to_string(), format!("/hostedzone/{zone}")] {
            let case = format!("the zone {zone} named as {spelling}");
            let reply = zone_listing(&router, &case, &spelling, "").await;
            let listed = instances_of(&reply, &case);
            assert_eq!(
                listed,
                in_zone(&all, zone),
                "case {case:?}: the listing must report every instance of that zone and no \
                 other, by DNS name then record type; body {}",
                reply.body
            );
            assert_ne!(
                reply.first("IsTruncated"),
                Some("true"),
                "case {case:?}: a listing that fits one page must not be truncated; body {}",
                reply.body
            );
            assert_eq!(
                next_zone_markers(&reply),
                None,
                "case {case:?}: an untruncated listing names no instance to resume at; body {}",
                reply.body
            );
            assert_eq!(
                reply.first("MaxItems"),
                Some(DEFAULT_PAGE_SIZE.to_string()).as_deref(),
                "case {case:?}: a request naming no maxitems asks for the default page; body {}",
                reply.body
            );

            // Every listed instance is cross-checked against what
            // GetTrafficPolicyInstance reports for the same id, so the listing
            // is proved against another reader of the same state.
            for (at, instance) in listed.iter().enumerate() {
                let read = get_instance(&router, &instance.id).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the listed instance {} must be readable, got {} with body {}",
                    instance.id, read.status, read.body
                );
                assert_eq!(
                    instances_of(&read, &case).first(),
                    Some(instance),
                    "case {case:?}: instance {at} must be listed as GetTrafficPolicyInstance \
                     reports it; body {}",
                    read.body
                );
                let state = read.first("TrafficPolicyInstance/State").unwrap_or("");
                assert!(
                    [
                        TRAFFIC_POLICY_INSTANCE_CREATING,
                        TRAFFIC_POLICY_INSTANCE_APPLIED
                    ]
                    .contains(&state),
                    "case {case:?}: instance {at} reported the state {state:?}; body {}",
                    read.body
                );
            }
            if spelling == **zone {
                concatenated.extend(listed);
            }
        }
    }

    assert_eq!(
        concatenated, all,
        "the per-zone listings taken in hosted-zone order must concatenate to the \
         account-wide listing exactly once"
    );
}

#[tokio::test]
async fn per_zone_paging_covers_that_zone_exactly_once() {
    // The rule as an invariant: a page holds at most MaxItems instances of the
    // named zone and the two markers say where the next page starts, so the
    // pages concatenate to that zone's whole listing for EVERY page size. The
    // page size and the zone are the varying axes.
    let router = router();
    let zones = create_zones(&router, "zone-instance-paging").await;
    let all = populate(&router, &zones).await;

    for zone in &zones {
        let expected = in_zone(&all, zone);
        assert!(
            expected.len() > 1,
            "the suite needs several instances in zone {zone} to page through, got {}",
            expected.len()
        );
        for page_size in 1..=expected.len() + 1 {
            let mut walked: Vec<InstanceView> = Vec::new();
            let mut resume: Option<InstanceView> = None;
            let mut pages = 0;
            loop {
                let case = format!("zone {zone}, maxitems={page_size}, resuming at {resume:?}");
                let query = match &resume {
                    Some(at) => zone_marker_query(at, Some(page_size)),
                    None => format!("&maxitems={page_size}"),
                };
                let page = zone_listing(&router, &case, zone, &query).await;
                assert_eq!(
                    page.first("MaxItems"),
                    Some(page_size.to_string()).as_deref(),
                    "case {case:?}: the response must echo the request's maxitems; body {}",
                    page.body
                );
                let listed = instances_of(&page, &case);
                assert!(
                    listed.len() <= page_size,
                    "case {case:?}: a page held {} instance(s); body {}",
                    listed.len(),
                    page.body
                );
                walked.extend(listed);
                pages += 1;
                assert!(
                    pages <= expected.len() + 2,
                    "case {case:?}: paging did not terminate"
                );
                if page.first("IsTruncated") != Some("true") {
                    assert_eq!(
                        next_zone_markers(&page),
                        None,
                        "case {case:?}: a final page names no instance to resume at; body {}",
                        page.body
                    );
                    break;
                }
                let markers = next_zone_markers(&page).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a truncated page must name the instance the next page \
                         starts at; body {}",
                        page.body
                    )
                });
                let next = expected.get(walked.len()).unwrap_or_else(|| {
                    panic!("case {case:?}: a truncated page must leave an instance to resume at")
                });
                assert_eq!(
                    markers,
                    (next.name.clone(), next.traffic_policy_type.clone()),
                    "case {case:?}: the markers must name the first instance of the next page; \
                     body {}",
                    page.body
                );
                resume = Some(next.clone());
            }
            assert_eq!(
                walked, expected,
                "case zone {zone}, maxitems={page_size}: paging must cover that zone's listing \
                 exactly once"
            );
        }
    }
}

#[tokio::test]
async fn the_per_zone_markers_resume_the_listing_at_the_instance_they_name() {
    // The markers name "the first traffic policy instance in the next group" of
    // the zone the request names, so a listing resumed at any position reports
    // exactly that zone's tail from that position. Every position is a row; so
    // is each half of the pair on its own, which positions the first instance of
    // the zone carrying it.
    let router = router();
    let zones = create_zones(&router, "zone-instance-markers").await;
    let all = populate(&router, &zones).await;

    for zone in &zones {
        let expected = in_zone(&all, zone);
        for (at, instance) in expected.iter().enumerate() {
            let case = format!("zone {zone}: the full marker pair of its instance {at}");
            let page = zone_listing(&router, &case, zone, &zone_marker_query(instance, None)).await;
            assert_eq!(
                instances_of(&page, &case),
                expected[at..].to_vec(),
                "case {case:?}: the listing must resume at the instance the markers name; \
                 body {}",
                page.body
            );
        }

        // One half of the pair on its own names the FIRST instance of the zone
        // carrying it, which is where the listing then resumes.
        for instance in &expected {
            let halves = [
                (
                    "the DNS name alone",
                    format!("&trafficpolicyinstancename={}", instance.name),
                    expected
                        .iter()
                        .position(|held| held.name == instance.name)
                        .expect("the name came from an instance of this zone"),
                ),
                (
                    "the record type alone",
                    format!(
                        "&trafficpolicyinstancetype={}",
                        instance.traffic_policy_type
                    ),
                    expected
                        .iter()
                        .position(|held| held.traffic_policy_type == instance.traffic_policy_type)
                        .expect("the type came from an instance of this zone"),
                ),
            ];
            for (half, query, expected_at) in halves {
                let case = format!("zone {zone}: {half}, {query}");
                let page = zone_listing(&router, &case, zone, &query).await;
                assert_eq!(
                    instances_of(&page, &case),
                    expected[expected_at..].to_vec(),
                    "case {case:?}: the listing must resume at the first instance of the zone \
                     the marker names; body {}",
                    page.body
                );
            }
        }
    }
}

#[tokio::test]
async fn the_per_zone_listing_refuses_what_it_cannot_position() {
    // A zone the account does not hold is REFUSED rather than reported as an
    // empty listing, markers that name no instance OF THAT ZONE position
    // nothing, and a page size below one asks for no page at all. The rows are
    // each spelling of an unheld zone id, markers belonging to the other zone or
    // to no instance, and the page sizes below one.
    let router = router();
    let zones = create_zones(&router, "zone-instance-refusals").await;
    let all = populate(&router, &zones).await;
    let first = &zones[0];
    // An instance of the OTHER zone, so one row's markers name an instance the
    // account holds but not one of the zone it asks about.
    let elsewhere = in_zone(&all, &zones[1])
        .first()
        .expect("the population holds an instance in the second zone")
        .clone();
    // An instance of the zone asked about, picked by WHAT IT IS rather than by
    // its place in the listing: the A instance at `www.example.com.`, whose name
    // a second instance of the same zone carries at another type.
    let held = in_zone(&all, first)
        .into_iter()
        .find(|instance| instance.name == "www.example.com." && instance.traffic_policy_type == "A")
        .expect("the population holds an A instance at www.example.com.");

    let unknown_zone = "Z0000000000000";
    let missing = [
        ("a hosted zone id the account holds no zone under", unknown_zone.to_string()),
        (
            "the /hostedzone/ spelling of that same id",
            format!("/hostedzone/{unknown_zone}"),
        ),
    ];
    for (case, spelling) in &missing {
        let reply = list_zone_instances(&router, spelling, "").await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "NoSuchHostedZone",
            "case {case:?}: expected the modelled NoSuchHostedZone, got {:?}; body {}",
            reply.error_code(),
            reply.body
        );
        assert_eq!(
            reply.first("Error/Message"),
            Some(missing_zone_message(unknown_zone)).as_deref(),
            "case {case:?}: expected the refusal to name the bare zone id, got {:?}; body {}",
            reply.first("Error/Message"),
            reply.body
        );
    }

    let unpositioned = [
        (
            "markers naming an instance of another zone",
            zone_marker_query(&elsewhere, None),
        ),
        (
            "a DNS name no instance of the zone answers for",
            "&trafficpolicyinstancename=nobody.example.com.".to_string(),
        ),
        (
            "a record type no instance of the zone carries",
            "&trafficpolicyinstancetype=SRV".to_string(),
        ),
        (
            "a name and a type that belong to different instances",
            format!(
                "&trafficpolicyinstancename={}&trafficpolicyinstancetype=MX",
                held.name
            ),
        ),
    ];
    for (case, query) in &unpositioned {
        let reply = list_zone_instances(&router, first, query).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "NoSuchTrafficPolicyInstance",
            "case {case:?}: expected the modelled NoSuchTrafficPolicyInstance, got {:?}; body {}",
            reply.error_code(),
            reply.body
        );
    }

    for page_size in [0, -1] {
        let case = format!("maxitems={page_size}");
        let reply = list_zone_instances(&router, first, &format!("&maxitems={page_size}")).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidInput",
            "case {case:?}: expected the modelled InvalidInput, got {:?}; body {}",
            reply.error_code(),
            reply.body
        );
        assert_eq!(
            reply.first("Error/Message"),
            Some(NON_POSITIVE_PAGE_SIZE_MESSAGE),
            "case {case:?}: expected the page-size refusal's own sentence, got {:?}; body {}",
            reply.first("Error/Message"),
            reply.body
        );
    }

    // The refusals left both zones' listings exactly as they were.
    for zone in &zones {
        let case = format!("zone {zone} after the refusals");
        assert_eq!(
            instances_of(&zone_listing(&router, &case, zone, "").await, &case),
            in_zone(&all, zone),
            "case {case:?}: a refused listing must leave the zone's instances alone"
        );
    }
}

#[tokio::test]
async fn the_per_zone_listing_echoes_its_page_size_and_changes_nothing() {
    // `MaxItems` in the response is "the value that you specified for the
    // MaxItems parameter", so it is echoed whatever the caller sent, while the
    // page itself never exceeds the 100 items this listing publishes. The rule
    // is also a report: repeating it answers the same listing, consumes nothing,
    // and what it reports still moves with a later create.
    let router = router();
    let zones = create_zones(&router, "zone-instance-page-size").await;
    let all = populate(&router, &zones).await;
    let first = zones[0].clone();
    let expected = in_zone(&all, &first);
    let before = instance_count(&router).await;

    for requested in [1, 2, PAGE_BOUND, PAGE_BOUND + 1] {
        let case = format!("maxitems={requested}");
        let page = zone_listing(&router, &case, &first, &format!("&maxitems={requested}")).await;
        assert_eq!(
            page.first("MaxItems"),
            Some(requested.to_string()).as_deref(),
            "case {case:?}: expected the requested maxitems echoed, got {:?}; body {}",
            page.first("MaxItems"),
            page.body
        );
        let listed = instances_of(&page, &case);
        let wanted = (requested as usize)
            .min(PAGE_BOUND as usize)
            .min(expected.len());
        assert_eq!(
            listed.len(),
            wanted,
            "case {case:?}: expected {wanted} instance(s), got {}; body {}",
            listed.len(),
            page.body
        );
        assert_eq!(
            page.first("IsTruncated") == Some("true"),
            listed.len() < expected.len(),
            "case {case:?}: IsTruncated must say whether more instances of the zone remain; \
             body {}",
            page.body
        );
    }

    for attempt in 1..=3 {
        let case = format!("per-zone listing {attempt}");
        assert_eq!(
            instances_of(&zone_listing(&router, &case, &first, "").await, &case),
            expected,
            "case {case:?}: a read must report what the first read did"
        );
        assert_eq!(
            instance_count(&router).await,
            before,
            "case {case:?}: listing must not change how many instances the account holds"
        );
    }

    let added = create_row(
        &router,
        &zones,
        &InstanceRow {
            case: "an instance created after the per-zone listing",
            zone: 0,
            name: "late.example.com",
            expect_name: "late.example.com.",
            rr_type: "A",
            ttl: 45,
        },
        population().len(),
    )
    .await;
    let mut after = expected.clone();
    after.push(added);
    let after = in_listing_order(&after);
    let case = "after a later create in the zone";
    assert_eq!(
        instances_of(&zone_listing(&router, case, &first, "").await, case),
        after,
        "case {case:?}: the per-zone listing must follow the state the zone is in"
    );
}

// ---------------------------------------------------------------------------
// ListTrafficPolicyInstancesByPolicy: the same listing, held to one version of
// one traffic policy
// ---------------------------------------------------------------------------
//
// The rule: the operation REPORTS the traffic policy instances that were created
// from the ONE traffic policy version the request's `(TrafficPolicyId,
// TrafficPolicyVersion)` pair names, in the account's own listing order — the
// hosted zone each instance's record sets live in, then the DNS name they answer
// for, then their record type, the triple this listing's markers carry — as a
// page that starts at the instance those three markers name, carries at most
// `MaxItems` instances, echoes that `maxitems` back and names in the markers the
// first instance of the next page. A pair the account holds no traffic policy
// version under is REFUSED rather than reported as empty, markers that name no
// instance created from that pair are refused, and nothing a caller reads
// changes what a later caller sees.
//
// The tables vary the PAIR listed (two versions of one policy, a policy of
// another record type, and a version no instance was created from), the
// POPULATION (none, then one more instance at a time), the hosted zone, the DNS
// name, the record type (including two instances in one zone at one name that
// differ only in it, each created from a different policy), the TTL, the page
// size and the markers.

/// One version of one traffic policy: the pair a request to this listing names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PolicyVersion {
    id: String,
    version: i32,
}

/// Send one `ListTrafficPolicyInstancesByPolicy` request for the policy version
/// `(id, version)`, with `query` appended to its query string.
async fn list_by_policy(
    router: &MultiServiceRouter,
    id: &str,
    version: &str,
    query: &str,
) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/2013-04-01/trafficpolicyinstances/trafficpolicy?id={id}&version={version}{query}"
        ),
        "",
    )
    .await
}

/// List one policy version's instances, asserting the call succeeded.
async fn policy_listing(
    router: &MultiServiceRouter,
    case: &str,
    pair: &PolicyVersion,
    query: &str,
) -> Reply {
    let reply = list_by_policy(router, &pair.id, &pair.version.to_string(), query).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ListTrafficPolicyInstancesByPolicy, got {} with \
         body {}",
        reply.status, reply.body
    );
    reply
}

/// The markers that resume this listing at `instance`, as a query string
/// appended to the pair the request already names.
fn policy_marker_query(instance: &InstanceView, max_items: Option<usize>) -> String {
    let size = max_items.map_or_else(String::new, |size| format!("&maxitems={size}"));
    format!(
        "&hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype={}{size}",
        instance.hosted_zone_id, instance.name, instance.traffic_policy_type
    )
}

/// One policy of the by-policy population: its name, the record type its
/// document creates, and how many versions of it the test creates.
struct PolicyRow {
    name: &'static str,
    rr_type: &'static str,
    versions: i32,
}

/// The policies the by-policy population's instances are created from: one with
/// TWO versions (so the pair tells one version of a policy from another), one of
/// another record type, and one no instance is ever created from (so a pair with
/// nothing to list is one of the rows).
const POLICY_ROWS: [PolicyRow; 3] = [
    PolicyRow {
        name: "by-policy-two-versions",
        rr_type: "A",
        versions: 2,
    },
    PolicyRow {
        name: "by-policy-other-type",
        rr_type: "TXT",
        versions: 1,
    },
    PolicyRow {
        name: "by-policy-without-instances",
        rr_type: "MX",
        versions: 1,
    },
];

/// One row of the by-policy population: the policy version the instance is
/// created from, which of the test's zones it lives in, the DNS name its record
/// sets answer for, and their TTL.
struct PolicyInstanceRow {
    case: &'static str,
    /// Index into [`POLICY_ROWS`].
    policy: usize,
    /// The version of that policy the instance is created from.
    version: i32,
    /// Index into the zones the test created.
    zone: usize,
    name: &'static str,
    /// The canonical form of that name, which is what a response reports.
    expect_name: &'static str,
    ttl: i64,
}

/// The zone names the by-policy population lives across: the two the population
/// fills, plus one it leaves empty so a marker can name a zone holding no
/// instance of a pair.
const BY_POLICY_ZONE_NAMES: [&str; 3] = ["example.com", "example.net", "example.org"];

/// The population every by-policy test lists: instances created from three
/// different policy versions, across two hosted zones, at names of differing
/// depth and spelling and with differing TTLs — including two in ONE zone at ONE
/// name that differ only in their record type, because they were created from
/// different policies.
fn by_policy_population() -> Vec<PolicyInstanceRow> {
    vec![
        PolicyInstanceRow {
            case: "an instance of the first policy's first version",
            policy: 0,
            version: 1,
            zone: 0,
            name: "www.example.com",
            expect_name: "www.example.com.",
            ttl: 60,
        },
        PolicyInstanceRow {
            case: "a second instance of that version, earlier in the zone's DNS order",
            policy: 0,
            version: 1,
            zone: 0,
            name: "alpha.example.com.",
            expect_name: "alpha.example.com.",
            ttl: 86_400,
        },
        PolicyInstanceRow {
            case: "a third instance of that version, in the other hosted zone",
            policy: 0,
            version: 1,
            zone: 1,
            name: "www.example.net",
            expect_name: "www.example.net.",
            ttl: 120,
        },
        PolicyInstanceRow {
            case: "an instance of the SECOND version of that same policy",
            policy: 0,
            version: 2,
            zone: 0,
            name: "deep.sub.example.com",
            expect_name: "deep.sub.example.com.",
            ttl: 300,
        },
        PolicyInstanceRow {
            case: "an instance of another policy, at a name the first policy also uses",
            policy: 1,
            version: 1,
            zone: 0,
            name: "WWW.Example.com",
            expect_name: "www.example.com.",
            ttl: 0,
        },
        PolicyInstanceRow {
            case: "a second instance of that policy, in the other hosted zone",
            policy: 1,
            version: 1,
            zone: 1,
            name: "mail.example.net",
            expect_name: "mail.example.net.",
            ttl: 900,
        },
    ]
}

/// Create every policy of [`POLICY_ROWS`], with each one's versions, and return
/// their ids in the order the rows name them.
async fn create_policies(router: &MultiServiceRouter, suffix: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for row in &POLICY_ROWS {
        let case = format!("the policy {}", row.name);
        let created = create_policy(
            router,
            &format!("{}-{suffix}", row.name),
            &document_with_start_endpoint(row.rr_type, value_for(row.rr_type)),
            None,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected 201 from CreateTrafficPolicy, got {} with body {}",
            created.status, created.body
        );
        let id = policy_of(&created, &case).id;
        for version in 2..=row.versions {
            let further = create_version(
                router,
                &id,
                &document_with_start_endpoint(row.rr_type, value_for(row.rr_type)),
                None,
            )
            .await;
            assert_eq!(
                further.status, 201,
                "case {case:?}: expected 201 from CreateTrafficPolicyVersion for version \
                 {version}, got {} with body {}",
                further.status, further.body
            );
            assert_eq!(
                policy_of(&further, &case).version,
                version.to_string(),
                "case {case:?}: the created version must be numbered {version}; body {}",
                further.body
            );
        }
        ids.push(id);
    }
    ids
}

/// Create the three zones every by-policy test lists across, and return their
/// bare ids.
async fn create_by_policy_zones(router: &MultiServiceRouter, suffix: &str) -> Vec<String> {
    let mut zones = Vec::new();
    for (index, name) in BY_POLICY_ZONE_NAMES.iter().enumerate() {
        zones.push(create_zone_ok(router, name, &format!("{suffix}-{index}")).await);
    }
    zones
}

/// Create the instance `row` describes and return what every listing of its
/// policy version must then report for it.
async fn create_policy_row(
    router: &MultiServiceRouter,
    zones: &[String],
    policies: &[String],
    row: &PolicyInstanceRow,
) -> InstanceView {
    let zone = &zones[row.zone];
    let policy = &policies[row.policy];
    let reply = create_instance(router, zone, row.name, row.ttl, policy, row.version).await;
    assert_eq!(
        reply.status, 201,
        "case {:?}: expected 201 from CreateTrafficPolicyInstance, got {} with body {}",
        row.case, reply.status, reply.body
    );
    let answered = instances_of(&reply, row.case);
    assert_eq!(
        answered.len(),
        1,
        "case {:?}: a create answers with exactly one instance; body {}",
        row.case,
        reply.body
    );
    let held = answered[0].clone();
    let version = row.version.to_string();
    assert_eq!(
        (
            held.hosted_zone_id.as_str(),
            held.name.as_str(),
            held.traffic_policy_id.as_str(),
            held.traffic_policy_version.as_str(),
            held.traffic_policy_type.as_str(),
        ),
        (
            zone.as_str(),
            row.expect_name,
            policy.as_str(),
            version.as_str(),
            POLICY_ROWS[row.policy].rr_type,
        ),
        "case {:?}: the create must persist the instance in the zone, at the canonical name \
         and from the policy version it was asked for, got {held:?}; body {}",
        row.case,
        reply.body
    );
    held
}

/// Create every row of [`by_policy_population`], in order, and return what the
/// account then holds.
async fn populate_by_policy(
    router: &MultiServiceRouter,
    zones: &[String],
    policies: &[String],
) -> Vec<InstanceView> {
    let mut created = Vec::new();
    for row in &by_policy_population() {
        created.push(create_policy_row(router, zones, policies, row).await);
    }
    created
}

/// The pairs every by-policy test lists: each version of each created policy, so
/// the table spans two versions of one policy, a policy of another record type
/// and a version no instance was created from.
fn pairs(policies: &[String]) -> Vec<PolicyVersion> {
    POLICY_ROWS
        .iter()
        .enumerate()
        .flat_map(|(at, row)| {
            (1..=row.versions).map(move |version| PolicyVersion {
                id: policies[at].clone(),
                version,
            })
        })
        .collect()
}

/// The instances of `created` that were created from `pair`, in the order this
/// listing has to report them in.
fn of_pair(created: &[InstanceView], pair: &PolicyVersion) -> Vec<InstanceView> {
    let held: Vec<InstanceView> = created
        .iter()
        .filter(|instance| {
            instance.traffic_policy_id == pair.id
                && instance.traffic_policy_version == pair.version.to_string()
        })
        .cloned()
        .collect();
    in_listing_order(&held)
}

/// One component of this listing's marker triple: the label a case carries, the
/// query parameter that names it, and the member of an instance it reads.
struct MarkerComponent {
    label: &'static str,
    parameter: &'static str,
    of: fn(&InstanceView) -> String,
}

/// The hosted zone of an instance, as its marker carries it.
fn marker_zone(instance: &InstanceView) -> String {
    instance.hosted_zone_id.clone()
}

/// The DNS name of an instance, as its marker carries it.
fn marker_name(instance: &InstanceView) -> String {
    instance.name.clone()
}

/// The record type of an instance, as its marker carries it.
fn marker_type(instance: &InstanceView) -> String {
    instance.traffic_policy_type.clone()
}

/// The three components of the marker triple, each of which positions the
/// listing on its own.
const MARKER_COMPONENTS: [MarkerComponent; 3] = [
    MarkerComponent {
        label: "the hosted zone alone",
        parameter: "hostedzoneid",
        of: marker_zone,
    },
    MarkerComponent {
        label: "the DNS name alone",
        parameter: "trafficpolicyinstancename",
        of: marker_name,
    },
    MarkerComponent {
        label: "the record type alone",
        parameter: "trafficpolicyinstancetype",
        of: marker_type,
    },
];

/// Assert that `reply` reports exactly `expected` and names no instance to
/// resume at.
fn assert_whole_policy_listing(reply: &Reply, case: &str, expected: &[InstanceView]) {
    assert_eq!(
        instances_of(reply, case),
        expected.to_vec(),
        "case {case:?}: the listing must report every instance created from the policy \
         version, by hosted zone, then DNS name, then record type; body {}",
        reply.body
    );
    assert_ne!(
        reply.first("IsTruncated"),
        Some("true"),
        "case {case:?}: a listing that fits one page must not be truncated; body {}",
        reply.body
    );
    assert_eq!(
        next_markers(reply),
        None,
        "case {case:?}: an untruncated listing names no instance to resume at; body {}",
        reply.body
    );
}

#[tokio::test]
async fn the_by_policy_listing_reports_the_instances_created_from_the_pair_it_names() {
    // The varying axes are the PAIR listed and the POPULATION: before every
    // create, each pair's listing is asserted against the instances created from
    // it so far — so each pair is asserted while it holds none, and again after
    // every instance of it, of another version of its policy and of another
    // policy. Every policy's record type is one the model's own `RRType` enum
    // names.
    for row in &POLICY_ROWS {
        assert!(
            R_R_TYPE_VALUES.contains(&row.rr_type),
            "the policy {} creates {}, which is not a modelled RRType",
            row.name,
            row.rr_type
        );
    }

    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-reports").await;
    let policies = create_policies(&router, "reports").await;
    let pairs = pairs(&policies);
    let mut created: Vec<InstanceView> = Vec::new();

    let rows = by_policy_population();
    for row in &rows {
        for pair in &pairs {
            let case = format!(
                "{} instance(s) held, listing version {} of policy {}, before creating {}",
                created.len(),
                pair.version,
                pair.id,
                row.case
            );
            let reply = policy_listing(&router, &case, pair, "").await;
            assert_whole_policy_listing(&reply, &case, &of_pair(&created, pair));
            assert_eq!(
                reply.first("MaxItems"),
                Some(DEFAULT_PAGE_SIZE.to_string()).as_deref(),
                "case {case:?}: a request naming no maxitems asks for the default page; body {}",
                reply.body
            );

            // Every listed instance is cross-checked against what
            // GetTrafficPolicyInstance reports for the same id, so this listing
            // is proved against another reader of the same state.
            for (at, instance) in instances_of(&reply, &case).iter().enumerate() {
                let read = get_instance(&router, &instance.id).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: the listed instance {} must be readable, got {} with body {}",
                    instance.id, read.status, read.body
                );
                assert_eq!(
                    instances_of(&read, &case).first(),
                    Some(instance),
                    "case {case:?}: instance {at} must be listed as GetTrafficPolicyInstance \
                     reports it; body {}",
                    read.body
                );
                let state = read.first("TrafficPolicyInstance/State").unwrap_or("");
                assert!(
                    [
                        TRAFFIC_POLICY_INSTANCE_CREATING,
                        TRAFFIC_POLICY_INSTANCE_APPLIED
                    ]
                    .contains(&state),
                    "case {case:?}: instance {at} reported the state {state:?}; body {}",
                    read.body
                );
            }
        }

        created.push(create_policy_row(&router, &zones, &policies, row).await);
    }

    // The loop asserts each pair BEFORE a create, so the whole population needs
    // one more round.
    for pair in &pairs {
        let case = format!(
            "the whole population, listing version {} of policy {}",
            pair.version, pair.id
        );
        let reply = policy_listing(&router, &case, pair, "").await;
        assert_whole_policy_listing(&reply, &case, &of_pair(&created, pair));
    }
}

#[tokio::test]
async fn by_policy_paging_covers_one_pairs_listing_exactly_once() {
    // The rule as an invariant: a page holds at most MaxItems instances of the
    // pair and the three markers name where the next page starts, so the pages
    // concatenate to that pair's whole listing for EVERY page size. The page
    // size and the pair are the varying axes; the sizes run from one to one past
    // the pair's population, plus the published page bound and one past it.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-paging").await;
    let policies = create_policies(&router, "paging").await;
    let created = populate_by_policy(&router, &zones, &policies).await;

    for pair in &pairs(&policies) {
        let all = of_pair(&created, pair);
        for page_size in (1..=all.len() + 1).chain([PAGE_BOUND as usize, PAGE_BOUND as usize + 1]) {
            let mut walked: Vec<InstanceView> = Vec::new();
            let mut resume: Option<InstanceView> = None;
            let mut pages = 0;
            loop {
                let case = format!(
                    "version {} of policy {} at maxitems={page_size}, resuming at {resume:?}",
                    pair.version, pair.id
                );
                let query = match &resume {
                    Some(at) => policy_marker_query(at, Some(page_size)),
                    None => format!("&maxitems={page_size}"),
                };
                let page = policy_listing(&router, &case, pair, &query).await;
                assert_eq!(
                    page.first("MaxItems"),
                    Some(page_size.to_string()).as_deref(),
                    "case {case:?}: the response must echo the request's maxitems; body {}",
                    page.body
                );
                let listed = instances_of(&page, &case);
                assert!(
                    listed.len() <= page_size.min(PAGE_BOUND as usize),
                    "case {case:?}: a page held {} instance(s); body {}",
                    listed.len(),
                    page.body
                );
                walked.extend(listed);
                pages += 1;
                assert!(
                    pages <= all.len() + 2,
                    "case {case:?}: paging did not terminate"
                );
                if page.first("IsTruncated") != Some("true") {
                    assert_eq!(
                        next_markers(&page),
                        None,
                        "case {case:?}: a final page names no instance to resume at; body {}",
                        page.body
                    );
                    break;
                }
                let markers = next_markers(&page).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a truncated page must name the instance the next page \
                         starts at; body {}",
                        page.body
                    )
                });
                let next = all.get(walked.len()).unwrap_or_else(|| {
                    panic!("case {case:?}: a truncated page must leave an instance to resume at")
                });
                assert_eq!(
                    markers,
                    (
                        next.hosted_zone_id.clone(),
                        next.name.clone(),
                        next.traffic_policy_type.clone()
                    ),
                    "case {case:?}: the markers must name the first instance of the next page; \
                     body {}",
                    page.body
                );
                resume = Some(next.clone());
            }
            assert_eq!(
                walked, all,
                "case version {} of policy {} at maxitems={page_size}: paging must cover that \
                 pair's listing exactly once",
                pair.version, pair.id
            );
        }
    }
}

#[tokio::test]
async fn the_by_policy_markers_resume_the_listing_at_the_instance_they_name() {
    // The markers name "the first traffic policy instance in the next group", so
    // a listing resumed at any position reports exactly the tail of that pair's
    // listing from that position. Every position of every pair is a row; so is
    // each component of the triple on its own, which positions the first
    // instance of the pair carrying it.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-markers").await;
    let policies = create_policies(&router, "markers").await;
    let created = populate_by_policy(&router, &zones, &policies).await;

    for pair in &pairs(&policies) {
        let all = of_pair(&created, pair);
        for (at, instance) in all.iter().enumerate() {
            let case = format!(
                "the full marker triple of instance {at} of version {} of policy {}",
                pair.version, pair.id
            );
            let page =
                policy_listing(&router, &case, pair, &policy_marker_query(instance, None)).await;
            assert_eq!(
                instances_of(&page, &case),
                all[at..].to_vec(),
                "case {case:?}: the listing must resume at the instance the markers name; \
                 body {}",
                page.body
            );
        }

        // One component of the triple on its own names the FIRST instance of the
        // pair carrying it, which is where the listing then resumes.
        for component in &MARKER_COMPONENTS {
            let read = component.of;
            let parameter = component.parameter;
            for instance in &all {
                let value = read(instance);
                let case = format!(
                    "{}: {parameter}={value} of version {} of policy {}",
                    component.label, pair.version, pair.id
                );
                let page =
                    policy_listing(&router, &case, pair, &format!("&{parameter}={value}")).await;
                let expected_at = all
                    .iter()
                    .position(|held| read(held) == value)
                    .expect("the value came from an instance of this pair's listing");
                assert_eq!(
                    instances_of(&page, &case),
                    all[expected_at..].to_vec(),
                    "case {case:?}: the listing must resume at the first instance of the pair \
                     the marker names; body {}",
                    page.body
                );
            }
        }
    }
}

#[tokio::test]
async fn by_policy_markers_naming_no_instance_of_the_pair_are_refused() {
    // Markers position the listing AT an instance of the pair, so markers the
    // pair holds no instance under name no position in it: the request is
    // refused rather than answered from somewhere else. The rows span a zone the
    // account holds but the pair has no instance in, a zone the account does not
    // hold at all, a name carried by an instance of ANOTHER pair, a name nothing
    // answers for, a type no instance of the pair carries, and parts that each
    // name an instance of the pair but never the same one.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-unknown-markers").await;
    let policies = create_policies(&router, "unknown-markers").await;
    let created = populate_by_policy(&router, &zones, &policies).await;

    // The pair with instances in both zones, and the instance of it picked by
    // WHAT IT IS rather than by its place in the listing, which depends on the
    // ids the zones were minted with.
    let pair = PolicyVersion {
        id: policies[0].clone(),
        version: 1,
    };
    let all = of_pair(&created, &pair);
    let held = all
        .iter()
        .find(|instance| instance.name == "www.example.com.")
        .expect("this pair holds an instance at www.example.com.")
        .clone();
    let other_zone = all
        .iter()
        .find(|instance| instance.hosted_zone_id != held.hosted_zone_id)
        .expect("this pair holds an instance in the other zone")
        .clone();
    // An instance of the SECOND version of the same policy: its name is one this
    // pair holds no instance under.
    let other_version = created
        .iter()
        .find(|instance| {
            instance.traffic_policy_id == pair.id && instance.traffic_policy_version == "2"
        })
        .expect("the population holds an instance of the second version")
        .clone();

    let rows = [
        (
            "a hosted zone the account holds no instance of the pair in",
            format!(
                "&hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype={}",
                zones[2], held.name, held.traffic_policy_type
            ),
        ),
        (
            "a hosted zone the account does not hold at all",
            format!(
                "&hostedzoneid=Z0000000000000&trafficpolicyinstancename={}\
                 &trafficpolicyinstancetype={}",
                held.name, held.traffic_policy_type
            ),
        ),
        (
            "a DNS name no instance of the pair answers for",
            format!(
                "&hostedzoneid={}&trafficpolicyinstancename=nobody.example.com.\
                 &trafficpolicyinstancetype={}",
                held.hosted_zone_id, held.traffic_policy_type
            ),
        ),
        (
            "the name of an instance of another version of the same policy",
            format!("&trafficpolicyinstancename={}", other_version.name),
        ),
        (
            "a record type no instance of the pair carries",
            format!(
                "&hostedzoneid={}&trafficpolicyinstancename={}&trafficpolicyinstancetype=SRV",
                held.hosted_zone_id, held.name
            ),
        ),
        (
            "a zone and a name that belong to different instances of the pair",
            format!(
                "&hostedzoneid={}&trafficpolicyinstancename={}",
                other_zone.hosted_zone_id, held.name
            ),
        ),
        (
            "a name alone that no instance answers for",
            "&trafficpolicyinstancename=nobody.example.org.".to_string(),
        ),
    ];

    for (case, query) in &rows {
        let reply = list_by_policy(&router, &pair.id, &pair.version.to_string(), query).await;
        assert_identity_only(&reply, case, "NoSuchTrafficPolicyInstance");
    }

    // The refusals left the pair's listing exactly as it was.
    let after = policy_listing(&router, "after the marker refusals", &pair, "").await;
    assert_whole_policy_listing(&after, "after the marker refusals", &all);
}

#[tokio::test]
async fn a_policy_version_the_account_does_not_hold_has_no_listing() {
    // The pair addresses ONE traffic policy version, so a pair the account holds
    // no such version under names nothing to list: it is refused by the error's
    // identity alone rather than answered as an empty listing. The rows span ids
    // that name no policy, versions a held policy never reached — including the
    // published maximum — a version a delete has flagged, and versions outside
    // the modelled range.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-unknown-pair").await;
    let policies = create_policies(&router, "unknown-pair").await;
    let created = populate_by_policy(&router, &zones, &policies).await;

    let unknown_ids = [
        (
            "an unminted id of the minted form",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        ("a hosted zone's id", zones[0].clone()),
        ("an id of no recognised form", "not-a-policy".to_string()),
        ("the shortest id the model admits", "x".to_string()),
    ];
    for (case, id) in &unknown_ids {
        for version in ["1", "2", BY_POLICY_VERSION_MAX] {
            let case = format!("{case} at version {version}");
            let reply = list_by_policy(&router, id, version, "").await;
            assert_no_such_traffic_policy(&reply, &case);
        }
    }

    // The other half of the pair: a policy the account holds, at a version it
    // does not. The first policy holds two versions, the others one.
    let version_max: i32 = BY_POLICY_VERSION_MAX
        .parse()
        .expect("the published version bound is a number");
    let unheld_versions = [
        ("the version after the ones it holds", 3),
        ("a version far past the ones it holds", 7),
        ("the published maximum version", version_max),
    ];
    for (case, version) in &unheld_versions {
        let case = format!("{case} of a policy the account holds");
        let reply = list_by_policy(&router, &policies[0], &version.to_string(), "").await;
        assert_no_such_traffic_policy(&reply, &case);
    }
    // A version of ANOTHER held policy that it never reached, so the refusal is
    // about the pair rather than about one policy.
    let reply = list_by_policy(&router, &policies[1], "2", "").await;
    assert_no_such_traffic_policy(&reply, "the second version of a one-version policy");

    // A version a delete has flagged is no longer one of the account's versions
    // either. The policy no instance was created from is the one that can be
    // deleted: a version instances were created from is refused as in use.
    let gone = delete_policy(&router, &policies[2], "1").await;
    assert_eq!(
        gone.status, 200,
        "the version with no instances must delete, got {} with body {}",
        gone.status, gone.body
    );
    let reply = list_by_policy(&router, &policies[2], "1", "").await;
    assert_no_such_traffic_policy(&reply, "a version a delete flagged");

    // A version outside the modelled range is refused by the constraint before
    // the handler: `TrafficPolicyVersion` is `range 1..1000`, and both published
    // bounds themselves are served above.
    for version in ["0", "-1", "1001"] {
        let reply = list_by_policy(&router, &policies[0], version, "").await;
        assert_eq!(
            reply.status, 400,
            "case {version:?}: expected HTTP 400 for a version outside the modelled range, \
             got {} with body {}",
            reply.status, reply.body
        );
    }

    // None of the refusals touched the instances the account holds.
    let pair = PolicyVersion {
        id: policies[0].clone(),
        version: 1,
    };
    let after = policy_listing(&router, "after the pair refusals", &pair, "").await;
    assert_whole_policy_listing(
        &after,
        "after the pair refusals",
        &of_pair(&created, &pair),
    );
}

/// The published maximum of `TrafficPolicyVersion`, which is `range 1..1000`
/// (smithy_query(query="shape",shape="TrafficPolicyVersion")).
const BY_POLICY_VERSION_MAX: &str = "1000";

#[tokio::test]
async fn the_by_policy_listing_echoes_its_page_size_and_refuses_a_page_of_none() {
    // `MaxItems` in the response is "the value that you specified for the
    // MaxItems parameter", so it is echoed whatever the caller sent — while the
    // page itself never exceeds the 100 items this listing publishes. The rows
    // are sizes below the population, the published bound and one past it, plus
    // the sizes that ask for no page at all.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-page-size").await;
    let policies = create_policies(&router, "page-size").await;
    let created = populate_by_policy(&router, &zones, &policies).await;
    let pair = PolicyVersion {
        id: policies[0].clone(),
        version: 1,
    };
    let all = of_pair(&created, &pair);

    for requested in [1, 2, PAGE_BOUND, PAGE_BOUND + 1] {
        let case = format!("maxitems={requested}");
        let page = policy_listing(&router, &case, &pair, &format!("&maxitems={requested}")).await;
        assert_eq!(
            page.first("MaxItems"),
            Some(requested.to_string()).as_deref(),
            "case {case:?}: expected the requested maxitems echoed, got {:?}; body {}",
            page.first("MaxItems"),
            page.body
        );
        let listed = instances_of(&page, &case);
        let wanted = (requested as usize).min(PAGE_BOUND as usize).min(all.len());
        assert_eq!(
            listed.len(),
            wanted,
            "case {case:?}: expected {wanted} instance(s), got {}; body {}",
            listed.len(),
            page.body
        );
        assert_eq!(
            page.first("IsTruncated") == Some("true"),
            listed.len() < all.len(),
            "case {case:?}: IsTruncated must say whether more instances remain; body {}",
            page.body
        );
    }

    // A page size below one asks for no page at all, which is refused rather
    // than served as some other size.
    for requested in ["0", "-1"] {
        let case = format!("maxitems={requested}");
        let reply = list_by_policy(
            &router,
            &pair.id,
            &pair.version.to_string(),
            &format!("&maxitems={requested}"),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400 for a page of none, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidInput",
            "case {case:?}: expected the modelled InvalidInput, got {:?}; body {}",
            reply.error_code(),
            reply.body
        );
        assert_eq!(
            reply.first("Error/Message"),
            Some(NON_POSITIVE_PAGE_SIZE_MESSAGE),
            "case {case:?}: the refusal must state why; body {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn listing_a_pairs_instances_leaves_the_account_exactly_as_it_was() {
    // The rule is a report: repeating it answers the same listing, creates and
    // consumes nothing, and the population it reports still moves with a later
    // create.
    let router = router();
    let zones = create_by_policy_zones(&router, "by-policy-reads").await;
    let policies = create_policies(&router, "reads").await;
    let created = populate_by_policy(&router, &zones, &policies).await;
    let before = instance_count(&router).await;
    let pairs = pairs(&policies);

    for attempt in 1..=3 {
        for pair in &pairs {
            let case = format!(
                "listing {attempt} of version {} of policy {}",
                pair.version, pair.id
            );
            assert_eq!(
                instances_of(&policy_listing(&router, &case, pair, "").await, &case),
                of_pair(&created, pair),
                "case {case:?}: a read must report what the first read did"
            );
        }
        assert_eq!(
            instance_count(&router).await,
            before,
            "listing {attempt} must not change how many instances the account holds"
        );
    }

    // A paged walk is a read as well: paging through one pair's listing changes
    // neither the population nor what an unpaged read reports.
    let pair = pairs[0].clone();
    let all = of_pair(&created, &pair);
    let mut resume: Option<InstanceView> = None;
    loop {
        let case = format!("a paged read resuming at {resume:?}");
        let query = match &resume {
            Some(at) => policy_marker_query(at, Some(1)),
            None => "&maxitems=1".to_string(),
        };
        let page = policy_listing(&router, &case, &pair, &query).await;
        if page.first("IsTruncated") != Some("true") {
            break;
        }
        let markers = next_markers(&page).expect("a truncated page names the next instance");
        resume = all
            .iter()
            .find(|held| {
                (
                    held.hosted_zone_id.as_str(),
                    held.name.as_str(),
                    held.traffic_policy_type.as_str(),
                ) == (markers.0.as_str(), markers.1.as_str(), markers.2.as_str())
            })
            .cloned();
        assert!(
            resume.is_some(),
            "case {case:?}: the markers must name an instance of the pair; body {}",
            page.body
        );
    }
    assert_eq!(
        instance_count(&router).await,
        before,
        "paging through the listing must not change how many instances the account holds"
    );

    // A later create of the SAME pair is reported by the next listing, and the
    // other pairs' listings do not move with it.
    let added = create_policy_row(
        &router,
        &zones,
        &policies,
        &PolicyInstanceRow {
            case: "an instance created after the listing",
            policy: 0,
            version: 1,
            zone: 1,
            name: "late.example.net",
            expect_name: "late.example.net.",
            ttl: 45,
        },
    )
    .await;
    let mut expected = created.clone();
    expected.push(added);
    for pair in &pairs {
        let case = format!(
            "after a later create, version {} of policy {}",
            pair.version, pair.id
        );
        let reply = policy_listing(&router, &case, pair, "").await;
        assert_whole_policy_listing(&reply, &case, &of_pair(&expected, pair));
    }
}
