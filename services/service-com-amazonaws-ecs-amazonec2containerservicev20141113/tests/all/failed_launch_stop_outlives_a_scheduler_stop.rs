// SPDX-License-Identifier: CC0-1.0

//! The stop a task whose LAUNCH Amazon ECS could not make earns is the stop it is reported
//! with, whatever its service's scheduler asks of it afterwards.
//!
//! The rule under test: `Task$stopCode` is "the stop code indicating why a task was stopped",
//! and the `TaskStopCode` member for a task that failed before its containers started is
//! `TaskFailedToStart` — which is what a task whose image could not be fetched from the
//! placement it was launched into is, with `Task$stoppedReason` ("the reason that the task was
//! stopped") carrying the container runtime's `CannotPullContainerError` for the reference it
//! could not resolve. `ServiceSchedulerInitiated` is the member for a task "the service
//! scheduler stopped" — one it took out of a task set it was holding up, which is why the
//! sentences it carries describe a scaling activity, a deployment or a failed health check. A
//! task whose launch had ALREADY failed when the scheduler asked it to stop therefore reports
//! that failure: it never ran, and nothing the scheduler does can launch it.
//!
//! The rows differ along WHICH stop the scheduler is asked for while the failed launch is
//! still coming up, which is what the rule generalises over: none at all, a scale-in to zero,
//! a scale-in followed by another scale-up, a new deployment rolled over it, and the delete of
//! the service that placed it. The last test is the other half of the same rule: a launch the
//! placement CAN make runs, and the scale-in that stops that task is the scheduler's own stop,
//! so the code a client branches on still tells the two apart.
//!
//! Nothing here reaches into either state struct: the network the fetch is decided against is
//! created through Amazon EC2's own API, and every value asserted is read back through
//! `ListTasks` and `DescribeTasks`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2, ec2_element, emulator_with_ec2, list, ok, string, ACCOUNT, AWSVPC_START,
    SIGNED_REGION,
};
use crate::task_lifecycle::container;

/// The cluster every service below runs in.
const CLUSTER: &str = "failedlaunchstop";

/// How often a poll re-reads a task, and how long it keeps trying: the whole start of an
/// `awsvpc` task, the attempt a launch failure waits out where nothing answers the fetch, the
/// stop that follows it and the release of the interface the task was allocated, which its
/// stop waits for — with generous margin for a loaded test machine, since the deadline is only
/// here so a failure is a failure rather than a hang.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = AWSVPC_START
    .saturating_add(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::FAILED_IMAGE_FETCH_TIMEOUT,
    )
    .saturating_add(Duration::from_secs(60));

/// The image every row's revision runs: a reference into a PUBLIC container registry, whose
/// registry is no AWS service and so is reachable only over the internet.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The image the control runs: a reference that names no registry at all, which a container
/// runtime resolves against Docker Hub — an image this emulator's gallery serves, so the
/// control's launch can actually be made.
const CONTROL_IMAGE: &str = "nginx:1.27";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The `TaskStopCode` members this suite tells apart
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";

/// The error class a container runtime reports a failed image fetch under.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// What the row asks of the scheduler while the launch it scaled up is still coming up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerStop {
    /// Nothing: the failed launch stops on its own.
    None,
    /// A scale-in to zero, which asks every task of the service to stop.
    ScaleIn,
    /// A scale-in to zero and another scale-up, the shape a client scaling a service down
    /// and back up produces.
    ScaleInAndUp,
    /// A new deployment, whose scheduler replaces the tasks the superseded one placed.
    Redeploy,
    /// The delete of the service, which drains the tasks it still holds.
    Delete,
}

/// One row: how many tasks the scale-up asks for, and what the scheduler is asked for while
/// those launches are failing.
struct Row {
    case: &'static str,
    name: &'static str,
    desired: i64,
    stop: SchedulerStop,
}

const ROWS: [Row; 6] = [
    Row {
        case: "nothing else asked of the scheduler",
        name: "plain",
        desired: 1,
        stop: SchedulerStop::None,
    },
    Row {
        case: "scaled back to zero while the launch was failing",
        name: "scalein",
        desired: 1,
        stop: SchedulerStop::ScaleIn,
    },
    Row {
        case: "scaled back to zero and up again while the launch was failing",
        name: "scaleinandup",
        desired: 1,
        stop: SchedulerStop::ScaleInAndUp,
    },
    Row {
        case: "three launches, all of them scaled back to zero",
        name: "three",
        desired: 3,
        stop: SchedulerStop::ScaleIn,
    },
    Row {
        case: "a new deployment rolled over the launch that was failing",
        name: "redeploy",
        desired: 1,
        stop: SchedulerStop::Redeploy,
    },
    Row {
        case: "the service deleted while the launch was failing",
        name: "deleted",
        desired: 2,
        stop: SchedulerStop::Delete,
    },
];

