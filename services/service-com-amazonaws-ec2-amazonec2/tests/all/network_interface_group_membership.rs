// SPDX-License-Identifier: CC0-1.0

//! Which security groups a network interface BELONGS TO, as `DescribeNetworkInterfaces`
//! reports them — end to end over the wire the service speaks (`ec2Query`, through the shared
//! [`super::ec2_query`] harness).
//!
//! The rules under test are:
//!
//! 1. "Any security groups for the network interface" (`NetworkInterface$Groups`): an
//!    interface reports, as `GroupIdentifier` entries carrying the group's id AND its name,
//!    exactly the groups its create named — one entry per distinct group, however many times
//!    the request named it — and the VPC's own default group when the create named none,
//!    "Amazon EC2 uses the default security group";
//! 2. so which groups of a VPC are ENI-ATTACHED is derived from that membership: the groups
//!    some interface of the account names are attached, and every other group of the VPC is
//!    not. The derivation follows the store — a later interface attaches the groups it names,
//!    and deleting an interface releases the groups only it named.
//!
//! The rows of each table are distinct inputs along what the rules generalise over: how many
//! groups a create names (none, one, two, three), whether the VPC's default group is among
//! them, whether a group is named twice, and how many interfaces the VPC holds. No row is the
//! specification.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, filters, member};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The CIDR block of the VPC each test creates, and the subnet inside it: the interfaces are
/// created in a VPC of this suite's own, so every group the derivation sees is one it created.
const VPC_CIDR: &str = "10.30.0.0/16";

/// The subnet the interfaces are created in.
const SUBNET_CIDR: &str = "10.30.1.0/24";

/// The name of the group "your VPC automatically comes with" (Amazon VPC User Guide, "Default
/// security groups for your VPCs"), which an interface created with no group of its own gets.
const DEFAULT_GROUP_NAME: &str = "default";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// Call `action`, require HTTP 200, and answer the body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Call `action`, require HTTP 200, and answer the first `id_element` of the response.
async fn reported_id(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    id_element: &str,
) -> String {
    let body = ok(router, case, action, params).await;
    element(&body, id_element)
        .unwrap_or_else(|| panic!("case {case:?}: expected a <{id_element}> in {body}"))
}

/// One VPC with one subnet in it, as `(vpcId, subnetId)`.
async fn a_vpc_with_a_subnet(router: &MultiServiceRouter) -> (String, String) {
    let vpc_id = reported_id(
        router,
        "the vpc",
        "CreateVpc",
        &[p("CidrBlock", VPC_CIDR)],
        "vpcId",
    )
    .await;
    let subnet_id = reported_id(
        router,
        "the subnet",
        "CreateSubnet",
        &[p("VpcId", &vpc_id), p("CidrBlock", SUBNET_CIDR)],
        "subnetId",
    )
    .await;
    (vpc_id, subnet_id)
}

/// One security group named `name` in `vpc_id`, as the group id it minted.
async fn a_group(router: &MultiServiceRouter, vpc_id: &str, name: &str) -> String {
    reported_id(
        router,
        name,
        "CreateSecurityGroup",
        &[
            p("VpcId", vpc_id),
            p("GroupName", name),
            p("GroupDescription", format!("the group {name}")),
        ],
        "groupId",
    )
    .await
}

/// The `(groupId, groupName)` of the default group of `vpc_id` — the group a create that names
/// none is expected to report.
async fn default_group(router: &MultiServiceRouter, vpc_id: &str) -> (String, String) {
    let body = ok(
        router,
        "the default group",
        "DescribeSecurityGroups",
        &filters(&[
            ("vpc-id", &[vpc_id]),
            ("group-name", &[DEFAULT_GROUP_NAME]),
        ]),
    )
    .await;
    let parsed = document("the default group", &body);
    let group = set_items(parsed.root_element(), "securityGroupInfo")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the VPC {vpc_id} must come with a default group: {body}"));
    let id = child_text(group, "groupId").unwrap_or_default();
    let name = child_text(group, "groupName").unwrap_or_default();
    assert_eq!(
        name, DEFAULT_GROUP_NAME,
        "the group the VPC comes with is named {DEFAULT_GROUP_NAME:?}, got {name:?}"
    );
    (id, name)
}

