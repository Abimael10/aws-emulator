// SPDX-License-Identifier: CC0-1.0

//! The network types a cache subnet group publishes when its subnets are addressed in BOTH
//! families, and the membership every answer that carries a `CacheSubnetGroup` derives them
//! from.
//!
//! ONE rule, over a set of inputs: `Subnet$SupportedNetworkTypes` and
//! `CacheSubnetGroup$SupportedNetworkTypes` are "Either `ipv4` | `ipv6` | `dual_stack`"
//! (their own member docs; the values and their order are the modelled `NetworkType` enum's)
//! and are DERIVED, in every answer that carries a `CacheSubnetGroup`, from the subnets the
//! group names as Amazon EC2 — the service that owns subnets — holds them at the instant of
//! the call: a subnet offers `ipv4` for an IPv4 block, `ipv6` for an IPv6 block and
//! `dual_stack` when it carries both, and the group, which a cache is placed ACROSS, offers
//! what every one of its subnets offers.
//!
//! The sibling suite [`crate::cache_subnet_group_network_types`] spans the family mixes a
//! subnet can be addressed in and the create/describe pair over the account's IPv4-only
//! subnets. The rows here are the ones only an IPv6-addressed network reaches: groups whose
//! subnets carry an IPv6 block too, `ModifyCacheSubnetGroup`'s OWN answer, and what a group
//! reports after the owner of its subnets changes their addressing or takes one away.
//!
//! Every subnet a row names was minted by EC2 and addressed through EC2's own operations,
//! and what it is addressed for is read back off EC2's record rather than assumed — so a
//! group that published something else would fail here.

use std::net::Ipv6Addr;

use crate::cache_cluster_lifecycle::{subnet_group_params, subnet_pairs};
use crate::cache_subnet_group_network_types::{
    expected_network_types, wire_group_network_types, wire_subnets,
};
use crate::harness::{params, Emulator, Reply, REGION};

/// The IPv4 CIDR of the one VPC every subnet below sits in. A group reports ONE `VpcId`, so
/// every row's subnets share a VPC; each subnet takes a /24 out of this /16.
const VPC_CIDR: &str = "10.90.0.0/16";

/// How many Availability Zones the fixture spreads its subnets over, so a row can name up to
/// three subnets in as many zones.
const FIXTURE_ZONES: usize = 3;

/// What a subnet addressed in IPv4 alone offers, in the order the modelled `NetworkType`
/// enum declares its values (`ipv4 | ipv6 | dual_stack`, from
/// `smithy_query(query="shape", shape="NetworkType")`).
const IPV4_ONLY: [&str; 1] = ["ipv4"];

/// What a subnet addressed in BOTH families offers; see [`IPV4_ONLY`] for the source of the
/// values and their order.
const DUAL_STACK: [&str; 3] = ["ipv4", "ipv6", "dual_stack"];

