// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the alias target a custom domain name publishes: the Route 53
//! hosted zone of the endpoint it is served by.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — in the Region its own credential scope
//! names, so what is asserted is what a client in that Region sees.
//!
//! The RULE under test, one level above any one record:
//!
//! > A custom domain name reports the hosted zone of the endpoint that serves it, and a
//! > hosted zone is a fact about the ENDPOINT rather than about the record: a `REGIONAL`
//! > record reports `regionalHostedZoneId`, "the region-specific Amazon Route 53 Hosted Zone
//! > ID of the regional endpoint" — one id per Region, the same for every regional record
//! > created there and for every read of one, in the form AWS publishes a hosted zone id in
//! > — while an `EDGE` record reports the single published `distributionHostedZoneId` of the
//! > CloudFront distribution in front of it and no regional zone, and a `PRIVATE` record,
//! > which is reached through VPC endpoints rather than through a public alias, reports
//! > neither. A record migrated from one endpoint type to the other reports the zone of the
//! > endpoint serving it NOW and none of the one it left.
//!
//! The rows span what the rule generalises over: the Regions whose id AWS publishes and
//! Regions it does not carry (which still report one id of their own), the endpoint types the
//! model declares (`smithy_query(query="shape", shape="EndpointType")`), two records in one
//! Region, every read that reports the record, and the migration in both directions.
//!
//! [`Gateway`] and [`string_member`] are this crate's one wire-test harness; the Region-scoped
//! call below is this suite's own, because the harness's own calls are scoped to one Region.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::domain_name_collection::create_request;
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// The hosted zone every CloudFront distribution is aliased through, which
/// `DomainName$distributionHostedZoneId` states ("The valid value is Z2FDTNDATAQYW2 for all
/// the regions"). Spelled out here so this suite asserts against the documented value rather
/// than against the constant the crate carries.
const CLOUDFRONT_HOSTED_ZONE_ID: &str = "Z2FDTNDATAQYW2";

/// The LENGTH of a hosted zone id AWS publishes for an endpoint: the leading `Z` and
/// thirteen characters after it, the width of the documented
/// [`CLOUDFRONT_HOSTED_ZONE_ID`].
const HOSTED_ZONE_ID_LEN: usize = 14;

/// The hosted zones AWS publishes for API Gateway's REGIONAL endpoints, as this suite's own
/// copy of the table `DomainName$regionalHostedZoneId`'s documentation points at ("AWS
/// Regions and Endpoints for API Gateway").
///
/// Kept here as well as in the crate so the service asserts nothing against itself: an id
/// reported for one of these Regions must be the PUBLISHED one, which is the id a caller's
/// Route 53 alias record has to target.
const PUBLISHED_ZONES: &[(&str, &str)] = &[
    ("us-east-1", "Z1UJRXOUMOOFQ8"),
    ("us-west-2", "Z2OJLYMUO9EFXC"),
];

/// Regions this emulator carries no published id for, which still report one id of their own.
const UNPUBLISHED_REGIONS: &[&str] = &["eu-central-1", "ap-southeast-2"];

/// The `domainNameStatus` of a record whose endpoint migration is still running: "if the
/// status is UPDATING, the domain cannot be modified further until the existing operation is
/// complete" (`DomainName$domainNameStatus`).
const UPDATING: &str = "UPDATING";

/// How long a case waits for a migration the service ACCEPTED to complete. A GENEROUS bound
/// on a transition the emulator's own clock owes; the wait is for the record's STATUS, looked
/// at again and again, never for a span.
const MIGRATION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits between two looks at a record's status.
const LOOK: Duration = Duration::from_millis(50);

