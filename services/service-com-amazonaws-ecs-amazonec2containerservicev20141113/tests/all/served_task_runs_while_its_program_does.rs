// SPDX-License-Identifier: CC0-1.0

//! A task whose essential container's program does NOT end: it reaches `RUNNING` and stays
//! there, with no stop recorded against it.
//!
//! The rule under test, over the whole class rather than one request:
//!
//! > `Task$lastStatus` is "the last known status for the task", and a task is RUNNING exactly
//! > while its essential container's program is still running. `Task$stopCode` and
//! > `Task$stoppedReason` ("the reason that the task was stopped") describe a task that HAS
//! > stopped, so a task whose program has not ended carries neither — and `Container$exitCode`,
//! > "the exit code returned from the container", is the status a container's program ENDED
//! > with, so a container still running has none. A program that starts a server in the
//! > foreground, or that waits round a loop, does not end: however it is spelled — the command
//! > the image itself declares, a server named under a shell, a server named as the container's
//! > own argument vector — the task it holds goes PENDING -> RUNNING and is still RUNNING on
//! > every later read, because nothing has stopped it.
//!
//! The rows differ along what the rule generalises over: WHICH form the never-ending program
//! takes (the image's own declared command, the same server written out under a shell, a
//! server invoked as the container's own argument vector on a port of its own, a wait the
//! program goes round a loop on), which server it is, and the launch type the task is placed
//! under.
//!
//! What is this suite's own: the SERVER forms, which no sibling suite covers — a container
//! whose program is the command its image declares, and one whose program is a server named
//! under a shell or as the container's own argument vector. The last row is the launch-type
//! dimension, on the one program form a sibling already pins
//! (`crate::container_whole_program_wait`): the rule is about the program, not about the
//! capacity the task was placed on, so the same program is asserted on both.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTasks`, and every wait is a poll for the CONDITION rather than for a span.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_absent, awsvpc_network_configuration, cluster_with_capacity, list, ok, poll_until,
    service, string,
};

/// The cluster every row's task runs in.
const CLUSTER: &str = "servedtasks";

/// The official image of the server whose own declared command runs it in the foreground.
const NGINX_IMAGE: &str = "public.ecr.aws/nginx/nginx:1.27";

/// The small image whose multi-call binary ships the other server these rows start, and the
/// shell the rows that need one run their program under.
const BUSYBOX_IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The one container name every row's revision declares.
const CONTAINER: &str = "app";

/// How a task that is up reports itself, which is both the status it reaches and the status it
/// is still in on a later read.
const RUNNING: &str = "RUNNING";

/// Which capacity a row's task is placed on — the two launch types a standalone task of these
/// suites takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// `awsvpc` networking on Fargate, which is the class the rule was reported against.
    Fargate,
    /// Capacity registered to the cluster, with the default networking of a revision that
    /// names no mode.
    Ec2,
}

/// One row: the family it registers, the image and command its single essential container
/// declares, and the capacity its task is placed on.
struct Row {
    case: &'static str,
    family: &'static str,
    image: &'static str,
    /// The `command` the container declares, or `None` for a container that runs the command
    /// its IMAGE declares.
    command: Option<&'static [&'static str]>,
    launch: Launch,
}

/// The rows, each its own task-definition family and its own standalone task.
const ROWS: &[Row] = &[
    Row {
        case: "the command the image itself declares",
        family: "servedimagecommand",
        image: NGINX_IMAGE,
        command: None,
        launch: Launch::Fargate,
    },
    Row {
        case: "that same server written out under a shell",
        family: "servedshellcommand",
        image: NGINX_IMAGE,
        command: Some(&["sh", "-c", "nginx -g 'daemon off;'"]),
        launch: Launch::Fargate,
    },
    Row {
        case: "the other server held in the foreground under a shell",
        family: "servedsmallshell",
        image: BUSYBOX_IMAGE,
        command: Some(&["sh", "-c", "httpd -f -p 80 -h /www"]),
        launch: Launch::Fargate,
    },
    Row {
        case: "that server as the container's own argument vector, on a port of its own",
        family: "servedsmallargv",
        image: BUSYBOX_IMAGE,
        command: Some(&["httpd", "-f", "-p", "8080", "-h", "/www"]),
        launch: Launch::Fargate,
    },
    Row {
        case: "the same program a sibling suite pins, placed on capacity registered to the \
               cluster rather than on Fargate",
        family: "servedloop",
        image: BUSYBOX_IMAGE,
        command: Some(&["sh", "-c", "while true; do sleep 1; done"]),
        launch: Launch::Ec2,
    },
];

