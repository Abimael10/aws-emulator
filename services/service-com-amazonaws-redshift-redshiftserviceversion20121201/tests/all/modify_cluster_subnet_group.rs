// SPDX-License-Identifier: CC0-1.0

//! `ModifyClusterSubnetGroup` REPLACES the subnet list of an existing group, end
//! to end over the awsQuery wire.
//!
//! The RULE every case here asserts: a modify of a cluster subnet group stores
//! EXACTLY the subnets the request names — "the operation replaces the existing
//! list of subnets with the new list of subnets" — resolved against the service
//! that owns subnets, leaves every member the request does not carry as it was
//! (the stored name, the tags, and the description, which is optional), and when
//! the list it carries is not one a group may hold — absent, longer than the
//! documented maximum, spanning two VPCs, or naming an identifier the account has
//! no subnet for — the call is REFUSED and the group keeps the list it already
//! had.
//!
//! The rows differ along what the rule generalises over: how many subnets the new
//! list has (one, several, the documented maximum, one past it), whether they are
//! the group's own VPC's or another VPC's, the ORDER they are listed in, whether
//! the same list is applied twice, the case the request spells the group's name
//! in, and — for the refusals — which bound the list falls outside of and where
//! the offending identifier sits.
//!
//! Nothing here reaches into either store: every subnet exists only because EC2
//! minted it and answered for it, and the group's list is read back through
//! Redshift's own `DescribeClusterSubnetGroups`, so a modify that stored nothing
//! cannot pass.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};
use crate::cluster_subnet_group_network::{
    assert_network, assert_refused, call, describe_group, modify_group, placements,
};

/// An identifier of the `subnet-` form EC2 mints no subnet for, so it reaches the
/// handler's resolution and resolves to nothing.
const ABSENT_SUBNET: &str = "subnet-0dddddddddddddddd";

/// The sentences Amazon Redshift words these refusals with, spelled out here so a
/// change to the fixed TEXT fails a test rather than passing one computed from the
/// same code under test. The variable part of the quota sentence is the allowance
/// itself, which the model publishes as "A maximum of 20 subnets can be modified
/// in a single request" (`ModifyClusterSubnetGroupMessage$SubnetIds`).
///
/// `SubnetIds` is a REQUIRED member of the modify's input
/// (`smithy_query(query="operation", operation="ModifyClusterSubnetGroup")`), and
/// an awsQuery request that lists no entry carries the member not at all, so a
/// request naming no subnet earns the required member's own refusal, in the
/// wording this protocol words a missing member with — the framework answers it
/// before the handler runs, so the handler's own "the list names no subnet"
/// branch is not what this row covers and is unreachable over this wire.
const NO_SUBNET_LIST_REFUSAL: &str = "1 validation error detected: Value null at 'subnetIds' \
     failed to satisfy constraint: Member must not be null";
const TWO_VPCS_REFUSAL: &str =
    "The requested subnet is not valid, or not all of the subnets are in the same VPC.";
const QUOTA_REFUSAL: &str =
    "The request would exceed the allowed number of subnets in a cluster subnet group (20).";

/// The sentence an identifier the account has no subnet for earns, spelled out
/// with ONE hole for the offending identifiers the request listed — comma-joined,
/// in request order — so the fixed text is a requirement here rather than whatever
/// the service's own formatter renders.
fn invalid_subnets_refusal(offending: &[String]) -> String {
    format!("Some input subnets in :[{}] are invalid.", offending.join(","))
}

/// The documented maximum number of subnets one request may name
/// (`ModifyClusterSubnetGroupMessage$SubnetIds`: "A maximum of 20 subnets can be
/// modified in a single request"), named from the service's own constant so the
/// published bound has ONE home.
const MAXIMUM_SUBNETS: usize = state::SUBNETS_PER_SUBNET_GROUP_QUOTA;

/// `CreateClusterSubnetGroup` over `subnet_ids`, carrying one tag — a member the
/// modify never names, so a later read shows whether the modify kept it.
async fn create_tagged_group(
    emulator: &Emulator,
    name: &str,
    description: &str,
    subnet_ids: &[String],
    tag: (&str, &str),
) -> Answer {
    let mut params: Vec<(String, String)> = vec![
        ("ClusterSubnetGroupName".to_string(), name.to_string()),
        ("Description".to_string(), description.to_string()),
        ("Tags.Tag.1.Key".to_string(), tag.0.to_string()),
        ("Tags.Tag.1.Value".to_string(), tag.1.to_string()),
    ];
    for (index, subnet_id) in subnet_ids.iter().enumerate() {
        params.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet_id.clone(),
        ));
    }
    call(emulator, "CreateClusterSubnetGroup", &params).await
}

