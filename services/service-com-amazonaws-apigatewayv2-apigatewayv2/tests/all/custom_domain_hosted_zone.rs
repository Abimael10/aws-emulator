// SPDX-License-Identifier: CC0-1.0

//! The hosted zone a custom domain name's endpoints are aliased through:
//! `DomainNameConfiguration$HostedZoneId`, "the Amazon Route 53 Hosted Zone ID of the
//! endpoint".
//!
//! The rule these cases assert, one level above the recorded case (one `REGIONAL`
//! endpoint read once it reached `AVAILABLE`):
//!
//! > **Every `DomainNameConfigurations` entry a writer or a reader of a custom domain name
//! > reports carries the hosted zone id of the endpoint it describes**, beside that
//! > endpoint's `ApiGatewayDomainName` — it is what a caller needs to point a Route 53
//! > ALIAS record at the domain. It is a fact about the ENDPOINT rather than about the
//! > resource: an `EDGE` endpoint is fronted by CloudFront, so every one of them reports
//! > the one zone id Route 53 publishes for a CloudFront distribution
//! > ([`CLOUDFRONT_HOSTED_ZONE_ID`], which `AliasTarget$HostedZoneId` states as
//! > "CloudFront distribution: Specify Z2FDTNDATAQYW2"), in every Region; a `REGIONAL`
//! > endpoint reports one id per Region, the same for every domain name and every read in
//! > that Region and a different one in another, and never the CloudFront one. Where AWS's
//! > own answer for the Region is known, a regional endpoint reports THAT id
//! > ([`REGIONAL_ENDPOINT_HOSTED_ZONES`], pinned here by value in [`PUBLISHED_ZONE_IDS`])
//! > rather than one of its own. An id is `Z` followed by the twelve or thirteen characters
//! > AWS's published endpoint zone ids carry.
//!
//! The rows span what the rule generalises over: the endpoint type (`REGIONAL` named,
//! `REGIONAL` left to the default, `EDGE` — the whole `EndpointType` catalog), the Region
//! the request is signed for, how many configurations one domain name carries, and whether
//! the entry names a certificate. On top of the table: two domain names in one Region, the
//! same shape in two Regions, every reader of the resource, and an update that moves an
//! endpoint from one type to the other.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use std::collections::BTreeMap;

use service_com_amazonaws_apigatewayv2_apigatewayv2::ops::types::ENDPOINT_TYPE_VALUES;
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    published_regional_endpoint_hosted_zone_id, CLOUDFRONT_HOSTED_ZONE_ID, DEFAULT_ENDPOINT_TYPE,
    EDGE_ENDPOINT_TYPE, REGIONAL_ENDPOINT_HOSTED_ZONES,
};

use crate::api_lifecycle::{as_str, call_in_region, emulator};

/// The member every configuration entry must carry.
const HOSTED_ZONE_ID: &str = "hostedZoneId";

/// How a published endpoint hosted zone id is shaped on the wire: a leading `Z` and twelve
/// or thirteen more characters (`Z2FDTNDATAQYW2` and `Z1UJRXOUMOOFQ8` carry thirteen,
/// `ZLY8HYME6SFDD` and `ZOJJZC49E0EPZ` twelve). Spelled as the WHOLE length, which is one
/// more than the crate's `HOSTED_ZONE_ID_LEN` (the count AFTER the leading `Z`), so the two
/// are never read as the same number.
const PUBLISHED_ZONE_ID_CHARS: (usize, usize) = (13, 14);

