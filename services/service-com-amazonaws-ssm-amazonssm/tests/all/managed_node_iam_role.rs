// SPDX-License-Identifier: CC0-1.0

//! The IAM role a managed node reports, and the filter on it.
//!
//! RULE under test: a managed node reports in `IamRole` the IAM ROLE its agent's credentials
//! come from, under the name IAM holds that role by — for a node an Amazon EC2 instance
//! registered, the role carried by the instance profile the instance runs as, which is
//! neither the profile's own name nor the role's path or ARN. The member is documented as
//! "the role assigned to an Amazon EC2 instance configured with a Systems Manager Quick
//! Setup host management configuration or the role assigned to an on-premises managed node …
//! This call doesn't return the IAM role for *unmanaged* Amazon EC2 instances"
//! (`smithy_query(query="doc", shape="InstanceInformation", member="IamRole")`), so an
//! instance that HAS registered — which is what makes it managed — reports it, and the value
//! the node holds is the one a filter on the `IamRole` key is matched against.
//!
//! Every call goes through the wire a client uses: an `awsJson1_1` `POST /` for Systems
//! Manager, an `ec2Query` form POST for Amazon EC2 and an `awsQuery` form POST for IAM, so
//! each component's state is written and read through the service that owns it.
//!
//! The rows differ along what the rule generalises over: whether the profile's name is its
//! role's, the IAM path the role is held under, how the registration call is granted (the
//! AWS-managed attachment or an inline document), and the length of the role's name, up to
//! the longest the modelled member admits. A second case varies the COUNT: two instances
//! running as one profile report one role between them.
//!
//! Nothing here waits a fixed time: the registration is a stored due instant, so a case
//! LOOKS AGAIN until the node is reported or the harness's generous deadline passes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    emulator, iam_ok, image_id, launch, node_when_registered, trust_policy, AL2023_PARAMETER,
    EC2_PRINCIPAL, GRANTS_REGISTRATION, SSM_CORE_POLICY,
};
use crate::parameter_store::{call_in_region, REGION};

/// The longest value the modelled member admits: `InstanceInformation$IamRole` targets
/// `IamRole`, a string of length 0..64 (`smithy_query(query="shape", shape="IamRole")`), and
/// IAM's own `RoleName` is bounded at 64 too — so a role named right at the bound is a role
/// a node can report in full.
const MAX_IAM_ROLE_LENGTH: usize = 64;

/// A role name exactly [`MAX_IAM_ROLE_LENGTH`] characters long.
const LONGEST_ROLE: &str = "ssm-node-registration-role-with-the-longest-name-iam-admits-abcd";

/// One row: how the account's IAM records are shaped, and the role the node must report.
#[derive(Debug, Clone, Copy)]
struct Case {
    /// What this row is about, named in every assertion it makes.
    case: &'static str,
    /// The IAM path the role is created under.
    path: &'static str,
    /// The name IAM holds the role by — the value the node must report.
    role: &'static str,
    /// The name of the instance profile the instance runs as, which need not be its role's.
    profile: &'static str,
    /// Whether the registration call is granted by attaching the AWS-managed policy, or by
    /// an inline document of the account's own naming that call.
    aws_managed: bool,
}

/// The table.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "a profile and its role named alike",
            path: "/",
            role: "ssm-core",
            profile: "ssm-core",
            aws_managed: true,
        },
        Case {
            case: "a profile whose name is not its role's",
            path: "/",
            role: "node-registration-role",
            profile: "node-registration-profile",
            aws_managed: false,
        },
        Case {
            case: "a role IAM holds under a path",
            path: "/service-role/",
            role: "scoped-ssm-role",
            profile: "scoped-ssm-profile",
            aws_managed: true,
        },
        Case {
            case: "the longest role name the member admits",
            path: "/",
            role: LONGEST_ROLE,
            profile: "longest-role-profile",
            aws_managed: false,
        },
    ]
}