/// Assert `answer` reports the one tag the create stored: the modify names no tag,
/// so the group's tags are the ones it already carried.
fn assert_keeps_tag(case: &str, answer: &Answer, tag: (&str, &str)) {
    assert_eq!(
        all_text(&answer.body, "Key"),
        vec![tag.0.to_string()],
        "case {case}: expected the tag key {:?} the group already carried, got body {}",
        tag.0,
        answer.body
    );
    assert_eq!(
        all_text(&answer.body, "Value"),
        vec![tag.1.to_string()],
        "case {case}: expected the tag value {:?} the group already carried, got body {}",
        tag.1,
        answer.body
    );
}

/// A modify stores exactly the list it names, at every size a group may hold and
/// in the order the request listed it — and a later read reports the same.
#[tokio::test]
async fn a_modify_stores_exactly_the_subnet_list_it_names() {
    let emulator = Emulator::new();
    // One VPC with the documented maximum number of subnets, so a row can name
    // any number of them up to that bound, and a second VPC the group moves into
    // whole.
    let own = emulator.subnet_ids("the group's own VPC", MAXIMUM_SUBNETS).await;
    let elsewhere = emulator
        .subnet_ids("another VPC the group moves into", 3)
        .await;

    let tag = ("owner", "finance");
    let created = create_tagged_group(
        &emulator,
        "replaced-subnets",
        "the group's first description",
        &own[..2],
        tag,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case seed the group: expected 200, got {} with body {}",
        created.status, created.body
    );
    assert_network(
        "seed the group",
        &created,
        &own[..2],
        &placements(&emulator, "the group's first two subnets", &own[..2]).await,
    );

    let reversed: Vec<String> = own[..3].iter().rev().cloned().collect();
    // (case, the name the request spells, the new list, the description it carries)
    let rows: Vec<(&str, &str, Vec<String>, Option<&str>)> = vec![
        (
            "grown to the documented maximum number of subnets",
            "replaced-subnets",
            own.clone(),
            Some("every subnet of the group's own VPC"),
        ),
        (
            "shrunk to one subnet of the same VPC",
            "replaced-subnets",
            own[..1].to_vec(),
            None,
        ),
        (
            // "Amazon Redshift stores the value as a lowercase string", so a
            // request spelling the name in another case names the SAME group.
            "three subnets in the reverse order, named in upper case",
            "REPLACED-SUBNETS",
            reversed.clone(),
            Some("the group's second description"),
        ),
        (
            "the very same list applied a second time",
            "replaced-subnets",
            reversed.clone(),
            None,
        ),
        (
            "moved into another VPC entirely",
            "Replaced-Subnets",
            elsewhere.clone(),
            None,
        ),
    ];

    let mut description = created
        .text("Description")
        .expect("the create reports the description it stored");
    for (case, requested, subnet_ids, requested_description) in rows {
        let placed = placements(&emulator, case, &subnet_ids).await;
        let modified = modify_group(&emulator, requested, requested_description, &subnet_ids).await;
        assert_eq!(
            modified.status, 200,
            "case {case}: expected 200, got {} with body {}",
            modified.status, modified.body
        );
        assert_network(case, &modified, &subnet_ids, &placed);
        assert_keeps_tag(case, &modified, tag);

        // "A text description of the subnet group to be modified" is optional, so
        // a request that carries none leaves the one the group had.
        description = requested_description
            .map(str::to_string)
            .unwrap_or(description);
        assert_eq!(
            modified.text("Description").as_deref(),
            Some(description.as_str()),
            "case {case}: expected the description {description:?}, got {:?}",
            modified.text("Description")
        );
        // The stored name is the folded one, whatever case the request spelled.
        assert_eq!(
            modified.text("ClusterSubnetGroupName").as_deref(),
            Some("replaced-subnets"),
            "case {case}: expected the group's stored name, got {:?}",
            modified.text("ClusterSubnetGroupName")
        );

        let described = describe_group(&emulator, "replaced-subnets").await;
        assert_eq!(
            described.status, 200,
            "case {case}, read back: expected 200, got {} with body {}",
            described.status, described.body
        );
        let read_back = format!("{case}, read back");
        assert_network(&read_back, &described, &subnet_ids, &placed);
        assert_keeps_tag(&read_back, &described, tag);
        assert_eq!(
            described.text("Description").as_deref(),
            Some(description.as_str()),
            "case {read_back}: expected the description {description:?}, got {:?}",
            described.text("Description")
        );
    }
}

