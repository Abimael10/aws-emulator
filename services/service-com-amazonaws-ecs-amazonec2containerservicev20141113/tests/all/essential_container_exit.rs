// SPDX-License-Identifier: CC0-1.0

//! What a service does when the CONTAINER of one of its tasks ends: the task that is
//! stopped with it, the replacement its scheduler places, and the rollout the deployment
//! circuit breaker fails when the replacements keep ending the same way.
//!
//! The rules under test:
//!
//! 1. `ContainerDefinition$essential`: "If the `essential` parameter of a container is
//!    marked as `true`, and that container fails or stops for any reason, all other
//!    containers that are part of the task are stopped" — so a task whose ESSENTIAL
//!    container's program has ended is stopped, with the `TaskStopCode`
//!    `EssentialContainerExited` ("The task stopped because an essential container
//!    exited"), whatever status that container exited with; and a task whose NON-essential
//!    container ends keeps running, because "if the `essential` parameter of a container is
//!    marked as `false`, its failure doesn't affect the rest of the containers in a task".
//! 2. `Deployment$failedTasks`: "The number of consecutively failed tasks in the
//!    deployment. A task is considered a failure if the service scheduler can't launch the
//!    task, the task doesn't transition to a `RUNNING` state, or if it fails any of its
//!    defined health checks and is stopped" — with the note that "once a service deployment
//!    has one or more successfully running tasks, the failed task count resets to zero". A
//!    rollout whose tasks keep ending therefore reaches the circuit breaker's threshold,
//!    and `Deployment$rolloutState` makes the result terminal: "If the service fails to
//!    reach a steady state and circuit breaker is turned on, the deployment transitions to
//!    a `FAILED` state", with `DeploymentCircuitBreaker$rollback` deciding where that
//!    leaves the service — "the service is rolled back to the last deployment that
//!    completed successfully".
//!
//! The containers below run REAL programs through the container data plane (the same
//! session a `RunTask` container's command runs in), so the exit each row asserts on is one
//! a program produced rather than one this suite wrote into the state struct. Every value
//! asserted is read back through `DescribeServices`, `ListTasks` and `DescribeTasks`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, count, list, ok, service, string};

/// The cluster every service below runs in.
const CLUSTER: &str = "exiting";

/// The image every container below names. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The command a container that stays up runs.
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// How often a poll re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying. Generous: a rollout takes one scheduler step per task it
/// replaces, the breaker tolerates several failures before it fires, and every poll stops
/// the instant its own condition holds.
const POLL_BUDGET: Duration = Duration::from_secs(90);

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The `rolloutState` of a rollout the circuit breaker failed, and the reason it carries.
const FAILED: &str = "FAILED";

/// One container definition running `command`, essential or not.
fn container(name: &str, command: &[&str], essential: bool) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "memory": 128,
        "essential": essential,
        "command": command,
    })
}

/// Register one revision of `family` with `containers`, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    containers: Vec<Value>,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": containers }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Create one service of `definition` in [`CLUSTER`], with `configuration` as its
/// `deploymentConfiguration`.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    configuration: Value,
    case: &str,
) {
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
            "deploymentConfiguration": configuration,
        }),
        case,
    )
    .await;
}

