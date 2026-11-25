// SPDX-License-Identifier: CC0-1.0

//! `RunTask` under a `distinctInstance` placement constraint, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`] with the Amazon EC2 service mounted beside
//! this one on the same engine — so every host is launched through EC2's OWN API and every
//! value asserted is read back through `RunTask`, `ListTasks` and `DescribeTasks`.
//!
//! # The rule under test
//!
//! > A `RunTask` whose `placementConstraints` carries an entry of type `distinctInstance`
//! > places at most ONE task per container instance of the cluster: it answers HTTP 200 with
//! > `min(count, hosts)` tasks on as many distinct hosts, and reports every candidate host
//! > the constraint then rejects as one `failures` entry carrying that host's
//! > `containerInstanceArn`.
//!
//! `PlacementConstraintType`'s own doc: "Use distinctInstance to ensure that each task in a
//! particular group is running on a different container instance" — so the hosts a run may
//! use bound how many of its tasks it can place, and the surplus is reported rather than
//! over-placed or refused as a whole.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, auto_scaling, create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok,
    string, SIGNED_REGION,
};

/// The cluster every row below places into.
const CLUSTER: &str = "distinctfleet";

/// The second cluster, for the placements that take no container instance of it at all.
const UNCAPPED_CLUSTER: &str = "distinctuncapped";

/// The AMI every launch boots. Well-formed (`ami-<17 hexadecimal digits>`), which is all a
/// launch needs; which AMI carries the container agent is not a fact this emulator
/// publishes, so the registration is not keyed on it.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type every host below is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// How often a poll re-reads the surface it waits on, and how long it keeps trying: the
/// agent's registration span plus a task's own start, with generous margin for a loaded
/// test machine.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// `ContainerInstance$status` of a registered host.
const ACTIVE: &str = "ACTIVE";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The `PlacementConstraintType` member under test.
const DISTINCT_INSTANCE: &str = "distinctInstance";

/// The fixed sentence a `Failure` of a host the constraint rejected carries.
const DISTINCT_INSTANCE_UNSATISFIED: &str = "DistinctInstance placement constraint unsatisfied.";

/// The user data a host is launched with: the documented shell line that appends the
/// container agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch
/// carries it.
fn launched_into(cluster: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

/// Create one VPC and one subnet per named zone, through Amazon EC2's own API, and answer
/// the subnet ids.
async fn network(router: &MultiServiceRouter, zones: &[String], case: &str) -> Vec<String> {
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.90.{}.0/24", index);
        let created = ec2_form(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr.as_str()),
                ("AvailabilityZone", zone.as_str()),
            ],
            case,
        )
        .await;
        subnets.push(ec2_element(&created, "subnetId", case));
    }
    subnets
}

/// The instance profile a host is launched with, as the documented way of launching an ECS
/// container instance carries one.
const INSTANCE_PROFILE_ARN: &str = "arn:aws:iam::000000000000:instance-profile/ecsInstanceRole";

/// Launch ONE instance into `subnet`, configured for `cluster`, and answer its instance id.
async fn launch(router: &MultiServiceRouter, subnet: &str, cluster: &str, case: &str) -> String {
    let user_data = launched_into(cluster);
    let launched = ec2_form(
        router,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", subnet),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("IamInstanceProfile.Arn", INSTANCE_PROFILE_ARN),
            ("UserData", user_data.as_str()),
        ],
        case,
    )
    .await;
    ec2_element(&launched, "instanceId", case)
}

/// The container instances `cluster` reports, described through
/// `ListContainerInstances` + `DescribeContainerInstances`.
async fn described_hosts(router: &MultiServiceRouter, cluster: &str, case: &str) -> Vec<Value> {
    let listing = ok(
        router,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
        case,
    )
    .await;
    let arns: Vec<String> = list(&listing, "containerInstanceArns", case)
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": arns }),
        case,
    )
    .await;
    list(&body, "containerInstances", case)
}

