// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a refusal carries on the network-interface, security-group and VPC create
//! paths — driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! ONE rule, asserted class by class: a refusal's message is the sentence EC2 words that
//! ERROR CLASS with — its fixed text exactly, and its variable parts taken from the request
//! and from this account's state — so a message is never a restatement in the emulator's own
//! words and never carries a value EC2's sentence does not. Each test drives one class over a
//! TABLE whose rows differ along what that class's sentence generalises over (which
//! operation, which member spelling, which resource, which value), so no single row is the
//! specification:
//!
//! 1. A page size beside an explicit id selection: the sentence names the SELECTION first
//!    and the page size second, over seven paged describes and seven id kinds.
//! 2. An attach at a device index the instance already uses: fixed text with the instance id
//!    and the index from the request, over two instances and two indexes.
//! 3. A detach of an instance's primary interface: fixed text with the attachment's two
//!    indexes, `networkCard` spelled as EC2 spells it, forced or not.
//! 4. An IPv6 assignment out of a subnet that has no IPv6 range: fixed text end to end, over
//!    two subnets, two operations and the ways an IPv6 ask is spelled — so no subnet id can
//!    appear in it.
//! 5. A create of an interface on a private address something already holds: fixed text end
//!    to end, over two addresses and an address named twice in one request.
//! 6. A delete of a VPC's default security group: fixed text with the group's id AND name,
//!    both read off the stored group, whichever way the request named it and in whichever
//!    VPC the group lives.
//! 7. A VPC create that names neither a block nor a pool: fixed text naming both wire
//!    members, whatever else the request carries.
//! 8. A rule whose port range ends before it starts: `InvalidParameterValue` with the range
//!    as the request sent it, on both sides of a group and in both spellings of a rule.
//!
//! The sentences are spelled out here as literals rather than read from the service's own
//! code: a suite that imported the handler's format string would pass by agreeing with
//! itself.

use std::net::Ipv4Addr;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, emulator, error_code, error_message, filters};
use super::wire::{Param, param};

/// The Region every call here is signed for: the emulator's own default Region, whose
/// default VPC and subnets a fresh account already holds.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers every refusal in this suite with.
const REFUSAL_STATUS: u16 = 400;

/// The image every launch names. Which image an instance ran from has no bearing on any
/// sentence here.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type every launch names: a type whose
/// `DescribeInstanceTypes`-published `NetworkInfo$MaximumNetworkInterfaces` is well above the
/// two device indexes rule 2 uses, so an attach is refused for the index being TAKEN rather
/// than for the instance being full.
const INSTANCE_TYPE: &str = "m5.4xlarge";

/// EC2's code for a request member whose value the operation cannot take.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One call that must be refused, as the error code and message it answered.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> (String, String) {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} from {action}, got {status} with body \
         {body}"
    );
    (error_code(&body), error_message(&body))
}

/// The id of the account's default VPC.
async fn default_vpc(router: &MultiServiceRouter) -> String {
    let body = ok(
        "the default network",
        router,
        "DescribeVpcs",
        &filters(&[("is-default", &["true"])]),
    )
    .await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("a fresh account holds a default VPC: {body}"))
}

/// The account's default subnets, in the order the describe reports them.
async fn default_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc_id = default_vpc(router).await;
    let body = ok(
        "the default network",
        router,
        "DescribeSubnets",
        &filters(&[("vpc-id", &[vpc_id.as_str()])]),
    )
    .await;
    let ids = elements(&body, "subnetId");
    assert!(
        ids.len() >= 2,
        "a fresh account holds a default subnet per Availability Zone, got {body}"
    );
    ids
}

/// The IPv4 CIDR block of `subnet_id`, as the describe reports it.
async fn subnet_cidr(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeSubnets",
        &[param("SubnetId.1", subnet_id)],
    )
    .await;
    element(&body, "cidrBlock").unwrap_or_else(|| panic!("case {case:?}: no block in {body}"))
}

