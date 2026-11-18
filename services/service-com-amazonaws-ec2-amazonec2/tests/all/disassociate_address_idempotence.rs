// SPDX-License-Identifier: CC0-1.0

//! Ending an Elastic IP association through the member that names the ADDRESS, over and
//! over — end to end over the wire the service speaks, `ec2Query`, through the crate's
//! shared [`super::ec2_query`] harness.
//!
//! The rule under test is an INVARIANT of `DisassociateAddress`: asked for an address the
//! account holds, it ends whatever association that address carries and then STANDS — "this
//! is an idempotent operation. If you perform the operation more than once, Amazon EC2
//! doesn't return an error" (`DisassociateAddress`) — leaving the address ALLOCATED, so a
//! repeat answers HTTP 200 again, the address record reports only its allocation, and the
//! allocation is still the account's to associate again or to give up. Only the ADDRESS
//! form (`$PublicIp`) can be repeated at all: an `$AssociationId` names the one association
//! the first call ended, and an id no address of the account carries names nothing.
//!
//! Each row is a distinct input along what the invariant generalises over — what the
//! address was bound to when the repeats began (nothing, an instance, a bare network
//! interface, a secondary private address of one) and how many times the call is repeated —
//! so no row is the specification.

use service_com_amazonaws_ec2_amazonec2::state::ELASTIC_IP_ASSOCIATION_ID_PREFIX;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here uses; `RunInstances` only needs one that resolves.
const AMI: &str = "ami-0123456789abcdef0";

fn emulator() -> MultiServiceRouter {
    ec2_query::emulator().0
}

async fn api(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    ec2_query::call(router, REGION, action, params).await
}

/// One call that must answer HTTP 200, as its body.
async fn served(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Allocate one Elastic IP address and answer `(allocation id, public ip)`.
async fn allocate(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let body = served(router, case, "AllocateAddress", &[]).await;
    let allocation_id = ec2_query::element(&body, "allocationId")
        .unwrap_or_else(|| panic!("case {case:?}: AllocateAddress minted no allocation id: {body}"));
    let public_ip = ec2_query::element(&body, "publicIp")
        .unwrap_or_else(|| panic!("case {case:?}: AllocateAddress minted no address: {body}"));
    (allocation_id, public_ip)
}

/// What the association members of one address record say, read back through
/// `DescribeAddresses` narrowed to the allocation: the four members an association is
/// reported on, `None` for each the record does not carry.
#[derive(Debug, Default, PartialEq, Eq)]
struct Association {
    association_id: Option<String>,
    instance_id: Option<String>,
    network_interface_id: Option<String>,
    private_ip_address: Option<String>,
}

/// The address record `DescribeAddresses` reports for `allocation_id`, as its public
/// address and the association members it carries.
async fn stored_address(
    router: &MultiServiceRouter,
    case: &str,
    allocation_id: &str,
) -> (Option<String>, Association) {
    let body = served(
        router,
        case,
        "DescribeAddresses",
        &[param("AllocationId.1", allocation_id)],
    )
    .await;
    let parsed = document(case, &body);
    let item = set_items(parsed.root_element(), "addressesSet")
        .into_iter()
        .find(|item| child_text(*item, "allocationId").as_deref() == Some(allocation_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: DescribeAddresses must report {allocation_id}: {body}")
        });
    (
        child_text(item, "publicIp"),
        Association {
            association_id: child_text(item, "associationId"),
            instance_id: child_text(item, "instanceId"),
            network_interface_id: child_text(item, "networkInterfaceId"),
            private_ip_address: child_text(item, "privateIpAddress"),
        },
    )
}

/// A subnet the account really holds: the first one `DescribeSubnets` reports, which is a
/// default subnet of the Region's default VPC.
async fn any_subnet(router: &MultiServiceRouter, case: &str) -> String {
    let body = served(router, case, "DescribeSubnets", &[]).await;
    ec2_query::element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: the account holds no subnet: {body}"))
}

/// Create one network interface with `extra` beside its subnet, and answer its id with the
/// private addresses it was given, primary first.
async fn create_interface(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    extra: &[Param],
) -> (String, Vec<String>) {
    let mut params = vec![param("SubnetId", subnet_id)];
    params.extend_from_slice(extra);
    let body = served(router, case, "CreateNetworkInterface", &params).await;
    let network_interface_id = ec2_query::element(&body, "networkInterfaceId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateNetworkInterface minted no interface: {body}")
    });
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element() && child_text(*node, "networkInterfaceId").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no interface in {body}"));
    let mut addresses: Vec<(bool, String)> = set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .filter_map(|held| {
            let address = child_text(held, "privateIpAddress")?;
            let primary = child_text(held, "primary").as_deref() == Some("true");
            // Primary first: `false` sorts before `true`, so the flag is negated.
            Some((!primary, address))
        })
        .collect();
    addresses.sort();
    let addresses: Vec<String> = addresses.into_iter().map(|(_, held)| held).collect();
    assert!(
        !addresses.is_empty(),
        "case {case:?}: a created interface must hold a private address: {body}"
    );
    (network_interface_id, addresses)
}