/// The ARNs of the hosts of `cluster` that are `ACTIVE` with their agent connected — the
/// capacity a placement can be made on.
async fn connected_hosts(router: &MultiServiceRouter, cluster: &str, case: &str) -> BTreeSet<String> {
    described_hosts(router, cluster, case)
        .await
        .iter()
        .filter(|host| {
            host["status"].as_str() == Some(ACTIVE) && host["agentConnected"].as_bool() == Some(true)
        })
        .filter_map(|host| host["containerInstanceArn"].as_str().map(str::to_string))
        .collect()
}

/// Wait until `cluster` reports `wanted` connected hosts, answering what it last read.
async fn settled_hosts(
    router: &MultiServiceRouter,
    cluster: &str,
    wanted: usize,
    case: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut last = connected_hosts(router, cluster, case).await;
    while last.len() < wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        last = connected_hosts(router, cluster, case).await;
    }
    last
}

/// Register one revision the FARGATE launch type can run: its own networking (`awsvpc`) and
/// the task-level size that infrastructure requires.
async fn register_fargate_definition(
    router: &MultiServiceRouter,
    family: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/docker/library/busybox:1.36",
                "essential": true,
                "command": ["sh", "-c", "sleep infinity"],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Register one revision whose containers share the host's networking (`bridge`), which is
/// what an EC2-launch-type placement runs.
async fn register_bridge_definition(
    router: &MultiServiceRouter,
    family: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/docker/library/busybox:1.36",
                "essential": true,
                "cpu": 64,
                "memory": 64,
                "command": ["sh", "-c", "sleep infinity"],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Run `count` tasks of `definition` into `cluster`, under a `distinctInstance` constraint
/// or under none, as one row of the table below asks.
async fn run_tasks(
    router: &MultiServiceRouter,
    cluster: &str,
    definition: &str,
    row: &Row,
) -> (u16, Value) {
    let mut request = json!({
        "cluster": cluster,
        "taskDefinition": definition,
        "launchType": "EC2",
        "count": row.count,
        "group": row.group,
        "startedBy": row.case,
    });
    if row.constrained {
        request["placementConstraints"] = json!([{ "type": DISTINCT_INSTANCE }]);
    }
    answered(router, "RunTask", request).await
}

/// ONE row of the table: a placement of `count` tasks of `group`, constrained or not, and
/// what the cluster's two hosts leave room for.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What the row is about, which is also the `startedBy` its tasks carry.
    case: &'static str,
    /// The task group the row places into: the member a `distinctInstance` constraint caps
    /// ("each task in a particular group"), so rows of one group share the cap and rows of
    /// different groups do not.
    group: &'static str,
    /// How many tasks the row asks for.
    count: i64,
    /// Whether the row states the constraint at all.
    constrained: bool,
    /// How many tasks the placement must make.
    placed: usize,
    /// How many `failures` entries it must report — one per candidate host a task it could
    /// not place was rejected on.
    rejected: usize,
    /// Whether the row's tasks are stopped (and waited out) before the next row runs.
    stop_after: bool,
}

/// Run `count` tasks of `definition` into `cluster` under a `distinctInstance` constraint.
async fn run_distinct(
    router: &MultiServiceRouter,
    cluster: &str,
    definition: &str,
    count: i64,
    started_by: &str,
) -> (u16, Value) {
    answered(
        router,
        "RunTask",
        json!({
            "cluster": cluster,
            "taskDefinition": definition,
            "launchType": "EC2",
            "count": count,
            "startedBy": started_by,
            "placementConstraints": [{ "type": DISTINCT_INSTANCE }],
        }),
    )
    .await
}

/// The `containerInstanceArn` of each task of a `RunTask` answer.
fn hosts_of(tasks: &[Value]) -> Vec<String> {
    tasks
        .iter()
        .map(|task| {
            task["containerInstanceArn"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The task ARNs `ListTasks` reports for `started_by`.
async fn listed_tasks(
    router: &MultiServiceRouter,
    cluster: &str,
    started_by: &str,
    case: &str,
) -> BTreeSet<String> {
    let body = ok(
        router,
        "ListTasks",
        json!({ "cluster": cluster, "startedBy": started_by }),
        case,
    )
    .await;
    list(&body, "taskArns", case)
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

/// Wait until every task of `arns` reports `wanted`, answering the statuses last read.
async fn settled_statuses(
    router: &MultiServiceRouter,
    cluster: &str,
    arns: &[String],
    wanted: &str,
    case: &str,
) -> Vec<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": arns }),
            case,
        )
        .await;
        let statuses: Vec<String> = list(&body, "tasks", case)
            .iter()
            .map(|task| task["lastStatus"].as_str().unwrap_or_default().to_string())
            .collect();
        if statuses.iter().all(|status| status == wanted) || Instant::now() >= deadline {
            return statuses;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn distinct_instance_caps_a_run_at_one_task_per_host() {
    let case = "distinctInstance over two hosts";
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, case).await;
    let zones = vec![format!("{SIGNED_REGION}a"), format!("{SIGNED_REGION}b")];
    let subnets = network(&router, &zones, case).await;
    let mut launched = Vec::new();
    for subnet in &subnets {
        launched.push(launch(&router, subnet, CLUSTER, case).await);
    }

    let hosts = settled_hosts(&router, CLUSTER, subnets.len(), case).await;
    assert_eq!(
        hosts.len(),
        subnets.len(),
        "case {case:?}: expected {} connected hosts for the instances {launched:?}, got \
         {hosts:?}",
        subnets.len()
    );

    let definition = register_bridge_definition(&router, "distinctapp", case).await;

    // The control: one task per host places them all, with nothing rejected.
    let (status, body) = run_distinct(&router, CLUSTER, &definition, 2, "ctl").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected RunTask(count=2) to answer 200, got {status} with {body}"
    );
    let tasks = list(&body, "tasks", case);
    let placed: BTreeSet<String> = hosts_of(&tasks).into_iter().collect();
    assert_eq!(
        placed, hosts,
        "case {case:?}: expected RunTask(count=2) to place one task on each of {hosts:?}, \
         got {placed:?} from {body}"
    );
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: expected no failures from RunTask(count=2), got {body}"
    );
    let control: Vec<String> = tasks
        .iter()
        .map(|task| string(task, "taskArn", case))
        .collect();
    let statuses = settled_statuses(&router, CLUSTER, &control, RUNNING, case).await;
    assert!(
        statuses.iter().all(|status| status == RUNNING),
        "case {case:?}: expected both placed tasks to reach RUNNING, got {statuses:?}"
    );

    // The hosts are freed again before the cap is exercised, so the rows differ only in the
    // count the constraint is applied to.
    for task in &control {
        ok(
            &router,
            "StopTask",
            json!({ "cluster": CLUSTER, "task": task, "reason": "making room" }),
            case,
        )
        .await;
    }
    let statuses = settled_statuses(&router, CLUSTER, &control, STOPPED, case).await;
    assert!(
        statuses.iter().all(|status| status == STOPPED),
        "case {case:?}: expected both control tasks to reach STOPPED, got {statuses:?}"
    );

    // The cap: one task more than there are hosts places as many as there are hosts and
    // reports every host the constraint rejected.
    let (status, body) = run_distinct(&router, CLUSTER, &definition, 3, "capped").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected RunTask(count=3) to answer 200, got {status} with {body}"
    );
    let tasks = list(&body, "tasks", case);
    let placed: BTreeSet<String> = hosts_of(&tasks).into_iter().collect();
    assert_eq!(
        placed, hosts,
        "case {case:?}: expected RunTask(count=3) to place one task on each of {hosts:?}, \
         got {placed:?} from {body}"
    );
    let failures = list(&body, "failures", case);
    let rejected: BTreeSet<String> = failures
        .iter()
        .map(|failure| string(failure, "arn", case))
        .collect();
    assert_eq!(
        rejected, hosts,
        "case {case:?}: expected one failure per rejected host {hosts:?}, got {failures:?}"
    );
    for failure in &failures {
        assert_eq!(
            failure["reason"].as_str(),
            Some(DISTINCT_INSTANCE_UNSATISFIED),
            "case {case:?}: expected the fixed distinctInstance reason, got {failure}"
        );
    }

    let listed = listed_tasks(&router, CLUSTER, "capped", case).await;
    let expected: Vec<String> = tasks
        .iter()
        .map(|task| string(task, "taskArn", case))
        .collect();
    let expected_set: BTreeSet<String> = expected.iter().cloned().collect();
    assert_eq!(
        listed, expected_set,
        "case {case:?}: expected ListTasks to report only the placed tasks {expected_set:?}, \
         got {listed:?}"
    );
    let statuses = settled_statuses(&router, CLUSTER, &expected, RUNNING, case).await;
    assert!(
        statuses.iter().all(|status| status == RUNNING),
        "case {case:?}: expected every placed task of the capped run to reach RUNNING, got \
         {statuses:?}"
    );
}

/// The table: what a two-host cluster leaves room for, row by row.
///
/// The rows differ along what the rule generalises over — the COUNT asked for against the
/// number of hosts (under it, equal to it, over it), the GROUP the constraint is applied
/// over (a group whose tasks already hold the hosts, and another group that is not capped by
/// them), and whether the constraint is stated at all, which is the contrast that shows the
/// cap is the constraint's and not the cluster's.
const ROWS: [Row; 6] = [
    Row {
        case: "fewer tasks than hosts",
        group: "family:distinctapp",
        count: 1,
        constrained: true,
        placed: 1,
        rejected: 0,
        stop_after: true,
    },
    Row {
        case: "exactly as many tasks as hosts",
        group: "family:distinctapp",
        count: 2,
        constrained: true,
        placed: 2,
        rejected: 0,
        stop_after: false,
    },
    Row {
        case: "one more task of the group while its tasks hold every host",
        group: "family:distinctapp",
        count: 1,
        constrained: true,
        placed: 0,
        rejected: 2,
        stop_after: false,
    },
    Row {
        case: "another group while the first group holds every host",
        group: "other-group",
        count: 2,
        constrained: true,
        placed: 2,
        rejected: 0,
        stop_after: true,
    },
    Row {
        case: "more tasks than hosts",
        group: "third-group",
        count: 3,
        constrained: true,
        placed: 2,
        rejected: 2,
        stop_after: true,
    },
    Row {
        case: "more tasks than hosts with no constraint stated",
        group: "unconstrained-group",
        count: 3,
        constrained: false,
        placed: 3,
        rejected: 0,
        stop_after: false,
    },
];

#[tokio::test]
async fn a_distinct_instance_constraint_caps_each_group_at_one_task_per_host() {
    let case = "the distinctInstance table";
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, case).await;
    let zones = vec![format!("{SIGNED_REGION}a"), format!("{SIGNED_REGION}b")];
    let subnets = network(&router, &zones, case).await;
    for subnet in &subnets {
        launch(&router, subnet, CLUSTER, case).await;
    }
    let hosts = settled_hosts(&router, CLUSTER, subnets.len(), case).await;
    assert_eq!(
        hosts.len(),
        subnets.len(),
        "case {case:?}: expected {} connected hosts before the table runs, got {hosts:?}",
        subnets.len()
    );
    let definition = register_bridge_definition(&router, "distinctapp", case).await;

    // The second row's tasks are left in place for the third and fourth, so the rows run in
    // the order they are written.
    for row in &ROWS {
        let case = row.case;
        let (status, body) = run_tasks(&router, CLUSTER, &definition, row).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RunTask to answer 200, got {status} with {body}"
        );
        let tasks = list(&body, "tasks", case);
        assert_eq!(
            tasks.len(),
            row.placed,
            "case {case:?}: expected {} placed tasks, got {} in {body}",
            row.placed,
            tasks.len()
        );
        let placed_hosts = hosts_of(&tasks);
        let distinct: BTreeSet<String> = placed_hosts.iter().cloned().collect();
        assert!(
            distinct.is_subset(&hosts),
            "case {case:?}: expected every task placed on a host of {hosts:?}, got \
             {placed_hosts:?}"
        );
        if row.constrained {
            assert_eq!(
                distinct.len(),
                placed_hosts.len(),
                "case {case:?}: expected one task per host, got {placed_hosts:?}"
            );
        }
        let failures = list(&body, "failures", case);
        assert_eq!(
            failures.len(),
            row.rejected,
            "case {case:?}: expected {} failures, got {failures:?}",
            row.rejected
        );
        if !failures.is_empty() {
            let rejected: BTreeSet<String> = failures
                .iter()
                .map(|failure| string(failure, "arn", case))
                .collect();
            assert_eq!(
                rejected, hosts,
                "case {case:?}: expected a failure for each candidate host {hosts:?}, got \
                 {failures:?}"
            );
            for failure in &failures {
                assert_eq!(
                    failure["reason"].as_str(),
                    Some(DISTINCT_INSTANCE_UNSATISFIED),
                    "case {case:?}: expected the fixed distinctInstance reason, got {failure}"
                );
            }
        }
        if row.stop_after {
            let arns: Vec<String> = tasks
                .iter()
                .map(|task| string(task, "taskArn", case))
                .collect();
            for arn in &arns {
                ok(
                    &router,
                    "StopTask",
                    json!({ "cluster": CLUSTER, "task": arn, "reason": "next row" }),
                    case,
                )
                .await;
            }
            let statuses = settled_statuses(&router, CLUSTER, &arns, STOPPED, case).await;
            assert!(
                statuses.iter().all(|status| status == STOPPED),
                "case {case:?}: expected the row's tasks to stop before the next row, got \
                 {statuses:?}"
            );
        }
    }
}