/// The single container definition one row declares.
fn container(row: &Row) -> Value {
    let mut container = json!({
        "name": CONTAINER,
        "image": row.image,
        // "at least one of 'memory' or 'memoryReservation' must be specified" for a container
        // of a revision this service places.
        "memory": 128,
        "essential": true,
    });
    if let Some(command) = row.command {
        container["command"] = json!(command);
    }
    container
}

/// Register one row's revision and place one standalone task of it, answering the task's ARN.
async fn place(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let mut definition = json!({
        "family": row.family,
        "containerDefinitions": [container(row)],
    });
    let mut request = json!({
        "cluster": CLUSTER,
        "taskDefinition": row.family,
        "count": 1,
    });
    match row.launch {
        Launch::Fargate => {
            definition["networkMode"] = json!("awsvpc");
            definition["requiresCompatibilities"] = json!(["FARGATE"]);
            definition["cpu"] = json!("256");
            definition["memory"] = json!("512");
            request["launchType"] = json!("FARGATE");
            request["networkConfiguration"] = awsvpc_network_configuration();
        }
        Launch::Ec2 => {
            definition["requiresCompatibilities"] = json!(["EC2"]);
            request["launchType"] = json!("EC2");
        }
    }
    ok(router, "RegisterTaskDefinition", definition, case).await;
    let placed = ok(router, "RunTask", request, case).await;
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

/// Everything the rule says of a task that is up: it is RUNNING and wanted RUNNING, nothing is
/// recorded about a stop of it, and its container reports no exit status because its program
/// has not ended.
///
/// `when` names the phase the reading was taken in, carried into the case so a failure says
/// which of the two reads of the row failed.
fn assert_up(task: &Value, row_case: &str, when: &str) {
    let case = format!("{row_case}, {when}");
    let case = case.as_str();
    assert_eq!(
        task["lastStatus"], json!(RUNNING),
        "case {case:?}: expected the task to be {RUNNING}, got {task}"
    );
    assert_eq!(
        task["desiredStatus"], json!(RUNNING),
        "case {case:?}: expected the task to be wanted {RUNNING}, got {task}"
    );
    for member in ["stopCode", "stoppedReason", "stoppedAt", "stoppingAt"] {
        assert_absent(task, member, case);
    }
    let containers = list(task, "containers", case);
    assert_eq!(
        containers.len(),
        1,
        "case {case:?}: expected the one container the revision declares, got {task}"
    );
    for reported in &containers {
        assert_eq!(
            reported["lastStatus"], json!(RUNNING),
            "case {case:?}: expected the container to be {RUNNING}, got {reported}"
        );
        assert_absent(reported, "exitCode", case);
    }
}

/// RULE: a task whose essential container's program does not end reaches RUNNING with no stop
/// recorded against it, and is still RUNNING on every later read.
#[tokio::test]
async fn a_task_whose_program_does_not_end_reaches_running_and_stays_there() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's task runs in").await;

    // Every row's task is placed up front, so each row's own clock-owed start runs beside the
    // others rather than one row's after another's.
    let mut placed: Vec<String> = Vec::new();
    for row in ROWS {
        placed.push(place(&router, row).await);
    }

    for (row, task_arn) in ROWS.iter().zip(&placed) {
        let case = row.case;
        let task = poll_until(
            case,
            &format!("the task to reach {RUNNING}"),
            || describe(&router, task_arn, case),
            |task| {
                // A task that STOPPED is settled too: the poll ends on it so the assertion
                // below reports the stop rather than timing out on it.
                task["lastStatus"] == json!(RUNNING) || task["lastStatus"] == json!("STOPPED")
            },
        )
        .await;
        assert_up(&task, case, "once its start has fallen due");
    }

    // Read every task again, now that every other row's waits have run: a task held up by a
    // program that does not end is still up, and the reads above took nothing from it.
    for (row, task_arn) in ROWS.iter().zip(&placed) {
        let case = row.case;
        let task = describe(&router, task_arn, case).await;
        assert_up(&task, case, "on a later read");
    }
}
