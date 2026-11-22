// SPDX-License-Identifier: CC0-1.0

//! What a reported NETWORK-INTERFACE ASSOCIATION says about the translation that carries
//! traffic to the public address bound there, driven end to end over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an Elastic IP address is a public address held by the network's
//! internet gateway, which translates between it and the private address it is bound to — so
//! every reported association that names the ALLOCATION the account holds that address as
//! reports `natEnabled` `true`, wherever that one association is read (on the interface
//! itself, and on the private address the public address is bound to). An association that
//! names no allocation — a public address AWS assigned the interface itself — reports nothing
//! of the kind, and neither does an interface that carries no association at all.
//!
//! The rows differ along what the rule generalises over: which member of the request named
//! the resource, whether the interface is held by an instance or by nothing, whether the
//! address is bound to the interface's primary or to a secondary private address, and which
//! member named the address — so no row is the specification.
//!
//! The flag is not a member `com.amazonaws.ec2#NetworkInterfaceAssociation` declares
//! (`smithy_query(query="shape",shape="NetworkInterfaceAssociation")` lists allocationId,
//! associationId, ipOwnerId, publicDnsName, publicIp, customerOwnedIp and carrierIp and no
//! such member), so what the service renders for an association the store holds no allocation
//! for is what it can derive for one: nothing. That is the evidence for the absence the second
//! test below reads — not a claim that the address AWS assigned is reached some other way.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here uses; `RunInstances` only needs one that resolves.
const AMI: &str = "ami-0123456789abcdef0";

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = ec2_query::call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A subnet the account really holds: the first one `DescribeSubnets` reports, which is a
/// default subnet of the Region's default VPC — a network that "comes with an internet
/// gateway", which is the precondition `AssociateAddress` holds every association to.
async fn any_subnet(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "DescribeSubnets", &[]).await;
    ec2_query::required(case, &body, "subnetId")
}

/// One Elastic IP address the account holds, as `(allocation id, public address)`.
async fn allocate(case: &str, router: &MultiServiceRouter) -> (String, String) {
    let body = ok(case, router, "AllocateAddress", &[]).await;
    (
        ec2_query::required(case, &body, "allocationId"),
        ec2_query::required(case, &body, "publicIp"),
    )
}

/// One network interface in `subnet_id` with `extra` beside it, as its id and the private
/// addresses it was given, primary first.
async fn create_interface(
    case: &str,
    router: &MultiServiceRouter,
    subnet_id: &str,
    extra: &[Param],
) -> (String, Vec<String>) {
    let mut params = vec![param("SubnetId", subnet_id)];
    params.extend_from_slice(extra);
    let body = ok(case, router, "CreateNetworkInterface", &params).await;
    let network_interface_id = ec2_query::required(case, &body, "networkInterfaceId");
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no interface element in {body}"));
    let mut held: Vec<(bool, String)> = set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .filter_map(|item| {
            let address = child_text(item, "privateIpAddress")?;
            let primary = child_text(item, "primary").as_deref() == Some("true");
            Some((!primary, address))
        })
        .collect();
    held.sort();
    assert!(
        !held.is_empty(),
        "case {case:?}: a created interface must hold a private address: {body}"
    );
    (
        network_interface_id,
        held.into_iter().map(|(_, address)| address).collect(),
    )
}

