// SPDX-License-Identifier: CC0-1.0

//! The TEARDOWN of a cache subnet group whose network the owner of subnets has taken away.
//!
//! ONE rule, over a set of inputs: a cache subnet group's membership
//! (`CacheSubnetGroup$Subnets` and `CacheSubnetGroup$SupportedNetworkTypes`) is resolved at
//! READ time against the subnets Amazon EC2 — the service that owns subnets — holds for the
//! account at that instant, while the group's own identity and its own LIFECYCLE are
//! independent of that network: a group left holding none of the subnets it was created over
//! is still a group of the account, so it is readable (by name and in the account's page),
//! and `DeleteCacheSubnetGroup` removes it — the group is in use only while a cache is placed
//! in it, and a subnet EC2 no longer holds places none — after which the name names no group,
//! and `DescribeCacheSubnetGroups` for it answers the one error it models,
//! `CacheSubnetGroupNotFoundFault` (400), worded `Cache subnet group <name> not found.`
//!
//! That is the WHOLE of the flow a client reconciling cache placement against its network
//! drives: read the group, find it holds nothing, garbage-collect it, and confirm it is gone.
//! The sibling suite [`crate::cache_subnet_group_outlives_its_network`] pins the reading of
//! the shrinking membership; this one pins what the lifecycle does over it, which is what
//! tells a client the orphan can be cleaned up at all.
//!
//! The rows differ along what the rule generalises over: how many subnets the group was built
//! over (one, two, three), how many of them the owner deletes (none, one, all) and in which
//! order (forwards, backwards, the middle alone), whether the VPC itself is deleted after
//! them, and the case the delete and the describe that follows spell the group's name in —
//! `CacheSubnetGroupName` "is stored as a lowercase string", so the group has one identity
//! whatever a request spells.
//!
//! Nothing here reaches into either store: every subnet and VPC is minted and deleted through
//! EC2's own operations, where the service under test resolves them, and a deleted VPC is
//! confirmed gone by asking EC2 for it.

use ::aws_sdk_ec2::error::ProvideErrorMetadata as _;

use crate::cache_cluster_lifecycle::subnet_group_params;
use crate::cache_subnet_group_network_types::wire_group_network_types;
use crate::cache_subnet_group_outlives_its_network::{account_network, Member};
use crate::harness::{params, Emulator, Reply, REGION};

/// The description every group below is created with; it is stored identity, so it survives
/// the network going away.
const DESCRIPTION: &str = "orphan teardown subnet group";

/// What a group whose every subnet is addressed in IPv4 alone offers, in the order the
/// modelled `NetworkType` enum declares its values
/// (`smithy_query(query="shape", shape="NetworkType")`: `ipv4 | ipv6 | dual_stack`).
const IPV4_ONLY: [&str; 1] = ["ipv4"];

/// One row: the group's name, how many subnets it is built over, which of those the owner
/// then deletes and in what order, whether the VPC goes with them, and the spelling the
/// teardown addresses the group by.
struct Case {
    label: &'static str,
    created_as: &'static str,
    addressed_as: &'static str,
    subnets: usize,
    deleted: &'static [usize],
    deletes_vpc: bool,
}

/// The rows, spanning the membership a group can be holding when its teardown is driven:
/// untouched, shrunk in the middle, emptied forwards and backwards, with and without the VPC
/// surviving, and addressed in each case the one identity can be spelled in.
const CASES: &[Case] = &[
    Case {
        label: "nothing deleted: the group still holds both subnets",
        created_as: "teardown-untouched",
        addressed_as: "teardown-untouched",
        subnets: 2,
        deleted: &[],
        deletes_vpc: false,
    },
    Case {
        label: "the middle of three subnets deleted, the group addressed in upper case",
        created_as: "teardown-partly-emptied",
        addressed_as: "TEARDOWN-PARTLY-EMPTIED",
        subnets: 3,
        deleted: &[1],
        deletes_vpc: false,
    },
    Case {
        label: "the one subnet deleted and then its VPC",
        created_as: "teardown-one-and-the-vpc",
        addressed_as: "teardown-one-and-the-vpc",
        subnets: 1,
        deleted: &[0],
        deletes_vpc: true,
    },
    Case {
        label: "both subnets deleted while the VPC stays, the group addressed in mixed case",
        created_as: "teardown-both-vpc-kept",
        addressed_as: "Teardown-Both-Vpc-Kept",
        subnets: 2,
        deleted: &[0, 1],
        deletes_vpc: false,
    },
    Case {
        label: "three subnets deleted back to front and then their VPC",
        created_as: "teardown-three-and-the-vpc",
        addressed_as: "teardown-three-and-the-vpc",
        subnets: 3,
        deleted: &[2, 1, 0],
        deletes_vpc: true,
    },
];

/// Create the group `name` over `members`, asserting the create itself succeeded.
async fn create_group(emulator: &Emulator, case: &str, name: &str, members: &[Member]) -> Reply {
    let ids: Vec<String> = members.iter().map(|(id, _)| id.clone()).collect();
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(name, DESCRIPTION, &ids),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the group {name} must be created, got {} {}",
        created.status, created.body
    );
    created
}

