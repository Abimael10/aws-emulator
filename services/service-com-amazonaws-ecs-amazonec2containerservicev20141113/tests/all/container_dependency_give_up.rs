// SPDX-License-Identifier: CC0-1.0

//! GIVING UP on a container dependency: what happens to a task one container of which waits
//! for a condition the container it waits on can no longer reach.
//!
//! The rule under test, over the whole class rather than one request:
//!
//! > A container whose `dependsOn` names a condition waits for the container it names to reach
//! > it, and `ContainerDefinition$startTimeout` states what ends that wait: "time duration (in
//! > seconds) to wait before giving up on resolving dependencies for a container. For example,
//! > you specify two containers in a task definition with containerA having a dependency on
//! > containerB reaching a `COMPLETE`, `SUCCESS`, or `HEALTHY` status. If a `startTimeout`
//! > value is specified for containerB and it doesn't reach the desired status within that time
//! > then containerA gives up and not start. This results in the task transitioning to a
//! > `STOPPED` state." So the wait ends — at once where the state the dependency has REACHED
//! > excludes the condition (a container that exited non-zero can never satisfy `SUCCESS`,
//! > which "requires that the container exits with a `zero` status", and one that has exited
//! > can never pass the health check `HEALTHY` waits for), and otherwise at the end of that
//! > `startTimeout` — and the task is then stopped as one whose launch could not be made:
//! > `Task$stopCode` `TaskFailedToStart` with a `stoppedReason` naming the container that gave
//! > up and the dependency it gave up on, and NO `exitCode` on a container that never ran.
//! > A condition the dependency DOES reach permits the start instead: `COMPLETE` "validates
//! > that a dependent container runs to completion (exits)" whatever status it exited with, so
//! > such a task runs its dependent container and ends with that container's own exit code
//! > under the stop an essential container's exit earns.
//!
//! The rows differ along what the rule generalises over: the CONDITION waited for (every
//! member of `ContainerCondition` the wait can end under), the status the dependency reaches
//! (a non-zero exit, a zero exit, a container that never exits at all), and which of the two
//! ends the wait — the state that excludes the condition, or the `startTimeout` running out.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTasks`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, poll_until, service, string};

/// The cluster every task below runs in.
const CLUSTER: &str = "dependson";

/// The image every container below names.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `Task$stopCode` of a task Amazon ECS could not launch, and of one whose essential
/// container exited, from the `TaskStopCode` enum (`smithy_query(query="shape",
/// shape="TaskStopCode")` -> TaskFailedToStart, EssentialContainerExited, UserInitiated,
/// ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth).
/// Spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The `stoppedReason` Amazon ECS words for the stop an essential container's exit earns.
const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// The `startTimeout` the one row that waits it out declares, in seconds: the smallest value
/// the member publishes for Fargate ("the valid values for Fargate are 2-120 seconds"), so
/// the row waits out a span AWS admits rather than one chosen for the test.
const START_TIMEOUT_SECONDS: i64 = 2;

/// The status the waiting container exits with when it DOES start, so a row that admits the
/// start reads the dependent container's own exit code rather than the dependency's.
const DEPENDENT_EXIT_STATUS: i64 = 3;

/// What the container the dependent waits on DOES.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dependency {
    /// It exits with a non-zero status, which `SUCCESS` excludes and `COMPLETE` admits.
    ExitsNonZero,
    /// It exits with a zero status, which both admit.
    ExitsZero,
    /// It never exits at all, so only a `startTimeout` can end a wait on it.
    StaysUp,
}

impl Dependency {
    /// The program the dependency container runs.
    fn command(self) -> Value {
        match self {
            // A non-zero status of its own, so the row reads a refusal that is the
            // dependency's exit status rather than an absent run.
            Self::ExitsNonZero => json!(["sh", "-c", "exit 7"]),
            Self::ExitsZero => json!(["sh", "-c", "exit 0"]),
            Self::StaysUp => json!(["sh", "-c", "while true; do sleep 1; done"]),
        }
    }
}

