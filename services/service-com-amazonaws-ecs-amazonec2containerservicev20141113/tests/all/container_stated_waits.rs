// SPDX-License-Identifier: CC0-1.0

//! What a duration a container's PROGRAM states costs this emulator, and what it does not.
//!
//! RULE under test: **the seconds in a container's `sleep` are a caller's word about how long
//! its program took, so a wait the program states BETWEEN two of its own steps is taken as
//! stated and the next step runs at once — the task stops on the status its program exited
//! with, however many seconds that program said it waited. The wait a program has nothing left
//! to do but sit in is another thing: a wait it ENDS on, one with no end of its own (`sleep
//! infinity`) and one it goes round a LOOP on are the container waiting for something else to
//! end it, which is how a container stays up, so those are honoured and the task goes on
//! RUNNING.**
//!
//! Why the bound has to be this emulator's: a container's program is request-supplied, and
//! everything that waits on its task waits with it — the AWS Batch job whose dependents are
//! held until it ends (`core/router/tests/all/batch_job_dependency_gate.rs`), the deployment
//! whose stabilisation is watched, the client polling `DescribeTasks`. A program's own words
//! must not decide how long any of them takes to reach a verdict, exactly as they must not
//! decide how many steps it may take (`MAX_COMMANDS_PER_RUN`) or how long the run may live
//! (the run's deadline).
//!
//! Sources: `smithy_query(query="operation",operation="DescribeTasks")` for `Task$lastStatus`,
//! `Task$stopCode`/`stoppedReason` and `Container$exitCode` ("the exit code returned from the
//! container"); `smithy_query(query="shape",shape="TaskStopCode")` for the stop code of a task
//! whose essential container exited. POSIX states what `sleep` means for the PROGRAM ("suspend
//! execution for at least the integral number of seconds"), which is the duration it states;
//! nothing in the model or the guide says a service's own reporting waits for it.
//!
//! The rows differ along what the rule generalises over: WHERE the wait stands in the program
//! (between two steps, among several, inside a branch that is followed by more, last, last
//! after other steps, inside a loop, with no end of its own), HOW LONG it states (seconds far
//! beyond this suite's whole budget, down to one second) and the FORM the container states it
//! in (a shell program and a bare argument vector).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, present_count, service, string};

/// The cluster every task below runs in.
const CLUSTER: &str = "statedwaits";

/// The image every container below names. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// How often a poll re-reads, and how long it keeps trying. The budget is generous and every
/// poll stops the instant its own condition holds; it is far under the seconds the programs of
/// the stopping rows state, which is what makes those rows about the RULE rather than about
/// this machine's speed.
const POLL_INTERVAL: Duration = Duration::from_millis(60);
const POLL_BUDGET: Duration = Duration::from_secs(90);

/// How long the rows that must GO ON running are watched, re-read inside the loop that spends
/// it. Far under the thirty seconds those programs end on, so a run that honoured the wait is
/// still running at the end of it however slowly the host got there.
const WATCH_WINDOW: Duration = Duration::from_secs(2);

/// `Task$lastStatus` members this suite names.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// What one row's program does with the time it states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The program states a wait and then goes ON: the task stops with this exit status, which
    /// a run that SPENT the stated seconds could not reach inside [`POLL_BUDGET`].
    Exits(i64),
    /// The program has nothing left to do but wait, so the container stays up and the task
    /// goes on RUNNING through [`WATCH_WINDOW`].
    RunsOn,
}

/// One row: the `command` the container states and what the task then does.
struct Row {
    case: &'static str,
    command: &'static [&'static str],
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        case: "a wait between two steps",
        command: &["/bin/sh", "-c", "sleep 600; exit 7"],
        ends: Ends::Exits(7),
    },
    Row {
        case: "a wait among several steps",
        command: &["/bin/sh", "-c", "echo one; sleep 1200; echo two; exit 0"],
        ends: Ends::Exits(0),
    },
    Row {
        // The wait stands inside a compound form, and what follows it is outside that form:
        // the program still has a step to come, so the wait is stated.
        case: "a wait inside a branch the program carries on from",
        command: &["/bin/sh", "-c", "if true; then sleep 900; fi; exit 3"],
        ends: Ends::Exits(3),
    },
    Row {
        // An and-or list: the second half of it is the step still to come.
        case: "a wait before the second half of an and-or list",
        command: &["/bin/sh", "-c", "sleep 3600 && exit 5"],
        ends: Ends::Exits(5),
    },
    Row {
        case: "a wait the program ends on",
        command: &["/bin/sh", "-c", "echo up; sleep 30"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait the program states as its whole argument vector",
        command: &["sleep", "30"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait the program goes round a loop on",
        command: &["sh", "-c", "while true; do sleep 1; done"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait with no end of its own",
        command: &["sh", "-c", "echo up; sleep infinity"],
        ends: Ends::RunsOn,
    },
];

/// `RunTask` one task of `family` in [`CLUSTER`], and answer its ARN.
async fn run_task(router: &MultiServiceRouter, family: &str, case: &str) -> String {
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
        .unwrap_or_else(|| panic!("case {case:?}: expected a task back, got {body}"));
    string(task, "taskArn", case)
}

/// The one task `DescribeTasks` reports for `arn`.
async fn described(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    tasks
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task {arn} back, got {body}"))
}

/// Read one task again until its `lastStatus` is `status`, and answer it. Waits for the
/// CONDITION, never for a span.
async fn until_status(
    router: &MultiServiceRouter,
    arn: &str,
    status: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = described(router, arn, case).await;
        if string(&task, "lastStatus", case) == status {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {status} within the poll budget, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

/// RULE: a wait a program states between its own steps is stated, not spent; a wait it has
/// nothing left to do but sit in keeps the container up.
#[tokio::test]
async fn a_wait_a_program_states_between_its_steps_is_not_spent() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "waits a program states").await;

    // Every row's task is launched FIRST and read afterwards: the runs happen in parallel in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("statedwait{index}");
        ok(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": &family,
                "containerDefinitions": [{
                    "name": "app",
                    "image": IMAGE,
                    "memory": 128,
                    "essential": true,
                    "command": row.command,
                }],
            }),
            row.case,
        )
        .await;
        launched.push((row, run_task(&router, &family, row.case).await));
    }

    // The rows whose program goes ON: the task stops on the status that program exited with,
    // inside a budget far shorter than the seconds it said it waited.
    for (row, arn) in &launched {
        let Ends::Exits(status) = row.ends else {
            continue;
        };
        let case = row.case;
        let task = until_status(&router, arn, STOPPED, case).await;
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

    // The rows whose program has nothing left to do but wait: each task reaches `RUNNING` and
    // STAYS there. Watched in ONE loop over every such row, so the window is spent once
    // rather than once per row, and re-read INSIDE it, so what the assertion rests on is the
    // condition holding read after read.
    let waiting: Vec<(&Row, &String)> = launched
        .iter()
        .filter(|(row, _)| row.ends == Ends::RunsOn)
        .map(|(row, arn)| (*row, arn))
        .collect();
    for (row, arn) in &waiting {
        until_status(&router, arn, RUNNING, row.case).await;
    }
    let until = Instant::now() + WATCH_WINDOW;
    while Instant::now() < until {
        tokio::time::sleep(POLL_INTERVAL).await;
        for (row, arn) in &waiting {
            let case = row.case;
            let task = described(&router, arn, case).await;
            assert_eq!(
                string(&task, "lastStatus", case),
                RUNNING,
                "case {case:?}: a container with nothing left to do but wait stays up, so its \
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
