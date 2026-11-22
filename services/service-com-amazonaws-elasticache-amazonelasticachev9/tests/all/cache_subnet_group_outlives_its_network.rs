// SPDX-License-Identifier: CC0-1.0

//! A cache subnet group outlives the network it was built over, and every answer that
//! carries it reports the membership the owner of subnets holds NOW.
//!
//! ONE rule, over a set of inputs: a cache subnet group is not deleted when the subnets or
//! the VPC it was created over are, so it keeps its STORED identity — its name, its
//! description, its ARN and the `VpcId` it was created in, whether or not that VPC still
//! exists — while its DERIVED members (`CacheSubnetGroup$Subnets`, each subnet's
//! `SubnetAvailabilityZone` and `SupportedNetworkTypes`, and the group's own
//! `SupportedNetworkTypes`) are re-resolved at read time against the subnets Amazon EC2 —
//! the service that owns subnets — holds for the account at that instant: an identifier EC2
//! answers a subnet for is reported, in the order the group names it, and one it answers
//! none for is no member at all, down to a group left holding no subnet and offering no
//! network type.
//!
//! The rows differ along what the rule generalises over: how many subnets the group was
//! built over (one, two, three), how many of them the owner then deletes (none, one, all),
//! WHICH position is deleted (the first, the middle, the last), whether the VPC itself is
//! deleted after them, and whether two groups share a subnet so one page has to resolve
//! several groups' identifiers at once. Every row is asserted over each reader that carries
//! a `CacheSubnetGroup`: the create's own answer, the describe of one group by name, the
//! describe of the whole page, and `ModifyCacheSubnetGroup`'s answer — and the named
//! describe twice, because a read must not change what a later caller sees.
//!
//! Nothing here reaches into either store: every subnet and VPC a row names was minted and
//! deleted through EC2's own operations, where the service under test resolves them, and a
//! VPC a row deletes is confirmed gone by asking EC2 for it.

use ::aws_sdk_ec2::error::ProvideErrorMetadata as _;

use crate::cache_cluster_lifecycle::subnet_group_params;
use crate::cache_subnet_group_network_types::wire_group_network_types;
use crate::harness::{params, Emulator, Reply, REGION};

/// The IPv4 block of the VPC every row mints for itself; each subnet takes a `/24` out of it.
const VPC_CIDR: &str = "10.91.0.0/16";

/// What a subnet addressed in IPv4 alone — and a group whose every subnet is — offers, in the
/// order the modelled `NetworkType` enum declares its values
/// (`smithy_query(query="shape", shape="NetworkType")`: `ipv4 | ipv6 | dual_stack`).
const IPV4_ONLY: [&str; 1] = ["ipv4"];

/// The description every group below is created with; it is stored identity, so it has to
/// survive the network going away.
const DESCRIPTION: &str = "storage app subnet group";

/// One subnet of a group as the account's own network holds it: the id EC2 minted and the
/// Availability Zone it was minted in.
type Member = (String, String);

/// One row: the group's name, how many subnets it is built over, which of those the owner
/// then deletes, and whether the VPC itself goes with them.
struct Case {
    label: &'static str,
    group: &'static str,
    subnets: usize,
    deleted: &'static [usize],
    deletes_vpc: bool,
}

/// The rows, spanning the membership a group can be left with: untouched, shrunk from either
/// end or the middle, and emptied — with and without the VPC surviving.
const CASES: &[Case] = &[
    Case {
        label: "nothing deleted: the group keeps every subnet",
        group: "outlives-none-deleted",
        subnets: 2,
        deleted: &[],
        deletes_vpc: false,
    },
    Case {
        label: "the first of two subnets deleted",
        group: "outlives-first-of-two",
        subnets: 2,
        deleted: &[0],
        deletes_vpc: false,
    },
    Case {
        label: "the middle of three subnets deleted",
        group: "outlives-middle-of-three",
        subnets: 3,
        deleted: &[1],
        deletes_vpc: false,
    },
    Case {
        label: "the last of three subnets deleted",
        group: "outlives-last-of-three",
        subnets: 3,
        deleted: &[2],
        deletes_vpc: false,
    },
    Case {
        label: "both subnets deleted while the VPC stays",
        group: "outlives-both-vpc-kept",
        subnets: 2,
        deleted: &[0, 1],
        deletes_vpc: false,
    },
    Case {
        label: "the one subnet deleted and then its VPC",
        group: "outlives-one-and-the-vpc",
        subnets: 1,
        deleted: &[0],
        deletes_vpc: true,
    },
    Case {
        label: "three subnets deleted and then their VPC",
        group: "outlives-three-and-the-vpc",
        subnets: 3,
        deleted: &[0, 1, 2],
        deletes_vpc: true,
    },
];

