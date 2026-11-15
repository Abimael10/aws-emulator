// SPDX-License-Identifier: CC0-1.0

//! `CreateDefaultSubnet` end to end over the wire the service actually speaks: `ec2Query`, an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=CreateDefaultSubnet`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol"). The Region lives in the
//! SigV4 credential scope of each call, so each call below is about one Region's own default
//! VPC.
//!
//! The rule under test: `CreateDefaultSubnet` gives ONE Availability Zone of the Region the
//! default subnet it is owed inside that Region's DEFAULT VPC — "a default subnet with a size
//! /20 IPv4 CIDR block in the specified Availability Zone in your default VPC"
//! (`CreateDefaultSubnet`), with `DefaultForAz` and "the auto-assign public IPv4 address
//! attribute set to `true`" (Amazon VPC User Guide, "Default VPCs") — and refuses every
//! request that cannot be one: "you can have only one default subnet per Availability Zone"
//! (`CreateDefaultSubnet`), a zone the Region does not have, a request that names no zone or
//! names one twice, and a Region whose default VPC is gone.
//!
//! The rows are the zone and the Region — what the rule generalises over, since the zone
//! decides WHICH `/20` of the default VPC the subnet takes and the Region decides which zones
//! exist and what the subnet's ARN names — plus which of the two members the request names the
//! zone by. The zone a row names is read off `DescribeAvailabilityZones` over the wire, so no
//! row can name a zone the service does not report, and every created subnet is read back
//! through `DescribeSubnets`.

use ec2::state::{
    is_catalog_region, ComAmazonawsEc2Amazonec2State as Ec2State, DEFAULT_SUBNET_ALREADY_EXISTS,
    DEFAULT_VPC_DOES_NOT_EXIST, INVALID_AVAILABILITY_ZONE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, filters, indexed};
use super::wire::{is_long_id, param, Param};

/// The Regions the tables below are over: the Region of the account's own calls, one of
/// another partition, and one more commercial Region, so no row's expectation can come from a
/// single Region's zone set. Every row is checked against the Region catalog inside the tests,
/// so this list cannot drift into naming a Region that does not exist.
const REGIONS: [&str; 3] = ["us-west-2", "us-gov-west-1", "eu-central-1"];

/// A Region code that is well formed but names no Region of the catalog: an account has no
/// default VPC — and so no default subnet — anywhere but in the Regions that exist.
const REGION_OUTSIDE_THE_CATALOG: &str = "zz-nowhere-1";

/// The published size of a default subnet's IPv4 CIDR block, as the operation's own doc gives
/// it: "a default subnet with a size /20 IPv4 CIDR block".
const DEFAULT_SUBNET_BLOCK_SIZE: &str = "/20";

/// The member a request names a zone by NAME under, as the refusals word it.
const ZONE_NAME_MEMBER: &str = "availabilityZone";

/// The member a request names a zone by ID under.
const ZONE_ID_MEMBER: &str = "availabilityZoneId";

/// Which of the two members a row names the zone by: both name one zone, from two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum By {
    Name,
    Id,
}

/// One `action` call signed for `region`, as its HTTP status and XML body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> (u16, String) {
    super::ec2_query::call(router, region, action, params).await
}

/// One `action` call signed for `region`, required to succeed, as its XML body.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One `CreateDefaultSubnet` call naming `zone` under the member `by` selects.
fn zone_param(by: By, zone: &(String, String)) -> Vec<Param> {
    match by {
        By::Name => vec![param("AvailabilityZone", &zone.0)],
        By::Id => vec![param("AvailabilityZoneId", &zone.1)],
    }
}