/// Describe the one group `name` names.
async fn describe(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DescribeCacheSubnetGroups",
            &params(&[("CacheSubnetGroupName", name)]),
        )
        .await
}

/// Delete the group `name` names.
async fn delete(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "DeleteCacheSubnetGroup",
            &params(&[("CacheSubnetGroupName", name)]),
        )
        .await
}

/// Every `CacheSubnetGroupName` a describe of the whole account reports.
async fn all_group_names(emulator: &Emulator) -> Vec<String> {
    let reply = emulator.call("DescribeCacheSubnetGroups", &[]).await;
    assert_eq!(
        reply.status, 200,
        "a describe of every cache subnet group must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
        .all("CacheSubnetGroups/CacheSubnetGroup/CacheSubnetGroupName")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The identity a request's `CacheSubnetGroupName` resolves to: "This value is stored as a
/// lowercase string" (`CreateCacheSubnetGroup$CacheSubnetGroupName`), which is the name every
/// answer reports and every refusal words itself with.
fn stored_identity(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// How the DESCRIBE of cache subnet groups words a name this account holds no group under:
/// the fixed text `Cache subnet group ` and ` not found.` around the name's stored identity.
fn describe_not_found_message(name: &str) -> String {
    format!("Cache subnet group {} not found.", stored_identity(name))
}

/// Assert the reply reports the one group `name`, created in `vpc`, holding exactly
/// `members` — the subnets the OWNER still holds — and the network types that membership
/// gives it.
fn assert_group_reports(
    case: &str,
    reader: &str,
    reply: &Reply,
    name: &str,
    vpc: &str,
    members: &[Member],
) {
    assert_eq!(
        reply.status, 200,
        "case {case:?} / {reader}: the group must be reported, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.all("CacheSubnetGroupName"),
        vec![stored_identity(name)],
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
    let expected_ids: Vec<&str> = members.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        reply.all("Subnets/Subnet/SubnetIdentifier"),
        expected_ids,
        "case {case:?} / {reader}: the group must hold only the subnets the owner still has, \
         expected {expected_ids:?}, got {:?}",
        reply.all("Subnets/Subnet/SubnetIdentifier")
    );
    let expected_zones: Vec<&str> = members.iter().map(|(_, zone)| zone.as_str()).collect();
    assert_eq!(
        reply.all("SubnetAvailabilityZone/Name"),
        expected_zones,
        "case {case:?} / {reader}: each member must report the zone EC2 minted it in, \
         expected {expected_zones:?}, got {:?}",
        reply.all("SubnetAvailabilityZone/Name")
    );
    let expected_types: Vec<String> = if members.is_empty() {
        Vec::new()
    } else {
        IPV4_ONLY.iter().map(|value| (*value).to_string()).collect()
    };
    assert_eq!(
        wire_group_network_types(reply),
        expected_types,
        "case {case:?} / {reader}: the group must offer {expected_types:?}, got {:?}",
        wire_group_network_types(reply)
    );
}

/// Assert `reply` is the delete's modelled SUCCESS: the 200, its own empty result envelope
/// and no error envelope at all, which is what tells a client the group is gone.
fn assert_deleted(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the delete of the group must answer 200, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        "",
        "case {case:?}: the delete must carry no error envelope, got {}",
        reply.body
    );
    assert!(
        reply.body.contains("DeleteCacheSubnetGroupResult"),
        "case {case:?}: the delete must answer its own result envelope, got {}",
        reply.body
    );
}

/// Assert `name` names no cache subnet group of the account any more: the describe answers
/// the one error it models, worded with the name's stored identity, and the account's page
/// does not carry it.
async fn assert_absent(emulator: &Emulator, case: &str, name: &str) {
    let described = describe(emulator, name).await;
    assert_eq!(
        described.status, 400,
        "case {case:?}: a describe of the deleted group must answer 400, got {} {}",
        described.status, described.body
    );
    assert_eq!(
        described.error_code(),
        "CacheSubnetGroupNotFoundFault",
        "case {case:?}: expected the modelled not-found code, got {} in {}",
        described.error_code(),
        described.body
    );
    assert_eq!(
        described.error_message(),
        describe_not_found_message(name),
        "case {case:?}: expected {:?}, got {:?}",
        describe_not_found_message(name),
        described.error_message()
    );
    let names = all_group_names(emulator).await;
    assert!(
        !names.contains(&stored_identity(name)),
        "case {case:?}: the account must hold no group {name} any more, got {names:?}"
    );
}

/// Take the subnets `deleted` names away through EC2's own `DeleteSubnet`, and the VPC too
/// when `deletes_vpc` — confirming with the owner that the VPC really is gone.
async fn tear_down_network(
    label: &str,
    deleted: &[usize],
    deletes_vpc: bool,
    ec2: &::aws_sdk_ec2::Client,
    vpc: &str,
    minted: &[Member],
) {
    for index in deleted {
        let (id, _) = &minted[*index];
        ec2.delete_subnet()
            .subnet_id(id)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {label:?}: EC2 must delete the subnet {id}: {error:?}")
            });
    }
    if !deletes_vpc {
        return;
    }
    ec2.delete_vpc()
        .vpc_id(vpc)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {label:?}: EC2 must delete the VPC {vpc}: {error:?}")
        });
    // The VPC really is gone, as the OWNER of VPCs answers for it, so the `VpcId` the group
    // goes on reporting below is one that no longer resolves.
    let refused = ec2
        .describe_vpcs()
        .vpc_ids(vpc)
        .send()
        .await
        .expect_err("EC2 must not report a VPC it deleted");
    assert_eq!(
        refused.code(),
        Some("InvalidVpcID.NotFound"),
        "case {label:?}: EC2 must answer InvalidVpcID.NotFound for the deleted VPC, got {:?}",
        refused.code()
    );
    assert_eq!(
        refused.message(),
        Some(format!("The vpc ID '{vpc}' does not exist").as_str()),
        "case {label:?}: expected the owner's own sentence for the deleted VPC, got {:?}",
        refused.message()
    );
}