/// One instance launched into the Region's default VPC, as `(instance id, its primary private
/// address)`.
async fn launch(case: &str, router: &MultiServiceRouter) -> (String, String) {
    let params = [
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    let body = ok(case, router, "RunInstances", &params).await;
    (
        ec2_query::required(case, &body, "instanceId"),
        ec2_query::required(case, &body, "privateIpAddress"),
    )
}

/// One reported association, as the members this suite reads off it.
#[derive(Debug, Default, PartialEq, Eq)]
struct ReportedAssociation {
    allocation_id: Option<String>,
    public_ip: Option<String>,
    /// Present only when the element is there at all, which is the difference a client
    /// reading it acts on.
    nat_enabled: Option<String>,
}

/// The association the interface `network_interface_id` reports, and the one its `bound_to`
/// private address reports — the same fact in the two places `DescribeNetworkInterfaces`
/// carries it. `None` where there is no `association` element at all.
async fn reported_associations(
    case: &str,
    router: &MultiServiceRouter,
    network_interface_id: &str,
    bound_to: &str,
) -> (Option<ReportedAssociation>, Option<ReportedAssociation>) {
    let params = [param("NetworkInterfaceId.1", network_interface_id)];
    let body = ok(case, router, "DescribeNetworkInterfaces", &params).await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref() == Some(network_interface_id)
        })
        .unwrap_or_else(|| panic!("case {case:?}: no interface {network_interface_id} in {body}"));
    let read = |node: roxmltree::Node<'_, '_>| ReportedAssociation {
        allocation_id: child_text(node, "allocationId"),
        public_ip: child_text(node, "publicIp"),
        nat_enabled: child_text(node, "natEnabled"),
    };
    let on_interface = child(interface, "association").map(read);
    let on_address = set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .find(|item| child_text(*item, "privateIpAddress").as_deref() == Some(bound_to))
        .and_then(|item| child(item, "association"))
        .map(read);
    (on_interface, on_address)
}

/// What one row's request names as the resource the Elastic IP address binds to.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// The INSTANCE, which binds the address to the interface its launch created.
    Instance,
    /// That same interface, named directly — so an interface an instance holds.
    InstancePrimaryInterface,
    /// An interface nothing holds, created for itself.
    FreeInterface,
    /// An interface nothing holds, with the address bound to a SECONDARY private address of
    /// it rather than to its primary one.
    FreeInterfaceSecondaryAddress,
}

/// One row of the association table.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    target: Target,
    /// Name the address by its deprecated `PublicIp` rather than by its allocation id.
    by_public_ip: bool,
}

/// RULE: an association that names the allocation of an Elastic IP address reports
/// `natEnabled` `true`, on the interface and on the private address the public address is
/// bound to — whichever way the request named the resource and the address.
#[tokio::test]
async fn an_elastic_ip_association_reports_the_translation_on_both_sides() {
    let rows = [
        Row {
            case: "the instance, which binds the address to its primary interface",
            target: Target::Instance,
            by_public_ip: false,
        },
        Row {
            case: "the instance's primary interface, named directly",
            target: Target::InstancePrimaryInterface,
            by_public_ip: false,
        },
        Row {
            case: "an interface nothing holds, the address named by its public address",
            target: Target::FreeInterface,
            by_public_ip: true,
        },
        Row {
            case: "a secondary private address of an interface nothing holds",
            target: Target::FreeInterfaceSecondaryAddress,
            by_public_ip: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, _engine) = ec2_query::emulator();
        let subnet_id = any_subnet(case, &router).await;
        let (allocation_id, public_ip) = allocate(case, &router).await;

        // The target, the interface the association binds to and the private address it is
        // bound to.
        let (mut params, network_interface_id, private_ip, unbound) = match row.target {
            Target::Instance => {
                let (instance_id, private_ip) = launch(case, &router).await;
                let network_interface_id =
                    ec2_query::primary_network_interface(&router, REGION, case, &instance_id).await;
                (
                    vec![param("InstanceId", &instance_id)],
                    network_interface_id,
                    private_ip,
                    None,
                )
            }
            Target::InstancePrimaryInterface => {
                let (instance_id, private_ip) = launch(case, &router).await;
                let network_interface_id =
                    ec2_query::primary_network_interface(&router, REGION, case, &instance_id).await;
                (
                    vec![param("NetworkInterfaceId", &network_interface_id)],
                    network_interface_id,
                    private_ip,
                    None,
                )
            }
            Target::FreeInterface => {
                let (network_interface_id, held) =
                    create_interface(case, &router, &subnet_id, &[]).await;
                let private_ip = held[0].clone();
                (
                    vec![param("NetworkInterfaceId", &network_interface_id)],
                    network_interface_id,
                    private_ip,
                    None,
                )
            }
            Target::FreeInterfaceSecondaryAddress => {
                let (network_interface_id, held) = create_interface(
                    case,
                    &router,
                    &subnet_id,
                    &[param("SecondaryPrivateIpAddressCount", "1")],
                )
                .await;
                assert!(
                    held.len() > 1,
                    "case {case:?}: the interface must hold a secondary private address, \
                     got {held:?}"
                );
                (
                    vec![
                        param("NetworkInterfaceId", &network_interface_id),
                        param("PrivateIpAddress", &held[1]),
                    ],
                    network_interface_id,
                    held[1].clone(),
                    Some(held[0].clone()),
                )
            }
        };
        if row.by_public_ip {
            params.push(param("PublicIp", &public_ip));
        } else {
            params.push(param("AllocationId", &allocation_id));
        }
        ok(case, &router, "AssociateAddress", &params).await;

        let expected = ReportedAssociation {
            allocation_id: Some(allocation_id.clone()),
            public_ip: Some(public_ip.clone()),
            nat_enabled: Some("true".to_string()),
        };
        let (on_interface, on_address) =
            reported_associations(case, &router, &network_interface_id, &private_ip).await;
        assert_eq!(
            on_interface.as_ref(),
            Some(&expected),
            "case {case:?}: the interface must report the translation of the Elastic IP \
             address it holds, got {on_interface:?}"
        );
        assert_eq!(
            on_address.as_ref(),
            Some(&expected),
            "case {case:?}: the private address the Elastic IP is bound to must report the \
             same translation, got {on_address:?}"
        );

        // A private address of the interface the public one is NOT bound to carries no
        // association, so there is no translation to report against it either.
        if let Some(unbound) = unbound.as_deref() {
            let (_, on_unbound) =
                reported_associations(case, &router, &network_interface_id, unbound).await;
            assert_eq!(
                on_unbound, None,
                "case {case:?}: the private address the Elastic IP is not bound to must \
                 report no association, got {on_unbound:?}"
            );
        }
    }
}