/// The zones EC2 reports for [`REGION`], in the order it reports them.
async fn zones(ec2: &::aws_sdk_ec2::Client) -> Vec<String> {
    ec2.describe_availability_zones()
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must report {REGION}'s zones: {error:?}"))
        .availability_zones()
        .iter()
        .filter_map(|zone| zone.zone_name().map(str::to_string))
        .filter(|zone| zone.starts_with(REGION))
        .collect()
}

/// Mint one VPC and `count` subnets in it through EC2's own operations, one subnet per zone,
/// and answer the VPC id and the members in the order they were minted.
async fn account_network(emulator: &Emulator, count: usize) -> (String, Vec<Member>) {
    let ec2 = emulator.ec2(REGION);
    let zones = zones(&ec2).await;
    assert!(
        zones.len() >= count,
        "setup: expected {REGION} to have at least {count} zones, got {zones:?}"
    );
    let vpc = ec2
        .create_vpc()
        .cidr_block(VPC_CIDR)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create the VPC: {error:?}"))
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .unwrap_or_else(|| panic!("setup: CreateVpc must report the VPC it minted"))
        .to_string();

    let mut minted = Vec::with_capacity(count);
    for (index, zone) in zones.iter().take(count).enumerate() {
        let subnet = ec2
            .create_subnet()
            .vpc_id(&vpc)
            .cidr_block(format!("10.91.{index}.0/24"))
            .availability_zone(zone)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("setup: EC2 must create a subnet in {zone}: {error:?}")
            })
            .subnet()
            .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
            .unwrap_or_else(|| panic!("setup: CreateSubnet must report the subnet it minted"))
            .to_string();
        minted.push((subnet, zone.clone()));
    }
    (vpc, minted)
}

/// Create the group `name` over `members`, asserting the create itself succeeded, and answer
/// its reply.
async fn create_group(emulator: &Emulator, case: &str, name: &str, members: &[Member]) -> Reply {
    let ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(name, DESCRIPTION, &ids),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: the group {name} must be created, got {} {}",
        created.status,
        created.body
    );
    created
}

/// Describe the one group `name`.
async fn describe(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DescribeCacheSubnetGroups",
            &params(&[("CacheSubnetGroupName", name)]),
        )
        .await
}