/// The members of `minted` a teardown of `deleted` leaves in place, in the order the group
/// names them.
fn survivors(deleted: &[usize], minted: &[Member]) -> Vec<Member> {
    minted
        .iter()
        .enumerate()
        .filter(|(index, _)| !deleted.contains(index))
        .map(|(_, member)| member.clone())
        .collect()
}

#[tokio::test]
async fn a_group_whose_network_is_gone_is_read_deleted_and_then_not_found() {
    for case in CASES {
        let emulator = Emulator::new();
        let ec2 = emulator.ec2(REGION);
        let (vpc, minted) = account_network(&emulator, case.subnets).await;

        let created = create_group(&emulator, case.label, case.created_as, &minted).await;
        assert_group_reports(
            case.label,
            "create",
            &created,
            case.created_as,
            &vpc,
            &minted,
        );

        tear_down_network(
            case.label,
            case.deleted,
            case.deletes_vpc,
            &ec2,
            &vpc,
            &minted,
        )
        .await;
        let left = survivors(case.deleted, &minted);

        // The group is still a group of the account, over whatever membership is left —
        // whichever reader a client reaches it through.
        let named = describe(&emulator, case.addressed_as).await;
        assert_group_reports(
            case.label,
            "describe by name",
            &named,
            case.addressed_as,
            &vpc,
            &left,
        );
        let names = all_group_names(&emulator).await;
        assert_eq!(
            names,
            vec![stored_identity(case.created_as)],
            "case {:?}: the account's page must carry the group, got {names:?}",
            case.label
        );

        // ... and because nothing of the deleted network holds it, the delete removes it.
        assert_deleted(case.label, &delete(&emulator, case.addressed_as).await);
        assert_absent(&emulator, case.label, case.addressed_as).await;
    }
}

/// One row of the second rule: the spelling the group is re-created with after its orphaned
/// namesake was deleted, over a network of its own.
struct ReuseCase {
    label: &'static str,
    created_as: &'static str,
    recreated_as: &'static str,
    subnets: usize,
}

const REUSE_CASES: &[ReuseCase] = &[
    ReuseCase {
        label: "the same spelling again, over one subnet",
        created_as: "teardown-reused-name",
        recreated_as: "teardown-reused-name",
        subnets: 1,
    },
    ReuseCase {
        label: "the same identity spelled in upper case, over two subnets",
        created_as: "teardown-reused-identity",
        recreated_as: "TEARDOWN-REUSED-IDENTITY",
        subnets: 2,
    },
];

#[tokio::test]
async fn the_name_of_a_deleted_orphan_is_a_free_name_again() {
    for case in REUSE_CASES {
        let emulator = Emulator::new();
        let ec2 = emulator.ec2(REGION);
        let (vpc, minted) = account_network(&emulator, case.subnets).await;
        create_group(&emulator, case.label, case.created_as, &minted).await;

        // The whole network goes, and the orphan with it.
        let every_position: Vec<usize> = (0..case.subnets).collect();
        tear_down_network(case.label, &every_position, true, &ec2, &vpc, &minted).await;
        assert_group_reports(
            case.label,
            "describe the orphan",
            &describe(&emulator, case.created_as).await,
            case.created_as,
            &vpc,
            &[],
        );
        assert_deleted(case.label, &delete(&emulator, case.created_as).await);
        assert_absent(&emulator, case.label, case.created_as).await;

        // The name is the account's to use again, and the group that takes it reports the
        // network it was created over rather than anything of its namesake.
        let (second_vpc, second_minted) = account_network(&emulator, case.subnets).await;
        let recreated =
            create_group(&emulator, case.label, case.recreated_as, &second_minted).await;
        assert_group_reports(
            case.label,
            "the create that takes the name again",
            &recreated,
            case.recreated_as,
            &second_vpc,
            &second_minted,
        );
        assert_group_reports(
            case.label,
            "describe the group that took the name",
            &describe(&emulator, case.recreated_as).await,
            case.recreated_as,
            &second_vpc,
            &second_minted,
        );
    }
}