impl Row {
    /// The task-definition family and the service the row's name is built from.
    fn family(&self) -> String {
        format!("failedlaunch-{}", self.name)
    }

    fn service_name(&self) -> String {
        format!("failedlaunch-{}", self.name)
    }
}

/// One subnet of `vpc_id`, created over Amazon EC2's own wire.
async fn subnet(router: &MultiServiceRouter, vpc_id: &str, cidr_block: &str, case: &str) -> String {
    let body = ec2(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc_id),
            ("CidrBlock", cidr_block),
            ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
        ],
        case,
    )
    .await;
    ec2_element(&body, "subnetId", case)
}

/// A route table of `vpc_id` associated with `subnet_id`, which is then the table that
/// decides where that subnet's traffic goes. Answers its id.
async fn route_table(
    router: &MultiServiceRouter,
    vpc_id: &str,
    subnet_id: &str,
    case: &str,
) -> String {
    let body = ec2(router, "CreateRouteTable", &[("VpcId", vpc_id)], case).await;
    let route_table_id = ec2_element(&body, "routeTableId", case);
    ec2(
        router,
        "AssociateRouteTable",
        &[("RouteTableId", &route_table_id), ("SubnetId", subnet_id)],
        case,
    )
    .await;
    route_table_id
}

/// The two subnets this suite places tasks in: one with a route table of its own and no route
/// in it — which is what makes it isolated, since every route table comes with the local route
/// of its VPC alone — and one whose table sends `0.0.0.0/0` to an attached internet gateway.
struct Network {
    isolated: String,
    routed: String,
}

/// Build that network through Amazon EC2's own API.
async fn network(router: &MultiServiceRouter) -> Network {
    let case = "network";
    let vpc = ec2_element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.98.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let isolated = subnet(router, &vpc, "10.98.0.0/20", case).await;
    route_table(router, &vpc, &isolated, case).await;

    // "To use an internet gateway, your subnet's route table must contain a route that
    // directs internet-bound traffic to the internet gateway" (Amazon VPC User Guide).
    let routed = subnet(router, &vpc, "10.98.16.0/20", case).await;
    let gateway = ec2_element(
        &ec2(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
        case,
    )
    .await;
    let table = route_table(router, &vpc, &routed, case).await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", &table),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", &gateway),
        ],
        case,
    )
    .await;
    Network { isolated, routed }
}

/// Register one Fargate-compatible `awsvpc` revision of `family` running `image`, pulling as
/// an execution role: the principal Amazon ECS fetches an image as.
async fn register(router: &MultiServiceRouter, family: &str, image: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/ecs-execution"),
            "containerDefinitions": [container("app", image)],
        }),
        case,
    )
    .await;
}

/// The `networkConfiguration` a placement in `subnet_id` runs under.
fn placement(subnet_id: &str) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": [subnet_id],
            "securityGroups": ["sg-0123456789abcdef0"],
            "assignPublicIp": "ENABLED",
        }
    })
}

/// Create one service of `family` placed in `subnet_id`, wanting no tasks yet — so nothing is
/// placed until the scale-up asks for it.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    family: &str,
    subnet_id: &str,
    case: &str,
) {
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": family,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": placement(subnet_id),
        }),
        case,
    )
    .await;
}

/// Scale one service to `count` through `UpdateService`.
async fn scale(router: &MultiServiceRouter, name: &str, count: i64, case: &str) {
    let body = ok(
        router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "desiredCount": count }),
        case,
    )
    .await;
    assert_eq!(
        body["service"]["desiredCount"].as_i64(),
        Some(count),
        "case {case:?}: the scaled service must want {count} task(s), got {body}"
    );
}