/// What AWS itself answers for a REGIONAL endpoint of a custom domain name created in a
/// Region, Region by Region: the zone of that Region's API Gateway endpoint, which AWS
/// assigns and this suite pins by VALUE.
///
/// The suite's OWN copy of the catalog the service carries
/// ([`REGIONAL_ENDPOINT_HOSTED_ZONES`]), so the service asserts nothing against itself: an
/// id added to or edited in the service's table without a published AWS answer behind it
/// fails [`the_service_carries_the_regional_zone_ids_aws_publishes`], and a Region added to
/// either belongs in both. `us-west-2` is the recorded AWS answer for a regional custom
/// domain name created there; every other row is AWS's published answer for its own Region.
const PUBLISHED_ZONE_IDS: &[(&str, &str)] = &[
    ("af-south-1", "Z2DHW2332DAMTN"),
    ("ap-east-1", "Z3FD1VL90ND7K5"),
    ("ap-northeast-1", "Z1YSHQZHG15GKL"),
    ("ap-northeast-2", "Z20JF4UZKIW1U8"),
    ("ap-northeast-3", "Z2YQB5RD63NC85"),
    ("ap-south-1", "Z3VO1THU9YC4UR"),
    ("ap-southeast-1", "ZL327KTPIQFUL"),
    ("ap-southeast-2", "Z2RPCDW04V8134"),
    ("ca-central-1", "Z19DQILCV0OWEC"),
    ("eu-central-1", "Z1U9ULNL0V5AJ3"),
    ("eu-north-1", "Z3UWIKFBOOGXPP"),
    ("eu-west-1", "ZLY8HYME6SFDD"),
    ("eu-west-2", "ZJ5UAJN8Y3Z2Q"),
    ("eu-west-3", "Z3KY65QIEKYHQQ"),
    ("me-south-1", "Z20ZBPC0SS8806"),
    ("sa-east-1", "ZCMLWB8V5SYIT"),
    ("us-east-1", "Z1UJRXOUMOOFQ8"),
    ("us-east-2", "ZOJJZC49E0EPZ"),
    ("us-west-1", "Z2MUQ32089INYE"),
    ("us-west-2", "Z2OJLYMUO9EFXC"),
];

/// Regions with no published API Gateway endpoint zone id carried by this service, spanning
/// two partitions' naming. A regional endpoint in one still reports an id of its own, which
/// is what keeps the member's per-Region invariance everywhere.
const UNPUBLISHED_REGIONS: &[&str] = &["us-gov-west-1", "cn-north-1"];

/// Send one control-plane request signed for `region`.
async fn call(
    emulator: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_in_region(emulator, region, method, uri, body).await
}

