// SPDX-License-Identifier: CC0-1.0

//! A Fargate task that RESOLVES ROLES and whose essential container's program does not end:
//! it settles at `RUNNING` and stays there, with `startedAt` recorded, its interface attached
//! and no stop recorded against it.
//!
//! The rule under test, over the whole class rather than one launch:
//!
//! > `Task$lastStatus` is "the last known status for the task" and `Task$startedAt` is "the
//! > time when the task transitioned from the `PENDING` state to the `RUNNING` state", so a
//! > task whose essential container's program is still running is `RUNNING`, wanted `RUNNING`,
//! > and carries that instant. `Task$stoppedReason` is "the reason that the task was stopped"
//! > and `Task$stopCode` classifies it, so both describe a task that HAS stopped and a task
//! > that has not carries neither; `EssentialContainerExited`
//! > (`smithy_query(query="shape",shape="TaskStopCode")`) states that "the task stopped
//! > because an essential container exited", so it is recorded only for a container whose
//! > program really ended, and the `Container$exitCode` reported with it — "the exit code
//! > returned from the container" — is the status that program ended with. None of this
//! > depends on what the launch had to resolve on the way up: the roles a revision or a call
//! > names, and the permissions in force on them, decide whether the task can be launched at
//! > all, not how long the program inside it then runs.
//!
//! The rows differ along what the rule generalises over: WHICH roles the launch resolves (the
//! revision's execution role, that role beside a task role, the call's own execution-role
//! override), what IAM holds in force on the execution role (the task-execution managed
//! policy, or nothing but its trust document), WHICH form the program that does not end takes
//! (a wait as the whole argument vector, a wait with no end of its own, that wait spelled as an
//! entry point and its argument, a wait a shell program goes round a loop on), and the
//! BOUNDARY — a program that really ENDS, with a failing status and with a successful one,
//! which is the only thing that earns a stop record.
//!
//! Every role is created through IAM's OWN API, and every value asserted is read back over the
//! wire through `DescribeTasks`: nothing here reaches into either state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_absent, awsvpc_network_configuration, create_cluster, emulator_with_ecr_and_iam, iam,
    list, ok, poll_until, present_count, string, ACCOUNT,
};

/// The cluster every row's task is placed into.
const CLUSTER: &str = "execrolerun";

/// The one container name every revision below declares.
const CONTAINER: &str = "app";

/// The images these rows start from, each on a public registry, which serves any principal.
const AMAZONLINUX_IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";
const BUSYBOX_IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The service principal Amazon ECS assumes a task's roles as.
const ECS_TASKS: &str = "ecs-tasks.amazonaws.com";

/// The AWS managed policy that grants a task execution role the calls the agent makes on the
/// caller's behalf — what a client attaches to the role it passes as `executionRoleArn`.
const TASK_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// `Task$lastStatus`/`Container$lastStatus` members this suite names.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Attachment$status` of an interface that is in place — the status an `awsvpc` task's own
/// elastic network interface settles at while the task is up.
const ATTACHED: &str = "ATTACHED";

/// `Task$stopCode` of a task whose essential container exited, and the sentence Amazon ECS
/// words that stop with. Spelled out here rather than imported, so a wrong wording in the
/// implementation cannot make this suite pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";
const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// What a row's container does with the program it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// The program does not end: the task settles at `RUNNING` and is still there on every
    /// later read.
    RunsOn,
    /// The program ends with this status, which is the one thing that earns a stop record.
    Exits(i64),
}

/// Which roles one row's launch makes Amazon ECS resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Roles {
    /// `TaskDefinition$executionRoleArn` alone.
    Execution,
    /// That role beside `TaskDefinition$taskRoleArn`, the role the containers are given.
    ExecutionAndTask,
    /// `RunTask$overrides.executionRoleArn` — "the task execution role override for the task"
    /// — standing in place of the revision's.
    OverriddenExecution,
}

/// One row: the roles its launch resolves, what IAM holds in force on the execution role, the
/// program its single essential container is given, and what that program then does.
struct Row {
    case: &'static str,
    roles: Roles,
    /// Whether [`TASK_EXECUTION_POLICY`] is attached to the execution role.
    execution_policy: bool,
    image: &'static str,
    /// `ContainerDefinition$entryPoint`, which the runtime exec's before the command.
    entry_point: Option<&'static [&'static str]>,
    /// `ContainerDefinition$command`.
    command: &'static [&'static str],
    ends: Ends,
}