/// Every `groupId` the VPC `vpc_id` holds a security group for.
async fn groups_of_vpc(router: &MultiServiceRouter, vpc_id: &str) -> BTreeSet<String> {
    let body = ok(
        router,
        "the groups of the vpc",
        "DescribeSecurityGroups",
        &filters(&[("vpc-id", &[vpc_id])]),
    )
    .await;
    let parsed = document("the groups of the vpc", &body);
    set_items(parsed.root_element(), "securityGroupInfo")
        .into_iter()
        .filter_map(|group| child_text(group, "groupId"))
        .collect()
}

/// One `CreateNetworkInterface` in `subnet_id` naming `groups`, as the interface id it minted.
async fn an_interface(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    groups: &[&str],
) -> String {
    let mut params = vec![p("SubnetId", subnet_id)];
    for (index, group_id) in groups.iter().enumerate() {
        params.push(p(&format!("SecurityGroupId.{}", index + 1), *group_id));
    }
    reported_id(
        router,
        case,
        "CreateNetworkInterface",
        &params,
        "networkInterfaceId",
    )
    .await
}

/// The `(groupId, groupName)` pairs of the `groupSet` of the ONE interface
/// `DescribeNetworkInterfaces` reports for `network_interface_id`.
async fn reported_groups(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> BTreeSet<(String, String)> {
    let body = ok(
        router,
        case,
        "DescribeNetworkInterfaces",
        &[p("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref()
                    == Some(network_interface_id)
        })
        .unwrap_or_else(|| panic!("case {case:?}: no {network_interface_id} in {body}"));
    let set = child(interface, "groupSet")
        .unwrap_or_else(|| panic!("case {case:?}: {network_interface_id} reports no groupSet \
                                  in {body}"));
    set.children()
        .filter(roxmltree::Node::is_element)
        .map(|item| {
            (
                child_text(item, "groupId").unwrap_or_default(),
                child_text(item, "groupName").unwrap_or_default(),
            )
        })
        .collect()
}

/// Every group id any network interface of the account names, read the way an unused-group
/// audit reads it: `DescribeNetworkInterfaces` page by page, collecting each interface's
/// `Groups`.
async fn groups_attached_to_an_interface(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeSet<String> {
    let mut attached: BTreeSet<String> = BTreeSet::new();
    let mut token: Option<String> = None;
    loop {
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(next) = token.as_deref() {
            params.push(p("NextToken", next));
        }
        let body = ok(router, case, "DescribeNetworkInterfaces", &params).await;
        let parsed = document(case, &body);
        for interface in parsed.descendants().filter(|node| {
            node.is_element() && child_text(*node, "networkInterfaceId").is_some()
        }) {
            let Some(set) = child(interface, "groupSet") else {
                continue;
            };
            for item in set.children().filter(roxmltree::Node::is_element) {
                if let Some(group_id) = child_text(item, "groupId") {
                    attached.insert(group_id);
                }
            }
        }
        token = member(&body, "nextToken").filter(|next| !next.is_empty());
        if token.is_none() {
            return attached;
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1: an interface reports the groups it was created with
// ---------------------------------------------------------------------------

/// One row of the create table: which of the VPC's groups the create names (by index into the
/// groups the test created, with `None` standing for the VPC's default group), and which of
/// them the interface must report.
struct GroupRow {
    case: &'static str,
    /// The `SecurityGroupId.N` entries the create carries, in request order.
    named: &'static [Option<usize>],
    /// The distinct groups the interface must report, as the same indices.
    expected: &'static [Option<usize>],
}

/// Each interface reports exactly the groups its create named, with each group's own name, and
/// the VPC's default group when the create named none.
#[tokio::test]
async fn an_interface_reports_the_security_groups_it_was_created_with() {
    let rows: [GroupRow; 6] = [
        GroupRow {
            case: "no group named at all",
            named: &[],
            expected: &[None],
        },
        GroupRow {
            case: "one group",
            named: &[Some(0)],
            expected: &[Some(0)],
        },
        GroupRow {
            case: "two groups",
            named: &[Some(0), Some(1)],
            expected: &[Some(0), Some(1)],
        },
        GroupRow {
            case: "the same group named twice",
            named: &[Some(1), Some(1)],
            expected: &[Some(1)],
        },
        GroupRow {
            case: "the VPC's default group named explicitly, beside two others",
            named: &[None, Some(1), Some(2)],
            expected: &[None, Some(1), Some(2)],
        },
        GroupRow {
            case: "three groups, in an order of their own",
            named: &[Some(2), Some(0), Some(1)],
            expected: &[Some(0), Some(1), Some(2)],
        },
    ];

    let router = emulator();
    let (vpc_id, subnet_id) = a_vpc_with_a_subnet(&router).await;
    let default = default_group(&router, &vpc_id).await;
    let mut created: Vec<(String, String)> = Vec::new();
    for name in ["web", "app", "data"] {
        created.push((a_group(&router, &vpc_id, name).await, name.to_string()));
    }
    // `(group id, group name)` for a row's index: `None` is the VPC's default group.
    let pair = |index: Option<usize>| -> (String, String) {
        match index {
            None => default.clone(),
            Some(index) => created[index].clone(),
        }
    };

    for row in rows {
        let case = row.case;
        let named: Vec<String> = row.named.iter().map(|index| pair(*index).0).collect();
        let named_refs: Vec<&str> = named.iter().map(String::as_str).collect();
        let interface_id = an_interface(&router, case, &subnet_id, &named_refs).await;

        let expected: BTreeSet<(String, String)> =
            row.expected.iter().map(|index| pair(*index)).collect();
        let reported = reported_groups(&router, case, &interface_id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: {interface_id} must report {expected:?}, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the attached set is derived from that membership
// ---------------------------------------------------------------------------

/// The groups some interface names are the ENI-ATTACHED ones, and every other group of the VPC
/// is unattached — and that derivation follows the store as interfaces come and go.
#[tokio::test]
async fn the_unattached_groups_of_a_vpc_are_the_ones_no_interface_names() {
    let router = emulator();
    let (vpc_id, subnet_id) = a_vpc_with_a_subnet(&router).await;
    let default = default_group(&router, &vpc_id).await.0;
    let mut created: Vec<String> = Vec::new();
    for name in ["front", "middle", "back", "batch", "audit"] {
        created.push(a_group(&router, &vpc_id, name).await);
    }
    let all_groups = groups_of_vpc(&router, &vpc_id).await;
    let mut expected_all: BTreeSet<String> = created.iter().cloned().collect();
    expected_all.insert(default.clone());
    assert_eq!(
        all_groups, expected_all,
        "the VPC must hold its default group and the five created ones, got {all_groups:?}"
    );

    /// What the audit asks of one round: which groups of the VPC no interface names.
    async fn unattached(
        router: &MultiServiceRouter,
        case: &str,
        all_groups: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let attached = groups_attached_to_an_interface(router, case).await;
        all_groups.difference(&attached).cloned().collect()
    }

    // Before any interface exists, every group of the VPC is unattached.
    let case = "no interface yet";
    let reported = unattached(&router, case, &all_groups).await;
    assert_eq!(
        reported, all_groups,
        "case {case:?}: every group must be unattached, got {reported:?}"
    );

    // One interface created with ONE group: that group is the only one excluded.
    let case = "one interface naming one group";
    let first = an_interface(&router, case, &subnet_id, &[created[1].as_str()]).await;
    let reported = unattached(&router, case, &all_groups).await;
    let expected: BTreeSet<String> = all_groups
        .iter()
        .filter(|group| *group != &created[1])
        .cloned()
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: {} must be the only group excluded, got {reported:?}",
        created[1]
    );

    // A second interface naming two more groups attaches exactly those two as well.
    let case = "a second interface naming two more groups";
    let second = an_interface(
        &router,
        case,
        &subnet_id,
        &[created[3].as_str(), created[4].as_str()],
    )
    .await;
    let reported = unattached(&router, case, &all_groups).await;
    let expected: BTreeSet<String> = all_groups
        .iter()
        .filter(|group| ![&created[1], &created[3], &created[4]].contains(group))
        .cloned()
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: only the groups no interface names stay unattached, got {reported:?}"
    );

    // Deleting the first interface releases the group only it named, and leaves the second
    // interface's groups attached: the derivation is read off the store, not off the creates.
    let case = "the first interface deleted";
    ok(
        &router,
        case,
        "DeleteNetworkInterface",
        &[p("NetworkInterfaceId", &first)],
    )
    .await;
    let reported = unattached(&router, case, &all_groups).await;
    let expected: BTreeSet<String> = all_groups
        .iter()
        .filter(|group| ![&created[3], &created[4]].contains(group))
        .cloned()
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: the deleted interface's group must be unattached again, got \
         {reported:?}"
    );

    // And the interface that remains still reports the two groups it was created with.
    let still = reported_groups(&router, case, &second).await;
    let still_ids: BTreeSet<String> = still.into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        still_ids,
        BTreeSet::from([created[3].clone(), created[4].clone()]),
        "case {case:?}: {second} must still report the groups it was created with, got \
         {still_ids:?}"
    );
}