/// RULE, read from the other side: the flag follows the ALLOCATION the association names, so
/// an association of a public address AWS assigned the interface itself — which the account
/// holds no allocation for — reports none, and an interface carrying no association at all
/// reports neither.
#[tokio::test]
async fn an_address_aws_assigned_reports_no_translation() {
    let case = "an interface AWS assigned a public address to";
    let (router, _engine) = ec2_query::emulator();
    let subnet_id = any_subnet(case, &router).await;
    let (network_interface_id, held) = create_interface(case, &router, &subnet_id, &[]).await;
    let private_ip = held[0].clone();

    // As created, the interface holds no public address at all.
    let (on_interface, on_address) =
        reported_associations(case, &router, &network_interface_id, &private_ip).await;
    assert_eq!(
        (on_interface.as_ref(), on_address.as_ref()),
        (None, None),
        "case {case:?}: an interface holding no public address reports no association, \
         got {on_interface:?} and {on_address:?}"
    );

    // "Indicates whether to assign a public IPv4 address to a network interface"
    // (`ModifyNetworkInterfaceAttributeRequest$AssociatePublicIpAddress`): the address is
    // AWS's own, so the association it writes names no allocation of the account's.
    ok(
        case,
        &router,
        "ModifyNetworkInterfaceAttribute",
        &[
            param("NetworkInterfaceId", &network_interface_id),
            param("AssociatePublicIpAddress", "true"),
        ],
    )
    .await;
    let (on_interface, on_address) =
        reported_associations(case, &router, &network_interface_id, &private_ip).await;
    for (side, reported) in [("the interface", &on_interface), ("the address", &on_address)] {
        let reported = reported
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: {side} must report an association"));
        assert!(
            reported.public_ip.is_some(),
            "case {case:?}: {side} must report the public address AWS assigned, \
             got {reported:?}"
        );
        assert_eq!(
            reported.allocation_id, None,
            "case {case:?}: {side} must report no allocation for an address AWS assigned, \
             got {reported:?}"
        );
        assert_eq!(
            reported.nat_enabled, None,
            "case {case:?}: {side} must report no translation for an address the account \
             holds no allocation for, got {reported:?}"
        );
    }
}