/// What the wait earns the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The dependent container gave up: the task never ran it, so it is stopped as a task
    /// whose launch could not be made.
    GaveUp,
    /// The condition was reached: the dependent container started, ran its program and
    /// exited, which is the stop an essential container's exit earns.
    Admitted,
}

/// One row: the family it registers, the condition the dependent waits for, what the
/// dependency does, the `startTimeout` the dependency declares, and what that owes the task.
struct Row {
    case: &'static str,
    family: &'static str,
    /// A member of `ContainerCondition`: START, COMPLETE, SUCCESS or HEALTHY.
    condition: &'static str,
    dependency: Dependency,
    /// Whether the dependency declares a `healthCheck` at all — the probe a `HEALTHY`
    /// condition waits for.
    health_check: bool,
    /// `ContainerDefinition$startTimeout` on the DEPENDENCY, for the row whose wait only that
    /// member can end.
    start_timeout: Option<i64>,
    outcome: Outcome,
}

/// The rows, each its own task-definition family and its own standalone task.
const ROWS: &[Row] = &[
    Row {
        case: "SUCCESS on a dependency that exited non-zero",
        family: "dependsuccessfail",
        condition: "SUCCESS",
        dependency: Dependency::ExitsNonZero,
        health_check: false,
        start_timeout: None,
        outcome: Outcome::GaveUp,
    },
    Row {
        case: "SUCCESS on a dependency that exited zero",
        family: "dependsuccessok",
        condition: "SUCCESS",
        dependency: Dependency::ExitsZero,
        health_check: false,
        start_timeout: None,
        outcome: Outcome::Admitted,
    },
    Row {
        case: "COMPLETE on a dependency that exited non-zero",
        family: "dependcompletefail",
        condition: "COMPLETE",
        dependency: Dependency::ExitsNonZero,
        health_check: false,
        start_timeout: None,
        outcome: Outcome::Admitted,
    },
    Row {
        case: "START on a dependency that exited non-zero",
        family: "dependstart",
        condition: "START",
        dependency: Dependency::ExitsNonZero,
        health_check: false,
        start_timeout: None,
        outcome: Outcome::Admitted,
    },
    Row {
        case: "HEALTHY on a dependency that exited before its probe passed",
        family: "dependhealthyexited",
        condition: "HEALTHY",
        dependency: Dependency::ExitsZero,
        health_check: true,
        start_timeout: None,
        outcome: Outcome::GaveUp,
    },
    Row {
        case: "HEALTHY on a dependency that passes its probe",
        family: "dependhealthyup",
        condition: "HEALTHY",
        dependency: Dependency::StaysUp,
        health_check: true,
        start_timeout: None,
        outcome: Outcome::Admitted,
    },
    Row {
        case: "COMPLETE on a dependency that never exits, under a startTimeout",
        family: "dependtimeout",
        condition: "COMPLETE",
        dependency: Dependency::StaysUp,
        health_check: false,
        start_timeout: Some(START_TIMEOUT_SECONDS),
        outcome: Outcome::GaveUp,
    },
];

/// The `stoppedReason` of a task whose `dependent` gave up on its dependency on `dependency`
/// reaching `condition`, as the implementation words it: fixed text stating what was given up
/// on, with the two container names and the condition taken from the task definition.
fn gave_up_reason(dependent: &str, dependency: &str, condition: &str) -> String {
    format!(
        "Container {dependent} gave up on resolving its dependency on container {dependency} \
         reaching a {condition} status."
    )
}

