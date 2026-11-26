// SPDX-License-Identifier: CC0-1.0

//! An OPTIONAL request member that carries the EMPTY STRING, driven over the wire the service
//! speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! ONE rule is under test: an optional member whose value is the empty string NAMES NO VALUE,
//! so the service reads it as the member being absent and the request runs under that member's
//! documented default — rather than resolving the empty text as a zone, an address or a name
//! and refusing the request for a value it never gave. For the Availability Zone that default
//! is published outright: "on input, you can specify `AvailabilityZone` or
//! `AvailabilityZoneId`, but not both. If you specify neither one, Amazon EC2 automatically
//! selects an Availability Zone for you" (`Placement$AvailabilityZone`).
//!
//! The rows span the class rather than one recorded request:
//!
//! * every optional member of a LAUNCH that is resolved against the account or the Region —
//!   the placement's zone, group name and host id, the primary private address and the key
//!   pair name — each left empty on its own and then all of them at once, each asserted to
//!   launch the same instance the request that OMITTED the member launches (the same zone, an
//!   address of the subnet, no placement group and no host);
//! * the same member on the other operations that resolve one: `CreateVolume`, where the zone
//!   member pair is how the volume is placed, and `RequestSpotInstances`, where it is the
//!   launch specification's own;
//! * and the rule's BOUNDARY: a value that is not empty is still resolved, so a zone the
//!   Region does not have, an address that is not an IPv4 address and a key pair name the
//!   account holds none of are each refused exactly as before.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The AMI every launch here names: `RunInstances` needs an image id that resolves, and the
/// facts of the image this crate publishes by default answer the rest.
const AMI: &str = "ami-0123456789abcdef0";