/// Launch one instance and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let params = [
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    let body = served(router, case, "RunInstances", &params).await;
    ec2_query::element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// What the address under test was bound to when the repeats begin.
#[derive(Debug, Clone, Copy)]
enum BoundTo {
    /// Nothing: the account allocated the address and never associated it.
    Nothing,
    /// An instance, through the one interface its launch created.
    Instance,
    /// A bare network interface, on its primary private address.
    Interface,
    /// A bare network interface, on a SECONDARY private address of it.
    SecondaryPrivateAddress,
}

/// Associate the address as `bound_to` says, and answer the association id it was given —
/// `None` for an address left on nothing.
async fn associate(
    router: &MultiServiceRouter,
    case: &str,
    allocation_id: &str,
    bound_to: BoundTo,
) -> Option<String> {
    let params = match bound_to {
        BoundTo::Nothing => return None,
        BoundTo::Instance => {
            let instance_id = launch(router, case).await;
            vec![
                param("AllocationId", allocation_id),
                param("InstanceId", &instance_id),
            ]
        }
        BoundTo::Interface => {
            let subnet_id = any_subnet(router, case).await;
            let (network_interface_id, _) = create_interface(router, case, &subnet_id, &[]).await;
            vec![
                param("AllocationId", allocation_id),
                param("NetworkInterfaceId", &network_interface_id),
            ]
        }
        BoundTo::SecondaryPrivateAddress => {
            let subnet_id = any_subnet(router, case).await;
            let (network_interface_id, addresses) = create_interface(
                router,
                case,
                &subnet_id,
                &[param("SecondaryPrivateIpAddressCount", "1")],
            )
            .await;
            assert!(
                addresses.len() > 1,
                "case {case:?}: the interface must hold a secondary private address, \
                 got {addresses:?}"
            );
            vec![
                param("AllocationId", allocation_id),
                param("NetworkInterfaceId", &network_interface_id),
                param("PrivateIpAddress", &addresses[1]),
            ]
        }
    };
    let body = served(router, case, "AssociateAddress", &params).await;
    let association_id = ec2_query::element(&body, "associationId").unwrap_or_else(|| {
        panic!("case {case:?}: AssociateAddress minted no association id: {body}")
    });
    assert!(
        association_id.starts_with(&format!("{ELASTIC_IP_ASSOCIATION_ID_PREFIX}-")),
        "case {case:?}: expected an {ELASTIC_IP_ASSOCIATION_ID_PREFIX}- association id, \
         got {association_id:?}"
    );
    Some(association_id)
}