/// Create one domain name in `region` and answer the created document.
async fn create(
    emulator: &MultiServiceRouter,
    case: &str,
    region: &str,
    domain: &str,
    configurations: &Value,
) -> Value {
    let (status, body) = call(
        emulator,
        region,
        "POST",
        "/v2/domainnames",
        Some(json!({
            "domainName": domain,
            "domainNameConfigurations": configurations
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDomainName must answer 201, got {status} {body}"
    );
    body
}

/// The `domainNameConfigurations` list of a response.
fn configurations(body: &Value) -> Vec<Value> {
    body.get("domainNameConfigurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `hostedZoneId` of each configuration entry, in the order the response lists them,
/// asserting that every entry carries one at all and that it is shaped like a published
/// endpoint zone id.
fn hosted_zone_ids(case: &str, body: &Value) -> Vec<String> {
    let entries = configurations(body);
    assert!(
        !entries.is_empty(),
        "case {case:?}: the response must carry the domain name's configurations, got {body}"
    );
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let zone = as_str(entry, HOSTED_ZONE_ID);
            assert!(
                !zone.is_empty(),
                "case {case:?}: configuration {index} must carry {HOSTED_ZONE_ID}, got {entry}"
            );
            let (shortest, longest) = PUBLISHED_ZONE_ID_CHARS;
            assert!(
                (shortest..=longest).contains(&zone.chars().count()),
                "case {case:?}: configuration {index} must carry a zone id of \
                 {shortest}..{longest} characters, got {zone:?}"
            );
            assert!(
                zone.starts_with('Z')
                    && zone
                        .chars()
                        .all(|character| character.is_ascii_uppercase()
                            || character.is_ascii_digit()),
                "case {case:?}: configuration {index} must carry an uppercase alphanumeric zone \
                 id beginning with 'Z', got {zone:?}"
            );
            // The endpoint a caller aliases to is reported beside its zone, so the pair is
            // complete enough to build the ALIAS record from.
            assert!(
                !as_str(entry, "apiGatewayDomainName").is_empty(),
                "case {case:?}: configuration {index} must name the endpoint its zone \
                 fronts, got {entry}"
            );
            zone
        })
        .collect()
}

/// One row of the table: the Region, the configurations the create asks for, and the
/// endpoint type each resulting entry describes.
struct Row {
    case: &'static str,
    region: &'static str,
    domain: &'static str,
    configurations: Value,
    /// The `EndpointType` of each entry, in order.
    endpoint_types: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a regional endpoint, left to the default endpoint type",
            region: "eu-central-1",
            domain: "default-type.example.com",
            configurations: json!([{}]),
            endpoint_types: vec![DEFAULT_ENDPOINT_TYPE],
        },
        Row {
            case: "a regional endpoint naming its type and a certificate",
            region: "eu-central-1",
            domain: "regional.example.com",
            configurations: json!([{
                "endpointType": "REGIONAL",
                "certificateArn": "arn:aws:acm:eu-central-1:123456789012:certificate/regional"
            }]),
            endpoint_types: vec!["REGIONAL"],
        },
        Row {
            case: "an edge-optimized endpoint",
            region: "eu-central-1",
            domain: "edge.example.com",
            configurations: json!([{ "endpointType": EDGE_ENDPOINT_TYPE }]),
            endpoint_types: vec![EDGE_ENDPOINT_TYPE],
        },
        Row {
            case: "an edge-optimized endpoint in another Region",
            region: "ap-southeast-2",
            domain: "edge-far.example.com",
            configurations: json!([{ "endpointType": EDGE_ENDPOINT_TYPE }]),
            endpoint_types: vec![EDGE_ENDPOINT_TYPE],
        },
        Row {
            case: "one domain name carrying both endpoint types, in the order given",
            region: "us-west-2",
            domain: "both-types.example.com",
            configurations: json!([
                { "endpointType": EDGE_ENDPOINT_TYPE },
                { "endpointType": "REGIONAL" }
            ]),
            endpoint_types: vec![EDGE_ENDPOINT_TYPE, "REGIONAL"],
        },
    ]
}

/// The zone id an entry of `endpoint_type` in `region` must report, asserted against what
/// the rule states for that type: the published CloudFront zone for an edge endpoint, and
/// for a regional one the zone AWS publishes for the Region where that is known — never the
/// CloudFront one either way round.
fn assert_zone_for_type(case: &str, index: usize, region: &str, endpoint_type: &str, zone: &str) {
    if endpoint_type == EDGE_ENDPOINT_TYPE {
        assert_eq!(
            zone, CLOUDFRONT_HOSTED_ZONE_ID,
            "case {case:?}: configuration {index} is an {EDGE_ENDPOINT_TYPE} endpoint, which \
             is aliased through {CLOUDFRONT_HOSTED_ZONE_ID}, got {zone:?}"
        );
        return;
    }
    assert_ne!(
        zone, CLOUDFRONT_HOSTED_ZONE_ID,
        "case {case:?}: configuration {index} is a {endpoint_type} endpoint, which is not \
         fronted by CloudFront, got {zone:?}"
    );
    if let Some((_, published)) = PUBLISHED_ZONE_IDS
        .iter()
        .find(|(published_region, _)| *published_region == region)
    {
        assert_eq!(
            zone, *published,
            "case {case:?}: configuration {index} is a {endpoint_type} endpoint in \
             {region:?}, whose published zone is {published:?}, got {zone:?}"
        );
    }
}

/// Every writer and every reader of a custom domain name reports, for each of its
/// configurations, the hosted zone id of the endpoint that entry describes.
#[tokio::test]
async fn every_configuration_reports_its_endpoint_s_hosted_zone_id() {
    let emulator = emulator();

    for row in rows() {
        let case = row.case;
        let created = create(
            &emulator,
            case,
            row.region,
            row.domain,
            &row.configurations,
        )
        .await;
        let zones = hosted_zone_ids(case, &created);
        assert_eq!(
            zones.len(),
            row.endpoint_types.len(),
            "case {case:?}: expected {} configurations, got {created}",
            row.endpoint_types.len()
        );
        for (index, endpoint_type) in row.endpoint_types.iter().enumerate() {
            assert_eq!(
                as_str(&configurations(&created)[index], "endpointType"),
                *endpoint_type,
                "case {case:?}: configuration {index} must describe a {endpoint_type} \
                 endpoint, got {created}"
            );
            assert_zone_for_type(case, index, row.region, endpoint_type, &zones[index]);
        }

        // The single reader: what the create answered is what a later read reports.
        let (status, read) = call(
            &emulator,
            row.region,
            "GET",
            &format!("/v2/domainnames/{}", row.domain),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDomainName must answer 200, got {status} {read}"
        );
        assert_eq!(
            hosted_zone_ids(case, &read),
            zones,
            "case {case:?}: GetDomainName must report the zones the create answered, got {read}"
        );

        // And the collection reader, which projects the same records.
        let (status, listed) = call(&emulator, row.region, "GET", "/v2/domainnames", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDomainNames must answer 200, got {status} {listed}"
        );
        let item = listed
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| as_str(item, "domainName") == row.domain)
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: GetDomainNames must list {}", row.domain));
        assert_eq!(
            hosted_zone_ids(case, &item),
            zones,
            "case {case:?}: GetDomainNames must report the same zones, got {item}"
        );
    }
}