/// The two container definitions one row registers: the DEPENDENCY, which is not essential —
/// so what stops the task is never its own exit — and the dependent `app`, which is.
fn containers(row: &Row) -> Value {
    let mut dependency = json!({
        "name": "dep",
        "image": IMAGE,
        "memory": 128,
        "essential": false,
        "command": row.dependency.command(),
    });
    if row.health_check {
        // A probe that always passes, believed on its first execution: what a row with a
        // `HEALTHY` condition waits for is the probe's verdict, and the verdict only ever
        // arrives for a container that is still running to be probed.
        dependency["healthCheck"] = json!({
            "command": ["CMD-SHELL", "exit 0"],
            "interval": 5,
            "timeout": 2,
            "retries": 1,
        });
    }
    if let Some(seconds) = row.start_timeout {
        dependency["startTimeout"] = json!(seconds);
    }
    json!([
        dependency,
        {
            "name": "app",
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", format!("exit {DEPENDENT_EXIT_STATUS}")],
            "dependsOn": [{ "containerName": "dep", "condition": row.condition }],
        },
    ])
}

/// Register one row's revision and run one standalone task of it, answering the task's ARN.
async fn run_one(router: &MultiServiceRouter, row: &Row) -> String {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": row.family, "containerDefinitions": containers(row) }),
        row.case,
    )
    .await;
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "count": 1,
            "launchType": "EC2",
        }),
        row.case,
    )
    .await;
    let task = list(&body, "tasks", row.case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {:?}: expected a task back, got {body}", row.case));
    string(&task, "taskArn", row.case)
}

/// Read one task back through `DescribeTasks`.
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// One container of a described task, by name.
fn container_of(task: &Value, name: &str, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"] == json!(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected the container {name}, got {task}"))
}

/// RULE: a container gives up on a dependency it can no longer have, and the task that holds
/// it is stopped as one that failed to start.
#[tokio::test]
async fn a_container_that_gives_up_on_a_dependency_fails_its_task_to_start() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's task runs in").await;

    // Every row's task is placed up front, so each row's own waits run beside the others
    // rather than one row's budget after another's.
    let mut placed: Vec<String> = Vec::new();
    for row in ROWS {
        placed.push(run_one(&router, row).await);
    }

    for (row, task_arn) in ROWS.iter().zip(&placed) {
        let case = row.case;
        let task = poll_until(
            case,
            "the task to stop",
            || describe(&router, task_arn, case),
            |task| task["lastStatus"] == json!("STOPPED"),
        )
        .await;
        let app = container_of(&task, "app", case);
        match row.outcome {
            Outcome::GaveUp => {
                assert_eq!(
                    string(&task, "stopCode", case),
                    TASK_FAILED_TO_START,
                    "case {case:?}: a task whose container never started failed to start, got \
                     {task}"
                );
                assert_eq!(
                    string(&task, "stoppedReason", case),
                    gave_up_reason("app", "dep", row.condition),
                    "case {case:?}: expected the reason to name the container that gave up and \
                     the dependency it gave up on, got {task}"
                );
                assert_eq!(
                    app["exitCode"],
                    Value::Null,
                    "case {case:?}: a container that never ran produced no exit code, got {app}"
                );
                assert_eq!(
                    string(&app, "reason", case),
                    gave_up_reason("app", "dep", row.condition),
                    "case {case:?}: expected the container that gave up to carry the detail of \
                     it, got {app}"
                );
            }
            Outcome::Admitted => {
                assert_eq!(
                    app["exitCode"],
                    json!(DEPENDENT_EXIT_STATUS),
                    "case {case:?}: expected the dependent container to have run and exited \
                     with its own status, got {app}"
                );
                assert_eq!(
                    string(&task, "stopCode", case),
                    ESSENTIAL_CONTAINER_EXITED,
                    "case {case:?}: a task whose essential container exited stops for that \
                     exit, got {task}"
                );
                assert_eq!(
                    string(&task, "stoppedReason", case),
                    ESSENTIAL_CONTAINER_EXITED_REASON,
                    "case {case:?}: expected the reason Amazon ECS words for that stop, got \
                     {task}"
                );
            }
        }
    }
}