const ROWS: &[Row] = &[
    Row {
        // The reported class: the image and the wait a client launches a long-lived task with,
        // under an execution role carrying the managed policy for one.
        case: "a wait as the whole program under an execution role with the managed policy",
        roles: Roles::Execution,
        execution_policy: true,
        image: AMAZONLINUX_IMAGE,
        entry_point: None,
        command: &["sleep", "3600"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait with no end of its own, under an execution role and a task role",
        roles: Roles::ExecutionAndTask,
        execution_policy: true,
        image: AMAZONLINUX_IMAGE,
        entry_point: None,
        command: &["sleep", "infinity"],
        ends: Ends::RunsOn,
    },
    Row {
        // Nothing but the role's trust document is in force on it: what the role may do decides
        // whether the launch gets as far as the program, not how long that program runs.
        case: "a wait of ten minutes under an execution role with no policy attached",
        roles: Roles::Execution,
        execution_policy: false,
        image: BUSYBOX_IMAGE,
        entry_point: None,
        command: &["sleep", "600"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "the same wait spelled as an entry point and its argument, under an override",
        roles: Roles::OverriddenExecution,
        execution_policy: true,
        image: AMAZONLINUX_IMAGE,
        entry_point: Some(&["sleep"]),
        command: &["900"],
        ends: Ends::RunsOn,
    },
    Row {
        case: "a wait a shell program goes round a loop on, under an execution role",
        roles: Roles::Execution,
        execution_policy: true,
        image: BUSYBOX_IMAGE,
        entry_point: None,
        command: &["sh", "-c", "while true; do sleep 1; done"],
        ends: Ends::RunsOn,
    },
    Row {
        // The boundary: a program that really ends is the one case a stop record states.
        case: "a program that ends with a failing status under an execution role",
        roles: Roles::Execution,
        execution_policy: true,
        image: AMAZONLINUX_IMAGE,
        entry_point: None,
        command: &["sh", "-c", "exit 7"],
        ends: Ends::Exits(7),
    },
    Row {
        case: "a program that ends successfully under an execution role and a task role",
        roles: Roles::ExecutionAndTask,
        execution_policy: true,
        image: BUSYBOX_IMAGE,
        entry_point: None,
        command: &["sh", "-c", "echo done"],
        ends: Ends::Exits(0),
    },
];

/// A trust policy admitting [`ECS_TASKS`] the assume Amazon ECS reaches a task's roles by.
fn ecs_tasks_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the IAM role `name` trusting Amazon ECS — through IAM's own API — attaching
/// [`TASK_EXECUTION_POLICY`] when `policy` asks for it, and answer the role's ARN.
async fn role(router: &MultiServiceRouter, name: &str, policy: bool, case: &str) -> String {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", ecs_tasks_trust()),
        ],
        case,
    )
    .await;
    if policy {
        iam(
            router,
            "AttachRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyArn", TASK_EXECUTION_POLICY.to_string()),
            ],
            case,
        )
        .await;
    }
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Register one row's FARGATE/`awsvpc` revision and place one task of it, answering the task's
/// ARN. The roles the row asks for are created first, so what the launch resolves is a role
/// this account's IAM really holds.
async fn place(router: &MultiServiceRouter, index: usize, row: &Row) -> String {
    let case = row.case;
    let execution_role = role(
        router,
        &format!("execrole-{index}"),
        row.execution_policy,
        case,
    )
    .await;
    let family = format!("execrolefamily{index}");
    let mut container = json!({
        "name": CONTAINER,
        "image": row.image,
        "essential": true,
        "command": row.command,
    });
    if let Some(entry_point) = row.entry_point {
        container["entryPoint"] = json!(entry_point);
    }
    let mut definition = json!({
        "family": &family,
        "networkMode": "awsvpc",
        "requiresCompatibilities": ["FARGATE"],
        "cpu": "256",
        "memory": "512",
        "containerDefinitions": [container],
    });
    let mut request = json!({
        "cluster": CLUSTER,
        "taskDefinition": &family,
        "launchType": "FARGATE",
        "networkConfiguration": awsvpc_network_configuration(),
        "count": 1,
    });
    match row.roles {
        Roles::Execution => definition["executionRoleArn"] = json!(execution_role),
        Roles::ExecutionAndTask => {
            definition["executionRoleArn"] = json!(execution_role);
            let task_role = role(router, &format!("taskrole-{index}"), false, case).await;
            definition["taskRoleArn"] = json!(task_role);
        }
        Roles::OverriddenExecution => {
            // The override stands in place of the revision's execution role, so the revision
            // names one role and the call the role actually resolved.
            definition["executionRoleArn"] = json!(execution_role);
            let override_role = role(
                router,
                &format!("overriderole-{index}"),
                row.execution_policy,
                case,
            )
            .await;
            request["overrides"] = json!({ "executionRoleArn": override_role });
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
    assert!(
        list(&placed, "failures", case).is_empty(),
        "case {case:?}: expected the placement to report no failure, got {placed}"
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

/// The one container of a described task.
fn only_container(task: &Value, case: &str) -> Value {
    let containers = list(task, "containers", case);
    assert_eq!(
        containers.len(),
        1,
        "case {case:?}: expected the one container the revision declares, got {task}"
    );
    containers[0].clone()
}

/// Everything the rule says of a task whose program has not ended: it is up, it records when it
/// started, the interface it was allocated is in place, and nothing about a stop of it is
/// reported.
///
/// `when` names the phase the reading was taken in, so a failure says which read of the row
/// failed.
fn assert_up(task: &Value, row_case: &str, when: &str) {
    let case = format!("{row_case}, {when}");
    let case = case.as_str();
    assert_eq!(
        string(task, "lastStatus", case),
        RUNNING,
        "case {case:?}: expected the task to be {RUNNING}, got {task}"
    );
    assert_eq!(
        string(task, "desiredStatus", case),
        RUNNING,
        "case {case:?}: expected the task to be wanted {RUNNING}, got {task}"
    );
    assert!(
        task["startedAt"].as_f64().is_some(),
        "case {case:?}: expected the instant the task started at, got {task}"
    );
    for member in ["stopCode", "stoppedReason", "stoppedAt", "stoppingAt"] {
        assert_absent(task, member, case);
    }
    // The interface an `awsvpc` task is allocated is attached before it is launched, so a task
    // that is up holds it.
    let attachments = list(task, "attachments", case);
    assert_eq!(
        attachments.len(),
        1,
        "case {case:?}: expected the one interface an awsvpc task is allocated, got {task}"
    );
    assert_eq!(
        string(&attachments[0], "status", case),
        ATTACHED,
        "case {case:?}: expected the task's interface to be in place, got {task}"
    );
    let container = only_container(task, case);
    assert_eq!(
        string(&container, "lastStatus", case),
        RUNNING,
        "case {case:?}: expected the container to be {RUNNING}, got {container}"
    );
    assert_absent(&container, "exitCode", case);
}

/// RULE: a Fargate task whose launch resolves roles and whose essential container's program
/// does not end settles at RUNNING with `startedAt` and no stop record, and only a program that
/// really ended earns `EssentialContainerExited` with that program's own exit status.
#[tokio::test]
async fn a_task_whose_launch_resolves_roles_runs_while_its_program_does() {
    let router = emulator_with_ecr_and_iam();
    create_cluster(&router, CLUSTER, "the cluster every row's task is placed into").await;

    // Every row's task is placed up front, so each row's clock-owed start runs beside the
    // others rather than one row's after another's.
    let mut placed: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        placed.push(place(&router, index, row).await);
    }

    // The rows whose program does not end: each task settles at RUNNING.
    for (row, task_arn) in ROWS.iter().zip(&placed) {
        if row.ends != Ends::RunsOn {
            continue;
        }
        let case = row.case;
        let task = poll_until(
            case,
            &format!("the task to reach {RUNNING}"),
            || describe(&router, task_arn, case),
            |task| {
                // A task that STOPPED is settled too: the poll ends on it so the assertion
                // below reports the stop rather than timing out on it.
                task["lastStatus"] == json!(RUNNING) || task["lastStatus"] == json!(STOPPED)
            },
        )
        .await;
        assert_up(&task, case, "once its start has fallen due");
    }

    // The boundary rows: a program that really ended is what a stop record states, carrying the
    // status that program ended with.
    for (row, task_arn) in ROWS.iter().zip(&placed) {
        let Ends::Exits(status) = row.ends else {
            continue;
        };
        let case = row.case;
        let task = poll_until(
            case,
            "the task to stop on the status its program ended with",
            || describe(&router, task_arn, case),
            |task| task["lastStatus"] == json!(STOPPED),
        )
        .await;
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop its essential container's exit earns, got {task}"
        );
        assert_eq!(
            string(&task, "stoppedReason", case),
            ESSENTIAL_CONTAINER_EXITED_REASON,
            "case {case:?}: expected the sentence Amazon ECS words that stop with, got {task}"
        );
        let container = only_container(&task, case);
        assert_eq!(
            present_count(&container, "exitCode", case),
            Some(status),
            "case {case:?}: expected the status the container's own program ended with, got \
             {container}"
        );
    }

    // Read every running row again, now that the rows whose programs ended have stopped and
    // every clock pass those reads drove has run: a task held up by a program that does not end
    // is still up, and nothing those reads did took it down.
    for (row, task_arn) in ROWS.iter().zip(&placed) {
        if row.ends != Ends::RunsOn {
            continue;
        }
        let case = row.case;
        let task = describe(&router, task_arn, case).await;
        assert_up(&task, case, "on a later read");
    }
}
