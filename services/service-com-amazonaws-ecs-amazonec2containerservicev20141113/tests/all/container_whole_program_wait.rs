// SPDX-License-Identifier: CC0-1.0

//! What a container GIVEN NOTHING BUT A WAIT does, and what a wait inside a program that does
//! something else still costs — driven end to end over the shared wire harness in
//! [`crate::ecs_wire`], so every value asserted is read back through `DescribeTasks`.
//!
//! RULE under test: **a container runs the program its definition states, so a container whose
//! WHOLE program is a wait is a container with nothing to do but wait: the seconds it names
//! are the span it runs for, not that program's account of work it did, so the task goes on
//! RUNNING and reports no exit status — exactly as a wait that names no end of its own keeps
//! it up.** A wait inside a program that does something ELSE is the other half of the same
//! rule, which [`crate::container_stated_waits`] pins: there the seconds are that program's
//! own account of how long it took, so they are taken as stated and what it does next — its
//! next step, or its own end — follows at once, and nothing waiting on the task is held by a
//! program's own words.
//!
//! POSIX states what the wait means for the program: "the `sleep` utility shall suspend
//! execution for at least the integral number of seconds specified by the `time` operand" — so
//! a process whose whole program is that utility is still there for those seconds, which is
//! what `Task$lastStatus` ("the last known status of the task") reports of a task while its
//! containers are up, and `Container$exitCode` ("the exit code returned from the container")
//! is absent until the program has ended.
//!
//! The rows differ along what the rule generalises over: HOW LONG the wait names (an hour, ten
//! minutes, two minutes, fifteen minutes — each far beyond this suite's own budget — and no
//! end at all), WHERE the program comes from (the `command` alone and the `entryPoint`/
//! `command` pair a runtime exec's as one vector), and the BOUNDARY: the same wait inside a
//! shell program, which is a program that states how long it took.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, poll_until, present_count, service, string,
};

/// The cluster every task below runs in.
const CLUSTER: &str = "wholeprogramwait";

/// The image every container below names. Nothing is fetched from it: what runs is the program
/// the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `Task$lastStatus` members this suite names.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How long the rows that must GO ON running are watched, re-read inside the loop that spends
/// it: their containers were given nothing but a wait far longer than this window, so a run
/// that spent one is still running at the end of it however slowly the host got there.
const WATCH_WINDOW: Duration = Duration::from_secs(2);

/// How often that window re-reads.
const WATCH_INTERVAL: Duration = Duration::from_millis(100);

/// What one row's container does with the time its program names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The wait is the whole program, or it names no end of its own: the container stays up,
    /// so the task goes on RUNNING through [`WATCH_WINDOW`] and reports no exit status.
    RunsOn,
    /// The program states the wait among steps of its own: the task stops with the status that
    /// program exited with, which a run that SPENT the seconds it named could not reach inside
    /// this suite's budget.
    Exits(i64),
}

/// One row: the program the container is given, and what its task then does.
struct Row {
    case: &'static str,
    /// `ContainerDefinition$entryPoint`, which the runtime exec's before the command.
    entry_point: Option<&'static [&'static str]>,
    /// `ContainerDefinition$command`.
    command: &'static [&'static str],
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        case: "a wait of an hour as the whole program",
        entry_point: None,
        command: &["sleep", "3600"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait of ten minutes as the whole program",
        entry_point: None,
        command: &["sleep", "600"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait of two minutes as the whole program",
        entry_point: None,
        command: &["sleep", "120"],
        ends: Ends::RunsOn,
    },
    Row {
        // The vector the runtime exec's is the entry point followed by the command, so the
        // whole program is still nothing but the wait.
        case: "the same wait spelled as an entry point and its argument",
        entry_point: Some(&["sleep"]),
        command: &["900"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait with no end of its own as the whole program",
        entry_point: None,
        command: &["sleep", "infinity"],
        ends: Ends::RunsOn,
    },
    Row {
        // The boundary: a shell program states how long it took, wherever the wait stands in
        // it, so the step after this one follows at once.
        case: "a wait a shell program carries on from",
        entry_point: None,
        command: &["/bin/sh", "-c", "sleep 1800; exit 4"],
        ends: Ends::Exits(4),
    },
    Row {
        case: "a wait a shell program ends on, after a step that wrote",
        entry_point: None,
        command: &["/bin/sh", "-c", "echo up; sleep 1800"],
        ends: Ends::Exits(0),
    },
];

/// Register one revision of `family` running the row's program, and place one task of it.
/// Answers the task's ARN.
async fn place(router: &MultiServiceRouter, family: &str, row: &Row) -> String {
    let case = row.case;
    let mut container = json!({
        "name": "app",
        "image": IMAGE,
        "memory": 128,
        "essential": true,
        "command": row.command,
    });
    if let Some(entry_point) = row.entry_point {
        container["entryPoint"] = json!(entry_point);
    }
    ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": [container] }),
        case,
    )
    .await;
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
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

/// The exit status the one container of a described task reports, and `None` for a container
/// that reported none.
fn exit_code_of(task: &Value, case: &str) -> Option<i64> {
    let containers = list(task, "containers", case);
    let container = containers
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected one container, got {task}"));
    present_count(container, "exitCode", case)
}

#[tokio::test]
async fn a_container_given_nothing_but_a_wait_goes_on_running() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "whole program wait").await;

    // Every row's task is launched FIRST and read afterwards: the runs happen in parallel in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("wholewait{index}");
        launched.push((row, place(&router, &family, row).await));
    }

    // The rows whose program goes on: the task stops on the status that program exited with,
    // inside a budget far shorter than the seconds it said it waited.
    for (row, task_arn) in &launched {
        let Ends::Exits(status) = row.ends else {
            continue;
        };
        let case = row.case;
        let task = poll_until(
            case,
            "the task to stop on the status its program exited with",
            || describe(&router, task_arn, case),
            |task| task["lastStatus"] == json!(STOPPED),
        )
        .await;
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: the task stopped because its essential container exited, got {task}"
        );
        assert_eq!(
            exit_code_of(&task, case),
            Some(status),
            "case {case:?}: the container reports the status its own program exited with, got \
             {task}"
        );
    }

    // The rows given nothing but a wait: each task reaches RUNNING and STAYS there. Watched in
    // ONE loop over every such row, so the window is spent once rather than once per row, and
    // re-read INSIDE it, so what the assertion rests on is the condition holding read after
    // read.
    let waiting: Vec<(&Row, &String)> = launched
        .iter()
        .filter(|(row, _)| row.ends == Ends::RunsOn)
        .map(|(row, task_arn)| (*row, task_arn))
        .collect();
    for (row, task_arn) in &waiting {
        let case = row.case;
        poll_until(
            case,
            "the task to reach RUNNING",
            || describe(&router, task_arn, case),
            |task| task["lastStatus"] == json!(RUNNING),
        )
        .await;
    }
    let until = Instant::now() + WATCH_WINDOW;
    while Instant::now() < until {
        tokio::time::sleep(WATCH_INTERVAL).await;
        for (row, task_arn) in &waiting {
            let case = row.case;
            let task = describe(&router, task_arn, case).await;
            assert_eq!(
                string(&task, "lastStatus", case),
                RUNNING,
                "case {case:?}: a container given nothing but a wait is still there, so its \
                 task goes on running, got {task}"
            );
            assert_eq!(
                exit_code_of(&task, case),
                None,
                "case {case:?}: no exit status is reported for a program that has not ended, \
                 got {task}"
            );
        }
    }
}