/// Assert `reply` reports exactly one group: `name`, created in `vpc`, holding `members` in
/// order and nothing else.
fn assert_group_reports(
    case: &str,
    reader: &str,
    reply: &Reply,
    name: &str,
    vpc: &str,
    members: &[Member],
) {
    assert_eq!(
        reply.status,
        200,
        "case {case:?} / {reader}: the group must still be reported, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.all("CacheSubnetGroupName"),
        vec![name],
        "case {case:?} / {reader}: the answer must carry the one group {name}, got {}",
        reply.body
    );
    assert_eq!(
        reply.text("CacheSubnetGroupDescription"),
        Some(DESCRIPTION),
        "case {case:?} / {reader}: the group must keep its stored description, got {:?}",
        reply.text("CacheSubnetGroupDescription")
    );
    assert_eq!(
        reply.text("VpcId"),
        Some(vpc),
        "case {case:?} / {reader}: the group must still report the VPC it was created in \
         ({vpc}), got {:?}",
        reply.text("VpcId")
    );
    let arn = reply.text("ARN").unwrap_or_default();
    assert!(
        arn.ends_with(&format!(":subnetgroup:{name}")),
        "case {case:?} / {reader}: the group must keep its ARN, got {arn:?}"
    );

    let expected_ids: Vec<&str> = members.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        reply.all("Subnets/Subnet/SubnetIdentifier"),
        expected_ids,
        "case {case:?} / {reader}: the group must hold only the subnets the owner still has, \
         got {:?}",
        reply.all("Subnets/Subnet/SubnetIdentifier")
    );
    let expected_zones: Vec<&str> = members.iter().map(|(_, zone)| zone.as_str()).collect();
    assert_eq!(
        reply.all("SubnetAvailabilityZone/Name"),
        expected_zones,
        "case {case:?} / {reader}: each member must report the zone EC2 minted it in, got {:?}",
        reply.all("SubnetAvailabilityZone/Name")
    );
    assert_eq!(
        reply.all("Subnets/Subnet/SupportedNetworkTypes/member"),
        vec!["ipv4"; members.len()],
        "case {case:?} / {reader}: each member addressed in IPv4 alone must offer {IPV4_ONLY:?}, \
         got {:?}",
        reply.all("Subnets/Subnet/SupportedNetworkTypes/member")
    );

    // What the GROUP offers follows from its members: a group left holding none offers none.
    let expected_group_types: Vec<String> = if members.is_empty() {
        Vec::new()
    } else {
        IPV4_ONLY.iter().map(|types| (*types).to_string()).collect()
    };
    assert_eq!(
        wire_group_network_types(reply),
        expected_group_types,
        "case {case:?} / {reader}: the group must offer {expected_group_types:?}, got {:?}",
        wire_group_network_types(reply)
    );
}

/// Run one row: build its network, create the group over it, take the network away, and
/// assert every reader reports the group over what the owner of subnets still holds.
async fn run(case: &Case) {
    let emulator = Emulator::new();
    let ec2 = emulator.ec2(REGION);
    let (vpc, minted) = account_network(&emulator, case.subnets).await;

    let created = create_group(&emulator, case.label, case.group, &minted).await;
    assert_group_reports(case.label, "create", &created, case.group, &vpc, &minted);

    // The OWNER of the subnets takes them away. Nothing of the cache subnet group is
    // deleted with them.
    for index in case.deleted {
        let (id, _) = &minted[*index];
        ec2.delete_subnet()
            .subnet_id(id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: EC2 must delete the subnet {id}: {error:?}", case.label)
            });
    }
    if case.deletes_vpc {
        ec2.delete_vpc()
            .vpc_id(&vpc)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: EC2 must delete the VPC {vpc}: {error:?}", case.label)
            });
        // The VPC really is gone, as the owner of VPCs answers for it — so what the group
        // reports below is a VPC that no longer exists.
        let refused = ec2
            .describe_vpcs()
            .vpc_ids(&vpc)
            .send()
            .await
            .expect_err("EC2 must not report a VPC it deleted");
        assert_eq!(
            refused.code(),
            Some("InvalidVpcID.NotFound"),
            "case {:?}: EC2 must answer InvalidVpcID.NotFound for the deleted VPC, got {:?}",
            case.label,
            refused.code()
        );
    }

    let survivors: Vec<Member> = minted
        .iter()
        .enumerate()
        .filter(|(index, _)| !case.deleted.contains(index))
        .map(|(_, member)| member.clone())
        .collect();

    // Reader 1: the describe of the one group by name.
    let named = describe(&emulator, case.group).await;
    assert_group_reports(
        case.label,
        "describe by name",
        &named,
        case.group,
        &vpc,
        &survivors,
    );
    // Reader 2: the same describe again — a read changes nothing a later caller sees.
    let again = describe(&emulator, case.group).await;
    assert_group_reports(
        case.label,
        "describe by name, a second time",
        &again,
        case.group,
        &vpc,
        &survivors,
    );
    // Reader 3: the describe of the whole page, which resolves every group it carries.
    let listed = emulator.call("DescribeCacheSubnetGroups", &[]).await;
    assert_group_reports(case.label, "describe the page", &listed, case.group, &vpc, &survivors);
    // Reader 4: the modify's OWN answer, for a modify that changed nothing about the
    // membership.
    let modified = emulator
        .call(
            "ModifyCacheSubnetGroup",
            &params(&[("CacheSubnetGroupName", case.group)]),
        )
        .await;
    assert_group_reports(case.label, "modify", &modified, case.group, &vpc, &survivors);
}