/// The launch template the Auto Scaling group below launches from, the group a capacity
/// provider binds onto, and that provider's name.
const PROVIDER_LAUNCH_TEMPLATE: &str = "distinct-uncapped-lt";
const PROVIDER_GROUP: &str = "distinct-uncapped-asg";
const PROVIDER: &str = "distinct-uncapped-provider";

/// Create the Auto Scaling group a capacity provider binds onto, through each service's own
/// API: a launch template in Amazon EC2, then the group in Amazon EC2 Auto Scaling.
async fn create_capacity_group(router: &MultiServiceRouter, case: &str) -> String {
    let user_data = launched_into(UNCAPPED_CLUSTER);
    ec2_form(
        router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", PROVIDER_LAUNCH_TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            ("LaunchTemplateData.UserData", user_data.as_str()),
        ],
        case,
    )
    .await;
    let zone = format!("{SIGNED_REGION}a");
    auto_scaling(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", PROVIDER_GROUP),
            ("LaunchTemplate.LaunchTemplateName", PROVIDER_LAUNCH_TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", zone.as_str()),
        ],
        case,
    )
    .await;
    crate::ecs_wire::auto_scaling_group_arn(router, PROVIDER_GROUP, case).await
}

/// What a placement that takes NO container instance of the cluster does under the same
/// constraint.
///
/// The constraint is over the cluster's container instances — "each task in a particular
/// group is running on a different container instance" — and `PlacementConstraint`'s own
/// note is that "if you're using the Fargate launch type, task placement constraints aren't
/// supported": a Fargate task runs on capacity AWS owns, and a placement routed through a
/// capacity provider carries no launch type, so neither takes a container instance of this
/// cluster and neither is capped by the constraint. Both rows ask for more tasks than the
/// cluster has hosts, and both must place every one of them and report nothing rejected —
/// read back through `DescribeTasks`, so a placement that stored nothing cannot pass.
#[tokio::test]
async fn a_placement_that_takes_no_container_instance_is_not_capped() {
    let case = "a placement off the cluster's own capacity";
    let router = emulator_with_ec2();
    create_cluster(&router, UNCAPPED_CLUSTER, case).await;
    let zones = vec![format!("{SIGNED_REGION}a"), format!("{SIGNED_REGION}b")];
    let subnets = network(&router, &zones, case).await;
    for subnet in &subnets {
        launch(&router, subnet, UNCAPPED_CLUSTER, case).await;
    }
    let hosts = settled_hosts(&router, UNCAPPED_CLUSTER, subnets.len(), case).await;
    assert_eq!(
        hosts.len(),
        subnets.len(),
        "case {case:?}: expected {} connected hosts before the rows run, got {hosts:?}",
        subnets.len()
    );

    // The capacity provider the second row places through, over an Auto Scaling group of the
    // account created through that service's own API.
    let group = create_capacity_group(&router, case).await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": PROVIDER,
            "autoScalingGroupProvider": { "autoScalingGroupArn": group },
        }),
        case,
    )
    .await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": UNCAPPED_CLUSTER,
            "capacityProviders": [PROVIDER],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    let fargate = register_fargate_definition(&router, "distinctfargate", case).await;
    let bridge = register_bridge_definition(&router, "distinctbridge", case).await;
    // One task more than the cluster has hosts, so a cap would show.
    let count = subnets.len() + 1;
    // (case, the request this row makes)
    let rows: [(&str, Value); 2] = [
        (
            "the Fargate launch type",
            json!({
                "cluster": UNCAPPED_CLUSTER,
                "taskDefinition": fargate,
                "launchType": "FARGATE",
                "count": count,
                "group": "fargate-group",
                "startedBy": "fargate-row",
                "placementConstraints": [{ "type": DISTINCT_INSTANCE }],
                "networkConfiguration": {
                    "awsvpcConfiguration": { "subnets": [subnets[0].as_str()] },
                },
            }),
        ),
        (
            "a capacity provider strategy, which names no launch type",
            json!({
                "cluster": UNCAPPED_CLUSTER,
                "taskDefinition": bridge,
                "count": count,
                "group": "provider-group",
                "startedBy": "provider-row",
                "placementConstraints": [{ "type": DISTINCT_INSTANCE }],
                "capacityProviderStrategy": [{ "capacityProvider": PROVIDER, "weight": 1 }],
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, body) = answered(&router, "RunTask", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RunTask to answer 200, got {status} with {body}"
        );
        let tasks = list(&body, "tasks", case);
        assert_eq!(
            tasks.len(),
            count,
            "case {case:?}: expected every one of the {count} tasks asked for to be placed, \
             got {} in {body}",
            tasks.len()
        );
        let failures = list(&body, "failures", case);
        assert!(
            failures.is_empty(),
            "case {case:?}: expected nothing to be reported as rejected, got {failures:?}"
        );
        // The tasks were STORED, not merely answered: every one of them is read back.
        let arns: Vec<String> = tasks
            .iter()
            .map(|task| string(task, "taskArn", case))
            .collect();
        let described = ok(
            &router,
            "DescribeTasks",
            json!({ "cluster": UNCAPPED_CLUSTER, "tasks": arns }),
            case,
        )
        .await;
        let read: BTreeSet<String> = list(&described, "tasks", case)
            .iter()
            .map(|task| string(task, "taskArn", case))
            .collect();
        let placed: BTreeSet<String> = arns.iter().cloned().collect();
        assert_eq!(
            read, placed,
            "case {case:?}: expected DescribeTasks to report every placed task {placed:?}, \
             got {read:?}"
        );
    }
}