/// A regional endpoint's zone is the REGION's: two domain names created in one Region
/// report the same id, and the same shape created in another Region reports a different
/// one — while an edge endpoint reports the one CloudFront zone in every Region.
#[tokio::test]
async fn a_regional_zone_is_per_region_and_an_edge_zone_is_global() {
    let emulator = emulator();
    let regional = json!([{ "endpointType": "REGIONAL" }]);
    let edge = json!([{ "endpointType": EDGE_ENDPOINT_TYPE }]);

    let case = "two regional domain names in one Region";
    let first = create(&emulator, case, "eu-west-1", "first.example.com", &regional).await;
    let second = create(&emulator, case, "eu-west-1", "second.example.com", &regional).await;
    assert_eq!(
        hosted_zone_ids(case, &first),
        hosted_zone_ids(case, &second),
        "case {case:?}: the zone is the endpoint's, so two domain names in one Region share \
         it, got {first} against {second}"
    );

    let case = "the same regional shape in another Region";
    let elsewhere = create(&emulator, case, "sa-east-1", "third.example.com", &regional).await;
    assert_ne!(
        hosted_zone_ids(case, &first),
        hosted_zone_ids(case, &elsewhere),
        "case {case:?}: a regional endpoint's zone is per Region, got {first} against \
         {elsewhere}"
    );

    let case = "an edge endpoint in each of those Regions";
    let near = create(&emulator, case, "eu-west-1", "near-edge.example.com", &edge).await;
    let far = create(&emulator, case, "sa-east-1", "far-edge.example.com", &edge).await;
    assert_eq!(
        hosted_zone_ids(case, &near),
        hosted_zone_ids(case, &far),
        "case {case:?}: every CloudFront-fronted endpoint shares one zone, got {near} \
         against {far}"
    );

    // And the catalog is covered: every value `EndpointType` publishes was driven above.
    for endpoint_type in ENDPOINT_TYPE_VALUES {
        assert!(
            [EDGE_ENDPOINT_TYPE, "REGIONAL"].contains(endpoint_type),
            "the EndpointType catalog grew a value this suite does not drive: \
             {endpoint_type:?}"
        );
    }
}

