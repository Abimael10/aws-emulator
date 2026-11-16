// SPDX-License-Identifier: CC0-1.0

//! That an ELASTIC IP ADDRESS CAN ONLY BE ASSOCIATED WITH A RESOURCE IN A NETWORK AN INTERNET
//! GATEWAY ATTACHES, driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an Elastic IP address is a public address, and traffic only reaches
//! the resource it is bound to through the network's internet gateway — so `AssociateAddress`
//! is refused with `Gateway.NotAttached` while the VPC of the resource it names has no
//! internet gateway attached, whichever way the request names that resource and whichever
//! member names the address, and it is served the moment one is attached. "You're trying to
//! associate an Elastic IP address with a VPC that has no internet gateway attached" is the
//! second thing EC2's error list gives that code for.
//!
//! The rows differ along what the rule generalises over: whether the request names a network
//! interface or an instance, whether it names the address by allocation id or by public
//! address, and which network the resource sits in — a VPC of the caller's own with no gateway,
//! the same VPC once a gateway is attached, and the Region's default VPC, which "comes with an
//! internet gateway" and so serves the association from the start.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch here uses; `RunInstances` only needs one that resolves.
const AMI: &str = "ami-0123456789abcdef0";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A VPC of the caller's own with a subnet in it: the two ids.
async fn network(case: &str, router: &MultiServiceRouter, cidr: &str, subnet: &str) -> (String, String) {
    let created = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    let vpc_id = member(case, &created, "vpcId");
    let subnet_body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", subnet)],
    )
    .await;
    let subnet_id = member(case, &subnet_body, "subnetId");
    (vpc_id, subnet_id)
}

/// One Elastic IP address the account holds, as `(allocation id, public address)`.
async fn allocate(case: &str, router: &MultiServiceRouter) -> (String, String) {
    let body = ok(case, router, "AllocateAddress", &[]).await;
    (
        member(case, &body, "allocationId"),
        member(case, &body, "publicIp"),
    )
}

/// One network interface in `subnet_id`, as its id.
async fn create_interface(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[param("SubnetId", subnet_id)],
    )
    .await;
    member(case, &body, "networkInterfaceId")
}

/// One instance launched into `subnet_id` (or the default VPC when none is named), as its id.
async fn launch(case: &str, router: &MultiServiceRouter, subnet_id: Option<&str>) -> String {
    let mut params = vec![
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    if let Some(subnet_id) = subnet_id {
        params.push(param("SubnetId", subnet_id));
    }
    let body = ok(case, router, "RunInstances", &params).await;
    member(case, &body, "instanceId")
}

/// Attach a fresh internet gateway to `vpc_id`, as the gateway's id.
async fn attach_gateway(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let created = ok(case, router, "CreateInternetGateway", &[]).await;
    let internet_gateway_id = member(case, &created, "internetGatewayId");
    ok(
        case,
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", vpc_id),
        ],
    )
    .await;
    internet_gateway_id
}

/// How a row names the resource the address is to be associated with.
enum Target {
    /// `NetworkInterfaceId`: the normal path for a secondary interface.
    Interface,
    /// `InstanceId`, which the service resolves to the instance's primary interface.
    Instance,
}

/// How a row names the address.
enum Named {
    /// `AllocationId`, "the allocation ID. This is required."
    Allocation,
    /// `PublicIp`, the deprecated way of naming the SAME address.
    PublicIp,
}