/// An AMI of the account's OWN published catalog, read out of the service's own state because
/// a Spot launch specification must name an image the account can resolve.
fn catalog_image() -> String {
    service_com_amazonaws_ec2_amazonec2::state::ComAmazonawsEc2Amazonec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// The network every launch here goes into.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDR: &str = "10.91.1.0/24";

/// One call that must answer HTTP 200, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One emulator with one VPC and one subnet of the caller's own, answered beside their ids.
async fn network(case: &str) -> (MultiServiceRouter, String, String) {
    let (router, _engine) = super::ec2_query::emulator();
    let body = ok(&router, case, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no VPC: {body}"));
    let body = ok(
        &router,
        case,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnet: {body}"));
    (router, vpc_id, subnet_id)
}

/// The members every launch below carries, beside the row's own.
fn launch(subnet_id: &str, extra: Vec<Param>) -> Vec<Param> {
    let mut params = vec![
        param("ImageId", AMI),
        param("InstanceType", "t3.micro"),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        param("SubnetId", subnet_id),
    ];
    params.extend(extra);
    params
}

/// What an instance reports of the members this suite leaves empty: where it was placed, the
/// private address it holds, and the placement group and host it names.
#[derive(Debug, PartialEq, Eq)]
struct Placed {
    availability_zone: Option<String>,
    private_ip_address: Option<String>,
    group_name: Option<String>,
    host_id: Option<String>,
}

/// Launch one instance and answer its id.
async fn launched(router: &MultiServiceRouter, case: &str, params: &[Param]) -> String {
    let body = ok(router, case, "RunInstances", params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// What `DescribeInstances` reports for `instance_id`.
async fn placed(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Placed {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = document(case, &body);
    let reservations = set_items(parsed.root_element(), "reservationSet");
    let reservation = *reservations.first().unwrap_or_else(|| {
        panic!("case {case:?}: DescribeInstances reported no reservation: {body}")
    });
    let instances = set_items(reservation, "instancesSet");
    let instance = *instances.first().unwrap_or_else(|| {
        panic!("case {case:?}: DescribeInstances reported no instance: {body}")
    });
    let placement = child(instance, "placement");
    Placed {
        availability_zone: placement.and_then(|placement| {
            child_text(placement, "availabilityZone").filter(|zone| !zone.is_empty())
        }),
        private_ip_address: child_text(instance, "privateIpAddress"),
        group_name: placement.and_then(|placement| {
            child_text(placement, "groupName").filter(|name| !name.is_empty())
        }),
        host_id: placement.and_then(|placement| child_text(placement, "hostId")),
    }
}

/// Whether `address` is one of [`SUBNET_CIDR`]'s own — the range a launch that names no
/// primary address takes one from.
fn in_the_subnet(address: &str) -> bool {
    address
        .parse::<std::net::Ipv4Addr>()
        .is_ok_and(|address| address.octets()[..3] == [10, 91, 1])
}

/// RULE: every optional member of a launch that carries the empty string is read as the member
/// being ABSENT, so the launch is served and the instance is placed exactly as the launch that
/// omitted the member places it.
#[tokio::test]
async fn a_launch_member_left_empty_is_read_as_the_member_being_absent() {
    let case = "the launch that names none of the members";
    let (router, _vpc_id, subnet_id) = network(case).await;

    // The launch every row below is compared against: the one that OMITS every member this
    // suite leaves empty.
    let omitted = launched(&router, case, &launch(&subnet_id, Vec::new())).await;
    let expected = placed(&router, case, &omitted).await;
    assert!(
        expected.availability_zone.is_some(),
        "case {case:?}: a launch that names no zone must still be placed in one, got {expected:?}"
    );
    assert!(
        expected
            .private_ip_address
            .as_deref()
            .is_some_and(in_the_subnet),
        "case {case:?}: a launch that names no address must take one from {SUBNET_CIDR}, got \
         {expected:?}"
    );

    // Each row: what it covers, and the members it carries EMPTY beside the launch's own.
    let rows: Vec<(&str, Vec<Param>)> = vec![
        (
            "an empty placement zone",
            vec![param("Placement.AvailabilityZone", "")],
        ),
        (
            "an empty primary private address",
            vec![param("PrivateIpAddress", "")],
        ),
        ("an empty key pair name", vec![param("KeyName", "")]),
        (
            "an empty placement group name",
            vec![param("Placement.GroupName", "")],
        ),
        ("an empty host id", vec![param("Placement.HostId", "")]),
        (
            "every one of them empty at once",
            vec![
                param("Placement.AvailabilityZone", ""),
                param("PrivateIpAddress", ""),
                param("KeyName", ""),
                param("Placement.GroupName", ""),
                param("Placement.HostId", ""),
            ],
        ),
    ];

    for (case, members) in rows {
        let instance_id = launched(&router, case, &launch(&subnet_id, members)).await;
        let reported = placed(&router, case, &instance_id).await;
        assert_eq!(
            reported.availability_zone, expected.availability_zone,
            "case {case:?}: expected the zone a launch naming none gets \
             ({zone:?}), got {reported:?}",
            zone = expected.availability_zone
        );
        assert!(
            reported
                .private_ip_address
                .as_deref()
                .is_some_and(in_the_subnet),
            "case {case:?}: expected an address of {SUBNET_CIDR}, got {reported:?}"
        );
        assert_eq!(
            reported.group_name, expected.group_name,
            "case {case:?}: expected the placement group a launch naming none gets \
             ({group:?}), got {reported:?}",
            group = expected.group_name
        );
        assert_eq!(
            reported.host_id, expected.host_id,
            "case {case:?}: expected the host a launch naming none gets ({host:?}), got \
             {reported:?}",
            host = expected.host_id
        );
    }
}

/// RULE (the boundary): a value that is NOT empty is still resolved, so a launch that really
/// names a zone, an address or a key pair the account cannot serve is refused exactly as it
/// was — the rule reads an ABSENT member, not a bad one.
#[tokio::test]
async fn a_launch_member_that_carries_a_value_is_still_resolved() {
    let case = "the network the refusals are asked of";
    let (router, vpc_id, subnet_id) = network(case).await;

    // Each row: what it covers, the member it carries, and the code the refusal answers.
    let rows: [(&str, Param, &str); 4] = [
        (
            "a zone the Region does not have",
            param("Placement.AvailabilityZone", "us-west-2zzz"),
            "InvalidParameterValue",
        ),
        (
            "an address that is not an IPv4 address",
            param("PrivateIpAddress", "not-an-address"),
            "InvalidParameterValue",
        ),
        (
            "a key pair name the account holds none of",
            param("KeyName", "never-created"),
            "InvalidKeyPair.NotFound",
        ),
        (
            "a placement group the account holds none of",
            param("Placement.GroupName", "never-created"),
            "InvalidPlacementGroup.Unknown",
        ),
    ];

    for (case, member, code) in rows {
        let (status, body) = call(
            &router,
            REGION,
            "RunInstances",
            &launch(&subnet_id, vec![member.clone()]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {member:?}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected the code {code:?}, got {body}"
        );
    }

    // And the zone a launch really names IS where the instance is placed: the rule above
    // changes nothing for a member that carries a value. The zone named is the Region's
    // SECOND, so it is neither the one chosen for a launch that names none nor the last, and
    // the launch goes into a subnet of that zone.
    let zones = ok(&router, case, "DescribeAvailabilityZones", &[]).await;
    let named = super::ec2_query::elements(&zones, "zoneName");
    assert!(
        named.len() >= 3,
        "case {case:?}: this Region must publish at least three zones, got {named:?}"
    );
    let zone = &named[1];
    let body = ok(
        &router,
        case,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.91.2.0/24"),
            param("AvailabilityZone", zone),
        ],
    )
    .await;
    let second_subnet = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnet: {body}"));
    let instance_id = launched(
        &router,
        case,
        &launch(
            &second_subnet,
            vec![param("Placement.AvailabilityZone", zone)],
        ),
    )
    .await;
    let reported = placed(&router, case, &instance_id).await;
    assert_eq!(
        reported.availability_zone.as_deref(),
        Some(zone.as_str()),
        "case {case:?}: a launch naming {zone} must be placed there, got {reported:?}"
    );
}

/// RULE, on the other operations that resolve the same member: `CreateVolume` places a volume
/// by the zone pair, so a request that leaves one of the two members empty names the zone the
/// OTHER one gives, and one that leaves both empty names no zone at all — the refusal every
/// request that names neither gets.
#[tokio::test]
async fn an_empty_zone_member_names_no_zone_on_the_operations_that_resolve_one() {
    let case = "the volumes";
    let (router, _engine) = super::ec2_query::emulator();
    let zones = ok(&router, case, "DescribeAvailabilityZones", &[]).await;
    let parsed = document(case, &zones);
    let first = *set_items(parsed.root_element(), "availabilityZoneInfo")
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the Region reports no zone: {zones}"));
    let zone_name = child_text(first, "zoneName")
        .unwrap_or_else(|| panic!("case {case:?}: a zone must report its name: {zones}"));
    let zone_id = child_text(first, "zoneId")
        .unwrap_or_else(|| panic!("case {case:?}: a zone must report its id: {zones}"));

    // Each row: what it covers, the zone members the create carries, and the zone the volume
    // must report.
    let served: Vec<(&str, Vec<Param>)> = vec![
        (
            "the zone named, the id member empty",
            vec![
                param("AvailabilityZone", &zone_name),
                param("AvailabilityZoneId", ""),
            ],
        ),
        (
            "the id named, the zone member empty",
            vec![
                param("AvailabilityZone", ""),
                param("AvailabilityZoneId", &zone_id),
            ],
        ),
    ];
    for (case, members) in served {
        let mut params = vec![param("Size", "1")];
        params.extend(members);
        let body = ok(&router, case, "CreateVolume", &params).await;
        assert_eq!(
            element(&body, "availabilityZone").as_deref(),
            Some(zone_name.as_str()),
            "case {case:?}: expected the volume in {zone_name}, got {body}"
        );
    }

    // Both members empty is the request that names NEITHER.
    let case = "both zone members empty";
    let (status, body) = call(
        &router,
        REGION,
        "CreateVolume",
        &[
            param("Size", "1"),
            param("AvailabilityZone", ""),
            param("AvailabilityZoneId", ""),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "MissingParameter",
        "case {case:?}: expected MissingParameter, got {body}"
    );
    assert_eq!(
        error_message(&body),
        "Either availabilityZone or availabilityZoneId must be specified",
        "case {case:?}: expected the sentence a request naming neither gets, got {body}"
    );
}

/// RULE, on the other launch path: a Spot Instance request whose launch specification leaves
/// the placement zone empty names no zone, so it is SERVED — the zone being chosen for it the
/// way it is for a specification that omitted the member.
#[tokio::test]
async fn an_empty_placement_zone_is_served_on_a_spot_instance_request() {
    let case = "a spot request whose placement zone is empty";
    let (router, _engine) = super::ec2_query::emulator();
    let image_id = catalog_image();
    let body = ok(
        &router,
        case,
        "RequestSpotInstances",
        &[
            param("InstanceCount", "1"),
            param("LaunchSpecification.ImageId", &image_id),
            param("LaunchSpecification.InstanceType", "t3.micro"),
            param("LaunchSpecification.Placement.AvailabilityZone", ""),
        ],
    )
    .await;
    let request_id = element(&body, "spotInstanceRequestId").unwrap_or_else(|| {
        panic!("case {case:?}: RequestSpotInstances reported no request: {body}")
    });
    assert!(
        request_id.starts_with("sir-"),
        "case {case:?}: expected a spot request id, got {request_id:?}"
    );
    // The same specification with a zone the Region does not have is still refused: the rule
    // reads an absent member, not a bad one.
    let (status, body) = call(
        &router,
        REGION,
        "RequestSpotInstances",
        &[
            param("InstanceCount", "1"),
            param("LaunchSpecification.ImageId", &image_id),
            param("LaunchSpecification.InstanceType", "t3.micro"),
            param(
                "LaunchSpecification.Placement.AvailabilityZone",
                "us-west-2zzz",
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a zone the Region does not have must still be refused, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case {case:?}: expected InvalidParameterValue, got {body}"
    );
}