/// One expected member of a group: the subnet id EC2 minted, and the network types that
/// subnet's addressing means it offers.
type Member = (String, Vec<&'static str>);

/// The account's network for these cases: one VPC with an Amazon-provided IPv6 block, and in
/// each of [`FIXTURE_ZONES`] zones one IPv4-only subnet and one that also carries an IPv6
/// block of the VPC's.
struct Network {
    emulator: Emulator,
    /// The zones EC2 reports for [`REGION`], in the order it reports them.
    zones: Vec<String>,
    vpc: String,
    /// The IPv6 block EC2 gave the VPC, which a subnet's own block is cut out of.
    vpc_ipv6: String,
    /// One IPv4-only subnet per zone, in zone order.
    ipv4_only: Vec<String>,
    /// One dual-stack subnet per zone, in zone order.
    dual: Vec<String>,
}

/// The `/64` block at `index` of the VPC's Amazon-provided `/56`, which is the size a
/// subnet's IPv6 block has ("an Amazon-provided IPv6 CIDR block with a /56 prefix length for
/// the VPC", `AssociateVpcCidrBlock$AmazonProvidedIpv6CidrBlock`; `AssociateSubnetCidrBlock`
/// takes the subnet's /64 out of it).
fn subnet_ipv6_block(vpc_block: &str, index: u16) -> String {
    let base: Ipv6Addr = vpc_block
        .split('/')
        .next()
        .unwrap_or_default()
        .parse()
        .unwrap_or_else(|error| {
            panic!("setup: EC2's VPC block {vpc_block:?} must be an IPv6 address: {error:?}")
        });
    let mut groups = base.segments();
    // The /56 fixes everything above the low byte of the fourth group, so that byte is the
    // subnet selector inside it.
    groups[3] |= index & 0xff;
    format!("{}/64", Ipv6Addr::from(groups))
}

/// One subnet `ec2:CreateSubnet` mints in `vpc_id`, taking the `/24` at `index` of
/// [`VPC_CIDR`], as the id EC2 reported for it.
async fn create_subnet(
    ec2: &::aws_sdk_ec2::Client,
    vpc_id: &str,
    index: usize,
    zone: &str,
) -> String {
    ec2.create_subnet()
        .vpc_id(vpc_id)
        .cidr_block(format!("10.90.{index}.0/24"))
        .availability_zone(zone)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create a subnet in {zone}: {error:?}"))
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .unwrap_or_else(|| panic!("setup: CreateSubnet must report the subnet it minted"))
        .to_string()
}

/// Have EC2 address `subnet_id` in IPv6 as well, with the `/64` at `index` of the VPC's
/// block.
async fn address_in_ipv6(
    ec2: &::aws_sdk_ec2::Client,
    vpc_ipv6: &str,
    subnet_id: &str,
    index: usize,
) {
    let block = subnet_ipv6_block(vpc_ipv6, u16::try_from(index).unwrap_or(1));
    ec2.associate_subnet_cidr_block()
        .subnet_id(subnet_id)
        .ipv6_cidr_block(&block)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("setup: EC2 must associate {block} with the subnet {subnet_id}: {error:?}")
        });
}

/// Stand up one emulator and build the network the rows are drawn from, through EC2's own
/// operations.
async fn network() -> Network {
    let emulator = Emulator::new();
    let ec2 = emulator.ec2(REGION);
    let zones: Vec<String> = ec2
        .describe_availability_zones()
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must report {REGION}'s zones: {error:?}"))
        .availability_zones()
        .iter()
        .filter_map(|zone| zone.zone_name().map(str::to_string))
        .filter(|zone| zone.starts_with(REGION))
        .collect();
    assert!(
        zones.len() >= FIXTURE_ZONES,
        "setup: expected {REGION} to have at least {FIXTURE_ZONES} zones, got {zones:?}"
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
    // A subnet's IPv6 block has to sit inside one its VPC holds, so the VPC takes an
    // Amazon-provided one first.
    let vpc_ipv6 = ec2
        .associate_vpc_cidr_block()
        .vpc_id(&vpc)
        .amazon_provided_ipv6_cidr_block(true)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("setup: EC2 must give {vpc} an Amazon-provided IPv6 block: {error:?}")
        })
        .ipv6_cidr_block_association()
        .and_then(|association| association.ipv6_cidr_block().map(str::to_string))
        .unwrap_or_else(|| panic!("setup: EC2 must report the IPv6 block it gave {vpc}"));

    let mut ipv4_only = Vec::with_capacity(FIXTURE_ZONES);
    let mut dual = Vec::with_capacity(FIXTURE_ZONES);
    for (index, zone) in zones.iter().take(FIXTURE_ZONES).enumerate() {
        ipv4_only.push(create_subnet(&ec2, &vpc, index, zone).await);

        let both = create_subnet(&ec2, &vpc, index + 10, zone).await;
        address_in_ipv6(&ec2, &vpc_ipv6, &both, index + 10).await;
        dual.push(both);
    }

    Network {
        emulator,
        zones,
        vpc,
        vpc_ipv6,
        ipv4_only,
        dual,
    }
}