/// THE RULE: while the resource's VPC has no internet gateway, the association is refused
/// with `Gateway.NotAttached` naming that VPC — and the same call is served once one is
/// attached, whichever way the request named the resource and the address.
#[tokio::test]
async fn an_association_needs_an_internet_gateway_on_the_resources_network() {
    // (case, how the resource is named, how the address is named)
    let rows: Vec<(&str, Target, Named)> = vec![
        (
            "a network interface, by allocation id",
            Target::Interface,
            Named::Allocation,
        ),
        (
            "a network interface, by public address",
            Target::Interface,
            Named::PublicIp,
        ),
        (
            "an instance, by allocation id",
            Target::Instance,
            Named::Allocation,
        ),
        (
            "an instance, by public address",
            Target::Instance,
            Named::PublicIp,
        ),
    ];

    for (index, (case, target, named)) in rows.into_iter().enumerate() {
        let (router, _engine) = super::ec2_query::emulator();
        let (vpc_id, subnet_id) = network(
            case,
            &router,
            &format!("10.{}.0.0/16", 170 + index),
            &format!("10.{}.1.0/24", 170 + index),
        )
        .await;
        let (allocation_id, public_ip) = allocate(case, &router).await;
        let target_member = match target {
            Target::Interface => param(
                "NetworkInterfaceId",
                &create_interface(case, &router, &subnet_id).await,
            ),
            Target::Instance => {
                param("InstanceId", &launch(case, &router, Some(&subnet_id)).await)
            }
        };
        let address_member = match named {
            Named::Allocation => param("AllocationId", &allocation_id),
            Named::PublicIp => param("PublicIp", &public_ip),
        };
        let params = vec![address_member, target_member];

        // No gateway yet: the association is refused, and the address stays associated with
        // nothing.
        let (status, body) = call(&router, "AssociateAddress", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 while the VPC has no gateway, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            "Gateway.NotAttached",
            "case {case:?}: expected Gateway.NotAttached, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Network {vpc_id} is not attached to any internet gateway"),
            "case {case:?}: expected the sentence naming the VPC from state, got {body}"
        );
        let described = ok(
            case,
            &router,
            "DescribeAddresses",
            &[param("AllocationId.1", &allocation_id)],
        )
        .await;
        assert!(
            elements(&described, "associationId").is_empty(),
            "case {case:?}: the refusal must have associated nothing, got {described}"
        );

        // Attach a gateway and the SAME call is served.
        attach_gateway(case, &router, &vpc_id).await;
        let associated = ok(case, &router, "AssociateAddress", &params).await;
        let association_id = member(case, &associated, "associationId");
        let described = ok(
            case,
            &router,
            "DescribeAddresses",
            &[param("AllocationId.1", &allocation_id)],
        )
        .await;
        assert_eq!(
            element(&described, "associationId").as_deref(),
            Some(association_id.as_str()),
            "case {case:?}: expected the describe to report the association the call minted, \
             got {described}"
        );
    }
}

/// The Region's default VPC "comes with an internet gateway", so an instance launched into it
/// serves the association from the start — the same rule, read on a network that already has
/// what the rows above had to attach.
#[tokio::test]
async fn the_default_vpc_already_has_the_gateway_the_rule_asks_for() {
    let case = "an instance of the default VPC";
    let (router, _engine) = super::ec2_query::emulator();
    let (allocation_id, _) = allocate(case, &router).await;
    let instance_id = launch(case, &router, None).await;
    let associated = ok(
        case,
        &router,
        "AssociateAddress",
        &[
            param("AllocationId", &allocation_id),
            param("InstanceId", &instance_id),
        ],
    )
    .await;
    let association_id = member(case, &associated, "associationId");
    assert!(
        !association_id.is_empty(),
        "case {case:?}: expected an association id, got {associated}"
    );
}

/// The gateway has to be attached to the resource's OWN network: a gateway attached to another
/// VPC of the account leaves this one with none, so the refusal stands.
#[tokio::test]
async fn a_gateway_on_another_network_does_not_serve_this_one() {
    let case = "a gateway attached to another VPC";
    let (router, _engine) = super::ec2_query::emulator();
    let (vpc_id, subnet_id) = network(case, &router, "10.180.0.0/16", "10.180.1.0/24").await;
    let (other_vpc, _) = network(case, &router, "10.181.0.0/16", "10.181.1.0/24").await;
    attach_gateway(case, &router, &other_vpc).await;
    let (allocation_id, _) = allocate(case, &router).await;
    let network_interface_id = create_interface(case, &router, &subnet_id).await;
    let (status, body) = call(
        &router,
        "AssociateAddress",
        &[
            param("AllocationId", &allocation_id),
            param("NetworkInterfaceId", &network_interface_id),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        "Gateway.NotAttached",
        "case {case:?}: expected Gateway.NotAttached, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Network {vpc_id} is not attached to any internet gateway"),
        "case {case:?}: expected the sentence to name the resource's own VPC, got {body}"
    );
}

/// And the precondition is read off the store each time: a network whose gateway has been
/// DETACHED is a network with none again.
#[tokio::test]
async fn a_detached_gateway_takes_the_association_away_again() {
    let case = "a VPC whose gateway was detached";
    let (router, _engine) = super::ec2_query::emulator();
    let (vpc_id, subnet_id) = network(case, &router, "10.190.0.0/16", "10.190.1.0/24").await;
    let internet_gateway_id = attach_gateway(case, &router, &vpc_id).await;
    let network_interface_id = create_interface(case, &router, &subnet_id).await;
    let (first_allocation, _) = allocate(case, &router).await;
    ok(
        case,
        &router,
        "AssociateAddress",
        &[
            param("AllocationId", &first_allocation),
            param("NetworkInterfaceId", &network_interface_id),
        ],
    )
    .await;

    ok(
        case,
        &router,
        "DetachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let (second_allocation, _) = allocate(case, &router).await;
    let (status, body) = call(
        &router,
        "AssociateAddress",
        &[
            param("AllocationId", &second_allocation),
            param("NetworkInterfaceId", &network_interface_id),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 after the detach, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        "Gateway.NotAttached",
        "case {case:?}: expected Gateway.NotAttached after the detach, got {body}"
    );
}
