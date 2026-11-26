// SPDX-License-Identifier: CC0-1.0

//! What a container reports as its exit status: the status its OWN program ended with, for a
//! non-essential init container as much as for any other — and what a dependency on that
//! status then admits.
//!
//! The rule under test, over the whole class rather than one request:
//!
//! > `Container$exitCode` is "the exit code returned from the container", so it is the status
//! > the container's own program ended with, whatever that program is and whether or not the
//! > container is essential. A `ContainerDependency` on it reads that same status:
//! > `ContainerDependency$condition` documents `COMPLETE` as "validates that a dependent
//! > container runs to completion (exits) before permitting other containers to start" and
//! > `SUCCESS` as "the same as `COMPLETE`, but it also requires that the container exits with a
//! > `zero` status" — so a container waiting on `SUCCESS` starts, and reports its OWN exit
//! > status in turn, exactly when the container it waits on ended zero, and gives up when that
//! > container ended non-zero.
//!
//! The rows differ along what the rule generalises over: the exit status the init container's
//! program ends with (zero and several non-zero values, including one past a single digit) and
//! the FORM the program ends it in (a wait and then an explicit status, an explicit status on
//! its own, a command that simply succeeds, a status reached through a conditional) — so no row
//! can be satisfied by a status fixed anywhere but the program.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTasks`, and every wait is a poll for the CONDITION rather than for a span.
//!
//! What is this suite's own: the init container's OWN `exitCode`. The sibling suite
//! `crate::container_dependency_give_up` tables the CONDITION a dependent waits for and what
//! the wait earns the TASK; this one tables the status the container waited on reports, which
//! is the value that condition is read off, and carries only as much of the dependent's
//! outcome as is needed to show the condition read that status and nothing else.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_absent, awsvpc_network_configuration, container_of, create_cluster, list, ok,
    poll_until, service, string,
};

/// The cluster every row's task runs in.
const CLUSTER: &str = "initexitstatus";

/// The image both containers of every row declare.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The two containers of every row: the non-essential one that runs first, and the essential
/// one whose start waits on it.
const INIT: &str = "init";
const APP: &str = "app";

/// The condition the essential container waits for, from `ContainerCondition`: the one that
/// "also requires that the container exits with a `zero` status".
const SUCCESS: &str = "SUCCESS";

/// The status the essential container's own program ends with, once it is permitted to start:
/// a value of its own, so a row cannot pass by reporting the init container's status for it.
const APP_EXIT_STATUS: i64 = 5;

/// The `TaskStopCode` of a task whose essential container ran and exited, and of one whose
/// container never started at all. Spelled out here rather than imported from the service
/// crate, so a wrong constant in the implementation cannot make this suite pass
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> TaskFailedToStart,
/// EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption,
/// TerminationNotice, InfrastructureHealth).
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// One row: the family it registers, the program its init container runs, and the status that
/// program ends with.
struct Row {
    case: &'static str,
    family: &'static str,
    /// The program the init container runs under a shell.
    program: &'static str,
    /// The status that program ends with, which is what the container reports.
    exit_status: i64,
}

/// The rows, each its own task-definition family and its own standalone task.
const ROWS: &[Row] = &[
    Row {
        case: "a wait and then a zero status",
        family: "initwaitzero",
        program: "sleep 3; exit 0",
        exit_status: 0,
    },
    Row {
        case: "a zero status on its own",
        family: "initzero",
        program: "exit 0",
        exit_status: 0,
    },
    Row {
        case: "a program that simply succeeds, with no status of its own",
        family: "initsucceeds",
        program: "echo ready",
        exit_status: 0,
    },
    Row {
        case: "a wait and then a one status",
        family: "initwaitone",
        program: "sleep 3; exit 1",
        exit_status: 1,
    },
    Row {
        case: "a non-zero status past a single digit",
        family: "initfortytwo",
        program: "exit 42",
        exit_status: 42,
    },
    Row {
        case: "a non-zero status reached through a conditional",
        family: "initconditional",
        program: "if [ -d / ]; then exit 9; else exit 0; fi",
        exit_status: 9,
    },
];

/// The two container definitions one row declares: the init container, which is NOT essential
/// — "if the `essential` parameter of a container is marked as `false`, its failure doesn't
/// affect the rest of the containers in a task" — and the essential `app`, whose start waits on
/// it ending zero.
fn containers(row: &Row) -> Value {
    json!([
        {
            "name": INIT,
            "image": IMAGE,
            "essential": false,
            "command": ["sh", "-c", row.program],
        },
        {
            "name": APP,
            "image": IMAGE,
            "essential": true,
            "command": ["sh", "-c", format!("exit {APP_EXIT_STATUS}")],
            "dependsOn": [{ "containerName": INIT, "condition": SUCCESS }],
        },
    ])
}

/// Register one row's Fargate revision and place one standalone task of it, answering the
/// task's ARN.
async fn place(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": containers(row),
        }),
        case,
    )
    .await;
    let placed = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&placed, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {placed}"
    );
    string(&tasks[0], "taskArn", case)
}

/// One task, as `DescribeTasks` reports it.
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the task {task_arn} back, got {body}"))
}

/// RULE: the init container reports the status its own program ended with, and the dependency
/// on a zero status is what decides whether the container waiting on it ever runs.
#[tokio::test]
async fn an_init_container_reports_the_status_its_own_program_ended_with() {
    let router = service();
    create_cluster(&router, CLUSTER, "the cluster every row's task runs in").await;

    // Every row's task is placed up front, so each row's own waits run beside the others
    // rather than one row's after another's.
    let mut placed: Vec<String> = Vec::new();
    for row in ROWS {
        placed.push(place(&router, row).await);
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

        let init = container_of(&task, INIT, case);
        assert_eq!(
            init["exitCode"],
            json!(row.exit_status),
            "case {case:?}: expected the init container to report the status its own program \
             ended with ({}), got {init}",
            row.exit_status
        );
        assert_eq!(
            init["lastStatus"],
            json!("STOPPED"),
            "case {case:?}: expected the init container to have run to completion, got {init}"
        );

        let app = container_of(&task, APP, case);
        if row.exit_status == 0 {
            assert_eq!(
                app["exitCode"],
                json!(APP_EXIT_STATUS),
                "case {case:?}: a dependency that ended zero admits the start, so the waiting \
                 container reports its own status ({APP_EXIT_STATUS}), got {app}"
            );
            assert_eq!(
                string(&task, "stopCode", case),
                ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: expected the stop an essential container's exit earns, got {task}"
            );
        } else {
            assert_absent(&app, "exitCode", case);
            assert_eq!(
                string(&task, "stopCode", case),
                TASK_FAILED_TO_START,
                "case {case:?}: expected the stop of a task whose container never started, got \
                 {task}"
            );
        }
    }
}