/// The service `DescribeServices` reports.
async fn describe(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
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

/// The service's current (`PRIMARY`) deployment.
fn primary(view: &Value, case: &str) -> Value {
    list(view, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {view}"))
}

/// Re-read one service until `settled` holds of it, and answer it.
async fn poll_service(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    what: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let view = describe(router, name, case).await;
        if settled(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {view}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The task ARNs `ListTasks` reports for one service, in the `desiredStatus` asked for.
async fn task_arns(
    router: &MultiServiceRouter,
    name: &str,
    desired_status: &str,
    case: &str,
) -> Vec<String> {
    let body = ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "desiredStatus": desired_status,
        }),
        case,
    )
    .await;
    list(&body, "taskArns", case)
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect()
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// One row of the essential-container table: what the revision's containers do, and whether
/// the task they belong to is stopped when the one that ends has ended.
#[derive(Debug, Clone, Copy)]
struct ExitCase {
    case: &'static str,
    /// The command the container that ENDS runs.
    command: &'static [&'static str],
    /// Whether that container is essential.
    essential: bool,
    /// Whether a SECOND, essential container that stays up is in the revision beside it.
    beside_a_container_that_stays_up: bool,
    /// Whether the task is stopped when that container has ended.
    stops_the_task: bool,
}

#[tokio::test]
async fn a_service_task_is_stopped_and_replaced_when_its_essential_container_ends() {
    let rows: [ExitCase; 4] = [
        ExitCase {
            // "that container fails or stops FOR ANY REASON": a clean exit ends the task
            // exactly as a failing one does.
            case: "an essential container that exits zero",
            command: &["sh", "-c", "exit 0"],
            essential: true,
            beside_a_container_that_stays_up: false,
            stops_the_task: true,
        },
        ExitCase {
            case: "an essential container that exits non-zero",
            command: &["sh", "-c", "echo leaving; exit 7"],
            essential: true,
            beside_a_container_that_stays_up: false,
            stops_the_task: true,
        },
        ExitCase {
            // The same exit beside a container that stays up: the task still stops,
            // because "all other containers that are part of the task are stopped".
            case: "an essential container that exits beside one that stays up",
            command: &["sh", "-c", "exit 3"],
            essential: true,
            beside_a_container_that_stays_up: true,
            stops_the_task: true,
        },
        ExitCase {
            // "If the essential parameter of a container is marked as false, its failure
            // doesn't affect the rest of the containers in a task."
            case: "a non-essential container that exits",
            command: &["sh", "-c", "exit 9"],
            essential: false,
            beside_a_container_that_stays_up: true,
            stops_the_task: false,
        },
    ];

    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let family = format!("exitingfamily{index}");
        let name = format!("exitingservice{index}");
        let mut containers = vec![container("ending", row.command, row.essential)];
        if row.beside_a_container_that_stays_up {
            containers.push(container("staying", &STAYS_UP, true));
        }
        let definition = register(&router, &family, containers, case).await;
        // The circuit breaker is OFF here: this row is about the task, and a rollout the
        // breaker could fail would stop the replacing this row observes.
        create_service(&router, &name, &definition, json!({}), case).await;

        if row.stops_the_task {
            // The task is stopped with the code a task whose essential container exited
            // earns, and the scheduler maintains the count: a replacement is placed.
            let deadline = Instant::now() + POLL_BUDGET;
            let (arn, task) = loop {
                let arns = task_arns(&router, &name, "STOPPED", case).await;
                if let Some(arn) = arns.first() {
                    let task = describe_task(&router, arn, case).await;
                    if !string(&task, "stopCode", case).is_empty() {
                        break (arn.clone(), task);
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected a stopped task with a stop code, got {arns:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            assert_eq!(
                string(&task, "stopCode", case),
                ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: a task whose essential container ended carries the stop \
                 code for it, got {task}"
            );
            assert!(
                !string(&task, "stoppedReason", case).is_empty(),
                "case {case:?}: such a task says why it stopped, got {task}"
            );
            let replaced = poll_service(
                &router,
                &name,
                case,
                "the scheduler to place a replacement",
                |view| count(view, "runningCount", case) + count(view, "pendingCount", case) > 0,
            )
            .await;
            let live = task_arns(&router, &name, "RUNNING", case).await;
            assert!(
                live.iter().all(|other| *other != arn),
                "case {case:?}: the replacement is a new task, got {live:?} beside {arn} \
                 ({replaced})"
            );
        } else {
            // The task keeps running: its essential container is still up, so nothing the
            // non-essential one did ends it.
            let running = poll_service(
                &router,
                &name,
                case,
                "the service to hold its task",
                |view| count(view, "runningCount", case) == 1,
            )
            .await;
            assert_eq!(
                count(&running, "runningCount", case),
                1,
                "case {case:?}: a non-essential container's exit ends no task, got {running}"
            );
            let stopped = task_arns(&router, &name, "STOPPED", case).await;
            assert!(
                stopped.is_empty(),
                "case {case:?}: no task of this service is stopped, got {stopped:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_circuit_breaker_fails_and_rolls_back_a_rollout_whose_tasks_keep_ending() {
    let case = "a rollout whose tasks keep ending";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let family = "breakerexiting";
    let good = register(
        &router,
        family,
        vec![container("app", &STAYS_UP, true)],
        case,
    )
    .await;
    let bad = register(
        &router,
        family,
        vec![container("app", &["sh", "-c", "exit 1"], true)],
        case,
    )
    .await;
    let name = "breakerexiting";
    create_service(
        &router,
        name,
        &good,
        json!({ "deploymentCircuitBreaker": { "enable": true, "rollback": true } }),
        case,
    )
    .await;
    // The rollout that COMPLETED, which is the one a rollback puts back.
    poll_service(
        &router,
        name,
        case,
        "the first rollout to complete",
        |view| string(&primary(view, case), "rolloutState", case) == "COMPLETED",
    )
    .await;

    ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "taskDefinition": bad }),
        case,
    )
    .await;

    // The breaker counts the tasks the rollout loses and fails it once they reach its
    // threshold: the rollout leaves IN_PROGRESS for the terminal FAILED, with the reason
    // that names the breaker.
    let failed = poll_service(
        &router,
        name,
        case,
        "the circuit breaker to fail the rollout",
        |view| {
            list(view, "deployments", case)
                .iter()
                .any(|deployment| string(deployment, "rolloutState", case) == FAILED)
        },
    )
    .await;
    let reason = list(&failed, "deployments", case)
        .iter()
        .find(|deployment| string(deployment, "rolloutState", case) == FAILED)
        .map(|deployment| string(deployment, "rolloutStateReason", case))
        .unwrap_or_default();
    assert!(
        reason.contains("circuit breaker"),
        "case {case:?}: the failed rollout's reason names the breaker, got {reason:?} \
         ({failed})"
    );

    // And the rollback takes the service back to the revision that completed, running the
    // count it wants.
    let rolled_back = poll_service(
        &router,
        name,
        case,
        "the service to be rolled back to the revision that completed",
        |view| {
            string(view, "taskDefinition", case) == good
                && count(view, "runningCount", case) == 1
                && count(view, "pendingCount", case) == 0
        },
    )
    .await;
    assert_eq!(
        string(&rolled_back, "taskDefinition", case),
        good,
        "case {case:?}: expected the service back on the revision that completed, got \
         {rolled_back}"
    );
}
