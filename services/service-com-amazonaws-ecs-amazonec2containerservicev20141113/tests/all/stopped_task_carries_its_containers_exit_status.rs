// SPDX-License-Identifier: CC0-1.0

//! The record a STOPPED task is read as already carries its container's own EXIT STATUS.
//!
//! RULE under test: **a standalone task is stopped by the EXIT of an essential container of
//! it, so the record `DescribeTasks` serialises at the FIRST read that reports the task
//! `lastStatus` `STOPPED` already carries, for that container, the status its program ended
//! with (`Container$exitCode`, "the exit code returned from the container") beside the
//! container's own `STOPPED` — together with the task's `stopCode`
//! `EssentialContainerExited` and the reason Amazon ECS words for that code. `exitCode` is
//! absent only for a container that never RAN: a task whose launch could not fetch an image
//! is stopped with `TaskFailedToStart` and its containers produced no status at all.**
//!
//! Sources: `smithy_query(query="operation", operation="DescribeTasks")` for the projection
//! (`Task$lastStatus`/`$stopCode`/`$stoppedReason`, `Task$containers` ->
//! `Container$lastStatus`/`$exitCode`) and `smithy_query(query="shape",
//! shape="TaskStopCode")` for the two stop codes named above (`EssentialContainerExited`,
//! `TaskFailedToStart`).
//!
//! Why the FIRST read matters, and why this suite exists beside the ones that pin what a
//! container's program does (`all/container_runtime.rs`,
//! `all/container_program_conditionals.rs`) and what a failed fetch earns
//! (`all/image_pull_placement.rs`): those suites look again until the status they want has
//! arrived, so they would pass just as well if a client that polls `DescribeTasks` for a
//! terminal `lastStatus` — the shape every run-to-completion (batch/job) client takes — found
//! `STOPPED` and no `exitCode` on the read it stops polling at, and had to keep reading for
//! the verdict. The exit status is the only machine-readable signal such a client has, so it
//! is PART of the stopped record rather than something a later read settles. Each row here
//! therefore asserts on the one record the poll's own predicate held of.
//!
//! The rows differ along what the rule generalises over: how the program ends (it falls off
//! its end, it names a status with `exit`, a command fails under `set -e`), the status that
//! produces (zero, a small non-zero one, one past a byte, the largest a shell reports), where
//! the program's effect lands (its own output, a file on its own filesystem it reads back),
//! and — the one case the member is absent in — whether the container ran at all.
//!
//! CloudWatch Logs is mounted BESIDE Amazon ECS on the SAME engine, so what a row reads back
//! through `GetLogEvents` is what the container's `awslogs` driver really delivered: that
//! delivery is what says the container RAN, which is the premise the rule is about.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, list, ok, poll_until, present_count,
    seed_awsvpc_subnets, string, SIGNED_REGION,
};

/// The account every emulator ARN carries, which the execution role's ARN names.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The task execution role the revisions name: "the task execution role that grants the
/// Amazon ECS container agent permission to make Amazon Web Services API calls on your
/// behalf" (`TaskDefinition$executionRoleArn`), which a Fargate revision delivering through
/// `awslogs` has to carry. Created through IAM's OWN API below.
const EXECUTION_ROLE: &str = "ecsExitStatusExecutionRole";

/// The service principal a task's roles are assumed by, and the action its trust policy
/// admits.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The cluster every task below is placed in.
const CLUSTER: &str = "exitstatus";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image the containers start from: a release the one public gallery this emulator serves
/// holds, so every row but the last has its image fetched.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// An image reference whose repository that gallery holds but whose TAG names no release of
/// it, so nothing serves it and the task it is launched for never starts a container.
const UNRELEASED_IMAGE: &str = "public.ecr.aws/docker/library/busybox:no-such-release";

/// The log group every container below delivers to, created through CloudWatch Logs' own API
/// the way a client sets one up.
const LOG_GROUP: &str = "/emulator/ecs/exit-status";

/// The `awslogs-stream-prefix` the revisions configure, and so the first segment of every
/// stream name they deliver to.
const STREAM_PREFIX: &str = "exit";

/// The value the file-writing row's program is given in its environment and writes to a file:
/// what it reports back is that file's own size, so the row's expectation is derived from this
/// value rather than stated as a number.
const MARKER: &str = "payload-marker-0123456789";

