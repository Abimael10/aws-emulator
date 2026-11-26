// SPDX-License-Identifier: CC0-1.0

//! The EC2 security groups a PCS create would attach the cluster's control-plane ENI to.
//!
//! The rule under test is the SECOND half of the sentence `tests/all/subnet_placement.rs`
//! drives the first half of: "The subnets and security groups for the cluster must already
//! exist before you use this API action" (`CreateCluster`'s own documentation). A security
//! group is EC2's resource, not PCS's, and `NetworkingRequest$securityGroupIds` is "a list of
//! security group IDs associated with the Elastic Network Interface (ENI) created in
//! subnets", so a create that names security groups resolves EVERY id against EC2 before it
//! mints anything: if any of them resolves to no group of the account, the create is refused
//! with the service's fixed sentence and persists nothing. A refusal is grounded in what EC2
//! ANSWERED, never assumed from silence, so an emulator with no EC2 to ask serves the create.
//!
//! The rows span what the rule generalises over: how many groups a request names, whether
//! each one resolves, where an unresolvable id falls in the list, how a group came to exist
//! (created through EC2, the default group of a VPC nobody created, one that was created and
//! then deleted) and which FORM an unresolvable id takes (a value outside the id alphabet EC2
//! mints, and a well-formed id the account was never issued). Nothing here reaches into
//! either store: a security group exists only because EC2 answered for it.

use serde_json::{json, Value};

use super::{cluster_names, create_vpc, create_subnet, emulator_with_ec2, VPC_CIDR};
use crate::cluster_lifecycle::{call, emulator};

/// What PCS answers a create whose security groups it could not resolve. The whole sentence
/// is fixed text: it names no security group id and no count.
const SECURITY_GROUPS_ABSENT: &str =
    "AWS PCS couldn't find 1 or more of the security groups you provided.";

/// The group name EC2 gives the one security group it creates WITH a VPC: "your VPC
/// automatically comes with a default security group" (Amazon VPC User Guide, "Default
/// security groups for your VPCs").
const DEFAULT_GROUP_NAME: &str = "default";

/// A security group of `vpc`, as EC2 minted it.
async fn create_security_group(ec2: &::aws_sdk_ec2::Client, vpc: &str, name: &str) -> String {
    ec2.create_security_group()
        .vpc_id(vpc)
        .group_name(name)
        .description(format!("the {name} group of a PCS cluster"))
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create the group {name}: {error:?}"))
        .group_id()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup: EC2 must report a GroupId for {name}"))
}

/// The DEFAULT security group of `vpc`, which the account holds without anybody creating it.
async fn default_security_group(ec2: &::aws_sdk_ec2::Client, vpc: &str) -> String {
    let of_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name("vpc-id")
        .values(vpc)
        .build();
    let named_default = ::aws_sdk_ec2::types::Filter::builder()
        .name("group-name")
        .values(DEFAULT_GROUP_NAME)
        .build();
    ec2.describe_security_groups()
        .filters(of_vpc)
        .filters(named_default)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must report the default group: {error:?}"))
        .security_groups()
        .iter()
        .find_map(|group| group.group_id().map(str::to_string))
        .unwrap_or_else(|| panic!("setup: expected {vpc} to hold a default security group"))
}

/// An id that is shaped like a security group id (`SecurityGroupId`'s `@pattern` is
/// `^sg-\w{8,17}$`, enforced at the boundary) but that the account can hold no group under:
/// EC2 issues resource ids whose label is lowercase HEX digits, so a label carrying a letter
/// outside that alphabet names no group EC2 could ever have minted. The `index` keeps each
/// row's unresolvable id its own.
fn outside_the_id_alphabet(index: usize) -> String {
    format!("sg-zzzzz{index:03}")
}

/// An id EC2 could have minted — `sg-` and lowercase hex digits — that this account was never
/// issued, so it resolves to no group either. The `index` keeps each row's id its own.
fn never_issued(index: usize) -> String {
    format!("sg-00000{index:03}")
}

