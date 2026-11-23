// SPDX-License-Identifier: CC0-1.0

//! What `DescribeVpcEndpointServices` publishes about an endpoint service Amazon Web Services
//! offers: the Availability Zones it is available in, the private DNS name it publishes and the
//! id it is reached by — driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rules under test, each a fact of the Region or of the service rather than of the call:
//!
//! * `ServiceDetail$AvailabilityZones` and `$AvailabilityZoneIds` report EVERY zone the Region
//!   has, in ONE order — the id at a position is the id of the zone named at that position, the
//!   same pairing `DescribeAvailabilityZones` reports for the Region. (Which zone NAME an
//!   account sees a given zone id under is mapped per account by AWS, so what a client can rely
//!   on is the completeness of both lists and that the two agree position by position.)
//! * An interface endpoint service that publishes a default public DNS name reports it as
//!   `$PrivateDnsName`, carries it in `$PrivateDnsNames` and reports its ownership as verified
//!   in `$PrivateDnsNameVerificationState` — and the name is the HOST form of the service's
//!   label under the domain its name family names. A service that publishes no such name, and a
//!   service reached through a gateway endpoint (which has no private hosted zone), report none
//!   of the three.
//! * `$ServiceId` takes the `vpce-svc-` prefix and 17 hexadecimal digits, and one service keeps
//!   one id across reads.
//!
//! The table spans what the rules generalise over: Regions with three, four and six zones and
//! one whose zone letters are not the first three of the alphabet; services from each of the
//! three name families Amazon Web Services publishes endpoint services under (`com.amazonaws`,
//! `aws.api` and `api.aws`), a service whose label is DOTTED (whose host form reverses it), a
//! service published with no private DNS name at all, and a gateway service.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, emulator, indexed};
use super::vpc_endpoint_services::is_derived_service_id;
use super::wire;

/// The `DnsNameState` an endpoint service of a Region's own catalog publishes for its name:
/// Amazon Web Services owns the domain the name sits in, so there is nothing left to verify.
const VERIFIED: &str = "verified";