/// A list no group may hold is refused, and the group keeps the subnets — and the
/// description — it already had: a refused modify changes nothing.
#[tokio::test]
async fn a_list_outside_the_bounds_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let held = emulator.subnet_ids("the group's own network", 2).await;
    // Every identifier of this VPC is one EC2 minted, so the row one past the
    // documented maximum is refused on its LENGTH and not on an identifier
    // nobody holds.
    let past_the_maximum = emulator
        .subnet_ids("one subnet past the documented maximum", MAXIMUM_SUBNETS + 1)
        .await;
    let elsewhere = emulator.subnet_ids("a subnet of another VPC", 1).await;
    let deleted = emulator.subnet_ids("a subnet minted and deleted", 1).await;
    emulator
        .ec2()
        .delete_subnet()
        .subnet_id(&deleted[0])
        .send()
        .await
        .expect("EC2 must delete the subnet it minted");

    let seed_description = "the description the refusals must not change";
    let tag = ("env", "prod");
    let created =
        create_tagged_group(&emulator, "kept-subnets", seed_description, &held, tag).await;
    assert_eq!(
        created.status, 200,
        "case seed the group: expected 200, got {} with body {}",
        created.status, created.body
    );
    let placed = placements(&emulator, "the group's own network", &held).await;

    // A list at the documented maximum whose identifiers belong to two VPCs: the
    // length is admissible, the network is not.
    let mut two_vpcs: Vec<String> = past_the_maximum[..MAXIMUM_SUBNETS - 1].to_vec();
    two_vpcs.push(elsewhere[0].clone());

    // (case, the list the request carries, the `<Code>` it earns, its sentence)
    let rows: Vec<(&str, Vec<String>, &str, String)> = vec![
        (
            "a request listing no subnet at all",
            Vec::new(),
            "ValidationError",
            NO_SUBNET_LIST_REFUSAL.to_string(),
        ),
        (
            "one subnet past the documented maximum, every identifier real",
            past_the_maximum.clone(),
            "ClusterSubnetQuotaExceededFault",
            QUOTA_REFUSAL.to_string(),
        ),
        (
            "at the documented maximum, but spanning two VPCs",
            two_vpcs,
            "InvalidSubnet",
            TWO_VPCS_REFUSAL.to_string(),
        ),
        (
            "an identifier the account has no subnet for, between real ones",
            vec![held[0].clone(), ABSENT_SUBNET.to_string(), held[1].clone()],
            "InvalidParameterValue",
            invalid_subnets_refusal(&[ABSENT_SUBNET.to_string()]),
        ),
        (
            "a subnet EC2 minted and then deleted",
            vec![deleted[0].clone()],
            "InvalidParameterValue",
            invalid_subnets_refusal(&[deleted[0].clone()]),
        ),
    ];

    for (case, subnet_ids, code, message) in rows {
        let refused = modify_group(
            &emulator,
            "kept-subnets",
            Some("a description a refused modify must not store"),
            &subnet_ids,
        )
        .await;
        assert_refused(case, &refused, 400, code);
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(message.as_str()),
            "case {case}: expected the refusal {message:?}, got {:?}",
            refused.text("Message")
        );

        // The group is exactly as the create left it: the refused list is not
        // half-stored, and the description the refused request carried is not
        // stored either.
        let described = describe_group(&emulator, "kept-subnets").await;
        assert_eq!(
            described.status, 200,
            "case {case}, read back: expected the group still readable, got {} with body {}",
            described.status, described.body
        );
        let read_back = format!("{case}, read back");
        assert_network(&read_back, &described, &held, &placed);
        assert_keeps_tag(&read_back, &described, tag);
        assert_eq!(
            described.text("Description").as_deref(),
            Some(seed_description),
            "case {read_back}: expected the description {seed_description:?}, got {:?}",
            described.text("Description")
        );
    }
}