/// The task ARNs one service wants RUNNING, as `ListTasks` reports them.
async fn placed_task_arns(router: &MultiServiceRouter, name: &str, case: &str) -> Vec<String> {
    let body = ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": name, "desiredStatus": RUNNING }),
        case,
    )
    .await;
    list(&body, "taskArns", case)
        .into_iter()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect()
}

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the call
/// that placed it.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// Poll one task until it reports `status`, and answer the record it reported it in. The wait
/// is for the CONDITION: every move here is one the clock owes, not one a call makes.
async fn reported_as(
    router: &MultiServiceRouter,
    task_arn: &str,
    status: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, task_arn, case).await;
        let last_status = string(&task, "lastStatus", case);
        if last_status == status {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {status}, it was still \
             {last_status:?}: {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_failed_launch_keeps_its_own_stop_through_every_scheduler_stop() {
    let router = emulator_with_ec2();
    let network = network(&router).await;
    create_cluster(&router, CLUSTER, "setup").await;

    // Every row's launches are scheduled first, and the ARNs of the tasks they placed are
    // read back before anything else is asked of the scheduler: the stop each row asks for
    // lands on exactly these tasks.
    let mut placed: Vec<Vec<String>> = Vec::new();
    for row in &ROWS {
        let case = row.case;
        register(&router, &row.family(), IMAGE, case).await;
        create_service(
            &router,
            &row.service_name(),
            &row.family(),
            &network.isolated,
            case,
        )
        .await;
        scale(&router, &row.service_name(), row.desired, case).await;
        let arns = placed_task_arns(&router, &row.service_name(), case).await;
        assert_eq!(
            arns.len(),
            usize::try_from(row.desired).unwrap_or_default(),
            "case {case:?}: expected the scale-up to place {} task(s), got {arns:?}",
            row.desired
        );
        placed.push(arns);
    }

    for row in &ROWS {
        let case = row.case;
        let service = row.service_name();
        match row.stop {
            SchedulerStop::None => {}
            SchedulerStop::ScaleIn => scale(&router, &service, 0, case).await,
            SchedulerStop::ScaleInAndUp => {
                scale(&router, &service, 0, case).await;
                scale(&router, &service, row.desired, case).await;
            }
            SchedulerStop::Redeploy => {
                // `networkConfiguration` is one of the members whose own doc says it
                // "triggers a new service deployment", so this rolls one over the launches
                // that are still coming up: their tasks are tasks the new rollout replaces.
                ok(
                    &router,
                    "UpdateService",
                    json!({
                        "cluster": CLUSTER,
                        "service": service,
                        "networkConfiguration": placement(&network.isolated),
                    }),
                    case,
                )
                .await;
            }
            SchedulerStop::Delete => {
                ok(
                    &router,
                    "DeleteService",
                    json!({ "cluster": CLUSTER, "service": service, "force": true }),
                    case,
                )
                .await;
            }
        }
    }

    for (row, arns) in ROWS.iter().zip(&placed) {
        let case = row.case;
        for arn in arns {
            let task = reported_as(&router, arn, STOPPED, case).await;
            assert_eq!(
                string(&task, "stopCode", case),
                TASK_FAILED_TO_START,
                "case {case:?}: a task whose image could not be fetched never ran, got {task}"
            );
            let reason = string(&task, "stoppedReason", case);
            assert!(
                reason.contains(CANNOT_PULL) && reason.contains(IMAGE),
                "case {case:?}: expected the stop to report {CANNOT_PULL} for {IMAGE}, got \
                 {reason:?}"
            );
            // "The Unix timestamp for the time when the task started (the task transitioned
            // from the PENDING state to the RUNNING state)" — a task that never ran has none,
            // and its containers never exited, so they report no exit code either.
            assert!(
                task["startedAt"].is_null(),
                "case {case:?}: expected no startedAt on a task that never ran, got {task}"
            );
            for reported in list(&task, "containers", case) {
                assert!(
                    reported["exitCode"].is_null(),
                    "case {case:?}: expected no exitCode on a container that never ran, got \
                     {reported}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_scheduler_stop_of_a_task_that_ran_is_the_schedulers_own() {
    let case = "a task the placement could launch, scaled in after it ran";
    let router = emulator_with_ec2();
    let network = network(&router).await;
    create_cluster(&router, CLUSTER, case).await;
    register(&router, "failedlaunch-control", CONTROL_IMAGE, case).await;
    create_service(
        &router,
        "failedlaunch-control",
        "failedlaunch-control",
        &network.routed,
        case,
    )
    .await;
    scale(&router, "failedlaunch-control", 1, case).await;
    let arns = placed_task_arns(&router, "failedlaunch-control", case).await;
    let arn = arns
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the scale-up to place one task"));

    // The placement reaches the registry, so the launch is made and the task RUNS.
    let running = reported_as(&router, arn, RUNNING, case).await;
    assert!(
        running["stopCode"].is_null(),
        "case {case:?}: a task that is running carries no stop code, got {running}"
    );
    assert!(
        !running["startedAt"].is_null(),
        "case {case:?}: a running task reports when it started, got {running}"
    );

    // And the scale-in that takes it away IS the scheduler's own stop.
    scale(&router, "failedlaunch-control", 0, case).await;
    let stopped = reported_as(&router, arn, STOPPED, case).await;
    assert_eq!(
        string(&stopped, "stopCode", case),
        SERVICE_SCHEDULER_INITIATED,
        "case {case:?}: a running task the scheduler scaled in was stopped by the scheduler, \
         got {stopped}"
    );
    assert!(
        !stopped["startedAt"].is_null(),
        "case {case:?}: the task ran before the scheduler stopped it, got {stopped}"
    );
}