/// One row of the table: the Region to describe in, the endpoint service name, and the default
/// public DNS name AWS publishes for it — `None` for a service that publishes none.
type Row = (&'static str, &'static str, Option<&'static str>);

/// One service per name family, plus a dotted label, a service with no private DNS name and a
/// gateway service, described in Regions with three, four and six zones.
const ROWS: &[Row] = &[
    (
        "us-west-2",
        "aws.api.us-west-2.emr-service-cell01",
        Some("emr-service-cell01.us-west-2.api.aws"),
    ),
    (
        "us-east-1",
        "com.amazonaws.us-east-1.kms",
        Some("kms.us-east-1.amazonaws.com"),
    ),
    (
        "eu-west-1",
        "api.aws.eu-west-1.cassandra-streams",
        Some("cassandra-streams.eu-west-1.api.aws"),
    ),
    // A dotted label names the host whose labels are its own in REVERSE order.
    (
        "ap-northeast-1",
        "com.amazonaws.ap-northeast-1.ecr.dkr",
        Some("dkr.ecr.ap-northeast-1.amazonaws.com"),
    ),
    // The AWS IoT Core data plane publishes no default public DNS name.
    ("us-west-2", "com.amazonaws.us-west-2.iot.data", None),
    // A gateway service is reached through its prefix list rather than a private hosted zone.
    ("us-east-1", "com.amazonaws.us-east-1.dynamodb", None),
];

/// The `serviceDetailSet` item `DescribeVpcEndpointServices` answers for exactly one service
/// name, as that call's own 200.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    service_name: &str,
) -> String {
    let (status, body) = call(
        router,
        region,
        "DescribeVpcEndpointServices",
        &indexed("ServiceName", &[service_name]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {service_name}, got {status} with body {body}"
    );
    body
}

/// The zone names and the zone ids of one reported service detail, in the order it reported
/// them.
fn reported_zones(case: &str, body: &str) -> (Vec<String>, Vec<String>) {
    let parsed = wire::document(case, body);
    let detail = parsed
        .descendants()
        .find(|node| node.is_element() && wire::child(*node, "serviceName").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported endpoint service in {body}"));
    let entries = |name: &str| -> Vec<String> {
        wire::child(detail, name)
            .map(|set| {
                set.children()
                    .filter(roxmltree::Node::is_element)
                    .map(|item| item.text().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    (
        entries("availabilityZoneSet"),
        entries("availabilityZoneIdSet"),
    )
}

/// One member of the reported service detail.
fn member(case: &str, body: &str, name: &str) -> Option<String> {
    let parsed = wire::document(case, body);
    let detail = parsed
        .descendants()
        .find(|node| node.is_element() && wire::child(*node, "serviceName").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported endpoint service in {body}"));
    wire::child_text(detail, name)
}

/// The zones the Region has, as `DescribeAvailabilityZones` reports them: `(name, id)` pairs in
/// the order that report lists them, which is the pairing an endpoint service's two zone lists
/// have to agree with.
async fn zones_of_region(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(router, region, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAvailabilityZones must succeed, got {status} with body {body}"
    );
    let parsed = wire::document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && wire::child(*node, "zoneName").is_some())
        .filter_map(|node| {
            Some((
                wire::child_text(node, "zoneName")?,
                wire::child_text(node, "zoneId")?,
            ))
        })
        .collect()
}

/// Rule: a reported endpoint service is available in every zone its Region has, and its two
/// zone lists agree position by position with what the Region itself reports.
#[tokio::test]
async fn an_endpoint_service_reports_every_zone_of_its_region_in_one_order() {
    let (router, _engine) = emulator();

    for (region, service_name, _) in ROWS {
        let case = *service_name;
        let expected = zones_of_region(case, &router, region).await;
        let body = described(case, &router, region, service_name).await;
        let (names, ids) = reported_zones(case, &body);

        let expected_names: Vec<String> =
            expected.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names, expected_names,
            "case {case:?}: expected availabilityZoneSet {expected_names:?}, got {names:?}"
        );
        let expected_ids: Vec<String> = expected.iter().map(|(_, id)| id.clone()).collect();
        assert_eq!(
            ids, expected_ids,
            "case {case:?}: expected availabilityZoneIdSet {expected_ids:?}, got {ids:?}"
        );
        // The two lists are one pairing read two ways, so neither can be short of the other.
        assert_eq!(
            names.len(),
            ids.len(),
            "case {case:?}: expected as many zone ids as zone names, got {names:?} and {ids:?}"
        );
    }
}

/// Rule: a service that publishes a default public DNS name reports it, carries it in its name
/// set and reports its ownership verified; one that publishes none reports none of the three.
#[tokio::test]
async fn the_private_dns_members_follow_the_name_the_service_publishes() {
    let (router, _engine) = emulator();

    for (region, service_name, private_dns_name) in ROWS {
        let case = *service_name;
        let body = described(case, &router, region, service_name).await;
        let reported = member(case, &body, "privateDnsName");
        match private_dns_name {
            Some(expected) => {
                assert_eq!(
                    reported.as_deref(),
                    Some(*expected),
                    "case {case:?}: expected privateDnsName {expected:?}, got {reported:?}"
                );
                // The set carries the same name: one published fact read twice.
                let set = wire::document(case, &body);
                let entry = set
                    .descendants()
                    .find(|node| node.is_element() && node.tag_name().name() == "privateDnsNameSet")
                    .and_then(|node| {
                        node.descendants()
                            .filter(|inner| {
                                inner.is_element() && inner.tag_name().name() == "privateDnsName"
                            })
                            .map(|inner| inner.text().unwrap_or_default().to_string())
                            .next()
                    });
                assert_eq!(
                    entry.as_deref(),
                    Some(*expected),
                    "case {case:?}: expected privateDnsNameSet to carry {expected:?}, got {entry:?}"
                );
                let state = member(case, &body, "privateDnsNameVerificationState");
                assert_eq!(
                    state.as_deref(),
                    Some(VERIFIED),
                    "case {case:?}: expected privateDnsNameVerificationState {VERIFIED:?}, \
                     got {state:?}"
                );
            }
            None => {
                assert!(
                    reported.is_none(),
                    "case {case:?}: a service publishing no name reports no privateDnsName, \
                     got {reported:?}"
                );
                for absent in ["privateDnsNameSet", "privateDnsNameVerificationState"] {
                    let reported = member(case, &body, absent);
                    assert!(
                        reported.is_none(),
                        "case {case:?}: a service publishing no name reports no {absent}, \
                         got {reported:?}"
                    );
                }
            }
        }
    }
}

/// Rule: the id of an endpoint service takes the `vpce-svc-` prefix and 17 hexadecimal digits,
/// and one service keeps one id across reads.
#[tokio::test]
async fn an_endpoint_service_id_is_the_published_seventeen_digit_form() {
    let (router, _engine) = emulator();

    for (region, service_name, _) in ROWS {
        let case = *service_name;
        let body = described(case, &router, region, service_name).await;
        let reported = member(case, &body, "serviceId")
            .unwrap_or_else(|| panic!("case {case:?}: no serviceId reported, got {body}"));
        // The one reading of that shape this crate's suites share
        // (`super::vpc_endpoint_services`), so no suite holds a looser copy of it.
        assert!(
            is_derived_service_id(&reported),
            "case {case:?}: expected a published endpoint service id, got {reported:?}"
        );
        // A read reports a fact of the catalog, so a second read reports the same id.
        let again = described(case, &router, region, service_name).await;
        let second = member(case, &again, "serviceId");
        assert_eq!(
            second.as_deref(),
            Some(reported.as_str()),
            "case {case:?}: expected the same serviceId on a second read, got {second:?}"
        );
    }
}