/// Assert `reply` reports exactly the members `expected` names, each offering the network
/// types its addressing means — asked of EC2 as well as stated by the row — and the group
/// offering what all of them offer.
async fn assert_group_reports(emulator: &Emulator, case: &str, reply: &Reply, expected: &[Member]) {
    let reported = wire_subnets(case, reply);
    assert_eq!(
        reported
            .iter()
            .map(|subnet| subnet.identifier.clone())
            .collect::<Vec<String>>(),
        expected
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<String>>(),
        "case {case}: the group must hold the subnets it was built over, got {reported:?}"
    );
    for (subnet, (id, offers)) in reported.iter().zip(expected) {
        assert_eq!(
            subnet.network_types, *offers,
            "case {case}: {id} must offer {offers:?}, got {:?}",
            subnet.network_types
        );
        let asked = expected_network_types(emulator, id).await;
        assert_eq!(
            subnet.network_types, asked,
            "case {case}: {id} must offer the types EC2 addresses it for ({asked:?}), got {:?}",
            subnet.network_types
        );
        assert!(
            !subnet.zone.is_empty(),
            "case {case}: {id} must report its zone, got {:?}",
            subnet.zone
        );
    }

    // What the GROUP offers is what every one of its subnets offers.
    let mut shared: Vec<&str> = expected
        .first()
        .map(|(_, offers)| offers.clone())
        .unwrap_or_default();
    for (_, offers) in expected.get(1..).unwrap_or_default() {
        shared.retain(|offered| offers.contains(offered));
    }
    assert_eq!(
        wire_group_network_types(reply),
        shared,
        "case {case}: the group must offer {shared:?}, got {:?}",
        wire_group_network_types(reply)
    );
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

#[tokio::test]
async fn a_group_publishes_the_network_types_of_the_families_its_subnets_are_addressed_in() {
    let net = network().await;
    let ipv4 = |index: usize| (net.ipv4_only[index].clone(), IPV4_ONLY.to_vec());
    let both = |index: usize| (net.dual[index].clone(), DUAL_STACK.to_vec());

    // The axis: which families the member subnets are addressed in, how many the group holds,
    // and in which order the mixed rows name them.
    let rows: Vec<(&str, Vec<Member>)> = vec![
        ("one IPv4-only subnet", vec![ipv4(0)]),
        ("two IPv4-only subnets", vec![ipv4(0), ipv4(1)]),
        ("one dual-stack subnet", vec![both(0)]),
        ("two dual-stack subnets", vec![both(0), both(1)]),
        ("three dual-stack subnets", vec![both(0), both(1), both(2)]),
        (
            "a dual-stack subnet then an IPv4-only one",
            vec![both(0), ipv4(1)],
        ),
        (
            "an IPv4-only subnet then a dual-stack one",
            vec![ipv4(0), both(1)],
        ),
    ];

    for (index, (case, members)) in rows.iter().enumerate() {
        let ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
        let name = format!("dual-net-types-{index}");
        let created = net
            .emulator
            .call(
                "CreateCacheSubnetGroup",
                &subnet_group_params(&name, case, &ids),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the group must be created, got {} {}",
            created.status, created.body
        );
        let described = describe(&net.emulator, &name).await;
        assert_eq!(
            described.status, 200,
            "case {case}: the group must be readable back, got {} {}",
            described.status, described.body
        );
        // The create's own answer and a later read of the same group report the same thing.
        for (reader, reply) in [("create", &created), ("describe", &described)] {
            assert_group_reports(&net.emulator, &format!("{case} / {reader}"), reply, members).await;
        }
    }
}

#[tokio::test]
async fn a_modify_reports_the_network_types_of_the_membership_it_leaves_the_group_with() {
    let net = network().await;
    let ipv4 = |index: usize| (net.ipv4_only[index].clone(), IPV4_ONLY.to_vec());
    let both = |index: usize| (net.dual[index].clone(), DUAL_STACK.to_vec());

    let name = "dual-net-types-modify";
    let mut expected: Vec<Member> = vec![ipv4(0), ipv4(1)];
    let created = net
        .emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(
                name,
                "two IPv4-only subnets",
                &expected
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<String>>(),
            ),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the group under test must be created, got {} {}",
        created.status, created.body
    );

    // The axis: what the modify changes — the description alone (the membership stands), and
    // each family mix a replacement membership can have.
    let rows: Vec<(&str, &str, Option<Vec<Member>>)> = vec![
        ("the description alone", "described again", None),
        (
            "replaced by two dual-stack subnets",
            "now dual-stack",
            Some(vec![both(0), both(1)]),
        ),
        (
            "replaced by a dual-stack and an IPv4-only subnet",
            "now mixed",
            Some(vec![both(0), ipv4(1)]),
        ),
        (
            "replaced by one IPv4-only subnet",
            "now IPv4-only again",
            Some(vec![ipv4(2)]),
        ),
        (
            "the description alone, over the replaced membership",
            "described again, same subnets",
            None,
        ),
    ];

    for (case, description, replacement) in rows {
        let mut pairs = params(&[
            ("CacheSubnetGroupName", name),
            ("CacheSubnetGroupDescription", description),
        ]);
        if let Some(members) = replacement {
            pairs.extend(subnet_pairs(
                "SubnetIdentifier",
                &members
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<String>>(),
            ));
            expected = members;
        }
        let modified = net.emulator.call("ModifyCacheSubnetGroup", &pairs).await;
        assert_eq!(
            modified.status, 200,
            "case {case}: the modify must be accepted, got {} {}",
            modified.status, modified.body
        );
        // The modify's OWN answer carries the member, and a later read agrees with it.
        assert_group_reports(
            &net.emulator,
            &format!("{case} / modify"),
            &modified,
            &expected,
        )
        .await;
        let described = describe(&net.emulator, name).await;
        assert_group_reports(
            &net.emulator,
            &format!("{case} / describe"),
            &described,
            &expected,
        )
        .await;
    }
}