/// The Availability Zones of `region` as `(zoneName, zoneId)`, in the order
/// `DescribeAvailabilityZones` reports them — read over the wire so a row's zone is one the
/// service really has, and so the refusals' zone lists are not written down here.
async fn zones(case: &str, router: &MultiServiceRouter, region: &str) -> Vec<(String, String)> {
    let body = ok(case, router, region, "DescribeAvailabilityZones", &[]).await;
    let names = elements(&body, "zoneName");
    let ids = elements(&body, "zoneId");
    assert_eq!(
        names.len(),
        ids.len(),
        "case {case:?}: every zone reports both a name and an id, got {body}"
    );
    assert!(
        names.len() >= 2,
        "case {case:?}: the Region must report at least two zones, got {names:?}"
    );
    names.into_iter().zip(ids).collect()
}

/// How many subnets the store holds, so a refusal can be shown to have written none.
fn held_subnets(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().subnets.len()
}

/// The id of the default VPC `region` holds, as the store answers it.
fn held_default_vpc(engine: &engine::InMemoryEngine, region: &str) -> Option<String> {
    engine.state::<Ec2State>().lock().default_vpc_id(region)
}

/// The default VPC of `region`, materialised the way any caller materialises the network its
/// account has held there since it existed: by reading it.
async fn default_vpc(case: &str, router: &MultiServiceRouter, region: &str, engine: &engine::InMemoryEngine) -> String {
    let _ = ok(case, router, region, "DescribeVpcs", &[]).await;
    held_default_vpc(engine, region)
        .unwrap_or_else(|| panic!("case {case:?}: an account holds a default VPC in {region}"))
}

/// The default subnet `vpc_id` holds in `zone_name` as `(subnetId, cidrBlock)`, or `None` when
/// that zone holds none — read through `DescribeSubnets`, narrowed by the VPC, the zone and
/// `default-for-az`.
async fn default_subnet_of_zone(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    vpc_id: &str,
    zone_name: &str,
) -> Option<(String, String)> {
    let body = ok(
        case,
        router,
        region,
        "DescribeSubnets",
        &filters(&[
            ("vpc-id", &[vpc_id]),
            ("availability-zone", &[zone_name]),
            ("default-for-az", &["true"]),
        ]),
    )
    .await;
    let ids = elements(&body, "subnetId");
    assert!(
        ids.len() <= 1,
        "case {case:?}: a zone holds at most one default subnet, got {body}"
    );
    let subnet_id = ids.into_iter().next()?;
    let cidr_block = element(&body, "cidrBlock")
        .unwrap_or_else(|| panic!("case {case:?}: a subnet reports its block: {body}"));
    Some((subnet_id, cidr_block))
}

/// Delete the default subnet `zone_name` holds and answer `(subnetId, cidrBlock)` as it was —
/// the state a caller that deleted one of its default subnets is in, reached over the wire
/// through the operation that does it.
async fn delete_default_subnet_of_zone(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    vpc_id: &str,
    zone_name: &str,
) -> (String, String) {
    let held = default_subnet_of_zone(case, router, region, vpc_id, zone_name)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: {zone_name} holds its default subnet"));
    let _ = ok(
        case,
        router,
        region,
        "DeleteSubnet",
        &[param("SubnetId", &held.0)],
    )
    .await;
    assert_eq!(
        default_subnet_of_zone(case, router, region, vpc_id, zone_name).await,
        None,
        "case {case:?}: after the delete {zone_name} holds no default subnet"
    );
    held
}

