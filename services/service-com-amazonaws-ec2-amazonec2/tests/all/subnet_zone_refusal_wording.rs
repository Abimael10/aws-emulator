// SPDX-License-Identifier: CC0-1.0

//! WHAT a subnet-creating refusal tells a caller about the Availability Zones it may use,
//! driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! ONE rule is under test: a `CreateSubnet` naming a zone the Region does not have is refused
//! HTTP 400 `InvalidParameterValue`, and the sentence it carries states the value the
//! request sent, the MEMBER it sent it under and the Region's FULL set of zones for that
//! member — the zone NAMES for `AvailabilityZone`, the zone IDS for `AvailabilityZoneId`, in
//! the order the Region reports them, and spelled `availability zones` in lower case.
//!
//! The set is read back from `DescribeAvailabilityZones` rather than written out here, so the
//! rule under test is the AGREEMENT between what the refusal advertises and what the Region
//! has: a refusal that advertised fewer zones than the Region reports would tell a caller
//! retrying with a real zone that it does not exist, and one that advertised more would offer
//! a zone no subnet can be created in. The final row creates a subnet in the LAST zone the
//! Region reports, so every zone the sentence offers is a zone the operation serves.
//!
//! The rows differ in the Region, in which member named the zone, and in how the value misses
//! — a zone id where a name is required, a zone of another Region, and a zone of no Region at
//! all.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};

/// The Regions this suite drives, one of which is not the emulator's default: the sentence is
/// a fact of the Region the request is signed for, so it is asserted in more than one.
const REGIONS: [&str; 3] = ["us-east-1", "us-west-2", "eu-central-1"];

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call against `region`, as its HTTP status and XML body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, region, action, params).await
}

/// The zone names and the zone ids `region` reports, in the order it reports them.
async fn reported_zones(router: &MultiServiceRouter, region: &str) -> (Vec<String>, Vec<String>) {
    let (status, body) = call(router, region, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "{region}: DescribeAvailabilityZones expected HTTP 200, got {status} with body {body}"
    );
    let names = elements(&body, "zoneName");
    let ids = elements(&body, "zoneId");
    assert!(
        !names.is_empty() && names.len() == ids.len(),
        "{region}: expected a zone name and a zone id per zone, got {names:?} and {ids:?}"
    );
    (names, ids)
}

/// A VPC of `region`, by id.
async fn create_vpc(router: &MultiServiceRouter, region: &str, cidr_block: &str) -> String {
    let params = [param("CidrBlock", cidr_block)];
    let (status, body) = call(router, region, "CreateVpc", &params).await;
    assert_eq!(
        status, 200,
        "{region}: CreateVpc {cidr_block} expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// The sentence EC2 refuses a subnet-creating request that names a zone the Region does not
/// have with, written out here rather than read from the crate so a reworded refusal still
/// fails: the value in round brackets, the member it was sent under, and the zones the Region
/// offers for that member — `availability zones` in lower case, comma-separated, ending in a
/// full stop.
fn expected_sentence(member: &str, requested: &str, offered: &[String]) -> String {
    format!(
        "Value ({requested}) for parameter {member} is invalid. Subnets can currently only be \
         created in the following availability zones: {}.",
        offered.join(", "),
    )
}

/// A zone the Region does not have is refused `InvalidParameterValue`, and the refusal
/// advertises exactly the zones the Region reports for the member the request used.
#[tokio::test]
async fn a_zone_the_region_does_not_have_is_refused_advertising_the_regions_own_zones() {
    let (router, _engine) = super::ec2_query::emulator();

    for (index, region) in REGIONS.into_iter().enumerate() {
        let (names, ids) = reported_zones(&router, region).await;
        let vpc_id = create_vpc(&router, region, &format!("10.{}.0.0/16", 200 + index)).await;

        // Each row: the case, the member the zone is named under, and the value it carries.
        // A zone id under the NAME member and a name under the ID member are the same
        // mistake from two sides; the third row is a value of no Region at all.
        let cases: [(&str, &str, String); 5] = [
            ("a zone id where a name is required", "AvailabilityZone", ids[0].clone()),
            (
                "another Region's zone name",
                "AvailabilityZone",
                format!("{}z", REGIONS[(index + 1) % REGIONS.len()]),
            ),
            ("no zone of any Region", "AvailabilityZone", "zone-none".to_string()),
            ("a zone name where an id is required", "AvailabilityZoneId", names[0].clone()),
            ("no zone id of any Region", "AvailabilityZoneId", "xyz1-az9".to_string()),
        ];

        for (case, member, requested) in cases {
            let params = [
                param("VpcId", &vpc_id),
                param("CidrBlock", &format!("10.{}.1.0/24", 200 + index)),
                param(member, &requested),
            ];
            let (status, body) = call(&router, region, "CreateSubnet", &params).await;
            assert_eq!(
                status, 400,
                "case {case:?} in {region}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValue",
                "case {case:?} in {region}: expected InvalidParameterValue, got {body}"
            );
            // The member the sentence names is the request's own member, spelled the way the
            // service spells a parameter in that sentence (a lower-case initial), and the set
            // it offers is the one that member selects a zone by.
            let (named_member, offered) = if member == "AvailabilityZone" {
                ("availabilityZone", &names)
            } else {
                ("availabilityZoneId", &ids)
            };
            let expected = expected_sentence(named_member, &requested, offered);
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?} in {region}: expected the message {expected:?}, got {body}"
            );
        }
    }
}

/// The other side of that rule: every zone the refusal advertises IS a zone a subnet can be
/// created in — asserted on the LAST zone the Region reports, the one a caller reaches only by
/// reading the sentence to its end.
#[tokio::test]
async fn the_last_zone_the_refusal_advertises_is_a_zone_a_subnet_can_be_created_in() {
    let (router, _engine) = super::ec2_query::emulator();

    for (index, region) in REGIONS.into_iter().enumerate() {
        let (names, ids) = reported_zones(&router, region).await;
        let vpc_id = create_vpc(&router, region, &format!("10.{}.0.0/16", 210 + index)).await;
        let last_name = names.last().cloned().unwrap_or_default();
        let last_id = ids.last().cloned().unwrap_or_default();

        let params = [
            param("VpcId", &vpc_id),
            param("CidrBlock", &format!("10.{}.2.0/24", 210 + index)),
            param("AvailabilityZone", &last_name),
        ];
        let (status, body) = call(&router, region, "CreateSubnet", &params).await;
        assert_eq!(
            status, 200,
            "{region}: a subnet in the last advertised zone {last_name:?} expected HTTP 200, \
             got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "availabilityZone").as_deref(),
            Some(last_name.as_str()),
            "{region}: expected the subnet in {last_name:?}, got {body}"
        );
        assert_eq!(
            element(&body, "availabilityZoneId").as_deref(),
            Some(last_id.as_str()),
            "{region}: expected the zone id {last_id:?} the Region reports for {last_name:?}, \
             got {body}"
        );
    }
}
