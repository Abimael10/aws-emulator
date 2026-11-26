// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for which RESOURCE a VPC Block Public Access exclusion reports, and for
//! `DeleteVpcBlockPublicAccessExclusion`, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries `Action=…`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test are:
//!
//! 1. an exclusion's `ResourceArn` is the ARN of the resource it EXEMPTS — the VPC or the
//!    subnet the create named — in the Region the request was signed for, and every operation
//!    that reports the exclusion (the create, the describe, the modify and the delete) reports
//!    that one ARN;
//! 2. a delete leaves the exclusion in the non-terminal `delete-in-progress` state carrying
//!    the instant it was deleted, the CLOCK takes it to `delete-complete`, and the exclusion is
//!    then gone for every later reader — the describe no longer reports it, its id no longer
//!    resolves, and the resource it exempted can be given an exclusion again;
//! 3. an exclusion id this account holds none under is refused with this resource's own
//!    not-found code naming that id, and a delete of an id an earlier call already deleted gets
//!    the same refusal.
//!
//! The rows differ along what the rules generalise over: which KIND of resource the exclusion
//! covers (a VPC and a subnet), which of several resources of that kind, and the mode the
//! exclusion is created with — so no row is the specification.

use std::time::Duration;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::param;

/// The Region every call here is signed for; an exclusion's ARN names the Region the request
/// was signed for, so the expected ARNs are built for this one.
const REGION: &str = "us-west-2";

/// How long a clock-owed transition takes to fall due: the service's own span, plus a margin
/// so a slow machine cannot read the state before it is due.
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

/// A VPC of the account, as its id.
async fn vpc(case: &str, router: &MultiServiceRouter, cidr: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"))
}

/// A subnet of `vpc_id`, as its id.
async fn subnet(case: &str, router: &MultiServiceRouter, vpc_id: &str, cidr: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", vpc_id), param("CidrBlock", cidr)],
    )
    .await;
    element(&body, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId in {body}"))
}

/// The ARN of the resource `resource_type`/`resource_id` in [`REGION`], as the emulator's own
/// account and partition write one — the same construction every ARN of this emulator takes,
/// so the expectation is not a second spelling of it.
fn resource_arn(resource_type: &str, resource_id: &str) -> String {
    engine::emulator_arn("ec2", REGION, &format!("{resource_type}/{resource_id}"))
}

/// The `<resourceArn>` of the one exclusion a body reports.
fn reported_arn(case: &str, body: &str) -> String {
    element(body, "resourceArn")
        .unwrap_or_else(|| panic!("case {case:?}: no resourceArn in {body}"))
}

/// RULE 1: the exclusion reports the ARN of the resource it exempts, on the create, on the
/// describe, on the modify and on the delete — never its own id.
#[tokio::test]
async fn an_exclusion_reports_the_arn_of_the_resource_it_exempts() {
    // (case, which member names the target, the resource type its ARN carries, the mode)
    let cases: [(&str, bool, &str, &str); 4] = [
        ("a VPC, bidirectional", true, "vpc", "allow-bidirectional"),
        ("a VPC, egress only", true, "vpc", "allow-egress"),
        ("a subnet, bidirectional", false, "subnet", "allow-bidirectional"),
        ("a subnet, egress only", false, "subnet", "allow-egress"),
    ];

    for (case, by_vpc, resource_type, mode) in cases {
        let (router, _engine) = super::ec2_query::emulator();
        // Two VPCs and two subnets, so the ARN under test names the resource the create
        // actually asked for rather than the only one the account holds.
        let first_vpc = vpc(case, &router, "10.70.0.0/16").await;
        let second_vpc = vpc(case, &router, "10.71.0.0/16").await;
        let _first_subnet = subnet(case, &router, &second_vpc, "10.71.0.0/24").await;
        let second_subnet = subnet(case, &router, &second_vpc, "10.71.1.0/24").await;

        let (target_member, target_id) = if by_vpc {
            ("VpcId", first_vpc.clone())
        } else {
            ("SubnetId", second_subnet.clone())
        };
        let expected = resource_arn(resource_type, &target_id);

        let created = ok(
            case,
            &router,
            "CreateVpcBlockPublicAccessExclusion",
            &[
                param(target_member, &target_id),
                param("InternetGatewayExclusionMode", mode),
            ],
        )
        .await;
        assert_eq!(
            reported_arn(case, &created),
            expected,
            "case {case:?}: the create must report the excluded resource's ARN, got {created}"
        );
        let exclusion_id = element(&created, "exclusionId")
            .unwrap_or_else(|| panic!("case {case:?}: no exclusionId in {created}"));

        let described = ok(
            case,
            &router,
            "DescribeVpcBlockPublicAccessExclusions",
            &[param("ExclusionId.1", &exclusion_id)],
        )
        .await;
        assert_eq!(
            reported_arn(case, &described),
            expected,
            "case {case:?}: the describe must report the same ARN, got {described}"
        );

        let modified = ok(
            case,
            &router,
            "ModifyVpcBlockPublicAccessExclusion",
            &[
                param("ExclusionId", &exclusion_id),
                param("InternetGatewayExclusionMode", "allow-bidirectional"),
            ],
        )
        .await;
        assert_eq!(
            reported_arn(case, &modified),
            expected,
            "case {case:?}: a modify must not change which resource is exempted, got {modified}"
        );

        let deleted = ok(
            case,
            &router,
            "DeleteVpcBlockPublicAccessExclusion",
            &[param("ExclusionId", &exclusion_id)],
        )
        .await;
        assert_eq!(
            reported_arn(case, &deleted),
            expected,
            "case {case:?}: the delete must report the same ARN, got {deleted}"
        );
    }
}

