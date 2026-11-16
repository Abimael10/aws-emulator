// SPDX-License-Identifier: CC0-1.0

//! ECS MANAGED SCALING of a capacity provider's Auto Scaling group, driven end to end over
//! the shared wire harness in [`crate::ecs_wire`] with Amazon EC2 Auto Scaling and Amazon EC2
//! mounted beside this service on one engine — so the group is created and read back through
//! that service's OWN API and the instances it launches are the account's own.
//!
//! # The rule under test
//!
//! > When a capacity provider's managed scaling is ON, Amazon ECS manages the scale-out of its
//! > Auto Scaling group: a cluster holding tasks it cannot place through that provider has the
//! > group's `DesiredCapacity` raised by the shortfall, held inside the provider's own
//! > `minimumScalingStepSize`/`maximumScalingStepSize` and the group's own `MaxSize` — and the
//! > instances that launch register as container instances, which is what the parked tasks are
//! > then placed on. A provider whose managed scaling is OFF moves nothing: "If managed
//! > scaling is off, the user must manage the scaling of the Auto Scaling group."
//!
//! `ManagedScaling`'s own documentation is the rule: "When managed scaling is turned on,
//! Amazon ECS manages the scale-in and scale-out actions of the Auto Scaling group";
//! `minimumScalingStepSize` is "the minimum number of Amazon EC2 instances that Amazon ECS
//! will scale out at one time" and `maximumScalingStepSize` the maximum. The capacity itself
//! is Amazon EC2 Auto Scaling's: a group never holds a desired capacity outside its own
//! `MinSize`/`MaxSize`.
//!
//! # What the rows span
//!
//! The table differs along what the rule generalises over: how many tasks are waiting (one,
//! the recorded count, and three), the provider's own step bounds (the defaults, a minimum
//! that is a floor under one task, a maximum that is a ceiling over three), the group's own
//! maximum (one that leaves room and one that caps the scale-out), and whether managed
//! scaling is ON at all — the row that must move nothing.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    auto_scaling, count, create_cluster, ec2_form, emulator_with_ec2, list, ok,
    register_definition, string, SIGNED_REGION,
};

/// How often a poll re-reads a surface it waits on, and how long it keeps trying: the
/// managed-scaling span plus the group's launch, the agent's registration and the task's own
/// start, with generous margin for a loaded test machine. The deadline is only here so a
/// failure is a failure rather than a hang.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(90);

/// How many looks a row that must move NOTHING takes at its group before judging it unmoved.
/// Together with [`POLL_INTERVAL`] a window far longer than the managed-scaling span, so a
/// capacity that stands is one that held rather than one read too early.
const QUIET_LOOKS: usize = 60;

/// The AMI the launch template of every row boots. Well-formed (`ami-<17 hexadecimal
/// digits>`), which is all a launch needs.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type the capacity is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// One row: how the provider's managed scaling is configured, how big the group may get, how
/// many tasks wait for the capacity, and the desired capacity the group must end up at.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// `managedScaling.status`, or `None` to leave the block out altogether — the documented
    /// default, which is managed scaling OFF.
    managed_scaling: Option<&'static str>,
    /// `minimumScalingStepSize` / `maximumScalingStepSize`, where the row states them.
    minimum_step: Option<i32>,
    maximum_step: Option<i32>,
    /// The group's own `MaxSize`, which no capacity may exceed.
    max_size: i32,
    /// The service's `desiredCount`: how many tasks wait for the capacity.
    desired_count: i32,
    /// The `DesiredCapacity` the group must report, and how many of the waiting tasks must
    /// then be running on the capacity that arrives.
    expected_capacity: i32,
    expected_running: i32,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: one unplaceable task on a 0-capacity group",
        managed_scaling: Some("ENABLED"),
        minimum_step: None,
        maximum_step: None,
        max_size: 2,
        desired_count: 1,
        expected_capacity: 1,
        expected_running: 1,
    },
    Row {
        case: "three unplaceable tasks: the shortfall is the step",
        managed_scaling: Some("ENABLED"),
        minimum_step: None,
        maximum_step: None,
        max_size: 4,
        desired_count: 3,
        expected_capacity: 3,
        expected_running: 3,
    },
    Row {
        case: "the minimum scaling step size is a floor under one task",
        managed_scaling: Some("ENABLED"),
        minimum_step: Some(2),
        maximum_step: None,
        max_size: 4,
        desired_count: 1,
        expected_capacity: 2,
        expected_running: 1,
    },
    Row {
        case: "the maximum scaling step size is a ceiling over three tasks",
        managed_scaling: Some("ENABLED"),
        minimum_step: Some(1),
        maximum_step: Some(1),
        max_size: 4,
        desired_count: 3,
        expected_capacity: 1,
        expected_running: 3,
    },
    Row {
        case: "the group's own maximum holds the scale-out",
        managed_scaling: Some("ENABLED"),
        minimum_step: None,
        maximum_step: None,
        max_size: 1,
        desired_count: 3,
        expected_capacity: 1,
        expected_running: 3,
    },
    Row {
        case: "managed scaling DISABLED: the capacity is the owner's to manage",
        managed_scaling: Some("DISABLED"),
        minimum_step: None,
        maximum_step: None,
        max_size: 2,
        desired_count: 1,
        expected_capacity: 0,
        expected_running: 0,
    },
    Row {
        case: "no managed scaling block at all: the documented default is off",
        managed_scaling: None,
        minimum_step: None,
        maximum_step: None,
        max_size: 2,
        desired_count: 1,
        expected_capacity: 0,
        expected_running: 0,
    },
];

