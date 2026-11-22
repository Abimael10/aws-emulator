// SPDX-License-Identifier: CC0-1.0

//! Managed-node registration FOLLOWS the instance profile's role: a host whose role is
//! granted the registration call after the launch joins the managed-node list, and one whose
//! role loses it leaves — each observable through `DescribeInstanceInformation` filtered on
//! the node id.
//!
//! RULE under test: whether an Amazon EC2 instance is reported as a Systems Manager managed
//! node is re-decided on every read from what its instance profile's role allows AT THAT
//! MOMENT, so a role that gains the registration grant after its instance was launched makes
//! that instance appear `Online`, and a role that loses it makes the same instance disappear
//! — the image the host was launched from and the network it sits in do not enter that
//! decision beyond letting the agent's own call out.
//!
//! The rows differ along what the rule generalises over: WHEN the grant reaches the role
//! (before the launch, after it, never, and withdrawn after the node had registered) and the
//! published Amazon Linux family the host was launched from. Every host sits in the same
//! self-built public subnet — its own VPC, an internet gateway, a default route, public
//! addressing on launch and a security group of its own — which is the network a client
//! builds for a host it means to manage.
//!
//! Nothing here waits a fixed time: every span another component's clock owes is a stored
//! due instant, so each step LOOKS AGAIN until the condition holds or the deadline passes.
//! The hosts whose grant arrives late (or never) are launched BEFORE the ones granted at
//! launch, so by the time the first registration has been observed every earlier host's own
//! span has passed too — which is what makes "it is not reported" an answer rather than a
//! race.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    create_profile, ec2, element, emulator, iam_ok, image_id, Grant, Profile, AL2023_PARAMETER,
    DEADLINE, EC2_PRINCIPAL, POLL_PAUSE, SSM_CORE_POLICY,
};
use crate::parameter_store::{call_in_region, REGION};

/// The instance type every host is launched as, a current general-purpose one.
const INSTANCE_TYPE: &str = "t3.micro";

/// The public Systems Manager parameter the Amazon Linux 2 image id is resolved through —
/// the other published Amazon Linux family beside the AL2023 one, so the rule is exercised on
/// two different images (`published_amazon_linux_specs` publishes both aliases).
const AL2_PARAMETER: &str = "/aws/service/ami-amazon-linux-latest/amzn2-ami-hvm-x86_64-gp2";

/// The address range of the VPC the scenario builds for itself.
const VPC_CIDR: &str = "10.0.0.0/16";

/// The address range of the subnet inside it every host is launched into.
const SUBNET_CIDR: &str = "10.0.1.0/24";

/// The destination of the default route that makes that subnet a PUBLIC one.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The `PingStatus` a managed node whose agent is reporting carries
/// (`InstanceInformation$PingStatus`, `PingStatusType`).
const PING_STATUS_ONLINE: &str = "Online";

/// When the registration grant reaches one host's role, relative to the launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timing {
    /// The role carries the grant before the instance is launched.
    BeforeLaunch,
    /// The role is given the grant after the instance is already running.
    AfterLaunch,
    /// The role never carries it.
    Never,
    /// The role carries it at launch and it is taken away once the node has registered.
    WithdrawnAfterRegistering,
}

/// One row of the table: a host launched from `parameter`'s image whose role is granted the
/// registration call at `timing`.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The name of the role and of the instance profile built for this host.
    profile: &'static str,
    /// The public parameter the image it is launched from is resolved through.
    parameter: &'static str,
    timing: Timing,
}

/// The rows, ORDERED so that every host that must not be reported is launched before the
/// ones that must: see the module docs.
const ROWS: &[Row] = &[
    Row {
        case: "an AL2 host whose role never carries the grant",
        profile: "vscan-never-al2",
        parameter: AL2_PARAMETER,
        timing: Timing::Never,
    },
    Row {
        case: "an AL2 host whose role is granted after the launch",
        profile: "vscan-repaired-al2",
        parameter: AL2_PARAMETER,
        timing: Timing::AfterLaunch,
    },
    Row {
        case: "an AL2023 host whose role is granted after the launch",
        profile: "vscan-repaired-al2023",
        parameter: AL2023_PARAMETER,
        timing: Timing::AfterLaunch,
    },
    Row {
        case: "an AL2 host whose role carries the grant at launch",
        profile: "vscan-granted-al2",
        parameter: AL2_PARAMETER,
        timing: Timing::BeforeLaunch,
    },
    Row {
        case: "an AL2023 host whose role carries the grant at launch",
        profile: "vscan-granted-al2023",
        parameter: AL2023_PARAMETER,
        timing: Timing::BeforeLaunch,
    },
    Row {
        case: "an AL2023 host whose role loses the grant after registering",
        profile: "vscan-withdrawn-al2023",
        parameter: AL2023_PARAMETER,
        timing: Timing::WithdrawnAfterRegistering,
    },
];