/// RULE 2: the delete leaves the exclusion going away, the clock finishes it, and the
/// exclusion is then gone for every later reader — and the resource it exempted can be given
/// an exclusion again.
#[tokio::test]
async fn a_deleted_exclusion_goes_away_and_frees_the_resource_it_exempted() {
    let case = "a subnet's exclusion is deleted";
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = vpc(case, &router, "10.72.0.0/16").await;
    let subnet_id = subnet(case, &router, &vpc_id, "10.72.3.0/24").await;

    let created = ok(
        case,
        &router,
        "CreateVpcBlockPublicAccessExclusion",
        &[
            param("SubnetId", &subnet_id),
            param("InternetGatewayExclusionMode", "allow-egress"),
        ],
    )
    .await;
    let exclusion_id = element(&created, "exclusionId")
        .unwrap_or_else(|| panic!("case {case:?}: no exclusionId in {created}"));

    // A second exclusion for the same subnet is a conflict while the first one is there.
    let (status, body) = call(
        &router,
        REGION,
        "CreateVpcBlockPublicAccessExclusion",
        &[
            param("SubnetId", &subnet_id),
            param("InternetGatewayExclusionMode", "allow-egress"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a second exclusion for one subnet is refused, got {status} {body}"
    );

    let deleted = ok(
        case,
        &router,
        "DeleteVpcBlockPublicAccessExclusion",
        &[param("ExclusionId", &exclusion_id)],
    )
    .await;
    assert_eq!(
        element(&deleted, "state").as_deref(),
        Some(ec2::state::VPC_BPA_EXCLUSION_STATE_DELETE_IN_PROGRESS),
        "case {case:?}: a caller reading straight after the delete still sees it going away, \
         got {deleted}"
    );
    assert!(
        element(&deleted, "deletionTimestamp").is_some(),
        "case {case:?}: a deleted exclusion reports when it was deleted, got {deleted}"
    );

    past_the_transition().await;

    // The clock finished the deletion, so the exclusion is gone: the unfiltered describe no
    // longer reports it and its id no longer resolves.
    let listed = ok(
        case,
        &router,
        "DescribeVpcBlockPublicAccessExclusions",
        &[],
    )
    .await;
    assert!(
        !listed.contains(exclusion_id.as_str()),
        "case {case:?}: the describe must not report the deleted {exclusion_id}, got {listed}"
    );
    let (status, body) = call(
        &router,
        REGION,
        "DeleteVpcBlockPublicAccessExclusion",
        &[param("ExclusionId", &exclusion_id)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a second delete is refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::INVALID_VPC_BPA_EXCLUSION_ID_NOT_FOUND,
        "case {case:?}: expected this resource's own not-found code, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "The {resource_type} ID '{exclusion_id}' does not exist",
            resource_type = ec2::state::RESOURCE_TYPE_VPC_BLOCK_PUBLIC_ACCESS_EXCLUSION
        ),
        "case {case:?}: the refusal names the id the request carried, got {body}"
    );

    // And the subnet is free again: the exclusion that conflicted is gone.
    let again = ok(
        case,
        &router,
        "CreateVpcBlockPublicAccessExclusion",
        &[
            param("SubnetId", &subnet_id),
            param("InternetGatewayExclusionMode", "allow-bidirectional"),
        ],
    )
    .await;
    assert_eq!(
        reported_arn(case, &again),
        resource_arn("subnet", &subnet_id),
        "case {case:?}: the new exclusion exempts the same subnet, got {again}"
    );
}

/// RULE 3: every exclusion id the account holds none under is refused with this resource's own
/// not-found code naming that id. The rows are the forms such a value can take.
#[tokio::test]
async fn an_unresolvable_exclusion_id_is_refused_with_the_not_found_code() {
    let (router, _engine) = super::ec2_query::emulator();
    let prefix = ec2::state::VPC_BPA_EXCLUSION_ID_PREFIX;
    let cases: [(&str, String); 4] = [
        ("the long form", format!("{prefix}-0123456789abcdef0")),
        ("the all-zero placeholder", format!("{prefix}-00000000000000000")),
        ("another resource's id", "vpc-0123456789abcdef0".to_string()),
        ("free text", "not-an-exclusion".to_string()),
    ];
    for (case, exclusion_id) in cases {
        for action in [
            "DeleteVpcBlockPublicAccessExclusion",
            "ModifyVpcBlockPublicAccessExclusion",
        ] {
            let mut params = vec![param("ExclusionId", &exclusion_id)];
            if action == "ModifyVpcBlockPublicAccessExclusion" {
                params.push(param("InternetGatewayExclusionMode", "allow-egress"));
            }
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_VPC_BPA_EXCLUSION_ID_NOT_FOUND,
                "case {case:?}: expected the not-found code from {action}, got {body}"
            );
            assert!(
                error_message(&body).contains(exclusion_id.as_str()),
                "case {case:?}: the refusal from {action} must name {exclusion_id}, got {body}"
            );
        }
    }
}
