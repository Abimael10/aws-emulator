// SPDX-License-Identifier: CC0-1.0

//! The service-linked role an Auto Scaling group is bound to.
//!
//! The rule: an Auto Scaling group is BOUND to the service-linked role Amazon EC2 Auto
//! Scaling calls other Amazon Web Services under when the group is created — the role the
//! request named, or, when it named none, the account's own default service-linked role for
//! this service ("by default, Amazon EC2 Auto Scaling uses a service-linked role named
//! `AWSServiceRoleForAutoScaling`, which it creates if it does not exist",
//! `CreateAutoScalingGroupType$ServiceLinkedRoleARN`) — so every read of the group reports
//! the role in force rather than an absent member, and a later update changes it only when
//! it names one.
//!
//! The table below spans what that rule generalises over: whether the create names a role at
//! all, whether the role it names is the default one, a role of the account at the root path
//! or under a path, or another service's own service-linked role, and whether the group is
//! placed by Availability Zone or by subnet. The recorded case — a create naming no role — is
//! one row of it.
//!
//! Auto Scaling, Amazon EC2 and IAM are mounted on ONE engine and driven over their own wire
//! protocols, so the roles these assertions resolve against are the ones IAM itself reports:
//! the ARN a group names is checked to be a role a client can read back under that name.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam_service;

use crate::support::{
    borrowed, create_launch_template, default_service_linked_role_arn, iam_query, p, query,
    seed_subnets, tag_value, tag_values, Placement, DEFAULT_SERVICE_LINKED_ROLE_NAME,
    DEFAULT_SERVICE_LINKED_ROLE_PATH,
};

/// Another service's own service-linked role, used by a row that names a role which is NOT
/// this service's default: Amazon EC2 Fleet's, as IAM publishes it.
const OTHER_SERVICE_ROLE_NAME: &str = "AWSServiceRoleForEC2Fleet";

/// The trust policy the roles this suite creates in IAM carry, which is all `CreateRole`
/// asks of one.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"autoscaling.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The launch template every group in this suite launches from.
const LAUNCH_TEMPLATE: &str = "lt-service-linked-role";

/// How the create names the service-linked role.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// The request carries no `ServiceLinkedRoleARN`.
    Nothing,
    /// It names the account's default service-linked role for this service.
    DefaultRole,
    /// It names a role of the account, created over IAM's wire at the root path.
    HeldRole,
    /// The same, filed under a path, so the binding is not a property of the default path.
    HeldRoleUnderPath,
    /// It names ANOTHER service's own service-linked role.
    OtherServicesRole,
}

/// One row: the case, what the create names, and how the group is placed.
struct Row {
    case: &'static str,
    named: Named,
    placement: Placement,
}

const ROWS: &[Row] = &[
    Row {
        case: "no role named, placed by Availability Zone",
        named: Named::Nothing,
        placement: Placement::Zone,
    },
    Row {
        case: "no role named, placed by subnet",
        named: Named::Nothing,
        placement: Placement::Subnet,
    },
    Row {
        case: "the default service-linked role named explicitly",
        named: Named::DefaultRole,
        placement: Placement::Zone,
    },
    Row {
        case: "a role of the account at the root path",
        named: Named::HeldRole,
        placement: Placement::Zone,
    },
    Row {
        case: "a role of the account under a path",
        named: Named::HeldRoleUnderPath,
        placement: Placement::Subnet,
    },
    Row {
        case: "another service's service-linked role",
        named: Named::OtherServicesRole,
        placement: Placement::Zone,
    },
];