/// A `CreateCluster` request placing the control plane in `subnets`, carrying `groups` as its
/// `networking.securityGroupIds` when there are any and omitting the member when there are
/// none.
fn create_cluster(name: &str, subnets: &[String], groups: Option<&[String]>) -> Value {
    let networking = match groups {
        Some(groups) => json!({"subnetIds": subnets, "securityGroupIds": groups}),
        None => json!({"subnetIds": subnets}),
    };
    json!({
        "clusterName": name,
        "scheduler": {"type": "SLURM", "version": "24.11"},
        "size": "SMALL",
        "networking": networking,
    })
}

#[tokio::test]
async fn create_cluster_resolves_every_security_group_it_is_given_against_ec2() {
    let (router, ec2) = emulator_with_ec2();
    let vpc = create_vpc(&ec2, VPC_CIDR).await;
    let subnet = create_subnet(&ec2, &vpc, "10.42.11.0/24").await;
    let subnets = vec![subnet];
    let held_one = create_security_group(&ec2, &vpc, "pcs-control-one").await;
    let held_two = create_security_group(&ec2, &vpc, "pcs-control-two").await;
    let by_default = default_security_group(&ec2, &vpc).await;

    // (case, the groups the create names, whether the membership resolves)
    let rows: Vec<(&str, Option<Vec<String>>, bool)> = vec![
        ("no securityGroupIds member at all", None, true),
        ("an empty securityGroupIds list", Some(Vec::new()), true),
        ("one group EC2 holds", Some(vec![held_one.clone()]), true),
        (
            "the default group of the VPC nobody created",
            Some(vec![by_default.clone()]),
            true,
        ),
        (
            "two groups EC2 holds",
            Some(vec![held_one.clone(), held_two.clone()]),
            true,
        ),
        (
            "one id outside the alphabet EC2 mints ids from",
            Some(vec![outside_the_id_alphabet(0)]),
            false,
        ),
        (
            "one well-formed id the account was never issued",
            Some(vec![never_issued(1)]),
            false,
        ),
        (
            "one held group and one unresolvable id",
            Some(vec![held_one.clone(), never_issued(2)]),
            false,
        ),
        (
            "an unresolvable id ahead of a held group",
            Some(vec![outside_the_id_alphabet(3), held_two.clone()]),
            false,
        ),
        (
            "two unresolvable ids",
            Some(vec![never_issued(4), outside_the_id_alphabet(5)]),
            false,
        ),
    ];

    let mut accepted = 0_usize;
    for (index, (case, groups, resolves)) in rows.iter().enumerate() {
        let name = format!("membership-{index}");
        let (status, body) = call(
            &router,
            "CreateCluster",
            &create_cluster(&name, &subnets, groups.as_deref()),
        )
        .await;
        if *resolves {
            accepted += 1;
            assert_eq!(
                status, 200,
                "case {case}: expected the create to be served, got {body}"
            );
            // The create answers from the stored record, so the membership reads back.
            let (status, read) =
                call(&router, "GetCluster", &json!({"clusterIdentifier": &name})).await;
            assert_eq!(status, 200, "case {case}: GetCluster got {read}");
            for reported in [&body, &read] {
                let membership = &reported["cluster"]["networking"]["securityGroupIds"];
                match groups.as_deref() {
                    Some(groups) if !groups.is_empty() => assert_eq!(
                        membership,
                        &json!(groups),
                        "case {case}: expected the groups to persist, got {reported}"
                    ),
                    _ => assert!(
                        membership.is_null() || membership == &json!([]),
                        "case {case}: expected no membership to be reported, got {reported}"
                    ),
                }
            }
        } else {
            assert_eq!(
                status, 400,
                "case {case}: expected the create to be refused, got {body}"
            );
            assert_eq!(
                body["__type"],
                json!("ValidationException"),
                "case {case}: got {body}"
            );
            assert_eq!(
                body["message"],
                json!(SECURITY_GROUPS_ABSENT),
                "case {case}: got {body}"
            );
            for id in groups.iter().flatten() {
                assert!(
                    !body["message"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(id.as_str()),
                    "case {case}: the sentence must name no security group id, got {body}"
                );
            }
        }
        let held = cluster_names(&router).await;
        assert_eq!(
            held.len(),
            accepted,
            "case {case}: expected only the served creates to have persisted, got {held:?}"
        );
    }
}

#[tokio::test]
async fn a_security_group_that_stops_existing_stops_resolving() {
    let (router, ec2) = emulator_with_ec2();
    let vpc = create_vpc(&ec2, VPC_CIDR).await;
    let subnet = create_subnet(&ec2, &vpc, "10.42.12.0/24").await;
    let subnets = vec![subnet];
    let group = create_security_group(&ec2, &vpc, "pcs-transient").await;
    let groups = vec![group.clone()];

    // Nothing is cached: while EC2 holds the group the create is served.
    let (status, served) = call(
        &router,
        "CreateCluster",
        &create_cluster("before-delete", &subnets, Some(&groups)),
    )
    .await;
    assert_eq!(status, 200, "case before the delete: got {served}");

    ec2.delete_security_group()
        .group_id(&group)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must delete the group: {error:?}"));

    let (status, refused) = call(
        &router,
        "CreateCluster",
        &create_cluster("after-delete", &subnets, Some(&groups)),
    )
    .await;
    assert_eq!(status, 400, "case after the delete: got {refused}");
    assert_eq!(
        refused["message"],
        json!(SECURITY_GROUPS_ABSENT),
        "case after the delete: got {refused}"
    );
    let held = cluster_names(&router).await;
    assert_eq!(
        held,
        vec!["before-delete".to_string()],
        "case after the delete: expected no second cluster, got {held:?}"
    );
}

#[tokio::test]
async fn an_unresolvable_group_is_refused_whichever_subnet_the_request_places_it_in() {
    // Both halves of the sentence are resolved before anything is minted, so a request that
    // names a group the account does not hold is refused even though its placement resolves —
    // and a request whose placement does NOT resolve either is refused as well, with one of
    // the two networking sentences rather than a success.
    let (router, ec2) = emulator_with_ec2();
    let vpc = create_vpc(&ec2, VPC_CIDR).await;
    let held_subnet = create_subnet(&ec2, &vpc, "10.42.13.0/24").await;
    let groups = vec![never_issued(0)];

    let (status, refused) = call(
        &router,
        "CreateCluster",
        &create_cluster("held-subnet", &[held_subnet], Some(&groups)),
    )
    .await;
    assert_eq!(status, 400, "case a held subnet: got {refused}");
    assert_eq!(
        refused["message"],
        json!(SECURITY_GROUPS_ABSENT),
        "case a held subnet: got {refused}"
    );

    let (status, refused) = call(
        &router,
        "CreateCluster",
        &create_cluster(
            "absent-subnet",
            &["subnet-zzzzz111".to_string()],
            Some(&groups),
        ),
    )
    .await;
    assert_eq!(status, 400, "case an unresolvable subnet: got {refused}");
    assert_eq!(
        refused["__type"],
        json!("ValidationException"),
        "case an unresolvable subnet: got {refused}"
    );

    let held = cluster_names(&router).await;
    assert!(
        held.is_empty(),
        "case neither request may persist a cluster, got {held:?}"
    );
}

#[tokio::test]
async fn an_emulator_with_no_ec2_to_ask_refuses_no_security_group() {
    // No EC2 is mounted here, so nothing is known about the account's security groups — and
    // a refusal is grounded in what EC2 answered, never assumed from silence.
    let router = emulator();
    let groups = vec![outside_the_id_alphabet(8), never_issued(9)];
    let (status, served) = call(
        &router,
        "CreateCluster",
        &create_cluster("no-peer", &["subnet-zzzzz222".to_string()], Some(&groups)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case no EC2 mounted: expected the create to be served, got {served}"
    );
    assert_eq!(
        served["cluster"]["networking"]["securityGroupIds"],
        json!(groups),
        "case no EC2 mounted: expected the groups to persist, got {served}"
    );
}