/// Create the role `case` describes and the instance profile carrying it, each through IAM's
/// own API.
async fn create_records(router: &MultiServiceRouter, case: &Case) {
    let trust = trust_policy(EC2_PRINCIPAL);
    iam_ok(
        router,
        case.case,
        "CreateRole",
        &[
            ("RoleName", case.role),
            ("Path", case.path),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    if case.aws_managed {
        iam_ok(
            router,
            case.case,
            "AttachRolePolicy",
            &[("RoleName", case.role), ("PolicyArn", SSM_CORE_POLICY)],
        )
        .await;
    } else {
        iam_ok(
            router,
            case.case,
            "PutRolePolicy",
            &[
                ("RoleName", case.role),
                ("PolicyName", "registration"),
                ("PolicyDocument", GRANTS_REGISTRATION),
            ],
        )
        .await;
    }
    iam_ok(
        router,
        case.case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", case.profile)],
    )
    .await;
    iam_ok(
        router,
        case.case,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", case.profile),
            ("RoleName", case.role),
        ],
    )
    .await;
}

/// The managed nodes this service reports for one `IamRole` filter value, as their node ids.
async fn nodes_with_role(router: &MultiServiceRouter, case: &str, role: &str) -> Vec<String> {
    let (status, body) = call_in_region(
        router,
        REGION,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "IamRole", "Values": [role] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a filter on IamRole {role:?} must be served, got {status} with {body}"
    );
    body["InstanceInformationList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the report carries a node list, got {body}"))
        .iter()
        .map(|node| {
            node["InstanceId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: every node reports an id, got {node}"))
                .to_string()
        })
        .collect()
}

/// The `IamRole` a reported node carries, or `None` for a node reporting none.
fn reported_role(node: &Value) -> Option<&str> {
    node["IamRole"].as_str()
}

/// A registered node reports the role its instance profile carries, and the filter on that
/// key selects it by exactly that value.
#[tokio::test]
async fn a_node_reports_the_role_its_instance_profile_carries() {
    assert_eq!(
        LONGEST_ROLE.len(),
        MAX_IAM_ROLE_LENGTH,
        "the longest-name row must sit exactly at the modelled bound"
    );
    let router = emulator();
    let image = image_id(&router, AL2023_PARAMETER).await;
    let table = rows();
    for case in &table {
        create_records(&router, case).await;
    }
    // Every instance is launched before any node is awaited, so one wait covers them all:
    // each registration is a stored due instant of its own launch.
    let mut launched: Vec<(Case, String)> = Vec::new();
    for case in &table {
        let instance = launch(&router, case.case, REGION, &image, Some(case.profile)).await;
        launched.push((*case, instance));
    }
    for (case, instance) in &launched {
        let node = node_when_registered(&router, case.case, REGION, instance).await;
        assert_eq!(
            reported_role(&node),
            Some(case.role),
            "case {:?}: the node of {instance} must report the role its profile carries \
             ({:?}), got {node}",
            case.case,
            case.role
        );
        // The profile's own name is not the role's name wherever a row made them differ, so
        // reporting the role is observable as such rather than as either name.
        assert_eq!(
            nodes_with_role(&router, case.case, case.role).await,
            vec![instance.clone()],
            "case {:?}: a filter on IamRole {:?} must select exactly {instance}",
            case.case,
            case.role
        );
        if case.profile != case.role {
            assert!(
                nodes_with_role(&router, case.case, case.profile)
                    .await
                    .is_empty(),
                "case {:?}: a filter on the PROFILE name {:?} must select no node",
                case.case,
                case.profile
            );
        }
    }
    assert!(
        nodes_with_role(&router, "a role no node runs as", "never-attached-role")
            .await
            .is_empty(),
        "a filter on a role name no node carries must select none"
    );
}

/// Two instances running as ONE instance profile report one role between them, and the
/// filter on it selects both.
#[tokio::test]
async fn instances_sharing_one_profile_report_one_role() {
    const CASE: &str = "two instances of one profile";
    let shared = Case {
        case: CASE,
        path: "/",
        role: "shared-registration-role",
        profile: "shared-registration-profile",
        aws_managed: true,
    };
    let router = emulator();
    let image = image_id(&router, AL2023_PARAMETER).await;
    create_records(&router, &shared).await;
    let first = launch(&router, CASE, REGION, &image, Some(shared.profile)).await;
    let second = launch(&router, CASE, REGION, &image, Some(shared.profile)).await;
    for instance in [&first, &second] {
        let node = node_when_registered(&router, CASE, REGION, instance).await;
        assert_eq!(
            reported_role(&node),
            Some(shared.role),
            "case {CASE:?}: the node of {instance} must report {:?}, got {node}",
            shared.role
        );
    }
    let mut selected = nodes_with_role(&router, CASE, shared.role).await;
    selected.sort();
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(
        selected, expected,
        "case {CASE:?}: a filter on IamRole {:?} must select both instances",
        shared.role
    );
}
