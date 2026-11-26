// SPDX-License-Identifier: CC0-1.0

//! The Availability Zone ids this account's zone NAMES map to, and the operations that echo
//! one.
//!
//! A zone id names one physical zone for every account alike, while a zone NAME is mapped
//! onto it per account ("we independently map Availability Zones to names for each Amazon Web
//! Services account", the EC2 User Guide's "Regions and Zones"), so the mapping is a fact of
//! the account rather than a function of the letter's position in the alphabet.
//!
//! The rules under test:
//!
//! 1. `DescribeAvailabilityZones` reports this account's mapping: in `us-west-2` the name
//!    `us-west-2a` is its SECOND zone (`usw2-az2`) and `us-west-2b` its first, and the two
//!    that follow are mapped in order.
//! 2. Every operation that echoes a zone id reads that same mapping, so an id a caller reads
//!    off one response can be joined back to the zone list: a volume created in a zone
//!    (`CreateVolume`), a subnet created in it (`CreateSubnet`) and the zone list itself all
//!    answer the one id for one name.
//! 3. The mapping is a read: asking twice answers the same ids.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, owned};

/// The emulator the tests drive.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The Region whose mapping this suite pins.
const REGION: &str = "us-west-2";

/// This account's zone NAMES in `REGION` and the zone id each one is mapped onto.
const ZONE_IDS: [(&str, &str); 4] = [
    ("us-west-2a", "usw2-az2"),
    ("us-west-2b", "usw2-az1"),
    ("us-west-2c", "usw2-az3"),
    ("us-west-2d", "usw2-az4"),
];

/// The `zoneName`/`zoneId` pairs one `DescribeAvailabilityZones` response carries, in the
/// order it reported them.
fn reported_pairs(case: &str, body: &str) -> Vec<(String, String)> {
    let parsed = super::wire::document(case, body);
    let list = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "availabilityZoneInfo")
        .unwrap_or_else(|| panic!("case {case:?}: no <availabilityZoneInfo> in {body}"));
    list.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .map(|item| {
            let member = |name: &str| {
                super::wire::child_text(item, name)
                    .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in a zone of {body}"))
            };
            (member("zoneName"), member("zoneId"))
        })
        .collect()
}

/// Rule 1 and rule 3: the zone list carries this account's mapping, and carries it again on
/// the next call.
#[tokio::test]
async fn the_zone_list_reports_this_accounts_name_to_id_mapping() {
    let router = emulator();
    for attempt in ["first read", "repeated read"] {
        let (status, body) = call(&router, REGION, "DescribeAvailabilityZones", &[]).await;
        assert_eq!(
            status, 200,
            "case {attempt:?}: expected 200, got {status} {body}"
        );
        let mut pairs = reported_pairs(attempt, &body);
        pairs.sort();
        let expected: Vec<(String, String)> = ZONE_IDS
            .iter()
            .map(|(name, id)| ((*name).to_string(), (*id).to_string()))
            .collect();
        assert_eq!(
            pairs, expected,
            "case {attempt:?}: expected this account's mapping {expected:?}, got {pairs:?}"
        );
    }
}

/// Rule 2 over every zone of the Region: a volume and a subnet created in a zone echo the id
/// the zone list reports for that zone's NAME.
///
/// The rows are the Region's four zone names, which is what the rule generalises over — two
/// of them are the pair this account maps across each other, and the other two are mapped in
/// order, so a sequential derivation cannot pass the table.
#[tokio::test]
async fn an_operation_that_echoes_a_zone_id_agrees_with_the_zone_list() {
    let (router, _engine) = super::ec2_query::emulator();
    // The VPC the subnets are created in: one network for the whole table.
    let (status, body) = call(
        &router,
        REGION,
        "CreateVpc",
        &[("CidrBlock".to_string(), "10.0.0.0/16".to_string())],
    )
    .await;
    assert_eq!(status, 200, "the VPC must be created, got {status} {body}");
    let vpc_id = element(&body, "vpcId").unwrap_or_else(|| panic!("no <vpcId> in {body}"));

    for (index, (zone_name, zone_id)) in ZONE_IDS.iter().enumerate() {
        let (status, body) = call(
            &router,
            REGION,
            "CreateVolume",
            &[
                ("AvailabilityZone".to_string(), (*zone_name).to_string()),
                ("Size".to_string(), "8".to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {zone_name:?}: expected a created volume, got {status} {body}"
        );
        let echoed = element(&body, "availabilityZoneId");
        assert_eq!(
            echoed.as_deref(),
            Some(*zone_id),
            "case {zone_name:?}: expected availabilityZoneId {zone_id:?}, got {body}"
        );

        let (status, body) = call(
            &router,
            REGION,
            "CreateSubnet",
            &[
                ("VpcId".to_string(), vpc_id.clone()),
                ("AvailabilityZone".to_string(), (*zone_name).to_string()),
                ("CidrBlock".to_string(), format!("10.0.{index}.0/24")),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {zone_name:?}: expected a created subnet, got {status} {body}"
        );
        let echoed = element(&body, "availabilityZoneId");
        assert_eq!(
            echoed.as_deref(),
            Some(*zone_id),
            "case {zone_name:?}: expected the subnet's availabilityZoneId {zone_id:?}, got {body}"
        );
    }
}

/// A Region this account maps in order is unaffected: the `n`th name is the `n`th zone.
#[tokio::test]
async fn a_region_mapped_in_order_reports_sequential_ids() {
    let router = emulator();
    let (status, body) = call(&router, "eu-west-1", "DescribeAvailabilityZones", &[]).await;
    assert_eq!(status, 200, "expected 200, got {status} {body}");
    let mut pairs = reported_pairs("eu-west-1", &body);
    pairs.sort();
    let ids: Vec<String> = pairs.into_iter().map(|(_, id)| id).collect();
    assert_eq!(
        ids,
        owned(&["euw1-az1", "euw1-az2", "euw1-az3"]),
        "expected eu-west-1's names mapped in order, got {ids:?}"
    );
}