/// The user data a launch into `cluster` carries: the documented shell line that appends the
/// container agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch
/// template carries it.
fn launched_into(cluster: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

/// Create the launch template one row's group launches from, through Amazon EC2's own API —
/// an AMI, an instance type and the user data that joins the instance to `cluster`.
async fn create_launch_template(
    router: &MultiServiceRouter,
    name: &str,
    cluster: &str,
    case: &str,
) {
    ec2_form(
        router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", name),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            ("LaunchTemplateData.UserData", &launched_into(cluster)),
        ],
        case,
    )
    .await;
}

/// Create the Auto Scaling group one row scales: no capacity yet, a maximum the row states,
/// and the launch template above.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    launch_template: &str,
    row: &Row,
    case: &str,
) {
    let zone = format!("{SIGNED_REGION}a");
    let max_size = row.max_size.to_string();
    auto_scaling(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", launch_template),
            ("MinSize", "0"),
            ("MaxSize", &max_size),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", &zone),
        ],
        case,
    )
    .await;
}

/// The `DesiredCapacity` Amazon EC2 Auto Scaling reports for `group`, read through that
/// service's own API.
async fn desired_capacity(router: &MultiServiceRouter, group: &str, case: &str) -> i32 {
    let payload = auto_scaling(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
        case,
    )
    .await;
    let (_, after) = payload
        .split_once("<DesiredCapacity>")
        .unwrap_or_else(|| panic!("case {case:?}: no DesiredCapacity for {group} in {payload}"));
    let (value, _) = after
        .split_once("</DesiredCapacity>")
        .unwrap_or_else(|| panic!("case {case:?}: unterminated DesiredCapacity in {payload}"));
    value
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {value:?} is no DesiredCapacity in {payload}"))
}

/// The group's `DesiredCapacity` once it has reached `wanted` — looked at again until it does,
/// or the budget runs out.
async fn settled_capacity(
    router: &MultiServiceRouter,
    group: &str,
    wanted: i32,
    case: &str,
) -> i32 {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let reported = desired_capacity(router, group, case).await;
        if reported == wanted || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a group reports after a WINDOW of being looked at again: the last reading, and the
/// first that differs from `unmoved` if any look finds a change.
///
/// What a row that must move nothing needs: a capacity that is still 0 now proves nothing on
/// its own, since the action under test is clock-owed, so the group is read over a window far
/// past the span it would have acted in.
async fn quiet_capacity(
    router: &MultiServiceRouter,
    group: &str,
    unmoved: i32,
    case: &str,
) -> i32 {
    let mut last = unmoved;
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL_INTERVAL).await;
        last = desired_capacity(router, group, case).await;
        if last != unmoved {
            return last;
        }
    }
    last
}