#[tokio::test]
async fn a_group_follows_the_addressing_the_owner_of_subnets_reports_now() {
    let net = network().await;
    let ec2 = net.emulator.ec2(REGION);
    // A subnet of this case's own, so re-addressing it and taking it away disturbs no other
    // row: the /24 and the /64 below are ones the fixture's own subnets do not hold.
    let subnet = create_subnet(&ec2, &net.vpc, 20, &net.zones[0]).await;

    let name = "dual-net-types-live";
    let created = net
        .emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(
                name,
                "one IPv4-only subnet",
                ::std::slice::from_ref(&subnet),
            ),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the group under test must be created, got {} {}",
        created.status, created.body
    );
    assert_group_reports(
        &net.emulator,
        "a subnet addressed in IPv4 alone / create",
        &created,
        &[(subnet.clone(), IPV4_ONLY.to_vec())],
    )
    .await;

    // The OWNER of the subnet addresses it in IPv6 as well, which the group named nothing
    // about: every answer that carries the group now offers the types that addressing means.
    address_in_ipv6(&ec2, &net.vpc_ipv6, &subnet, 20).await;
    let after = [(subnet.clone(), DUAL_STACK.to_vec())];
    let described = describe(&net.emulator, name).await;
    assert_group_reports(
        &net.emulator,
        "after the owner addressed it in IPv6 / describe",
        &described,
        &after,
    )
    .await;
    let touched = net
        .emulator
        .call(
            "ModifyCacheSubnetGroup",
            &params(&[
                ("CacheSubnetGroupName", name),
                ("CacheSubnetGroupDescription", "same subnet, now dual-stack"),
            ]),
        )
        .await;
    assert_eq!(
        touched.status, 200,
        "the modify must be accepted, got {} {}",
        touched.status, touched.body
    );
    assert_group_reports(
        &net.emulator,
        "after the owner addressed it in IPv6 / modify",
        &touched,
        &after,
    )
    .await;

    // The owner takes the subnet away. The group survives it, and its derived membership
    // shrinks to the subnets that are still there — so there is no network type left to
    // offer, on either path that reports the group.
    ec2.delete_subnet()
        .subnet_id(&subnet)
        .send()
        .await
        .unwrap_or_else(|error| panic!("EC2 must delete the subnet it minted: {error:?}"));
    let gone_describe = describe(&net.emulator, name).await;
    let gone_modify = net
        .emulator
        .call(
            "ModifyCacheSubnetGroup",
            &params(&[
                ("CacheSubnetGroupName", name),
                ("CacheSubnetGroupDescription", "the subnet is gone"),
            ]),
        )
        .await;
    for (reader, reply) in [("describe", &gone_describe), ("modify", &gone_modify)] {
        let case = format!("after the owner deleted the subnet / {reader}");
        assert_eq!(
            reply.status, 200,
            "case {case}: the group must still be reported, got {} {}",
            reply.status, reply.body
        );
        assert!(
            reply.all("Subnets/Subnet/SubnetIdentifier").is_empty(),
            "case {case}: the group must hold no subnet the owner no longer has, got {}",
            reply.body
        );
        assert!(
            wire_group_network_types(reply).is_empty(),
            "case {case}: a group holding no subnet must offer no network type, got {:?}",
            wire_group_network_types(reply)
        );
    }
}