/// The public subnet every host is launched into, and the security group it carries.
struct PublicSubnet {
    subnet_id: String,
    group_id: String,
}

/// One Amazon EC2 action that must succeed, as its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = ec2(router, REGION, action, parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} {parameters:?} must succeed, got {status} with {body}"
    );
    body
}

/// Build the public subnet a client builds for hosts it means to manage: its own VPC, an
/// internet gateway with a default route to it, public addressing on launch, and one security
/// group of its own.
async fn public_subnet(router: &MultiServiceRouter) -> PublicSubnet {
    let case = "the public subnet";
    let body = ec2_ok(router, case, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reports a vpc id: {body}"));
    let body = ec2_ok(
        router,
        case,
        "CreateSubnet",
        &[("VpcId", vpc_id.as_str()), ("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reports a subnet id: {body}"));
    let body = ec2_ok(router, case, "CreateInternetGateway", &[]).await;
    let gateway_id = element(&body, "internetGatewayId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateInternetGateway reports a gateway id: {body}")
    });
    ec2_ok(
        router,
        case,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway_id.as_str()),
            ("VpcId", vpc_id.as_str()),
        ],
    )
    .await;
    let body = ec2_ok(
        router,
        case,
        "CreateRouteTable",
        &[("VpcId", vpc_id.as_str())],
    )
    .await;
    let route_table_id = element(&body, "routeTableId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRouteTable reports a table id: {body}"));
    ec2_ok(
        router,
        case,
        "CreateRoute",
        &[
            ("RouteTableId", route_table_id.as_str()),
            ("DestinationCidrBlock", DEFAULT_ROUTE),
            ("GatewayId", gateway_id.as_str()),
        ],
    )
    .await;
    ec2_ok(
        router,
        case,
        "AssociateRouteTable",
        &[
            ("RouteTableId", route_table_id.as_str()),
            ("SubnetId", subnet_id.as_str()),
        ],
    )
    .await;
    ec2_ok(
        router,
        case,
        "ModifySubnetAttribute",
        &[
            ("SubnetId", subnet_id.as_str()),
            ("MapPublicIpOnLaunch.Value", "true"),
        ],
    )
    .await;
    let body = ec2_ok(
        router,
        case,
        "CreateSecurityGroup",
        &[
            ("GroupName", "vscan-hosts"),
            ("GroupDescription", "hosts the scan manages"),
            ("VpcId", vpc_id.as_str()),
        ],
    )
    .await;
    let group_id = element(&body, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup reports a group id: {body}"));
    PublicSubnet {
        subnet_id,
        group_id,
    }
}

