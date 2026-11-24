// SPDX-License-Identifier: CC0-1.0

//! What a member an Auto Scaling group reports IN SERVICE is, where Amazon EC2 holds it.
//!
//! THE RULE: a group brings a member into service together with the instance it launched for
//! it, so a member the group reports `InService` is an instance Amazon EC2 reports `running` —
//! never one that service still reports `pending`. The group's member and the Amazon EC2
//! instance behind it are two views of ONE launch
//! (`CreateAutoScalingGroup$DesiredCapacity`: "the desired capacity is the initial capacity of
//! the Auto Scaling group at the time of its creation and the capacity it attempts to
//! maintain"), so the transient state the two services report for it is one span, and neither
//! reports the launch finished while the other still has it in flight. A client therefore
//! never has to wait on a second span to use a member the group already serves.
//!
//! The rows span the launches a group makes — every path that puts a member in service:
//!
//! * the launch the group's own CREATE makes for the capacity it is created with;
//! * the launch a capacity change makes (`SetDesiredCapacity`);
//! * the launch the group was left OWING, made once the client has detached the target group
//!   whose deletion stopped it (registering the instance is part of bringing it into service);
//! * the launch a `LaunchInstances` request makes.
//!
//! The replacement a group launches for a member recorded unhealthy is the same rule on the
//! same path, and is pinned by `scale_out_launches_ec2_instances/instance_health.rs`, which
//! reads the replacement back through Amazon EC2's own `DescribeInstances`.
//!
//! Three services are mounted on ONE engine and driven over their own wire protocols, the way
//! the emulator serves them, so every id these assertions follow is an id a client is given.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, create_target_group, ec2_query, elbv2_query, emulator, p,
    query, seed_subnets, tag_value, tag_values, vpc_of,
};

/// How long a row waits for the launch it drives, and how often it looks.
///
/// FAR above every span this service owes, because the deadline is only here so a failure is a
/// failure rather than a hang: every wait below looks for its CONDITION, so machine load
/// decides when a row finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID"), which is
/// the only kind an Auto Scaling group's instances are registered in.
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the row's target group is created on: fixed, because what the rows differ along is
/// which launch brings the members into service.
const TARGET_GROUP_PORT: i32 = 80;

/// The `LifecycleState` of a member that is in service, and the `InstanceStateName` Amazon EC2
/// reports an instance that runs.
const LIFECYCLE_IN_SERVICE: &str = "InService";
const EC2_RUNNING: &str = "running";

/// Which launch brings the row's members into service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// The launch the group's own create makes for the capacity it is created with.
    GroupCreate,
    /// The launch a capacity change makes.
    CapacityChange,
    /// The launch the group was left owing, made once the target group that stopped it has
    /// been detached.
    OwedAfterRepair,
    /// The launch a `LaunchInstances` request makes.
    Requested,
}

/// One row of the in-service table.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    launch: Launch,
    /// The capacity the group is created with.
    created: usize,
    /// The capacity it holds once the row's launch has settled.
    settled: usize,
}

/// ROW 1: the members the group's own create launched.
#[tokio::test]
async fn the_members_a_groups_create_launched_run_in_amazon_ec2() {
    members_in_service_run(
        1,
        Row {
            case: "the launch a group's create makes",
            launch: Launch::GroupCreate,
            created: 2,
            settled: 2,
        },
    )
    .await;
}

/// ROW 2: the member a capacity change launched.
#[tokio::test]
async fn the_member_a_capacity_change_launched_runs_in_amazon_ec2() {
    members_in_service_run(
        2,
        Row {
            case: "the launch a capacity change makes",
            launch: Launch::CapacityChange,
            created: 1,
            settled: 3,
        },
    )
    .await;
}

/// ROW 3: the member the group was OWED, launched once the target group that is gone was
/// detached — the launch a client waits on after repairing the attachment.
#[tokio::test]
async fn the_member_owed_after_a_detached_target_group_runs_in_amazon_ec2() {
    members_in_service_run(
        3,
        Row {
            case: "the launch the group owes once the target group that is gone is detached",
            launch: Launch::OwedAfterRepair,
            created: 1,
            settled: 2,
        },
    )
    .await;
}

/// ROW 4: the members a `LaunchInstances` request launched.
#[tokio::test]
async fn the_members_a_launch_request_launched_run_in_amazon_ec2() {
    members_in_service_run(
        4,
        Row {
            case: "the launch a LaunchInstances request makes",
            launch: Launch::Requested,
            created: 1,
            settled: 3,
        },
    )
    .await;
}