#[tokio::test]
async fn a_group_reports_only_the_subnets_the_owner_still_holds() {
    for case in CASES.iter().filter(|case| !case.deletes_vpc) {
        run(case).await;
    }
}

#[tokio::test]
async fn an_orphaned_group_outlives_its_subnets_and_its_vpc() {
    for case in CASES.iter().filter(|case| case.deletes_vpc) {
        run(case).await;
    }
}

#[tokio::test]
async fn every_group_of_a_page_is_reported_over_the_subnets_the_owner_holds_now() {
    let case = "two groups sharing a subnet";
    let emulator = Emulator::new();
    let ec2 = emulator.ec2(REGION);
    let (vpc, minted) = account_network(&emulator, 3).await;

    // Two groups over overlapping halves of the same network, so the middle subnet is a
    // member of BOTH and one page has to resolve both groups' identifiers.
    let first = "page-live-first";
    let second = "page-live-second";
    create_group(&emulator, case, first, &minted[0..2]).await;
    create_group(&emulator, case, second, &minted[1..3]).await;

    // The owner takes the SHARED subnet away: it drops out of both groups, and each group
    // keeps the member it does not share.
    ec2.delete_subnet()
        .subnet_id(&minted[1].0)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must delete the shared subnet: {error:?}"));

    assert_group_reports(
        case,
        "describe the first group",
        &describe(&emulator, first).await,
        first,
        &vpc,
        &minted[0..1],
    );
    assert_group_reports(
        case,
        "describe the second group",
        &describe(&emulator, second).await,
        second,
        &vpc,
        &minted[2..3],
    );
    let listed = emulator.call("DescribeCacheSubnetGroups", &[]).await;
    assert_eq!(
        listed.all("CacheSubnetGroupName"),
        vec![first, second],
        "case {case:?}: the page must carry both groups, got {}",
        listed.body
    );
    assert_eq!(
        listed.all("Subnets/Subnet/SubnetIdentifier"),
        vec![minted[0].0.as_str(), minted[2].0.as_str()],
        "case {case:?}: each group of the page must hold only the subnet it still has, got {:?}",
        listed.all("Subnets/Subnet/SubnetIdentifier")
    );

    // The rest of the network goes too, the VPC with it: both groups survive it, holding no
    // subnet and still naming the VPC they were created in.
    for (id, _) in [&minted[0], &minted[2]] {
        ec2.delete_subnet()
            .subnet_id(id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: EC2 must delete the subnet {id}: {error:?}")
            });
    }
    ec2.delete_vpc()
        .vpc_id(&vpc)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must delete the VPC {vpc}: {error:?}"));

    let emptied = emulator.call("DescribeCacheSubnetGroups", &[]).await;
    assert_eq!(
        emptied.all("CacheSubnetGroupName"),
        vec![first, second],
        "case {case:?}: both groups must outlive their network, got {}",
        emptied.body
    );
    assert_eq!(
        emptied.all("VpcId"),
        vec![vpc.as_str(), vpc.as_str()],
        "case {case:?}: both groups must still report the VPC they were created in, got {:?}",
        emptied.all("VpcId")
    );
    assert!(
        emptied.all("Subnets/Subnet/SubnetIdentifier").is_empty(),
        "case {case:?}: a group whose subnets are all gone must hold none, got {}",
        emptied.body
    );
    assert!(
        emptied.all("SupportedNetworkTypes/member").is_empty(),
        "case {case:?}: a group holding no subnet must offer no network type, got {}",
        emptied.body
    );
}
