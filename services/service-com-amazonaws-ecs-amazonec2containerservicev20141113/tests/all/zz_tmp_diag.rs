// SPDX-License-Identifier: CC0-1.0

//! WHICH container program lets an Amazon ECS service keep the task it placed, driven end to
//! end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > A service's rollout is stable only while the tasks it placed are RUNNING, and a task runs
//! > only while its essential container's program does. A wait a program states in seconds is a
//! > duration this emulator takes as STATED rather than one it spends, so a program that ends on
//! > such a wait ends at once: its task stops with `TaskStopCode` `EssentialContainerExited` and
//! > the status that program exited with. A wait the program goes round a LOOP on names no
//! > duration of the program's own work, so it is honoured — which is how a container stays up
//! > and how the service's rollout comes to rest with its desired count running.
//!
//! The docs the rule is read off:
//!
//! * `Task$stopCode` -> `TaskStopCode` (`smithy_query(query="shape",shape="TaskStopCode")`:
//!   TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated,
//!   SpotInterruption, TerminationNotice, InfrastructureHealth) and `Task$stoppedReason`,
//!   "The reason that the task was stopped".
//! * `Container$exitCode`, "The exit code returned from the container".
//! * `ServiceDeployment$status` -> `ServiceDeploymentStatus`, whose `SUCCESSFUL` is where a
//!   rollout that stabilised comes to rest, and `Service$runningCount`, "The number of tasks in
//!   the cluster that are in the RUNNING state".
//!
//! The rows differ along the one thing the rule turns on — whether the program's wait names an
//! end of its own or is one the program goes round a loop on — and each is asserted on a
//! CONDITION a poll waits for, never on how long anything took. The sibling suite
//! `crate::container_stated_waits` pins the same reading on a task run on its own; this one pins
//! what it means for the SERVICE that placed the task, which is the premise every rollout suite
//! of this crate rests on.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, poll_until, service, string};

/// The cluster both services below run in.
const CLUSTER: &str = "staysupcluster";

/// A program whose wait is one it goes round a LOOP on, which is the container that stays up.
const LOOPS: &str = "echo up; while true; do sleep 1; done";

/// A program that ENDS on a wait it states in seconds: the wait is taken as stated, so the
/// program's own end follows at once and the container exits with its status.
const STATES_A_WAIT: &str = "echo up; sleep 600";

/// The `TaskStopCode` of a task whose essential container ended. Spelled out here rather than
/// imported from the service crate, so a wrong constant in the implementation cannot make this
/// suite pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The `ServiceDeploymentStatus` a rollout that stabilised comes to rest in.
const SUCCESSFUL: &str = "SUCCESSFUL";

/// Register one revision of `family` whose single essential container runs `program` under a
/// shell, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    program: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/nginx/nginx:latest",
                // "at least one of 'memory' or 'memoryReservation' must be specified" for a
                // container of a revision this service places.
                "memory": 128,
                "essential": true,
                "command": ["sh", "-c", program],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Create a service of one task on `definition`, and answer the ARN of the service deployment
/// its release is recorded as.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    string(&body["service"], "currentServiceDeployment", case)
}

/// The service, as `DescribeServices` reports it.
async fn describe_service(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// The rollout, as `DescribeServiceDeployments` reports it.
async fn describe_deployment(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [arn] }),
        case,
    )
    .await;
    list(&body, "serviceDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout {arn} back, got {body}"))
}

/// One STOPPED task of the service, as `DescribeTasks` reports it, looked for until the service
/// has one.
async fn a_stopped_task(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let group = format!("service:{name}");
    poll_until(
        case,
        "a stopped task of the service",
        || async {
            let listed = ok(
                router,
                "ListTasks",
                json!({ "cluster": CLUSTER, "family": name, "desiredStatus": "STOPPED" }),
                case,
            )
            .await;
            let arns: Vec<String> = listed["taskArns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .take(1)
                .collect();
            if arns.is_empty() {
                return json!({ "tasks": [] });
            }
            ok(
                router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": arns }),
                case,
            )
            .await
        },
        move |answered| {
            answered["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|task| {
                    task["lastStatus"] == json!("STOPPED") && task["group"] == json!(group)
                })
        },
    )
    .await["tasks"][0]
        .clone()
}

/// RULE: a wait the program goes round a LOOP on keeps the container up, so the service's
/// rollout comes to rest SUCCESSFUL with its desired count running.
#[tokio::test]
async fn a_rollout_stabilises_on_a_program_whose_wait_is_a_loop() {
    let case = "a program whose wait is one it goes round a loop on";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register(&router, "loopingfamily", LOOPS, case).await;
    let released = create_service(&router, "loopingfamily", &definition, case).await;

    let deployment = poll_until(
        case,
        &format!("the rollout to reach {SUCCESSFUL:?}"),
        || describe_deployment(&router, &released, case),
        |deployment| deployment["status"] == json!(SUCCESSFUL),
    )
    .await;
    assert_eq!(
        deployment["targetServiceRevision"]["runningTaskCount"].as_i64(),
        Some(1),
        "case {case:?}: expected the rollout's revision to hold its one running task, got \
         {deployment}"
    );
    let described = describe_service(&router, "loopingfamily", case).await;
    assert_eq!(
        described["desiredCount"].as_i64(),
        Some(1),
        "case {case:?}: expected the service to want its one task, got {described}"
    );
    assert_eq!(
        described["runningCount"].as_i64(),
        Some(1),
        "case {case:?}: expected the service to run that one task, got {described}"
    );
}

/// RULE: a program that ENDS on a wait it states in seconds ends at once, so the task the
/// service placed stops with `EssentialContainerExited` and the status that program exited
/// with — which is why no rollout can rest on such a program.
#[tokio::test]
async fn a_task_whose_program_only_states_its_wait_stops_at_once() {
    let case = "a program that ends on a wait it states in seconds";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register(&router, "statedfamily", STATES_A_WAIT, case).await;
    create_service(&router, "statedfamily", &definition, case).await;

    let task = a_stopped_task(&router, "statedfamily", case).await;
    assert_eq!(
        task["stopCode"].as_str(),
        Some(ESSENTIAL_CONTAINER_EXITED),
        "case {case:?}: expected the stop of a container that ended, got {task}"
    );
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?}: expected the status the program exited with, got {task}"
    );
}