/// RULE, as an INVARIANT: `DisassociateAddress` naming the ADDRESS is idempotent — it ends
/// whatever association the address carries and every repeat answers HTTP 200 again, with
/// the address left allocated and carrying no association, whatever it was bound to.
#[tokio::test]
async fn disassociating_an_address_by_its_public_ip_is_idempotent() {
    let router = emulator();
    // Rows: `(case, what the address was bound to, how many times the call is repeated)`.
    let rows: [(&str, BoundTo, usize); 5] = [
        ("an address bound to nothing, ended once", BoundTo::Nothing, 1),
        ("an address bound to nothing, ended three times", BoundTo::Nothing, 3),
        ("an address bound to an instance, ended twice", BoundTo::Instance, 2),
        ("an address bound to a bare interface, ended three times", BoundTo::Interface, 3),
        (
            "an address bound to a secondary private address, ended twice",
            BoundTo::SecondaryPrivateAddress,
            2,
        ),
    ];

    for (case, bound_to, repeats) in rows {
        let (allocation_id, public_ip) = allocate(&router, case).await;
        let association_id = associate(&router, case, &allocation_id, bound_to).await;

        // Whatever the row bound the address to is what the record reports before the first
        // call, so each row really exercises a different standing state.
        let (_, before) = stored_address(&router, case, &allocation_id).await;
        assert_eq!(
            before.association_id, association_id,
            "case {case:?}: the address must report the association the row made, \
             got {before:?}"
        );

        // Every call of the repeat is served, and leaves the same record behind: the
        // allocation, the address, and no association at all.
        for attempt in 1..=repeats {
            served(
                &router,
                case,
                "DisassociateAddress",
                &[param("PublicIp", &public_ip)],
            )
            .await;
            let (reported_ip, after) = stored_address(&router, case, &allocation_id).await;
            assert_eq!(
                reported_ip.as_deref(),
                Some(public_ip.as_str()),
                "case {case:?}: attempt {attempt} must leave the address allocated, \
                 got {reported_ip:?}"
            );
            assert_eq!(
                after,
                Association::default(),
                "case {case:?}: attempt {attempt} must leave no association on the address, \
                 got {after:?}"
            );
        }

        // And the allocation nothing holds is the account's to give up, which is what being
        // left ALLOCATED rather than released means.
        served(
            &router,
            case,
            "ReleaseAddress",
            &[param("AllocationId", &allocation_id)],
        )
        .await;
    }
}

/// RULE: an `AssociationId` names ONE association, so only the address form repeats — the
/// id the first call ended names nothing afterwards, while the address can be associated
/// again and ended by the new id it is given.
#[tokio::test]
async fn an_association_id_names_the_one_association_it_ended() {
    let router = emulator();
    let case = "one address, associated and ended twice over";
    let (allocation_id, public_ip) = allocate(&router, case).await;
    let subnet_id = any_subnet(&router, case).await;
    let (network_interface_id, addresses) = create_interface(&router, case, &subnet_id, &[]).await;

    let bind = [
        param("AllocationId", &allocation_id),
        param("NetworkInterfaceId", &network_interface_id),
    ];
    let first = served(&router, case, "AssociateAddress", &bind).await;
    let first = ec2_query::element(&first, "associationId").unwrap_or_default();
    served(
        &router,
        case,
        "DisassociateAddress",
        &[param("AssociationId", &first)],
    )
    .await;

    // The id of an association that has been ended names no association of the account, so
    // the second call by that id is refused rather than served.
    let (status, body) = api(
        &router,
        "DisassociateAddress",
        &[param("AssociationId", &first)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: an ended association id must be refused, got {status} with body {body}"
    );
    let code = ec2_query::error_code(&body);
    assert_eq!(
        code, "InvalidAssociationID.NotFound",
        "case {case:?}: expected InvalidAssociationID.NotFound, got {code:?} with body {body}"
    );

    // The ADDRESS, though, is still the account's: associating it again mints its own id,
    // which is reported on the record and ends it in turn.
    let second = served(&router, case, "AssociateAddress", &bind).await;
    let second = ec2_query::element(&second, "associationId").unwrap_or_default();
    assert_ne!(
        second, first,
        "case {case:?}: the association made after the first was ended gets its own id, \
         got {second:?}"
    );
    let (_, standing) = stored_address(&router, case, &allocation_id).await;
    assert_eq!(
        standing,
        Association {
            association_id: Some(second.clone()),
            instance_id: None,
            network_interface_id: Some(network_interface_id.clone()),
            private_ip_address: Some(addresses[0].clone()),
        },
        "case {case:?}: the record must report the new association, got {standing:?}"
    );
    served(
        &router,
        case,
        "DisassociateAddress",
        &[param("AssociationId", &second)],
    )
    .await;
    let (reported_ip, ended) = stored_address(&router, case, &allocation_id).await;
    assert_eq!(
        (reported_ip.as_deref(), &ended),
        (Some(public_ip.as_str()), &Association::default()),
        "case {case:?}: the address must keep only its allocation, got {ended:?}"
    );
}