/// One emulator with the three services on ONE engine: the group is Auto Scaling's, the
/// subnet and launch template Amazon EC2's, and the role IAM's, and one engine is what makes
/// the three views one account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Create one role of the account over IAM's own wire and answer the ARN IAM minted for it —
/// never one this suite spells out.
async fn create_role(router: &MultiServiceRouter, role_name: &str, path: &str) -> String {
    let (status, body) = iam_query(
        router,
        "CreateRole",
        &[
            p("RoleName", role_name),
            p("Path", path),
            p("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {role_name:?} under {path:?}: expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "Arn");
    assert!(
        !arn.is_empty(),
        "CreateRole must report the role's ARN, got {body}"
    );
    arn
}

/// The ARN IAM reports for the role it holds under `role_name`, read back through IAM's own
/// `GetRole`.
async fn role_arn_from_iam(router: &MultiServiceRouter, role_name: &str) -> (String, String) {
    let (status, body) = iam_query(router, "GetRole", &[p("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "GetRole {role_name:?}: expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "Arn");
    assert!(
        !arn.is_empty(),
        "GetRole {role_name:?} must report the role's ARN, got {body}"
    );
    (arn, tag_value(&body, "Path"))
}

/// The `ServiceLinkedRoleARN` the group `name` reports, read back over the wire.
async fn reported_role(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?}: expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_values(&body, "AutoScalingGroupName"),
        vec![name.to_string()],
        "DescribeAutoScalingGroups must report the one group asked for, got {body}"
    );
    tag_value(&body, "ServiceLinkedRoleARN")
}

/// Create one group that launches from [`LAUNCH_TEMPLATE`], holding no instances, placed as
/// `placement` says and naming `role` when the row names one.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    placement: Placement,
    zone: &str,
    subnet: &str,
    role: Option<&str>,
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
    ];
    match placement {
        Placement::Zone => params.push(p("AvailabilityZones.member.1", zone)),
        Placement::Subnet => params.push(p("VPCZoneIdentifier", subnet)),
    }
    if let Some(role) = role {
        params.push(p("ServiceLinkedRoleARN", role));
    }
    query(router, "CreateAutoScalingGroup", &borrowed(&params)).await
}

#[tokio::test]
async fn a_group_reports_the_service_linked_role_it_is_bound_to() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = emulator();
        let subnet = seed_subnets(&engine, 1).remove(0);
        // This suite names the template by name, so the id and version the helper answers
        // are not read here.
        let _ = create_launch_template(&router, LAUNCH_TEMPLATE).await;

        let expected = match row.named {
            Named::Nothing | Named::DefaultRole => default_service_linked_role_arn(),
            Named::HeldRole => create_role(&router, "asg-role", "/").await,
            Named::HeldRoleUnderPath => {
                create_role(&router, "asg-role-under-path", "/service-role/").await
            }
            Named::OtherServicesRole => {
                role_arn_from_iam(&router, OTHER_SERVICE_ROLE_NAME).await.0
            }
        };
        let named = match row.named {
            Named::Nothing => None,
            _ => Some(expected.clone()),
        };

        let (status, body) = create_group(
            &router,
            "asg-slr",
            row.placement,
            &subnet.zone,
            &subnet.id,
            named.as_deref(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );

        let reported = reported_role(&router, "asg-slr").await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the group to report {expected:?} as the role it is bound \
             to, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn the_default_role_is_the_one_iam_holds_for_this_service() {
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1).remove(0);
    let _ = create_launch_template(&router, LAUNCH_TEMPLATE).await;

    let (status, body) = create_group(
        &router,
        "asg-default-role",
        Placement::Subnet,
        &subnet.zone,
        &subnet.id,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let reported = reported_role(&router, "asg-default-role").await;

    // The ARN the group names is a role IAM itself reports under the published name, filed
    // under the path IAM reserves for a service-linked role: a client that reads the group's
    // role to grant it something can resolve that role.
    let (arn, path) = role_arn_from_iam(&router, DEFAULT_SERVICE_LINKED_ROLE_NAME).await;
    assert_eq!(
        reported, arn,
        "case \"the default role\": expected the group to name the role IAM reports under \
         {DEFAULT_SERVICE_LINKED_ROLE_NAME:?} ({arn:?}), got {reported:?}"
    );
    assert_eq!(
        path, DEFAULT_SERVICE_LINKED_ROLE_PATH,
        "case \"the default role\": expected IAM to file it under \
         {DEFAULT_SERVICE_LINKED_ROLE_PATH:?}, got {path:?}"
    );
    assert_eq!(
        reported,
        default_service_linked_role_arn(),
        "case \"the default role\": expected the published ARN, got {reported:?}"
    );
}

#[tokio::test]
async fn an_update_changes_the_bound_role_only_when_it_names_one() {
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1).remove(0);
    let _ = create_launch_template(&router, LAUNCH_TEMPLATE).await;
    let held = create_role(&router, "asg-updated-role", "/").await;

    let (status, body) = create_group(
        &router,
        "asg-update-role",
        Placement::Zone,
        &subnet.zone,
        &subnet.id,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup expected 200, got {status}: {body}"
    );

    // (case, the `ServiceLinkedRoleARN` the update names, the role in force after it)
    let steps: Vec<(&str, Option<String>, String)> = vec![
        (
            "an update that names no role",
            None,
            default_service_linked_role_arn(),
        ),
        (
            "an update that names a role of the account",
            Some(held.clone()),
            held.clone(),
        ),
        (
            "another update that names no role",
            None,
            held.clone(),
        ),
    ];
    for (case, named, expected) in steps {
        let mut params = vec![
            p("AutoScalingGroupName", "asg-update-role"),
            // One member every step carries, so each is a real update even when it names
            // no role.
            p("MaxSize", "2"),
        ];
        if let Some(named) = named.as_deref() {
            params.push(p("ServiceLinkedRoleARN", named));
        }
        let (status, body) =
            query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
        );
        let reported = reported_role(&router, "asg-update-role").await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the role in force to be {expected:?}, got {reported:?}"
        );
    }
}