/// Drive one row: create the group, make the row's launch, and read every member the group
/// then reports in service back through Amazon EC2's own `DescribeInstances`.
async fn members_in_service_run(index: usize, row: Row) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets[0].id.clone();
    let vpc = vpc_of(&router, &subnets[0]).await;
    let template = format!("lt-in-service-{index}");
    create_launch_template(&router, &template).await;
    let group = format!("asg-in-service-{index}");

    // The target group the owed-launch row's group is attached to: registering the instance is
    // part of bringing it into service, so deleting that target group out of band is what
    // leaves the row's launch owed.
    let attached = if row.launch == Launch::OwedAfterRepair {
        Some(
            create_target_group(
                &router,
                &format!("tg-in-service-{index}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                &vpc,
            )
            .await,
        )
    } else {
        None
    };

    let mut params = vec![
        p("AutoScalingGroupName", group.clone()),
        p("LaunchTemplate.LaunchTemplateName", template.clone()),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", row.created.to_string()),
        p("VPCZoneIdentifier", subnet),
    ];
    if let Some(arn) = attached.clone() {
        params.push(p("TargetGroupARNs.member.1", arn));
    }
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be created, got {status} with {body:?}"
    );
    in_service(&router, case, &group, row.created).await;

    match row.launch {
        // The create's own launch is the row's launch: there is nothing further to ask for.
        Launch::GroupCreate => {}
        Launch::CapacityChange => {
            set_desired_capacity(&router, case, &group, row.settled).await;
        }
        Launch::OwedAfterRepair => {
            let arn = attached.clone().expect("the row attached a target group");
            delete_target_group(&router, case, &arn).await;
            // The capacity change the attachment that is gone stops: it is served, and it
            // places nothing while the attachment stands.
            set_desired_capacity(&router, case, &group, row.settled).await;
            detach_target_group(&router, case, &group, &arn).await;
        }
        Launch::Requested => {
            launch_instances(
                &router,
                case,
                &group,
                row.settled.saturating_sub(row.created),
                index,
            )
            .await;
        }
    }

    // Every member the group now reports in service is an instance Amazon EC2 reports running:
    // one launch, settled in both services.
    let members = in_service(&router, case, &group, row.settled).await;
    for instance_id in &members {
        let state = ec2_instance_state(&router, case, instance_id).await;
        assert_eq!(
            state, EC2_RUNNING,
            "case {case:?}: the member {instance_id} the group reports {LIFECYCLE_IN_SERVICE} \
             must be an instance Amazon EC2 reports {EC2_RUNNING}, got {state:?} for \
             {members:?}"
        );
    }
}

/// Wait until the group holds exactly `count` members and every one of them is in service, and
/// answer their ids, sorted.
///
/// Looks for that CONDITION until [`DEADLINE`], so a launch that settles later still passes and
/// one that never settles fails with what the group last reported.
async fn in_service(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    count: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (ids, states) = members(router, case, group).await;
        let settled = ids.len() == count && states.iter().all(|state| state == LIFECYCLE_IN_SERVICE);
        if settled {
            return ids;
        }
        if Instant::now() >= deadline {
            panic!(
                "case {case:?}: the group must hold {count} member(s), every one of them \
                 {LIFECYCLE_IN_SERVICE}, got {ids:?} in {states:?}"
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The ids the group reports, sorted, and the `LifecycleState` of each member in the order the
/// group reports them.
async fn members(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups({group:?}) expected 200, got {status}: {body}"
    );
    let mut ids = tag_values(&body, "InstanceId");
    ids.sort();
    (ids, tag_values(&body, "LifecycleState"))
}

/// What Amazon EC2 reports one instance's state as, read from a `DescribeInstances` that named
/// exactly that instance — so the state belongs to it and to no other member.
async fn ec2_instance_state(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id:?}) expected 200, got {status}: {body}"
    );
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// Ask the group for `capacity`, without honouring its cooldown.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    capacity: usize,
) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", capacity.to_string().as_str()),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity({capacity}) must be served, got {status}: {body}"
    );
}

/// Launch `count` instances into the group through `LaunchInstances`.
async fn launch_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    count: usize,
    index: usize,
) {
    let (status, body) = query(
        router,
        "LaunchInstances",
        &[
            ("AutoScalingGroupName", group),
            ("RequestedCapacity", count.to_string().as_str()),
            ("ClientToken", format!("in-service-{index}").as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: LaunchInstances({count}) must be served, got {status}: {body}"
    );
}

/// Delete the target group `arn` in Elastic Load Balancing v2 — out of band, as far as the
/// group is concerned.
async fn delete_target_group(router: &MultiServiceRouter, case: &str, arn: &str) {
    let (status, body) =
        elbv2_query(router, "DeleteTargetGroup", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTargetGroup({arn:?}) must be served, got {status}: {body}"
    );
}

/// Detach the target group `arn` from the group, which is the client's own repair of the
/// attachment that stopped the launch.
async fn detach_target_group(router: &MultiServiceRouter, case: &str, group: &str, arn: &str) {
    let (status, body) = query(
        router,
        "DetachLoadBalancerTargetGroups",
        &[
            ("AutoScalingGroupName", group),
            ("TargetGroupARNs.member.1", arn),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetachLoadBalancerTargetGroups({arn:?}) must be served, got {status}: \
         {body}"
    );
}