/// The address `host` positions into the block `cidr`, as a caller writes one: the block's
/// own network address with its last octet replaced, which for the account's default
/// subnets is inside the block and clear of the addresses AWS reserves at each end.
fn address_in(cidr: &str, host: u8) -> String {
    let network: Ipv4Addr = cidr
        .split('/')
        .next()
        .unwrap_or_default()
        .parse()
        .unwrap_or_else(|_| panic!("the reported block {cidr:?} is an IPv4 CIDR block"));
    let mut octets = network.octets();
    octets[3] = host;
    Ipv4Addr::from(octets).to_string()
}

/// One interface created in `subnet_id`, as its id.
async fn create_interface(
    case: &str,
    router: &MultiServiceRouter,
    subnet_id: &str,
    extra: &[Param],
) -> String {
    let mut params = vec![param("SubnetId", subnet_id)];
    params.extend_from_slice(extra);
    let body = ok(case, router, "CreateNetworkInterface", &params).await;
    element(&body, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no interface id in {body}"))
}

/// One instance launched into `subnet_id`, as its id.
async fn launch(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("InstanceType", INSTANCE_TYPE),
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("SubnetId", subnet_id),
        ],
    )
    .await;
    element(&body, "instanceId").unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"))
}

// ---------------------------------------------------------------------------
// 1. A page size cannot be combined with an explicit id selection
// ---------------------------------------------------------------------------

/// One paged describe, the form field its id selector is sent as, and a well-formed id of
/// that kind.
struct Paged {
    action: &'static str,
    id_field: &'static str,
    id: &'static str,
}

/// The paged describes rule 1 is asserted over: seven operations and seven id kinds, so no
/// one operation's members are the rule.
///
/// Every row sends a page size of 5 — the smallest value the `@range(min: 5, max: 1000)` the
/// model publishes for `DescribeSecurityGroupsMaxResults` / `DescribeSubnetsMaxResults`
/// admits — so each row reaches the handler rather than the boundary validator in front of
/// it, and a well-formed id, so each reaches the combination check rather than an id-syntax
/// refusal in front of it.
const PAGED: [Paged; 7] = [
    Paged {
        action: "DescribeNetworkInterfaces",
        id_field: "NetworkInterfaceId.1",
        id: "eni-0123456789abcdef0",
    },
    Paged {
        action: "DescribeSubnets",
        id_field: "SubnetId.1",
        id: "subnet-0123456789abcdef0",
    },
    Paged {
        action: "DescribeInstances",
        id_field: "InstanceId.1",
        id: "i-0123456789abcdef0",
    },
    Paged {
        action: "DescribeSecurityGroups",
        id_field: "GroupId.1",
        id: "sg-0123456789abcdef0",
    },
    Paged {
        action: "DescribeRouteTables",
        id_field: "RouteTableId.1",
        id: "rtb-0123456789abcdef0",
    },
    Paged {
        action: "DescribeHosts",
        id_field: "HostId.1",
        id: "h-0123456789abcdef0",
    },
    Paged {
        action: "DescribeStoreImageTasks",
        id_field: "ImageId.1",
        id: "ami-0123456789abcdef0",
    },
];

/// The two parameter names out of the one sentence EC2 refuses a parameter pair with, in the
/// order the sentence names them.
fn named_parameters(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("The parameter ")?;
    let (first, second) = rest.split_once(" cannot be used with the parameter ")?;
    Some((first.to_string(), second.to_string()))
}