/// The terminal status this suite reads, of a task and of its container.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` and `Task$stoppedReason` of a task stopped because an essential container
/// of it exited, spelled out here rather than imported from the service crate so a wrong
/// constant in the implementation cannot make this suite pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";
const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// What the container runtime words a fetch it could not make as, which the `stoppedReason` of
/// such a task carries.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// One row of the table.
struct Row {
    case: &'static str,
    /// The shell program the container's `command` runs.
    program: &'static str,
    /// The image the container starts from.
    image: &'static str,
    /// The lines the program is expected to deliver to its own log stream, in order. Empty
    /// for a program that writes nothing — and for the row whose container never ran.
    delivered: fn() -> Vec<String>,
    /// The status the container is expected to report, and `None` for a container that
    /// produced none because it never ran.
    exit_code: Option<i64>,
    /// The `stopCode` the task is expected to carry.
    stop_code: &'static str,
}

/// The size the file-writing row reports: the marker and the newline `echo` ends it with.
fn marker_file_size() -> usize {
    MARKER.len() + 1
}

const ROWS: &[Row] = &[
    Row {
        case: "a program that writes a line and falls off its end",
        program: "echo A-DONE",
        image: IMAGE,
        delivered: || vec!["A-DONE".to_string()],
        exit_code: Some(0),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        case: "the status an explicit exit names, after a write",
        program: "echo leaving; exit 7",
        image: IMAGE,
        delivered: || vec!["leaving".to_string()],
        exit_code: Some(7),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        // `exit` takes its operand modulo 256, so a status past one byte wraps.
        case: "a status past one byte, wrapped as a shell wraps it, with nothing written",
        program: "exit 300",
        image: IMAGE,
        delivered: Vec::new,
        exit_code: Some(44),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        case: "the largest status a shell reports",
        program: "echo last; exit 255",
        image: IMAGE,
        delivered: || vec!["last".to_string()],
        exit_code: Some(255),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        // "the shell shall immediately exit" when a command fails under `set -e`, so the
        // status is the failing command's and the line after it is never written.
        case: "a command that failed under set -e, which ends the program there",
        program: "set -e; echo before; false; echo never",
        image: IMAGE,
        delivered: || vec!["before".to_string()],
        exit_code: Some(1),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        // The shape a job's program takes: it writes a file, syncs it and reports what it
        // read back off its own filesystem, through a redirection and a command
        // substitution.
        case: "a program that writes a file and reports what it read back",
        program: "set -e; echo \"$MARKER\" > /tmp/payload; sync; \
                  echo \"SIZE=$(wc -c < /tmp/payload)\"; echo A-DONE",
        image: IMAGE,
        delivered: || {
            vec![
                format!("SIZE={}", marker_file_size()),
                "A-DONE".to_string(),
            ]
        },
        exit_code: Some(0),
        stop_code: ESSENTIAL_CONTAINER_EXITED,
    },
    Row {
        // The ONE case AWS leaves the member out in: nothing ran, so there is no status to
        // report, and the stop says so.
        case: "an image no registry serves, so no container of the task ever ran",
        program: "echo never",
        image: UNRELEASED_IMAGE,
        delivered: Vec::new,
        exit_code: None,
        stop_code: TASK_FAILED_TO_START,
    },
];

/// Amazon ECS, IAM and CloudWatch Logs over ONE shared engine — the three services a
/// container's output travels through, mounted the way the production router mounts them —
/// holding the one Amazon VPC subnet an `awsvpc` placement of this suite names in the
/// account's records.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// Create the task execution role the revisions below name, through IAM's OWN API, with a
/// trust policy that lets Amazon ECS assume it and the grant the `awslogs` driver's two calls
/// need, and answer its ARN.
async fn create_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    })
    .to_string();
    crate::ecs_wire::iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    let delivery = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    crate::ecs_wire::iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "log-delivery".to_string()),
            ("PolicyDocument", delivery),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// One CloudWatch Logs call, the way a client makes it: `POST /` with
