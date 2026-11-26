// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH security groups a network interface of a VPC can be given —
//! `CreateNetworkInterface`, `ModifyNetworkInterfaceAttribute` and `RunInstances` against a
//! group `AssociateSecurityGroupVpc` associated with another VPC — driven over the wire the
//! service actually speaks: `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rule under test: a security group can be used by an interface in a VPC when it was
//! CREATED in that VPC, or when `AssociateSecurityGroupVpc` has associated it with that VPC
//! and the association has reached `associated` — which is what `GetSecurityGroupsForVpc`
//! reports as the groups that "can be associated … with network interfaces in the specified
//! VPC". A group neither created in nor associated with the VPC is no group of it, and while
//! an interface of that VPC still uses an associated group, the disassociate is refused with
//! the association's own in-use code.
//!
//! The rows differ along what the rule generalises over: WHICH operation applies the group (a
//! create, a modify of the same member, and a launch), and whether the group reaches the VPC
//! by having been created in it or by an association.

use std::time::Duration;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// How long a clock-owed transition takes to fall due: the service's own span, plus a margin
/// so a slow machine cannot read the association before it is established.
async fn past_the_transition() {
    tokio::time::sleep(ec2::state::TRANSITION_DELAY + Duration::from_millis(400)).await;
}

/// One call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A VPC with one subnet, as (VPC id, subnet id).
async fn network(
    case: &str,
    router: &MultiServiceRouter,
    cidr: &str,
    subnet_cidr: &str,
) -> (String, String) {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    let vpc_id = element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId"));
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", subnet_cidr)],
    )
    .await;
    let subnet_id =
        element(&body, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId"));
    (vpc_id, subnet_id)
}

/// A security group of `vpc_id` named `name`, as its id.
async fn security_group(case: &str, router: &MultiServiceRouter, vpc_id: &str, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            param("VpcId", vpc_id),
            param("GroupName", name),
            param("GroupDescription", "a group this suite applies"),
        ],
    )
    .await;
    element(&body, "groupId").unwrap_or_else(|| panic!("case {case:?}: no groupId in {body}"))
}

/// The id of one AMI the published catalog holds, which a launch needs: the catalogue is what
/// a client without an AMI id of its own reads.
async fn published_image(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "DescribeImages", &[param("Owner.1", "amazon")]).await;
    elements(&body, "imageId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the published catalog holds an AMI"))
}

/// RULE: a group associated with a VPC is usable by that VPC's interfaces as soon as the
/// association is established — through every operation that applies a group to one — and a
/// group that reaches the VPC neither way is refused.
#[tokio::test]
async fn a_group_is_usable_in_the_vpc_it_was_created_in_or_associated_with() {
    let case = "a group associated with a second VPC";
    let (router, _engine) = super::ec2_query::emulator();
    let (home_vpc, _home_subnet) = network(case, &router, "10.50.0.0/16", "10.50.1.0/24").await;
    let (other_vpc, other_subnet) = network(case, &router, "10.51.0.0/16", "10.51.1.0/24").await;
    // The group the association makes usable in the other VPC, and one that stays at home.
    let shared = security_group(case, &router, &home_vpc, "shared").await;
    let unshared = security_group(case, &router, &home_vpc, "unshared").await;

    // Before the association, neither group of the home VPC can be applied to an interface of
    // the other one.
    for group in [shared.as_str(), unshared.as_str()] {
        let (status, body) = call(
            &router,
            REGION,
            "CreateNetworkInterface",
            &[
                param("SubnetId", &other_subnet),
                param("SecurityGroupId.1", group),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a group of another VPC is no group of this one, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_GROUP_NOT_FOUND,
            "case {case:?}: expected {}, got {body}",
            ec2::state::INVALID_GROUP_NOT_FOUND
        );
    }

    ok(
        case,
        &router,
        "AssociateSecurityGroupVpc",
        &[param("GroupId", &shared), param("VpcId", &other_vpc)],
    )
    .await;
    past_the_transition().await;

    // The association is established, so `GetSecurityGroupsForVpc` reports the group as one
    // the VPC's interfaces can use — and every operation that applies a group agrees.
    let usable = ok(
        case,
        &router,
        "GetSecurityGroupsForVpc",
        &[param("VpcId", &other_vpc)],
    )
    .await;
    assert!(
        elements(&usable, "groupId").contains(&shared),
        "case {case:?}: the associated group is usable in {other_vpc}, got {usable}"
    );
    assert!(
        !elements(&usable, "groupId").contains(&unshared),
        "case {case:?}: the unassociated group is not, got {usable}"
    );

    // 1. the create that the recorded scenario could not even set up
    let created = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", &other_subnet),
            param("SecurityGroupId.1", &shared),
        ],
    )
    .await;
    let interface_id = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkInterfaceId in {created}"));
    assert!(
        elements(&created, "groupId").contains(&shared),
        "case {case:?}: the interface reports the group it was given, got {created}"
    );

    // 2. the modify of the same member, on an interface created without the group
    let plain = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &other_subnet)],
    )
    .await;
    let plain_id = element(&plain, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkInterfaceId in {plain}"));
    ok(
        case,
        &router,
        "ModifyNetworkInterfaceAttribute",
        &[
            param("NetworkInterfaceId", &plain_id),
            param("SecurityGroupId.1", &shared),
        ],
    )
    .await;
    let described = ok(
        case,
        &router,
        "DescribeNetworkInterfaces",
        &[param("NetworkInterfaceId.1", &plain_id)],
    )
    .await;
    assert!(
        elements(&described, "groupId").contains(&shared),
        "case {case:?}: the modify applied the associated group, got {described}"
    );

    // 3. the launch, whose primary interface sits in the same subnet
    let image_id = published_image(case, &router).await;
    let launched = ok(
        case,
        &router,
        "RunInstances",
        &[
            param("ImageId", &image_id),
            param("InstanceType", "t3.micro"),
            param("MinCount", "1"),
            param("MaxCount", "1"),
            param("SubnetId", &other_subnet),
            param("SecurityGroupId.1", &shared),
        ],
    )
    .await;
    assert!(
        elements(&launched, "groupId").contains(&shared),
        "case {case:?}: the launch applied the associated group, got {launched}"
    );

    // And while an interface of that VPC still uses the group, the disassociate is refused
    // with the association's own in-use code — not with a generic dependency refusal.
    let (status, body) = call(
        &router,
        REGION,
        "DisassociateSecurityGroupVpc",
        &[param("GroupId", &shared), param("VpcId", &other_vpc)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 while {interface_id} holds the group, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::SECURITY_GROUP_VPC_ASSOCIATION_IN_USE,
        "case {case:?}: expected {}, got {body}",
        ec2::state::SECURITY_GROUP_VPC_ASSOCIATION_IN_USE
    );
}