/// The service's `runningCount` once it has reached `wanted` — looked at again until it does,
/// or the budget runs out.
async fn settled_running_count(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    wanted: i32,
    case: &str,
) -> i64 {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let described = ok(
            router,
            "DescribeServices",
            json!({ "cluster": cluster, "services": [service] }),
            case,
        )
        .await;
        let services = list(&described, "services", case);
        let view = services
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {described}"));
        let running = count(view, "runningCount", case);
        if running == i64::from(wanted) || Instant::now() >= deadline {
            return running;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many container instances the cluster holds, as `ListContainerInstances` reports them.
async fn registered_instances(router: &MultiServiceRouter, cluster: &str, case: &str) -> usize {
    let listed = ok(
        router,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
        case,
    )
    .await;
    list(&listed, "containerInstanceArns", case).len()
}

#[tokio::test]
async fn managed_scaling_raises_the_capacity_a_clusters_unplaceable_tasks_need() {
    let router = emulator_with_ec2();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // Each row gets its own cluster, group and launch template, so the capacity one row
        // acquires is never the capacity another row's placement finds.
        let cluster = format!("managedscaling{index}");
        let group = format!("managed-scaling-group-{index}");
        let launch_template = format!("managed-scaling-lt-{index}");
        let provider = format!("managedprovider{index}");
        let service = format!("managedservice{index}");
        let family = format!("managedfamily{index}");

        create_launch_template(&router, &launch_template, &cluster, case).await;
        create_group(&router, &group, &launch_template, row, case).await;
        create_cluster(&router, &cluster, case).await;
        register_definition(&router, &family, case).await;

        let mut group_provider = json!({ "autoScalingGroupArn": group });
        let mut scaling = json!({});
        if let Some(status) = row.managed_scaling {
            scaling["status"] = json!(status);
        }
        if let Some(minimum) = row.minimum_step {
            scaling["minimumScalingStepSize"] = json!(minimum);
        }
        if let Some(maximum) = row.maximum_step {
            scaling["maximumScalingStepSize"] = json!(maximum);
        }
        if row.managed_scaling.is_some() || row.minimum_step.is_some() || row.maximum_step.is_some()
        {
            group_provider["managedScaling"] = scaling;
        }
        ok(
            &router,
            "CreateCapacityProvider",
            json!({ "name": provider, "autoScalingGroupProvider": group_provider }),
            case,
        )
        .await;
        ok(
            &router,
            "PutClusterCapacityProviders",
            json!({
                "cluster": cluster,
                "capacityProviders": [provider],
                "defaultCapacityProviderStrategy": [],
            }),
            case,
        )
        .await;

        // The group holds no capacity when the service is created: the tasks it places have
        // nowhere to run, which is what managed scaling answers.
        let created = ok(
            &router,
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": service,
                "taskDefinition": family,
                "desiredCount": row.desired_count,
                "capacityProviderStrategy": [
                    { "capacityProvider": provider, "weight": 1 },
                ],
            }),
            case,
        )
        .await;
        assert_eq!(
            count(&created["service"], "runningCount", case),
            0,
            "case {case:?}: no task can be running before any capacity exists, got {created}"
        );
        assert_eq!(
            desired_capacity(&router, &group, case).await,
            0,
            "case {case:?}: the scaling action is never instant, so the group still holds the \
             capacity its owner left it at"
        );

        if row.expected_capacity == 0 {
            // A provider whose managed scaling is off: the capacity is the owner's, so it
            // stands over a window far longer than the span an action would have taken.
            let reported = quiet_capacity(&router, &group, 0, case).await;
            assert_eq!(
                reported, 0,
                "case {case:?}: expected the group's desired capacity to stay 0, got {reported}"
            );
            let running =
                settled_running_count(&router, &cluster, &service, row.expected_running, case)
                    .await;
            assert_eq!(
                running,
                i64::from(row.expected_running),
                "case {case:?}: expected no task to run without capacity, got {running}"
            );
            continue;
        }

        // Managed scaling raises the group's capacity, and it is Amazon EC2 Auto Scaling that
        // holds it: the number is read back through that service's own API.
        let reported = settled_capacity(&router, &group, row.expected_capacity, case).await;
        assert_eq!(
            reported, row.expected_capacity,
            "case {case:?}: expected Amazon ECS to raise the group's desired capacity to {}, \
             got {reported}",
            row.expected_capacity
        );

        // The instances that capacity launched register as container instances of the
        // cluster, and the tasks that were waiting are placed on them and run.
        let running =
            settled_running_count(&router, &cluster, &service, row.expected_running, case).await;
        assert_eq!(
            running,
            i64::from(row.expected_running),
            "case {case:?}: expected the waiting tasks to run on the capacity managed scaling \
             acquired, got {running}"
        );
        let instances = registered_instances(&router, &cluster, case).await;
        assert_eq!(
            instances,
            usize::try_from(row.expected_capacity).unwrap_or(0),
            "case {case:?}: expected one container instance per unit of capacity, got \
             {instances}"
        );

        // The tasks really run on that capacity: each names the container instance that hosts
        // it.
        let listed = ok(
            &router,
            "ListTasks",
            json!({ "cluster": cluster, "serviceName": service, "desiredStatus": "RUNNING" }),
            case,
        )
        .await;
        let arns = list(&listed, "taskArns", case);
        assert_eq!(
            arns.len(),
            usize::try_from(row.expected_running).unwrap_or(0),
            "case {case:?}: expected one running task per asked-for task, got {listed}"
        );
        let described = ok(
            &router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": arns }),
            case,
        )
        .await;
        for task in list(&described, "tasks", case) {
            assert_eq!(
                string(&task, "lastStatus", case),
                "RUNNING",
                "case {case:?}: expected every placed task to be RUNNING, got {task}"
            );
            assert_eq!(
                string(&task, "capacityProviderName", case),
                provider,
                "case {case:?}: expected the task to name the provider whose capacity it \
                 waited for, got {task}"
            );
        }
    }
}