/// `X-Amz-Target: Logs_20140328.<operation>` and a credential scoped to `logs`, which is what
/// the router picks the service from.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /logs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// The stream one task's container delivers to:
/// `<awslogs-stream-prefix>/<container name>/<task id>`.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// The messages one stream holds, oldest first, and an empty list for a stream nothing has
/// delivered to yet.
async fn delivered_lines(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (_, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Register one FARGATE revision whose single, essential container runs the row's program
/// through a shell and delivers through `awslogs`, and place one task of it. Answers the
/// task's ARN.
async fn place_task(
    router: &MultiServiceRouter,
    family: &str,
    execution_role: &str,
    row: &Row,
) -> String {
    let case = row.case;
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": row.image,
                "essential": true,
                "command": ["sh", "-c", row.program],
                "environment": [{ "name": "MARKER", "value": MARKER }],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": SIGNED_REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
        case,
    )
    .await;
    let placed = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "count": 1,
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
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

#[tokio::test]
async fn the_record_a_stopped_task_is_read_as_carries_its_containers_exit_status() {
    let router = emulator();
    create_cluster(&router, CLUSTER, "exit status").await;
    let execution_role = create_execution_role(&router, "exit status").await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateLogGroup must answer 200, got {status} with {created}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let family = format!("exitstatus-{index}");
        let task_arn = place_task(&router, &family, &execution_role, row).await;

        // The ONE record the rule is about: the first read that reports the task STOPPED.
        // Nothing is read again before the assertions below, so a member that only a later
        // read would carry cannot satisfy them.
        let stopped = poll_until(
            case,
            "the task to report lastStatus STOPPED",
            || describe(&router, &task_arn, case),
            |task| task["lastStatus"] == json!(STOPPED),
        )
        .await;

        assert_eq!(
            string(&stopped, "stopCode", case),
            row.stop_code,
            "case {case:?}: expected stopCode {:?}, got {stopped}",
            row.stop_code
        );
        let containers = list(&stopped, "containers", case);
        assert_eq!(
            containers.len(),
            1,
            "case {case:?}: expected the task's one container back, got {stopped}"
        );
        let container = &containers[0];
        assert_eq!(
            string(container, "lastStatus", case),
            STOPPED,
            "case {case:?}: expected the container to be {STOPPED} too, got {container}"
        );
        let reported = present_count(container, "exitCode", case);
        assert_eq!(
            reported, row.exit_code,
            "case {case:?}: expected exitCode {:?} on the record that first reported \
             {STOPPED}, got {reported:?} in {stopped}",
            row.exit_code
        );

        match row.exit_code {
            // A container that EXITED is what the stop states, in Amazon ECS's own words.
            Some(_) => assert_eq!(
                string(&stopped, "stoppedReason", case),
                ESSENTIAL_CONTAINER_EXITED_REASON,
                "case {case:?}: expected the reason Amazon ECS words for \
                 {ESSENTIAL_CONTAINER_EXITED}, got {stopped}"
            ),
            // A container that never ran: the stop reports the fetch nothing served, and the
            // task never started at all.
            None => {
                let reason = string(&stopped, "stoppedReason", case);
                assert!(
                    reason.contains(CANNOT_PULL) && reason.contains(row.image),
                    "case {case:?}: expected the stoppedReason to report {CANNOT_PULL} for \
                     {}, got {reason:?}",
                    row.image
                );
                assert!(
                    stopped["startedAt"].is_null(),
                    "case {case:?}: expected no startedAt on a task that never ran, got \
                     {stopped}"
                );
            }
        }

        // What the container WROTE, read back through CloudWatch Logs' own API: the delivery
        // is what says the program really ran, which is the premise of the rule above. A row
        // whose program writes nothing has no stream to read.
        let expected = (row.delivered)();
        if expected.is_empty() {
            continue;
        }
        let stream = stream_of(&task_arn);
        let read = poll_until(
            case,
            &format!("{} line(s) delivered to {stream}", expected.len()),
            || async { json!(delivered_lines(&router, &stream).await) },
            |read| {
                read.as_array()
                    .is_some_and(|lines| lines.len() >= expected.len())
            },
        )
        .await;
        let lines: Vec<String> = read
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|line| line.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            lines, expected,
            "case {case:?}: expected {expected:?} in {stream}, got {lines:?}"
        );
    }
}
