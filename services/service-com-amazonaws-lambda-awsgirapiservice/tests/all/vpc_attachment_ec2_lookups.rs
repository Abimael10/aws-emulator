// SPDX-License-Identifier: CC0-1.0

//! Looking a VPC-attached function's network interfaces up in the ACCOUNT's own Amazon EC2
//! inventory, the way a client that holds only the VPC resources can.
//!
//! RULE: "To connect a function to a VPC, Lambda creates an elastic network interface for each
//! subnet in your function's VPC configuration" — and those interfaces are ordinary members of
//! the account's EC2 inventory, so `DescribeNetworkInterfaces` finds them through the filters it
//! documents: a filter naming an attribute the attachment SET selects exactly the function's
//! interfaces, and one naming a value the attachment did NOT set selects none of them. Nothing
//! about the function has to be known to find them: the security group the configuration named
//! is enough, and it finds only the interfaces of the function that named it.
//!
//! Every VPC, subnet and security group here is minted through Amazon EC2's own
//! `CreateVpc`/`CreateSubnet`/`CreateSecurityGroup`, and every interface is read back through its
//! own `DescribeNetworkInterfaces` — two services on the ONE engine the router mounts them on —
//! so nothing passes off a record of its own as one the account holds.
//!
//! The rows differ along what the rule generalises over: WHICH documented filter the lookup uses
//! (the security group, the subnet, the VPC, the interface type, the description, the requester,
//! the status, the attachment, the owner, the interface's own id, its address, its MAC and its
//! Availability Zone), and whether the value the filter names is one the attachment set.

use std::collections::BTreeSet;

use aws_sdk_ec2::error::ProvideErrorMetadata as _;
use aws_sdk_ec2::types::{Filter, NetworkInterface};
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{json_call, ACCOUNT};
use crate::vpc_attachment_interfaces::{
    create_attached, emulator, function_interfaces, interface_description, network,
    LAMBDA_INTERFACE_TYPE, LAMBDA_SERVICE_PRINCIPAL,
};
use crate::vpc_attachment_role_grant::another_group;

/// The owner `NetworkInterfaceAttachment$InstanceOwnerId` names for an attachment AWS itself
/// made, which is what holds an interface no instance of the account does.
const SERVICE_ATTACHMENT_OWNER: &str = "amazon-aws";

/// "If the network interface is not attached to an instance, the status is `available`; if a
/// network interface is attached to an instance the status is `in-use`"
/// (`DescribeNetworkInterfacesRequest$Filters`, the `status` filter): an interface the function's
/// attachment holds is held, so it is `in-use`.
const STATUS_IN_USE: &str = "in-use";

/// The status of an interface nothing holds, which no interface of an attached function is.
const STATUS_AVAILABLE: &str = "available";

/// The attachment status of an interface that is attached (`attachment.status`, one of
/// `attaching` | `attached` | `detaching` | `detached`).
const ATTACHMENT_ATTACHED: &str = "attached";

/// A form-valid instance id no account ever minted: the value an `attachment.instance-id` lookup
/// names, which a service-made attachment carries none of.
const UNKNOWN_INSTANCE: &str = "i-0123456789abcdef0";

/// Create the function `name` attached to `subnet_ids` with `group_ids`, and answer the
/// interfaces the attachment stood up, read back through EC2.
async fn given_attached_function(
    emulator: &crate::vpc_attachment_interfaces::Emulator,
    case: &str,
    name: &str,
    subnet_ids: &[String],
    group_ids: &[String],
) -> Vec<NetworkInterface> {
    let answer = create_attached(&emulator.router, name, subnet_ids, group_ids).await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let interfaces = function_interfaces(&emulator.ec2, name).await;
    assert_eq!(
        interfaces.len(),
        subnet_ids.len(),
        "case {case:?}: expected one interface per named subnet ({}), got {interfaces:?}",
        subnet_ids.len()
    );
    interfaces
}