/// Delete the WHOLE default network of `region` — its subnets first, then the internet
/// gateway it comes with, because "you must detach or delete all gateways and resources that
/// are associated with the VPC before you can delete it" (`DeleteVpc`) — so the Region has no
/// default VPC left to put a default subnet in.
async fn delete_the_default_network(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    engine: &engine::InMemoryEngine,
) {
    let vpc_id = default_vpc(case, router, region, engine).await;
    let body = ok(
        case,
        router,
        region,
        "DescribeSubnets",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    for subnet_id in elements(&body, "subnetId") {
        let _ = ok(
            case,
            router,
            region,
            "DeleteSubnet",
            &[param("SubnetId", &subnet_id)],
        )
        .await;
    }
    let gateways = ok(
        case,
        router,
        region,
        "DescribeInternetGateways",
        &filters(&[("attachment.vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    for internet_gateway_id in elements(&gateways, "internetGatewayId") {
        let _ = ok(
            case,
            router,
            region,
            "DetachInternetGateway",
            &[
                param("InternetGatewayId", &internet_gateway_id),
                param("VpcId", &vpc_id),
            ],
        )
        .await;
        let _ = ok(
            case,
            router,
            region,
            "DeleteInternetGateway",
            &[param("InternetGatewayId", &internet_gateway_id)],
        )
        .await;
    }
    let _ = ok(
        case,
        router,
        region,
        "DeleteVpc",
        &[param("VpcId", &vpc_id)],
    )
    .await;
    assert_eq!(
        held_default_vpc(engine, region),
        None,
        "case {case:?}: after the delete the Region holds no default VPC"
    );
}

/// The sentence EC2 refuses a zone a subnet cannot be created in with, as the recorded refusal
/// words it: the value the request sent, the member it sent it under, and the zones that Region
/// offers for that member.
fn unknown_zone_message(member: &str, requested: &str, known: &[String]) -> String {
    format!(
        "Value ({requested}) for parameter {member} is invalid. Subnets can currently only \
         be created in the following availability zones: {known}.",
        known = known.join(", "),
    )
}

// ---------------------------------------------------------------------------
// What the create makes
// ---------------------------------------------------------------------------

/// The rule: a zone whose default subnet is gone gets exactly that subnet back — the zone's own
/// `/20` of the Region's default VPC, `DefaultForAz`, auto-assigning a public IPv4 address, in
/// `available` — and every member of it reads back through `DescribeSubnets`.
///
/// The rows differ in the zone (which decides the block), in the member the request names the
/// zone by, and in the Region (which decides the zone set and the subnet's ARN).
#[tokio::test]
async fn create_default_subnet_gives_a_zone_back_the_default_subnet_it_lost() {
    let (router, engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id().to_string();

    // (case, region, which zone of the Region, which member names it)
    let cases: [(&str, &str, usize, By); 5] = [
        ("the first zone, by name", REGIONS[0], 0, By::Name),
        ("the second zone, by id", REGIONS[0], 1, By::Id),
        ("the last zone, by name", REGIONS[0], usize::MAX, By::Name),
        ("another partition's Region, by name", REGIONS[1], 0, By::Name),
        ("a third Region, by id", REGIONS[2], 1, By::Id),
    ];

    for (case, region, zone_index, by) in cases {
        assert!(
            is_catalog_region(region),
            "case {case:?}: this table row must name a Region of the catalog"
        );
        let region_zones = zones(case, &router, region).await;
        let zone_index = zone_index.min(region_zones.len() - 1);
        let zone = region_zones[zone_index].clone();
        let vpc_id = default_vpc(case, &router, region, &engine).await;

        let before = held_subnets(&engine);
        let (deleted_id, block) =
            delete_default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await;
        assert!(
            block.ends_with(DEFAULT_SUBNET_BLOCK_SIZE),
            "case {case:?}: a default subnet's block is a {DEFAULT_SUBNET_BLOCK_SIZE}, got \
             {block:?}"
        );

        let body = ok(
            case,
            &router,
            region,
            "CreateDefaultSubnet",
            &zone_param(by, &zone),
        )
        .await;
        let subnet_id = element(&body, "subnetId")
            .unwrap_or_else(|| panic!("case {case:?}: the create must report the subnet: {body}"));
        assert!(
            is_long_id("subnet", &subnet_id),
            "case {case:?}: expected a subnet-<17 hex> id, got {subnet_id:?}"
        );
        assert_ne!(
            subnet_id, deleted_id,
            "case {case:?}: the new default subnet is a new resource, not the deleted one"
        );
        // The block is the one THIS zone's default subnet takes, so the zone gets back exactly
        // what it lost rather than the next free block in the VPC.
        for (name, expected) in [
            ("cidrBlock", block.clone()),
            ("availabilityZone", zone.0.clone()),
            ("availabilityZoneId", zone.1.clone()),
            ("vpcId", vpc_id.clone()),
            ("defaultForAz", "true".to_string()),
            ("mapPublicIpOnLaunch", "true".to_string()),
            ("state", "available".to_string()),
            ("ownerId", account.clone()),
        ] {
            assert_eq!(
                element(&body, name),
                Some(expected.clone()),
                "case {case:?}: expected <{name}>{expected}</{name}>, got {body}"
            );
        }
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: the Region holds the subnets it held before the delete again"
        );

        // And it reads back as the account's subnet, through the operation a caller reads it
        // with.
        let read_back = ok(
            case,
            &router,
            region,
            "DescribeSubnets",
            &indexed("SubnetId", &[subnet_id.as_str()]),
        )
        .await;
        for (name, expected) in [
            ("cidrBlock", block.clone()),
            ("availabilityZone", zone.0.clone()),
            ("defaultForAz", "true".to_string()),
            ("mapPublicIpOnLaunch", "true".to_string()),
        ] {
            assert_eq!(
                element(&read_back, name),
                Some(expected.clone()),
                "case {case:?}: expected <{name}>{expected}</{name}> from DescribeSubnets, \
                 got {read_back}"
            );
        }
        assert_eq!(
            default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await,
            Some((subnet_id, block)),
            "case {case:?}: the zone's default subnet is the one just created"
        );
    }
}

/// The rule: the block each zone's default subnet takes is the zone's own, so two zones that
/// both lose their default subnet get two different blocks back — one per zone, none
/// overlapping.
#[tokio::test]
async fn each_zone_gets_its_own_block_back() {
    let (router, engine) = super::ec2_query::emulator();
    let region = REGIONS[0];
    let region_zones = zones("every zone of the Region", &router, region).await;
    let vpc_id = default_vpc("every zone of the Region", &router, region, &engine).await;

    let mut was: Vec<(String, String)> = Vec::new();
    for (zone_name, _) in &region_zones {
        let case = format!("the default subnet of {zone_name}");
        was.push((
            zone_name.clone(),
            delete_default_subnet_of_zone(&case, &router, region, &vpc_id, zone_name)
                .await
                .1,
        ));
    }
    let mut created: Vec<(String, String)> = Vec::new();
    for (zone_name, _) in &region_zones {
        let case = format!("recreating the default subnet of {zone_name}");
        let body = ok(
            &case,
            &router,
            region,
            "CreateDefaultSubnet",
            &[param("AvailabilityZone", zone_name)],
        )
        .await;
        let block = element(&body, "cidrBlock")
            .unwrap_or_else(|| panic!("case {case:?}: the create reports the block: {body}"));
        created.push((zone_name.clone(), block));
    }
    assert_eq!(
        created, was,
        "every zone must get the block its own default subnet had, it had {was:?}"
    );
    let blocks: std::collections::BTreeSet<&String> =
        created.iter().map(|(_, block)| block).collect();
    assert_eq!(
        blocks.len(),
        created.len(),
        "no two zones may be given the same block, got {created:?}"
    );
}

// ---------------------------------------------------------------------------
// What the create refuses
// ---------------------------------------------------------------------------

/// The rule: "you can have only one default subnet per Availability Zone", so a zone that still
/// has its own is refused and nothing is written — whether or not anything had read that
/// Region's network before, because the network has been owed since the account existed rather
/// than since the first read.
///
/// The rows differ in the Region, in whether its network had been read, in the member naming
/// the zone, and in whether the zone's subnet is the ambient one or one this operation created.
#[tokio::test]
async fn create_default_subnet_refuses_a_zone_that_already_has_one() {
    let (router, engine) = super::ec2_query::emulator();

    // (case, region, read the Region's network first, which zone, which member, recreate first)
    let cases: [(&str, &str, bool, usize, By, bool); 5] = [
        ("a Region nothing has read yet", REGIONS[0], false, 0, By::Name, false),
        ("a Region already read", REGIONS[0], true, 1, By::Name, false),
        ("the zone named by its id", REGIONS[0], true, 2, By::Id, false),
        ("another partition's Region", REGIONS[1], false, 0, By::Id, false),
        ("a zone whose subnet this operation made", REGIONS[2], true, 1, By::Name, true),
    ];

    for (case, region, read_first, zone_index, by, recreate_first) in cases {
        let region_zones = zones(case, &router, region).await;
        let zone = region_zones[zone_index.min(region_zones.len() - 1)].clone();
        if read_first {
            let _ = ok(case, &router, region, "DescribeSubnets", &[]).await;
        }
        if recreate_first {
            let vpc_id = default_vpc(case, &router, region, &engine).await;
            let _ = delete_default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await;
            let _ = ok(
                case,
                &router,
                region,
                "CreateDefaultSubnet",
                &zone_param(by, &zone),
            )
            .await;
        }

        let before = held_subnets(&engine);
        let (status, body) = call(
            &router,
            region,
            "CreateDefaultSubnet",
            &zone_param(by, &zone),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a second default subnet, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            DEFAULT_SUBNET_ALREADY_EXISTS,
            "case {case:?}: expected {DEFAULT_SUBNET_ALREADY_EXISTS}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "A default subnet already exists in availability zone {zone_name}.",
                zone_name = zone.0
            ),
            "case {case:?}: expected the refusal to name the zone it is about, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: a refused create must write nothing, it held {before} subnets"
        );

        // And the default subnet the refusal was about is the one the zone reports: a read of
        // the Region materialises the network the account has been owed, and that network has
        // a default subnet in this zone.
        let vpc_id = default_vpc(case, &router, region, &engine).await;
        let held = default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await;
        assert!(
            held.is_some(),
            "case {case:?}: the zone the refusal named must hold its default subnet"
        );
    }
}

/// The rule: a zone the Region does not have is refused with [`INVALID_AVAILABILITY_ZONE`],
/// listing the zones that Region offers for the member the request named — the zones
/// `DescribeAvailabilityZones` reports, read over the wire rather than written down here — and
/// nothing is written.
///
/// The rows differ in the member at fault and in HOW the value misses: text that is no zone at
/// all, a zone of ANOTHER Region, a zone id of another Region, and a value that only looks like
/// one of this Region's.
#[tokio::test]
async fn create_default_subnet_refuses_a_zone_the_region_does_not_have() {
    let (router, engine) = super::ec2_query::emulator();
    let region = REGIONS[0];
    let region_zones = zones("the Region's own zones", &router, region).await;
    let names: Vec<String> = region_zones.iter().map(|zone| zone.0.clone()).collect();
    let ids: Vec<String> = region_zones.iter().map(|zone| zone.1.clone()).collect();
    let another_region_zones = zones("another Region's zones", &router, REGIONS[2]).await;

    // (case, member, the value the request sends, the values that member is compared against)
    let cases: [(&str, &str, String, &Vec<String>); 5] = [
        (
            "text that is no zone at all",
            ZONE_NAME_MEMBER,
            "bogus-az-does-not-exist".to_string(),
            &names,
        ),
        (
            "a zone of another Region",
            ZONE_NAME_MEMBER,
            another_region_zones[0].0.clone(),
            &names,
        ),
        (
            "a zone letter the Region does not reach",
            ZONE_NAME_MEMBER,
            format!("{region}z"),
            &names,
        ),
        (
            "a zone id that is no id at all",
            ZONE_ID_MEMBER,
            "no-such-az-id".to_string(),
            &ids,
        ),
        (
            "a zone id of another Region",
            ZONE_ID_MEMBER,
            another_region_zones[0].1.clone(),
            &ids,
        ),
    ];

    for (case, member, requested, known) in cases {
        assert!(
            !known.contains(&requested),
            "case {case:?}: this row must name a value the Region does not offer, it offers \
             {known:?}"
        );
        let request_member = if member == ZONE_NAME_MEMBER {
            "AvailabilityZone"
        } else {
            "AvailabilityZoneId"
        };
        let before = held_subnets(&engine);
        let (status, body) = call(
            &router,
            region,
            "CreateDefaultSubnet",
            &[param(request_member, &requested)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a zone the Region does not have, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_AVAILABILITY_ZONE,
            "case {case:?}: expected {INVALID_AVAILABILITY_ZONE}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            unknown_zone_message(member, &requested, known),
            "case {case:?}: expected the refusal to quote the value and list the Region's \
             zones, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: a refused create must write nothing, it held {before} subnets"
        );
    }
}

/// The rule: "either AvailabilityZone or AvailabilityZoneId must be specified, but not both" —
/// a request that names no zone says nothing about where the subnet goes, and one that names a
/// zone twice names it under two members that cannot both be honoured.
///
/// The rows differ in which way the pair misses, including the case where the two members name
/// the SAME zone: the doc admits neither pair.
#[tokio::test]
async fn create_default_subnet_refuses_a_request_that_names_no_zone_or_names_it_twice() {
    let (router, engine) = super::ec2_query::emulator();
    let region = REGIONS[0];
    let region_zones = zones("the Region's own zones", &router, region).await;
    let zone = region_zones[0].clone();

    // (case, the members the request carries, the code it is refused with)
    let cases: [(&str, Vec<Param>, &str); 4] = [
        ("neither member", Vec::new(), "MissingParameter"),
        (
            "an empty zone name",
            vec![param("AvailabilityZone", "")],
            "MissingParameter",
        ),
        (
            "both members, naming one zone",
            vec![
                param("AvailabilityZone", &zone.0),
                param("AvailabilityZoneId", &zone.1),
            ],
            "InvalidParameterCombination",
        ),
        (
            "both members, naming two zones",
            vec![
                param("AvailabilityZone", &zone.0),
                param("AvailabilityZoneId", &region_zones[1].1),
            ],
            "InvalidParameterCombination",
        ),
    ];

    for (case, params, expected_code) in cases {
        let before = held_subnets(&engine);
        let (status, body) = call(&router, region, "CreateDefaultSubnet", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: a refused create must write nothing, it held {before} subnets"
        );
    }
}

/// The rule: a default subnet is created "in your default VPC", so a Region with no default VPC
/// has nowhere to put one — the account deleted it, or the Region does not exist at all — and
/// the request is refused with [`DEFAULT_VPC_DOES_NOT_EXIST`] rather than served a subnet in no
/// VPC.
#[tokio::test]
async fn create_default_subnet_refuses_a_region_with_no_default_vpc() {
    let (router, engine) = super::ec2_query::emulator();
    assert!(
        !is_catalog_region(REGION_OUTSIDE_THE_CATALOG),
        "this test needs a Region code the catalog does not hold"
    );
    let zone = zones("the deleted Region's zones", &router, REGIONS[0]).await[0].clone();
    delete_the_default_network("the deleted Region", &router, REGIONS[0], &engine).await;

    // (case, region, the zone the request names)
    let cases: [(&str, &str, String); 2] = [
        (
            "a Region whose default VPC was deleted",
            REGIONS[0],
            zone.0.clone(),
        ),
        (
            "a Region the catalog does not hold",
            REGION_OUTSIDE_THE_CATALOG,
            format!("{REGION_OUTSIDE_THE_CATALOG}a"),
        ),
    ];

    for (case, region, zone_name) in cases {
        let before = held_subnets(&engine);
        let (status, body) = call(
            &router,
            region,
            "CreateDefaultSubnet",
            &[param("AvailabilityZone", &zone_name)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 where there is no default VPC, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            DEFAULT_VPC_DOES_NOT_EXIST,
            "case {case:?}: expected {DEFAULT_VPC_DOES_NOT_EXIST}, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: a refused create must write nothing, it held {before} subnets"
        );
        assert_eq!(
            held_default_vpc(&engine, region),
            None,
            "case {case:?}: the refusal must not create a default VPC either"
        );
    }
}

/// The rule: `DryRun` reports the answer the request would have got and writes nothing —
/// `DryRunOperation` for a create that would have succeeded, and the refusal itself for one
/// that would not.
///
/// The rows differ in whether the zone still has its default subnet, which is what decides
/// which of the two a dry run reports.
#[tokio::test]
async fn create_default_subnet_dry_run_reports_the_outcome_without_writing() {
    let (router, engine) = super::ec2_query::emulator();
    let region = REGIONS[0];
    let region_zones = zones("the Region's own zones", &router, region).await;
    let vpc_id = default_vpc("the dry runs", &router, region, &engine).await;

    // (case, which zone, whether its default subnet is deleted first, status, code)
    let cases: [(&str, usize, bool, u16, &str); 2] = [
        (
            "a zone that still has its default subnet",
            0,
            false,
            400,
            DEFAULT_SUBNET_ALREADY_EXISTS,
        ),
        (
            "a zone whose default subnet was deleted",
            1,
            true,
            412,
            "DryRunOperation",
        ),
    ];

    for (case, zone_index, delete_first, expected_status, expected_code) in cases {
        let zone = region_zones[zone_index].clone();
        if delete_first {
            let _ = delete_default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await;
        }
        let before = held_subnets(&engine);

        let (status, body) = call(
            &router,
            region,
            "CreateDefaultSubnet",
            &[param("AvailabilityZone", &zone.0), param("DryRun", "true")],
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: a dry run must write nothing, it held {before} subnets"
        );
        if delete_first {
            // And the real call after it still works: the dry run refused nothing but itself.
            let body = ok(
                case,
                &router,
                region,
                "CreateDefaultSubnet",
                &[param("AvailabilityZone", &zone.0)],
            )
            .await;
            assert_eq!(
                element(&body, "defaultForAz"),
                Some("true".to_string()),
                "case {case:?}: the create after a dry run must make the default subnet, got \
                 {body}"
            );
        }
    }
}

/// The rule: an IPv6 only subnet is work this emulator holds no state for, so the request for
/// one answers the scaffold's HTTP 501 — "nobody has written this yet" — rather than a refusal
/// blaming the caller or a subnet with an IPv4 block it did not ask for.
///
/// The rows differ in whether the zone's default subnet is there, so the 501 is shown to be the
/// answer to the MEMBER rather than to the state of the zone.
#[tokio::test]
async fn create_default_subnet_answers_an_ipv6_only_subnet_with_not_implemented() {
    let (router, engine) = super::ec2_query::emulator();
    let region = REGIONS[0];
    let region_zones = zones("the Region's own zones", &router, region).await;
    let vpc_id = default_vpc("the IPv6 only requests", &router, region, &engine).await;

    // (case, which zone, whether its default subnet is deleted first)
    let cases: [(&str, usize, bool); 2] = [
        ("a zone that still has its default subnet", 0, false),
        ("a zone whose default subnet was deleted", 1, true),
    ];

    for (case, zone_index, delete_first) in cases {
        let zone = region_zones[zone_index].clone();
        if delete_first {
            let _ = delete_default_subnet_of_zone(case, &router, region, &vpc_id, &zone.0).await;
        }
        let before = held_subnets(&engine);
        let (status, body) = call(
            &router,
            region,
            "CreateDefaultSubnet",
            &[
                param("AvailabilityZone", &zone.0),
                param("Ipv6Native", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 501,
            "case {case:?}: expected HTTP 501 for an IPv6 only subnet, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotImplemented",
            "case {case:?}: expected NotImplemented, got {body}"
        );
        assert_eq!(
            held_subnets(&engine),
            before,
            "case {case:?}: the 501 must write nothing, it held {before} subnets"
        );
    }
}