/// Launch one host of `row` into `subnet` from `image`, as the instance id Amazon EC2
/// answered.
async fn launch(
    router: &MultiServiceRouter,
    row: &Row,
    subnet: &PublicSubnet,
    image: &str,
) -> String {
    let body = ec2_ok(
        router,
        row.case,
        "RunInstances",
        &[
            ("ImageId", image),
            ("InstanceType", INSTANCE_TYPE),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("SubnetId", subnet.subnet_id.as_str()),
            ("SecurityGroupId.1", subnet.group_id.as_str()),
            ("IamInstanceProfile.Name", row.profile),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {:?}: RunInstances reports an instance id: {body}", row.case))
}

/// Every `InstanceInformation` this service reports for the node id `instance_id`, read the
/// way a client asks whether one host is managed — the server-side `InstanceIds` filter.
async fn rows_for(router: &MultiServiceRouter, instance_id: &str) -> Vec<Value> {
    let (status, body) = call_in_region(
        router,
        REGION,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceInformation({instance_id}) must answer, got {status} with {body}"
    );
    body["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Look again until exactly one row is reported for `instance_id`, and answer it.
async fn reported_when_registered(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = rows_for(router, instance_id).await;
        if let Some(row) = reported.first() {
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected exactly one row for {instance_id}, got {reported:?}"
            );
            return row.clone();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to be reported as a managed node, and \
             DescribeInstanceInformation reported none"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Look again until NO row is reported for `instance_id`.
async fn reported_when_withdrawn(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = rows_for(router, instance_id).await;
        if reported.is_empty() {
            return;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to stop being reported once its role lost \
             the grant, got {reported:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Assert one reported row is this host's and reports its agent as reporting.
fn assert_online(case: &str, instance_id: &str, reported: &Value) {
    assert_eq!(
        reported["InstanceId"].as_str(),
        Some(instance_id),
        "case {case:?}: expected the row to be {instance_id}'s, got {reported}"
    );
    assert_eq!(
        reported["PingStatus"].as_str(),
        Some(PING_STATUS_ONLINE),
        "case {case:?}: expected PingStatus {PING_STATUS_ONLINE} for {instance_id}, got \
         {reported}"
    );
}

/// RULE: the managed-node list follows what the host's instance-profile role allows NOW — it
/// holds the hosts whose role carries the registration grant, whenever that grant arrived,
/// and holds no host whose role does not.
#[tokio::test]
async fn the_managed_node_list_follows_the_profile_roles_grant() {
    let router = emulator();
    let subnet = public_subnet(&router).await;

    // One role and instance profile per row, each granted the registration call only where
    // the row says the grant is there before the launch.
    for row in ROWS {
        create_profile(
            &router,
            &Profile {
                name: row.profile,
                trusts: EC2_PRINCIPAL,
                grant: match row.timing {
                    Timing::BeforeLaunch | Timing::WithdrawnAfterRegistering => Grant::AwsManaged,
                    Timing::AfterLaunch | Timing::Never => Grant::None,
                },
            },
        )
        .await;
    }

    let mut launched: Vec<(Row, String)> = Vec::new();
    for row in ROWS {
        let image = image_id(&router, row.parameter).await;
        let instance_id = launch(&router, row, &subnet, &image).await;
        launched.push((*row, instance_id));
    }

    // The hosts whose role already carries the grant: each is reported, and reported Online.
    for (row, instance_id) in &launched {
        if row.timing == Timing::BeforeLaunch || row.timing == Timing::WithdrawnAfterRegistering {
            let reported = reported_when_registered(&router, row.case, instance_id).await;
            assert_online(row.case, instance_id, &reported);
        }
    }

    // Every earlier host's own registration span has passed by now, so the absence of a host
    // whose role carries no grant is an answer rather than a race.
    for (row, instance_id) in &launched {
        if row.timing == Timing::AfterLaunch || row.timing == Timing::Never {
            let reported = rows_for(&router, instance_id).await;
            assert!(
                reported.is_empty(),
                "case {:?}: expected {instance_id} not to be reported while its role carries \
                 no grant, got {reported:?}",
                row.case
            );
        }
    }

    // The repair: the grant reaches the role of a host that is ALREADY running, and the host
    // is rebooted, which is what a client does to make its agent pick the credentials up.
    for (row, instance_id) in &launched {
        if row.timing == Timing::AfterLaunch {
            iam_ok(
                &router,
                row.case,
                "AttachRolePolicy",
                &[("RoleName", row.profile), ("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
            ec2_ok(
                &router,
                row.case,
                "RebootInstances",
                &[("InstanceId.1", instance_id.as_str())],
            )
            .await;
        }
    }
    for (row, instance_id) in &launched {
        if row.timing == Timing::AfterLaunch {
            let reported = reported_when_registered(&router, row.case, instance_id).await;
            assert_online(row.case, instance_id, &reported);
        }
    }

    // The other half of the same rule: a role that LOSES the grant withdraws its host.
    for (row, instance_id) in &launched {
        if row.timing == Timing::WithdrawnAfterRegistering {
            iam_ok(
                &router,
                row.case,
                "DetachRolePolicy",
                &[("RoleName", row.profile), ("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
            reported_when_withdrawn(&router, row.case, instance_id).await;
        }
    }

    // A host whose role never carried the grant is still absent, and the hosts granted at
    // launch are still reported: one role's grant says nothing about another's.
    for (row, instance_id) in &launched {
        match row.timing {
            Timing::Never => {
                let reported = rows_for(&router, instance_id).await;
                assert!(
                    reported.is_empty(),
                    "case {:?}: expected {instance_id} to stay unreported, got {reported:?}",
                    row.case
                );
            }
            Timing::BeforeLaunch => {
                let reported = reported_when_registered(&router, row.case, instance_id).await;
                assert_online(row.case, instance_id, &reported);
            }
            Timing::AfterLaunch | Timing::WithdrawnAfterRegistering => {}
        }
    }
}