/// `DeleteFunction` on the function `name`.
async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let answer = json_call(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        204,
        "case {case:?}: expected HTTP 204 from DeleteFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// The ids of `interfaces`, as a set compares them.
fn ids(interfaces: &[NetworkInterface]) -> BTreeSet<String> {
    interfaces
        .iter()
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect()
}

/// Every interface id the account's inventory holds right now, read with no filter at all — the
/// baseline a lookup's answer is measured against, so an interface that was there before any
/// function was created can never be mistaken for one an attachment stood up.
async fn every_interface_id(ec2: &aws_sdk_ec2::Client) -> BTreeSet<String> {
    let described = ec2
        .describe_network_interfaces()
        .send()
        .await
        .expect("EC2 must report the account's network interfaces");
    ids(described.network_interfaces())
}

/// The ids `DescribeNetworkInterfaces` answers for the filter `name` over `values`, with every
/// interface the baseline already held taken out.
async fn looked_up(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    filters: &[(&str, Vec<String>)],
    baseline: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut request = ec2.describe_network_interfaces();
    for (name, values) in filters {
        let mut filter = Filter::builder().name(*name);
        for value in values {
            filter = filter.values(value);
        }
        request = request.filters(filter.build());
    }
    let described = request.send().await.unwrap_or_else(|error| {
        panic!("case {case:?}: DescribeNetworkInterfaces must answer the lookup: {error:?}")
    });
    ids(described.network_interfaces())
        .into_iter()
        .filter(|id| !baseline.contains(id))
        .collect()
}

/// One attribute of an interface, read off the interface rather than out of the lookup being
/// checked.
fn attribute(case: &str, what: &str, value: Option<&str>) -> String {
    value
        .unwrap_or_else(|| panic!("case {case:?}: expected the interface to report its {what}"))
        .to_string()
}

/// One lookup: the name of a documented filter and the values it names.
type Lookup = (&'static str, Vec<String>);

/// One row of the lookup table: what the lookup asks, the filters it sends (several are an
/// AND), and the interfaces of the attachment it must find.
type Row = (&'static str, Vec<Lookup>, BTreeSet<String>);

/// RULE: every documented `DescribeNetworkInterfaces` filter that names an attribute the
/// attachment set finds the function's interfaces, and one naming a value it did not set finds
/// none of them.
///
/// The filter names are the ones the operation documents for its `Filters` member
/// (`smithy_query(query="doc",shape="DescribeNetworkInterfacesRequest",member="Filters")`); the
/// values are read off the attachment the create stood up, or are resources of the account that
/// no configuration named.
#[tokio::test]
async fn every_documented_lookup_over_an_attachment_attribute_finds_the_functions_interfaces() {
    let case = "two attached subnets, one carried group";
    let emulator = emulator();
    // Three subnets: the configuration names the first two, so the third is a subnet of the
    // same VPC that no attachment ever placed an interface in.
    let held = network(&emulator.ec2, case, 81, 3).await;
    let spare_subnet = held.subnet_ids[2].clone();
    let attached_subnets = held.subnet_ids[..2].to_vec();
    // A sibling group of the SAME VPC that no function carries, and a whole second VPC.
    let spare_group = another_group(&emulator.ec2, case, &held.vpc_id, 9).await;
    let elsewhere = network(&emulator.ec2, case, 82, 1).await;

    let baseline = every_interface_id(&emulator.ec2).await;
    let name = "ec2-lookup-probe";
    let interfaces = given_attached_function(
        &emulator,
        case,
        name,
        &attached_subnets,
        std::slice::from_ref(&held.group_id),
    )
    .await;
    let both = ids(&interfaces);
    let first = &interfaces[0];
    let first_id = attribute(case, "id", first.network_interface_id());
    let first_only: BTreeSet<String> = std::iter::once(first_id.clone()).collect();
    let first_subnet = attribute(case, "subnet", first.subnet_id());
    let in_first_subnet: BTreeSet<String> = interfaces
        .iter()
        .filter(|interface| interface.subnet_id() == Some(first_subnet.as_str()))
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();
    let first_zone = attribute(case, "Availability Zone", first.availability_zone());
    let in_first_zone: BTreeSet<String> = interfaces
        .iter()
        .filter(|interface| interface.availability_zone() == Some(first_zone.as_str()))
        .filter_map(|interface| interface.network_interface_id().map(str::to_string))
        .collect();
    let none: BTreeSet<String> = BTreeSet::new();

    // (what the lookup asks, the filter and its values, the interfaces it must find)
    let rows: Vec<Row> = vec![
        // The lookup a client makes knowing only the security group it configured, which is the
        // one the function's VPC attachment is found by.
        (
            "the security group the configuration named",
            vec![("group-id", vec![held.group_id.clone()])],
            both.clone(),
        ),
        (
            "a sibling group of the same VPC that no function carries",
            vec![("group-id", vec![spare_group.clone()])],
            none.clone(),
        ),
        (
            "one of the subnets the configuration named",
            vec![("subnet-id", vec![first_subnet.clone()])],
            in_first_subnet,
        ),
        (
            "a subnet of the same VPC that no configuration named",
            vec![("subnet-id", vec![spare_subnet.clone()])],
            none.clone(),
        ),
        (
            "the VPC those subnets belong to",
            vec![("vpc-id", vec![held.vpc_id.clone()])],
            both.clone(),
        ),
        (
            "another VPC of the account",
            vec![("vpc-id", vec![elsewhere.vpc_id.clone()])],
            none.clone(),
        ),
        (
            "the interface type a function's interfaces are of",
            vec![("interface-type", vec![LAMBDA_INTERFACE_TYPE.to_string()])],
            both.clone(),
        ),
        (
            "the description naming the function",
            vec![("description", vec![interface_description(name)])],
            both.clone(),
        ),
        (
            "the description naming a function nothing created",
            vec![("description", vec![interface_description("absent-fn")])],
            none.clone(),
        ),
        (
            "the principal that created them",
            vec![("requester-id", vec![LAMBDA_SERVICE_PRINCIPAL.to_string()])],
            both.clone(),
        ),
        (
            "interfaces no AWS service manages, which these are",
            vec![("requester-managed", vec!["false".to_string()])],
            both.clone(),
        ),
        (
            "interfaces an AWS service manages, which these are not",
            vec![("requester-managed", vec!["true".to_string()])],
            none.clone(),
        ),
        (
            "the status of an interface something holds",
            vec![("status", vec![STATUS_IN_USE.to_string()])],
            both.clone(),
        ),
        (
            "the status of an interface nothing holds",
            vec![("status", vec![STATUS_AVAILABLE.to_string()])],
            none.clone(),
        ),
        (
            "the attachment status of an attached interface",
            vec![("attachment.status", vec![ATTACHMENT_ATTACHED.to_string()])],
            both.clone(),
        ),
        (
            "the owner of an attachment AWS itself made",
            vec![(
                "attachment.instance-owner-id",
                vec![SERVICE_ATTACHMENT_OWNER.to_string()],
            )],
            both.clone(),
        ),
        (
            "an instance id, which a service-made attachment carries none of",
            vec![("attachment.instance-id", vec![UNKNOWN_INSTANCE.to_string()])],
            none.clone(),
        ),
        (
            "the account that owns them",
            vec![("owner-id", vec![ACCOUNT.to_string()])],
            both.clone(),
        ),
        (
            "one interface's own id",
            vec![("network-interface-id", vec![first_id])],
            first_only.clone(),
        ),
        (
            "the address one interface holds in its subnet",
            vec![(
                "private-ip-address",
                vec![attribute(case, "address", first.private_ip_address())],
            )],
            first_only.clone(),
        ),
        (
            "one interface's MAC address",
            vec![(
                "mac-address",
                vec![attribute(case, "MAC address", first.mac_address())],
            )],
            first_only,
        ),
        (
            "the Availability Zone one interface sits in",
            vec![("availability-zone", vec![first_zone])],
            in_first_zone,
        ),
        // Two filters are an AND, so a group the function carries together with a subnet it was
        // never placed in selects nothing.
        (
            "the carried group AND a subnet no configuration named",
            vec![
                ("group-id", vec![held.group_id.clone()]),
                ("subnet-id", vec![spare_subnet]),
            ],
            none.clone(),
        ),
        // Several values of ONE filter are an OR, so naming the carried group beside a group
        // nothing carries still finds the whole attachment.
        (
            "the carried group OR a group no function carries",
            vec![("group-id", vec![held.group_id.clone(), spare_group])],
            both.clone(),
        ),
    ];

    for (asked, filters, expected) in rows {
        let found = looked_up(&emulator.ec2, asked, &filters, &baseline).await;
        assert_eq!(
            found, expected,
            "case {case:?}, looking up {asked:?} with {filters:?}: expected {expected:?}, got \
             {found:?}"
        );
    }
}

/// RULE (the complement): a security-group lookup finds the interfaces of the function that
/// named THAT group and no other, and it stops finding them when the function that held them is
/// deleted — which is also when the group it carried becomes deletable.
#[tokio::test]
async fn a_group_lookup_finds_only_the_function_that_carries_that_group() {
    let case = "two functions in one VPC, each carrying its own group";
    let emulator = emulator();
    let held = network(&emulator.ec2, case, 83, 2).await;
    let other_group = another_group(&emulator.ec2, case, &held.vpc_id, 1).await;
    let baseline = every_interface_id(&emulator.ec2).await;

    let one = "group-lookup-one";
    let one_interfaces = given_attached_function(
        &emulator,
        case,
        one,
        &held.subnet_ids[..1],
        std::slice::from_ref(&held.group_id),
    )
    .await;
    let two = "group-lookup-two";
    let two_interfaces = given_attached_function(
        &emulator,
        case,
        two,
        &held.subnet_ids,
        std::slice::from_ref(&other_group),
    )
    .await;

    let first = ids(&one_interfaces);
    let second = ids(&two_interfaces);
    let everything: BTreeSet<String> = first.union(&second).cloned().collect();
    assert_eq!(
        everything.len(),
        first.len() + second.len(),
        "case {case:?}: the two functions must hold interfaces of their own, got {first:?} and \
         {second:?}"
    );

    for (asked, group, expected) in [
        ("the group only the first function carries", &held.group_id, &first),
        ("the group only the second function carries", &other_group, &second),
    ] {
        let found = looked_up(
            &emulator.ec2,
            asked,
            &[("group-id", vec![group.clone()])],
            &baseline,
        )
        .await;
        assert_eq!(
            found, *expected,
            "case {case:?}, looking up {asked:?}: expected {expected:?}, got {found:?}"
        );
    }
    let found = looked_up(
        &emulator.ec2,
        "both groups at once",
        &[(
            "group-id",
            vec![held.group_id.clone(), other_group.clone()],
        )],
        &baseline,
    )
    .await;
    assert_eq!(
        found, everything,
        "case {case:?}: a lookup naming both groups must find every interface, expected \
         {everything:?}, got {found:?}"
    );

    // While the second function holds them, the group it carries holds a dependent object.
    let error = emulator
        .ec2
        .delete_security_group()
        .group_id(&other_group)
        .send()
        .await
        .expect_err("the group an attached function's interface carries must not delete");
    assert_eq!(
        error.code(),
        Some("DependencyViolation"),
        "case {case:?}: expected DependencyViolation from DeleteSecurityGroup, got {error:?}"
    );

    // Deleting that function gives its interfaces back: the lookup on its group finds nothing,
    // the group becomes deletable, and the OTHER function's interfaces are untouched.
    delete_function(&emulator.router, case, two).await;
    let found = looked_up(
        &emulator.ec2,
        "the deleted function's group",
        &[("group-id", vec![other_group.clone()])],
        &baseline,
    )
    .await;
    assert!(
        found.is_empty(),
        "case {case:?}: the interfaces of a deleted function must not outlive it, got {found:?}"
    );
    emulator
        .ec2
        .delete_security_group()
        .group_id(&other_group)
        .send()
        .await
        .expect("the group must delete once the function that carried it is gone");
    let found = looked_up(
        &emulator.ec2,
        "the surviving function's group",
        &[("group-id", vec![held.group_id.clone()])],
        &baseline,
    )
    .await;
    assert_eq!(
        found, first,
        "case {case:?}: deleting one function must leave the other's interfaces, expected \
         {first:?}, got {found:?}"
    );
}