/// One API Gateway control-plane call in `region`, answering its status and parsed body.
///
/// The Region a record is created in is the Region of the caller's SigV4 credential scope, so
/// a case addresses a Region by signing for it.
async fn call_in(
    gateway: &Gateway,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("apigateway.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/apigateway/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `CreateDomainName` for `domain_name` served by `endpoint_type`, in `region`, asserting it
/// was accepted.
async fn create_in(
    gateway: &Gateway,
    region: &str,
    domain_name: &str,
    endpoint_type: &str,
    case: &str,
) -> Value {
    let (status, body) = call_in(
        gateway,
        region,
        "POST",
        "/domainnames",
        Some(create_request(
            domain_name,
            json!({ "endpointConfiguration": { "types": [endpoint_type] } }),
        )),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201, got {status} with {body:?}"
    );
    body
}

/// `GetDomainName` for `domain_name` in `region`, asserting the record is there.
async fn read_in(gateway: &Gateway, region: &str, domain_name: &str, case: &str) -> Value {
    let (status, body) = call_in(
        gateway,
        region,
        "GET",
        &format!("/domainnames/{domain_name}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetDomainName to answer 200, got {status} with {body:?}"
    );
    body
}

/// The record `domain_name` holds on the `GetDomainNames` page of `region`.
async fn listed_in(gateway: &Gateway, region: &str, domain_name: &str, case: &str) -> Value {
    let (status, body) = call_in(gateway, region, "GET", "/domainnames?limit=500", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetDomainNames to answer 200, got {status} with {body:?}"
    );
    page_items(&body, case)
        .into_iter()
        .find(|item| item["domainName"].as_str() == Some(domain_name))
        .unwrap_or_else(|| panic!("case {case:?}: expected {domain_name} on the page, got {body:?}"))
}

/// The record `domain_name` once no migration is running on it any more, so the next edit is
/// one the service takes.
///
/// A migration the service accepted completes on its own clock, so this LOOKS AGAIN until the
/// record reports a status other than [`UPDATING`] — never waits a span out — and the
/// deadline is generous enough that only a defect reaches it.
async fn settled_in(gateway: &Gateway, region: &str, domain_name: &str, case: &str) -> Value {
    let deadline = Instant::now() + MIGRATION_DEADLINE;
    loop {
        let read = read_in(gateway, region, domain_name, case).await;
        if read["domainNameStatus"].as_str() != Some(UPDATING) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the migration to complete, the record is still {UPDATING}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Assert `zone` has the form AWS publishes a hosted zone id in: a leading `Z` and
/// [`HOSTED_ZONE_ID_LEN`] characters in all, uppercase alphanumerics throughout.
fn assert_published_form(zone: &str, case: &str) {
    assert_eq!(
        zone.len(),
        HOSTED_ZONE_ID_LEN,
        "case {case:?}: expected a {HOSTED_ZONE_ID_LEN}-character hosted zone id, got {zone:?}"
    );
    assert!(
        zone.starts_with('Z')
            && zone
                .chars()
                .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit()),
        "case {case:?}: expected an uppercase alphanumeric hosted zone id, got {zone:?}"
    );
}

#[tokio::test]
async fn a_regional_domain_name_reports_the_hosted_zone_of_its_region() {
    let gateway = Gateway::new();
    let regions: Vec<&str> = PUBLISHED_ZONES
        .iter()
        .map(|(region, _)| *region)
        .chain(UNPUBLISHED_REGIONS.iter().copied())
        .collect();

    let mut by_region: Vec<(&str, String)> = Vec::new();
    for region in &regions {
        let case = format!("a regional domain name in {region}");
        let domain_name = format!("zone-{region}.example.com");
        let created = create_in(&gateway, region, &domain_name, "REGIONAL", &case).await;
        let zone = string_member(&created, "regionalHostedZoneId", &case);
        assert_published_form(&zone, &case);

        // Where AWS publishes the Region's id, that is the id reported: a caller's alias
        // record targets the zone AWS would serve the endpoint from.
        if let Some((_, published)) = PUBLISHED_ZONES
            .iter()
            .find(|(published_region, _)| published_region == region)
        {
            assert_eq!(
                zone, *published,
                "case {case:?}: expected the published zone {published}, got {zone:?}"
            );
        }

        // Every read of the record reports the same zone, and so does a second record created
        // in the same Region: the id is the Region's, not the record's.
        let read = read_in(&gateway, region, &domain_name, &case).await;
        assert_eq!(
            read["regionalHostedZoneId"].as_str(),
            Some(zone.as_str()),
            "case {case:?}: expected GetDomainName to report {zone:?}, got {:?}",
            read["regionalHostedZoneId"]
        );
        let listed = listed_in(&gateway, region, &domain_name, &case).await;
        assert_eq!(
            listed["regionalHostedZoneId"].as_str(),
            Some(zone.as_str()),
            "case {case:?}: expected the listed record to report {zone:?}, got {listed:?}"
        );
        let sibling = format!("zone-second-{region}.example.com");
        let second = create_in(&gateway, region, &sibling, "REGIONAL", &case).await;
        assert_eq!(
            second["regionalHostedZoneId"].as_str(),
            Some(zone.as_str()),
            "case {case:?}: expected a second record in the Region to report {zone:?}, got {:?}",
            second["regionalHostedZoneId"]
        );
        by_region.push((region, zone));
    }

    // Different Regions are different zones: the member identifies the Region's endpoint.
    for (region, zone) in &by_region {
        for (other_region, other_zone) in &by_region {
            if region == other_region {
                continue;
            }
            assert_ne!(
                zone, other_zone,
                "case \"one zone per Region\": expected {region} and {other_region} to report \
                 different zones, both reported {zone:?}"
            );
        }
    }
}

#[tokio::test]
async fn only_a_regional_domain_name_reports_a_regional_hosted_zone() {
    let gateway = Gateway::new();
    let region = "us-east-1";

    // (the endpoint type, does it report a regional zone?, the distribution zone it reports)
    let rows: &[(&str, bool, Option<&str>)] = &[
        ("REGIONAL", true, None),
        ("EDGE", false, Some(CLOUDFRONT_HOSTED_ZONE_ID)),
        ("PRIVATE", false, None),
    ];
    for (endpoint_type, regional, distribution) in rows {
        let case = format!("a {endpoint_type} domain name");
        let domain_name = format!("alias-{}.example.com", endpoint_type.to_lowercase());
        let created = create_in(&gateway, region, &domain_name, endpoint_type, &case).await;
        assert_eq!(
            created["regionalHostedZoneId"].is_string(),
            *regional,
            "case {case:?}: expected regionalHostedZoneId present={regional}, got {:?}",
            created["regionalHostedZoneId"]
        );
        assert_eq!(
            created["distributionHostedZoneId"].as_str(),
            *distribution,
            "case {case:?}: expected distributionHostedZoneId {distribution:?}, got {:?}",
            created["distributionHostedZoneId"]
        );
    }
}

#[tokio::test]
async fn a_migrated_domain_name_reports_the_zone_of_the_endpoint_serving_it_now() {
    let gateway = Gateway::new();
    let region = "us-west-2";
    let domain_name = "migrated-zone.example.com";

    let case = "an edge-optimized record before the migration";
    let created = create_in(&gateway, region, domain_name, "EDGE", case).await;
    assert_eq!(
        (
            created["regionalHostedZoneId"].as_str(),
            created["distributionHostedZoneId"].as_str()
        ),
        (None, Some(CLOUDFRONT_HOSTED_ZONE_ID)),
        "case {case:?}: expected only the distribution zone, got {created:?}"
    );

    // Each migration reassigns the alias members to the endpoint serving the record now.
    for (case, migrated_to, regional_zone, distribution_zone) in [
        (
            "migrated onto the regional endpoint",
            "REGIONAL",
            PUBLISHED_ZONES
                .iter()
                .find(|(published_region, _)| *published_region == region)
                .map(|(_, zone)| *zone),
            None,
        ),
        (
            "migrated back onto the edge-optimized endpoint",
            "EDGE",
            None,
            Some(CLOUDFRONT_HOSTED_ZONE_ID),
        ),
    ] {
        let (status, patched) = call_in(
            &gateway,
            region,
            "PATCH",
            &format!("/domainnames/{domain_name}"),
            Some(json!({
                "patchOperations": [{
                    "op": "replace",
                    "path": "/endpointConfiguration/types",
                    "value": migrated_to,
                }]
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {patched:?}"
        );
        assert_eq!(
            (
                patched["regionalHostedZoneId"].as_str(),
                patched["distributionHostedZoneId"].as_str()
            ),
            (regional_zone, distribution_zone),
            "case {case:?}: expected ({regional_zone:?}, {distribution_zone:?}), got {patched:?}"
        );
        // And the stored record reports the same pair, so the migration is what a later
        // caller reads too — read once the migration this patch started has completed, since
        // a record still migrating takes no further edit.
        let read = settled_in(&gateway, region, domain_name, case).await;
        assert_eq!(
            (
                read["regionalHostedZoneId"].as_str(),
                read["distributionHostedZoneId"].as_str()
            ),
            (regional_zone, distribution_zone),
            "case {case:?}: expected the stored record to report the same pair, got {read:?}"
        );
    }
}