/// The zone follows the ENDPOINT, so an update that moves a domain name onto the other
/// endpoint type reports the other type's zone from then on.
#[tokio::test]
async fn moving_the_endpoint_type_moves_the_reported_zone() {
    let emulator = emulator();
    let region = "us-east-2";
    let case = "a regional domain name patched onto an edge endpoint";
    let domain = "migrating.example.com";

    let created = create(
        &emulator,
        case,
        region,
        domain,
        &json!([{ "endpointType": "REGIONAL" }]),
    )
    .await;
    let before = hosted_zone_ids(case, &created);
    assert_zone_for_type(case, 0, region, "REGIONAL", &before[0]);

    let (status, patched) = call(
        &emulator,
        region,
        "PATCH",
        &format!("/v2/domainnames/{domain}"),
        Some(json!({
            "domainNameConfigurations": [{ "endpointType": EDGE_ENDPOINT_TYPE }]
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateDomainName must answer 200, got {status} {patched}"
    );
    let after = hosted_zone_ids(case, &patched);
    assert_zone_for_type(case, 0, region, EDGE_ENDPOINT_TYPE, &after[0]);
    assert_ne!(
        before, after,
        "case {case:?}: the endpoint moved, so its zone must move with it, got {before:?} \
         against {after:?}"
    );

    let (status, read) = call(
        &emulator,
        region,
        "GET",
        &format!("/v2/domainnames/{domain}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetDomainName must answer 200, got {status} {read}"
    );
    assert_eq!(
        hosted_zone_ids(case, &read),
        after,
        "case {case:?}: the read must report the patched endpoint's zone, got {read}"
    );
}

/// A regional endpoint in a Region AWS's answer is known for reports THAT id, through every
/// reader, and one in a Region it is not known for still reports an id of its own.
///
/// The rows are the published catalog itself — twenty Regions, of which the recorded case
/// (`us-west-2`) is one — plus Regions carrying no published id.
#[tokio::test]
async fn a_regional_endpoint_reports_the_zone_aws_publishes_for_its_region() {
    let emulator = emulator();
    let regional = json!([{ "endpointType": "REGIONAL" }]);
    let mut by_region: BTreeMap<&str, String> = BTreeMap::new();

    for region in PUBLISHED_ZONE_IDS
        .iter()
        .map(|(region, _)| *region)
        .chain(UNPUBLISHED_REGIONS.iter().copied())
    {
        let case = region;
        let domain = format!("zone-{region}.example.com");
        let created = create(&emulator, case, region, &domain, &regional).await;
        let zones = hosted_zone_ids(case, &created);
        assert_zone_for_type(case, 0, region, "REGIONAL", &zones[0]);

        // Every reader of the resource reports the id the create answered.
        let (status, read) = call(
            &emulator,
            region,
            "GET",
            &format!("/v2/domainnames/{domain}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDomainName must answer 200, got {status} {read}"
        );
        assert_eq!(
            hosted_zone_ids(case, &read),
            zones,
            "case {case:?}: the read must report the zone the create answered, got {read}"
        );

        by_region.insert(region, zones[0].clone());
    }

    // One zone per Region: no two Regions share an id.
    for (region, id) in &by_region {
        let sharing: Vec<&&str> = by_region
            .iter()
            .filter(|(other, other_id)| *other != region && *other_id == id)
            .map(|(other, _)| other)
            .collect();
        assert!(
            sharing.is_empty(),
            "case {region:?}: expected a hosted zone id of its own, but {sharing:?} report \
             {id:?} too"
        );
    }
}

/// The service carries exactly the regional ids AWS publishes: every entry of its catalog is
/// a Region AWS's answer is known for and carries that answer, and a Region AWS's answer is
/// not known for carries nothing.
#[test]
fn the_service_carries_the_regional_zone_ids_aws_publishes() {
    let carried: BTreeMap<&str, &str> = REGIONAL_ENDPOINT_HOSTED_ZONES
        .iter()
        .map(|zone| (zone.region, zone.hosted_zone_id))
        .collect();
    let published: BTreeMap<&str, &str> = PUBLISHED_ZONE_IDS.iter().copied().collect();
    assert_eq!(
        carried, published,
        "expected the service to carry AWS's published zone ids, region for region, got \
         {carried:?} against {published:?}"
    );
    assert_eq!(
        carried.len(),
        REGIONAL_ENDPOINT_HOSTED_ZONES.len(),
        "expected one entry per region in the carried catalog, got {carried:?}"
    );
    let (shortest, longest) = PUBLISHED_ZONE_ID_CHARS;
    for (region, id) in &published {
        assert!(
            (shortest..=longest).contains(&id.chars().count()),
            "case {region:?}: expected a published id of {shortest}..{longest} characters, \
             got {id:?}"
        );
        assert_eq!(
            published_regional_endpoint_hosted_zone_id(region),
            Some(*id),
            "case {region:?}: expected the region's published id, got {:?}",
            published_regional_endpoint_hosted_zone_id(region)
        );
    }
    for region in UNPUBLISHED_REGIONS {
        assert_eq!(
            published_regional_endpoint_hosted_zone_id(region),
            None,
            "case {region:?}: AWS's answer for this region is not known, so the service \
             carries none, got {:?}",
            published_regional_endpoint_hosted_zone_id(region)
        );
    }
}