/// RULE: a page size beside an explicit id selection is refused with the sentence that names
/// the ID SELECTION first and the page size second — so tooling that reads the offending
/// parameter out of the sentence drops the selection rather than the page size — and a
/// selection spelled as a public member name is named in its PLURAL form.
#[tokio::test]
async fn a_page_size_refusal_names_the_id_selection_before_the_page_size() {
    let (router, _engine) = emulator();

    for Paged {
        action,
        id_field,
        id,
    } in PAGED
    {
        let case = format!("{action} with {id_field} and MaxResults");
        let (code, message) = refused(
            &case,
            &router,
            action,
            &[param(id_field, id), param("MaxResults", "5")],
        )
        .await;
        assert_eq!(
            code,
            ec2::state::INVALID_PARAMETER_COMBINATION,
            "case {case:?}: expected {:?}, got {code:?} with message {message:?}",
            ec2::state::INVALID_PARAMETER_COMBINATION
        );
        let Some((selection, page_size)) = named_parameters(&message) else {
            panic!("case {case:?}: expected EC2's parameter-pair sentence, got {message:?}");
        };
        assert!(
            page_size.eq_ignore_ascii_case("maxresults"),
            "case {case:?}: expected the page size named SECOND, got {message:?}"
        );
        assert!(
            !selection.eq_ignore_ascii_case("maxresults"),
            "case {case:?}: expected the id selection named FIRST, got {message:?}"
        );
        let lowered = selection.to_ascii_lowercase();
        assert!(
            lowered.contains("id") || lowered.ends_with("set"),
            "case {case:?}: expected the first parameter to name the id selection, got \
             {selection:?}"
        );
        // A selection EC2 spells as a PUBLIC member name (initial capital, as against the
        // wire set names it uses on other operations) is the model's plural member.
        if selection.starts_with(|c: char| c.is_ascii_uppercase()) {
            assert!(
                selection.ends_with("Ids"),
                "case {case:?}: expected the plural member name, got {selection:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 2. An attach at a device index the instance already uses
// ---------------------------------------------------------------------------

/// RULE: an attach at a device index the instance already has an interface at is refused
/// with the sentence naming THAT instance and THAT index and nothing else — no
/// parameter-value clause in front of it, and the terminating full stop EC2 writes.
///
/// The rows span the index (the primary interface's 0 and a secondary one an earlier attach
/// made) and the instance, so neither value can be a constant in the handler.
#[tokio::test]
async fn an_attach_at_an_occupied_device_index_names_the_instance_and_the_index() {
    let (router, _engine) = emulator();
    let subnets = default_subnets(&router).await;
    let subnet_id = &subnets[0];

    let first_instance = launch("the first launch", &router, subnet_id).await;
    let second_instance = launch("the second launch", &router, subnet_id).await;

    // A secondary attachment, so that index 1 of the first instance is occupied too.
    let held = create_interface("the held interface", &router, subnet_id, &[]).await;
    ok(
        "the attach that occupies index 1",
        &router,
        "AttachNetworkInterface",
        &[
            param("NetworkInterfaceId", &held),
            param("InstanceId", &first_instance),
            param("DeviceIndex", "1"),
        ],
    )
    .await;

    for (case, instance_id, device_index) in [
        (
            "the index the launch's primary interface sits at",
            &first_instance,
            "0",
        ),
        ("an index an earlier attach occupied", &first_instance, "1"),
        ("another instance's primary index", &second_instance, "0"),
    ] {
        let interface = create_interface(case, &router, subnet_id, &[]).await;
        let (code, message) = refused(
            case,
            &router,
            "AttachNetworkInterface",
            &[
                param("NetworkInterfaceId", &interface),
                param("InstanceId", instance_id),
                param("DeviceIndex", device_index),
            ],
        )
        .await;
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {code:?} with message \
             {message:?}"
        );
        let expected = format!(
            "Instance '{instance_id}' already has an interface attached at device index \
             '{device_index}'."
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. A detach of an instance's primary interface
// ---------------------------------------------------------------------------

/// RULE: the attachment of an instance's PRIMARY interface cannot be taken down, and the
/// refusal names the attachment's device index and its network card index with EC2's own
/// spelling of the second — `networkCard`, in camel case — whatever `Force` says.
#[tokio::test]
async fn a_detach_of_the_primary_interface_names_both_indexes_as_ec2_spells_them() {
    let (router, _engine) = emulator();
    let subnets = default_subnets(&router).await;
    let instance_id = launch("the launch", &router, &subnets[0]).await;

    let described = ok(
        "the launched instance",
        &router,
        "DescribeInstances",
        &[param("InstanceId.1", &instance_id)],
    )
    .await;
    let attachment_id = element(&described, "attachmentId")
        .unwrap_or_else(|| panic!("a launch attaches a primary interface, got {described}"));

    for (case, extra) in [
        ("a plain detach of the primary interface", Vec::new()),
        (
            "a forced detach of the primary interface",
            vec![param("Force", "true")],
        ),
    ] {
        let mut params = vec![param("AttachmentId", &attachment_id)];
        params.extend_from_slice(&extra);
        let (code, message) = refused(case, &router, "DetachNetworkInterface", &params).await;
        assert_eq!(
            code, "OperationNotPermitted",
            "case {case:?}: expected OperationNotPermitted, got {code:?} with message \
             {message:?}"
        );
        assert_eq!(
            message,
            "The network interface at device index 0 and networkCard index 0 cannot be \
             detached.",
            "case {case:?}: expected EC2's sentence for this class, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. An IPv6 ask against a subnet with no IPv6 range
// ---------------------------------------------------------------------------

/// The sentence EC2 refuses an IPv6 ask against a subnet with no IPv6 range with: fixed text
/// end to end, naming no subnet.
const NO_IPV6_RANGE: &str = "Subnet does not contain any IPv6 CIDR block ranges";

/// RULE: an operation that would take an IPv6 address out of a subnet with no IPv6 CIDR
/// block is refused `InvalidParameterValue` with that one sentence, which carries no id — so
/// no row's subnet, interface or asked-for address can appear in it.
///
/// The rows span the subnet (two of the account's default subnets, which have distinct ids
/// and no IPv6 range), the spelling of the ask (a count of addresses, a count of prefixes, a
/// named address) and the OPERATION (the assignment, and the attribute that makes an
/// interface's primary IPv6 address).
#[tokio::test]
async fn an_ipv6_assignment_out_of_a_subnet_with_no_ipv6_range_names_no_subnet() {
    let (router, _engine) = emulator();
    let subnets = default_subnets(&router).await;

    for subnet_id in subnets.iter().take(2) {
        let interface = create_interface("the interface", &router, subnet_id, &[]).await;
        for (ask, action, extra) in [
            (
                "one address by count",
                "AssignIpv6Addresses",
                vec![param("Ipv6AddressCount", "1")],
            ),
            (
                "several addresses by count",
                "AssignIpv6Addresses",
                vec![param("Ipv6AddressCount", "3")],
            ),
            (
                "a named address",
                "AssignIpv6Addresses",
                vec![param("Ipv6Addresses.1", "2600:1f18:1::1")],
            ),
            (
                "a prefix by count",
                "AssignIpv6Addresses",
                vec![param("Ipv6PrefixCount", "1")],
            ),
            (
                "a primary IPv6 address",
                "ModifyNetworkInterfaceAttribute",
                vec![param("EnablePrimaryIpv6", "true")],
            ),
        ] {
            let case = format!("{ask} in {subnet_id}");
            let mut params = vec![param("NetworkInterfaceId", &interface)];
            params.extend_from_slice(&extra);
            let (code, message) = refused(&case, &router, action, &params).await;
            assert_eq!(
                code, INVALID_PARAMETER_VALUE,
                "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {code:?} with message \
                 {message:?}"
            );
            assert_eq!(
                message, NO_IPV6_RANGE,
                "case {case:?}: expected EC2's sentence for this class, got {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 5. A create on a private address something already holds
// ---------------------------------------------------------------------------

/// The sentence EC2 refuses a create naming a held private address with: fixed text end to
/// end, naming no address.
const ADDRESS_ALREADY_IN_USE: &str = "The specified address is already in use.";

/// RULE: `CreateNetworkInterface` naming a private IPv4 address something already holds is
/// refused `InvalidIPAddress.InUse` with EC2's fixed sentence for that class, which names no
/// address — the request named the one address the refusal is about.
///
/// The rows span the address (two different ones, so no value can be substituted into the
/// sentence) and how it came to be held: by an interface created earlier, and by the SAME
/// request naming one address twice.
#[tokio::test]
async fn a_create_naming_a_held_private_address_names_no_address() {
    let (router, _engine) = emulator();
    let subnets = default_subnets(&router).await;
    let subnet_id = &subnets[0];
    let cidr = subnet_cidr("the subnet", &router, subnet_id).await;

    for host in [20_u8, 21] {
        let address = address_in(&cidr, host);
        let case = format!("the address {address} another interface holds");
        create_interface(
            &case,
            &router,
            subnet_id,
            &[param("PrivateIpAddress", &address)],
        )
        .await;
        let (code, message) = refused(
            &case,
            &router,
            "CreateNetworkInterface",
            &[
                param("SubnetId", subnet_id),
                param("PrivateIpAddress", &address),
            ],
        )
        .await;
        assert_eq!(
            code, "InvalidIPAddress.InUse",
            "case {case:?}: expected InvalidIPAddress.InUse, got {code:?} with message \
             {message:?}"
        );
        assert_eq!(
            message, ADDRESS_ALREADY_IN_USE,
            "case {case:?}: expected EC2's sentence for this class, got {message:?}"
        );
    }

    // One request that names the same address twice holds it the first time and is refused
    // the second, with the same sentence.
    let address = address_in(&cidr, 30);
    let case = format!("the address {address} named twice in one request");
    let (code, message) = refused(
        &case,
        &router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", subnet_id),
            param("PrivateIpAddresses.1.PrivateIpAddress", &address),
            param("PrivateIpAddresses.2.PrivateIpAddress", &address),
        ],
    )
    .await;
    assert_eq!(
        code, "InvalidIPAddress.InUse",
        "case {case:?}: expected InvalidIPAddress.InUse, got {code:?} with message {message:?}"
    );
    assert_eq!(
        message, ADDRESS_ALREADY_IN_USE,
        "case {case:?}: expected EC2's sentence for this class, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. A delete of a VPC's default security group
// ---------------------------------------------------------------------------

/// The default security group of `vpc_id`, as its id and the name it reports.
async fn default_group(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> (String, String) {
    let body = ok(
        case,
        router,
        "DescribeSecurityGroups",
        &filters(&[("vpc-id", &[vpc_id]), ("group-name", &["default"])]),
    )
    .await;
    let group_id = element(&body, "groupId").unwrap_or_else(|| {
        panic!("case {case:?}: every VPC comes with a default group, got {body}")
    });
    let group_name = element(&body, "groupName")
        .unwrap_or_else(|| panic!("case {case:?}: a group reports its name, got {body}"));
    (group_id, group_name)
}

/// RULE: the default security group of a VPC cannot be deleted on its own, and the refusal
/// names the group by BOTH its id and its name, each double-quoted and read off the STORED
/// group — so a caller that deleted by name is told the id and one that deleted by id is
/// told the name.
///
/// The rows span the VPC the group belongs to (the account's default VPC and one this test
/// creates, which have different group ids) and the member the request named the group with.
#[tokio::test]
async fn a_delete_of_a_default_security_group_names_its_id_and_name() {
    let (router, _engine) = emulator();

    let vpc_id = default_vpc(&router).await;
    let (default_vpc_group, default_vpc_group_name) =
        default_group("the default VPC", &router, &vpc_id).await;

    let created = ok(
        "a VPC of its own",
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.99.0.0/16")],
    )
    .await;
    let created_vpc = element(&created, "vpcId").unwrap_or_default();
    let (created_vpc_group, created_vpc_group_name) =
        default_group("a VPC of its own", &router, &created_vpc).await;

    for (case, selector, group_id, group_name) in [
        (
            "the default VPC's group by id",
            vec![param("GroupId", &default_vpc_group)],
            &default_vpc_group,
            &default_vpc_group_name,
        ),
        (
            // "For security groups for a default VPC you can specify either the ID or the
            // name of the security group" (`DeleteSecurityGroupRequest$GroupName`).
            "the default VPC's group by name",
            vec![param("GroupName", &default_vpc_group_name)],
            &default_vpc_group,
            &default_vpc_group_name,
        ),
        (
            "another VPC's group by id",
            vec![param("GroupId", &created_vpc_group)],
            &created_vpc_group,
            &created_vpc_group_name,
        ),
    ] {
        let (code, message) = refused(case, &router, "DeleteSecurityGroup", &selector).await;
        assert_eq!(
            code, "CannotDelete",
            "case {case:?}: expected CannotDelete, got {code:?} with message {message:?}"
        );
        let expected = format!(
            "the specified group: \"{group_id}\" name: \"{group_name}\" cannot be deleted by \
             a user"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. A VPC create that names neither a block nor a pool
// ---------------------------------------------------------------------------

/// RULE: a VPC create carrying neither an IPv4 CIDR block nor an IPAM pool to allocate one
/// from is refused `MissingParameter` with EC2's sentence naming both WIRE member names,
/// single-quoted — whatever else the request carries.
#[tokio::test]
async fn a_create_vpc_with_no_block_and_no_pool_names_both_members() {
    let (router, _engine) = emulator();

    for (case, params) in [
        ("a create that carries nothing at all", Vec::new()),
        (
            "a create whose block is empty",
            vec![param("CidrBlock", "")],
        ),
        (
            "a create that carries only a tenancy",
            vec![param("InstanceTenancy", "default")],
        ),
        (
            "a create that carries only a tag",
            vec![
                param("TagSpecification.1.ResourceType", "vpc"),
                param("TagSpecification.1.Tag.1.Key", "team"),
                param("TagSpecification.1.Tag.1.Value", "network"),
            ],
        ),
    ] {
        let (code, message) = refused(case, &router, "CreateVpc", &params).await;
        assert_eq!(
            code, "MissingParameter",
            "case {case:?}: expected MissingParameter, got {code:?} with message {message:?}"
        );
        assert_eq!(
            message, "Either 'cidrBlock' or 'ipv4IpamPoolId' should be provided.",
            "case {case:?}: expected EC2's sentence for this class, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. A rule whose port range ends before it starts
// ---------------------------------------------------------------------------

/// RULE: a rule whose `FromPort` is above its `ToPort` is a bad VALUE —
/// `InvalidParameterValue` with the two ends as the request sent them, in EC2's fixed
/// `Invalid TCP/UDP port range(<from>:<to>)` — rather than a malformed permission, and the
/// emulator's own entry numbering appears nowhere in it.
///
/// The rows span the protocol, the two ends, the side of the group the rule is authorized on
/// and, inbound, the two spellings of one rule (a permission, and the flat members that
/// describe a single rule — which only the inbound action takes).
#[tokio::test]
async fn an_inverted_port_range_is_an_invalid_parameter_value_naming_the_range() {
    let (router, _engine) = emulator();

    let vpc_id = default_vpc(&router).await;
    let created = ok(
        "the group under test",
        &router,
        "CreateSecurityGroup",
        &[
            param("GroupName", "inverted-port-ranges"),
            param("GroupDescription", "a group the port-range rows author on"),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let group_id =
        element(&created, "groupId").unwrap_or_else(|| panic!("no group id in {created}"));

    for action in [
        "AuthorizeSecurityGroupIngress",
        "AuthorizeSecurityGroupEgress",
    ] {
        for (range, protocol, from_port, to_port) in [
            ("ssh behind telnet", "tcp", "23", "22"),
            ("https behind http", "udp", "443", "80"),
            ("one port apart", "tcp", "81", "80"),
        ] {
            for (spelling, params) in [
                (
                    "a permission",
                    vec![
                        param("IpPermissions.1.IpProtocol", protocol),
                        param("IpPermissions.1.FromPort", from_port),
                        param("IpPermissions.1.ToPort", to_port),
                        param("IpPermissions.1.IpRanges.1.CidrIp", "10.0.0.0/8"),
                    ],
                ),
                (
                    "the flat members",
                    vec![
                        param("IpProtocol", protocol),
                        param("FromPort", from_port),
                        param("ToPort", to_port),
                        param("CidrIp", "10.0.0.0/8"),
                    ],
                ),
            ] {
                // "We recommend that you use a set of IP permissions instead"
                // (`AuthorizeSecurityGroupEgressRequest$CidrIp`): the flat members describe
                // one INBOUND rule, and the outbound action takes permissions only.
                if spelling == "the flat members" && action.ends_with("Egress") {
                    continue;
                }
                let case = format!("{action}: {range} as {spelling}");
                let mut fields = vec![param("GroupId", &group_id)];
                fields.extend_from_slice(&params);
                let (code, message) = refused(&case, &router, action, &fields).await;
                assert_eq!(
                    code, INVALID_PARAMETER_VALUE,
                    "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {code:?} with \
                     message {message:?}"
                );
                let expected = format!("Invalid TCP/UDP port range({from_port}:{to_port})");
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected the message {expected:?}, got {message:?}"
                );
            }
        }
    }
}
