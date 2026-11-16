// SPDX-License-Identifier: CC0-1.0

//! What a container definition's `secrets` reference RESOLVES TO, driven end to end over the
//! wire with AWS Secrets Manager and CloudWatch Logs mounted beside Amazon ECS on ONE shared
//! engine — so the secret every reference names is created through the owning service's own
//! API and every value asserted is read back through `DescribeTasks` and CloudWatch Logs'
//! own API.
//!
//! RULE: the agent retrieves every `secrets` `valueFrom` of a task's containers BEFORE any
//! container of the task exists, and what a reference resolves to is the value Secrets
//! Manager holds for it: the whole string value for a reference naming the secret alone, and
//! — "you can also specify a specific JSON key within the secret" — the value of that one key
//! for a reference whose selector names one. So:
//!
//! * a reference that resolves runs: the value is exposed to the container's process as the
//!   environment variable the entry's `name` spells, the program writes it to the `awslogs`
//!   stream its log configuration names, and the container reports the status it exited with;
//! * a reference that resolves to NOTHING launches nothing — a selector naming a key the
//!   retrieved document does not state, or naming one in a value that is no document at all,
//!   leaves the agent with no resource to give the container. The task never runs: it reaches
//!   `STOPPED` with `Task$stopCode` `TaskFailedToStart` and a `stoppedReason` of the agent's
//!   `ResourceInitializationError` class naming the secret, it carries no `startedAt`, its
//!   container reports no `exitCode` — nothing ran to produce one — and nothing is delivered
//!   to the log group its configuration names.
//!
//! The rows differ along what the rule generalises over: the FORM of the reference (the
//! secret's ARN alone, a selector with the version positions left empty, a selector with the
//! staging label written out), the TYPE of the selected JSON value (a string, a number), and
//! — for the refused table — WHY nothing resolves (a key the document does not state, a key
//! selected out of a value that is no document, a secret this account no longer holds).
//!
//! The `stoppedReason` class is spelled out here rather than imported, so a wrong wording in
//! the implementation cannot make a test pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, list, ok, present_count, seed_awsvpc_subnets,
    string, ACCOUNT, SIGNED_REGION,
};

/// The log group every container below delivers to, created through CloudWatch Logs' own API
/// before any task is launched.
const LOG_GROUP: &str = "/emulator/ecs/secret-values";

/// The `awslogs-stream-prefix` the task definitions below configure, and so the first segment
/// of every stream name they deliver to.
const STREAM_PREFIX: &str = "ecs";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Task$stopCode` of a task whose essential container ran and exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The fixed text of the `stoppedReason` the agent words for an execution resource it could
/// not retrieve from Secrets Manager, spelled out here rather than imported.
const RESOURCE_INITIALIZATION_ERROR: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve secret from asm: service call has been retried 1 \
     time(s): ";

/// How often a poll re-reads the emulator.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up: comfortably past every clock-owed
/// transition a task takes to come up, run its work and stop, with generous margin for a
/// loaded test machine.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// A router carrying Amazon ECS, AWS Secrets Manager — which holds the secrets the references
/// name — and CloudWatch Logs, which the containers' output is delivered to, over ONE shared
/// engine.
///
/// No IAM store is mounted: WHICH role may retrieve a secret is the subject of
/// `container_secret_injection`, and an account whose IAM holds no record for the role a
/// launch names shows nothing about that role, so every row here turns on what its reference
/// RESOLVES TO and on nothing else ([`execution_role_arn`]).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // A Fargate task is allocated its own elastic network interface in a subnet of the
    // account, so the placement below resolves against Amazon VPC's records.
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call to another service of the router, the way a client makes it: the
/// target prefix and the credential scope are what pick the service.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /{signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
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

/// One CloudWatch Logs call.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(router, "logs", &format!("Logs_20140328.{operation}"), body).await
}

/// One Secrets Manager call that must succeed, returning its body.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) =
        json_call(router, "secretsmanager", &format!("secretsmanager.{operation}"), body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Secrets Manager {operation} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// Create the log group every container of this suite delivers to.
async fn create_log_group(router: &MultiServiceRouter, case: &str) {
    let (status, body) = logs(router, "CreateLogGroup", json!({ "logGroupName": LOG_GROUP })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLogGroup to answer 200, got {status} with {body}"
    );
}

/// Create the secret `name` holding `value` through Secrets Manager's OWN API, and answer the
/// ARN it allocated — the ARN a `valueFrom` names it by.
async fn create_secret(router: &MultiServiceRouter, name: &str, value: &str, case: &str) -> String {
    // A write that stores a value creates a version the caller names, and the modelled
    // `ClientRequestToken` is at least 32 characters long.
    let mut token = format!("{name}-version-");
    while token.len() < 32 {
        token.push('0');
    }
    let created = secrets(
        router,
        "CreateSecret",
        json!({ "Name": name, "SecretString": value, "ClientRequestToken": token }),
        case,
    )
    .await;
    string(&created, "ARN", case)
}

/// Delete the secret `name` holds, so the account no longer holds a value to retrieve.
async fn delete_secret(router: &MultiServiceRouter, name: &str, case: &str) {
    secrets(
        router,
        "DeleteSecret",
        json!({ "SecretId": name, "ForceDeleteWithoutRecovery": true }),
        case,
    )
    .await;
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming the driver's own
/// `awslogs-stream-prefix` documentation states. Spelled out here rather than imported.
fn stream_of(task_arn: &str) -> String {
    format!(
        "{STREAM_PREFIX}/{CONTAINER}/{}",
        task_arn.rsplit('/').next().unwrap_or_default()
    )
}

/// The messages CloudWatch Logs holds in the stream one task's container delivers to, oldest
/// first. An unknown stream reads as no messages, which is what a stream nothing was
/// delivered to holds.
async fn delivered(router: &MultiServiceRouter, task_arn: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream_of(task_arn),
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
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

/// The streams the log group holds for one task's container — none for a task whose container
/// never started.
async fn streams_of(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": LOG_GROUP, "logStreamNamePrefix": stream_of(task_arn) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeLogStreams to answer 200, got {status} with {body}"
    );
    body["logStreams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The task execution role every revision below declares. A Fargate revision whose container
/// configures the `awslogs` driver has to name one ("Fargate requires task definition to have
/// execution role ARN to support log driver awslogs"), and this account's IAM holds no record
/// for THIS one — so nothing is shown about the role and no gate over a role's documents
/// decides anything for these launches, which is what leaves each row turning on what its
/// reference resolves to.
fn execution_role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/secret-values-exec")
}

/// Register one revision of `family` whose single container takes `value_from` as the
/// environment variable `INJECTED` and writes it to its `awslogs` stream.
async fn register(router: &MultiServiceRouter, family: &str, value_from: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            // A documented Fargate task size: 256 CPU units, 512 MiB.
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role_arn(),
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "command": ["sh", "-c", "echo \"injected=$INJECTED\""],
                "secrets": [{ "name": "INJECTED", "valueFrom": value_from }],
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
}

/// Launch one standalone Fargate task of `family` in `cluster` and answer its ARN.
async fn run_task(router: &MultiServiceRouter, cluster: &str, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": cluster,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
}

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
async fn stopped_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [arn] }),
            case,
        )
        .await;
        let task = list(&body, "tasks", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"));
        if string(&task, "lastStatus", case) == STOPPED {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {STOPPED} within the poll budget, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one container of a task's record.
fn container_of(task: &Value, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"))
}

/// One row of the resolving table: a reference, and the value it resolves to.
struct Resolved {
    case: &'static str,
    /// The value the secret holds, as `CreateSecret` stored it.
    secret_value: &'static str,
    /// What is written after the secret's ARN in the `valueFrom`: the JSON key and the
    /// version positions, empty for a reference to the whole value.
    selector: &'static str,
    /// The value the container's process is given.
    injected: &'static str,
}

const RESOLVING: &[Resolved] = &[
    Resolved {
        case: "the secret's ARN alone, which is the whole value",
        secret_value: "{\"USERNAME\":\"admin\",\"PASSWORD\":\"only-this\"}",
        selector: "",
        injected: "{\"USERNAME\":\"admin\",\"PASSWORD\":\"only-this\"}",
    },
    Resolved {
        case: "a JSON key with the version positions left empty",
        secret_value: "{\"USERNAME\":\"admin\",\"PASSWORD\":\"only-this\"}",
        selector: ":PASSWORD::",
        injected: "only-this",
    },
    Resolved {
        case: "a JSON key with no version positions written at all",
        secret_value: "{\"USERNAME\":\"admin\",\"PASSWORD\":\"only-this\"}",
        selector: ":USERNAME",
        injected: "admin",
    },
    Resolved {
        case: "a JSON key beside the staging label of the current version",
        secret_value: "{\"PASSWORD\":\"staged\"}",
        selector: ":PASSWORD:AWSCURRENT:",
        injected: "staged",
    },
    Resolved {
        case: "a JSON key whose value is a number, as the document writes it",
        secret_value: "{\"PORT\":5432}",
        selector: ":PORT::",
        injected: "5432",
    },
    Resolved {
        case: "a value that is no document at all, taken whole",
        secret_value: "plain-value",
        selector: "",
        injected: "plain-value",
    },
];

/// RULE, the resolving half: a `secrets` reference is exposed to the container's process
/// under the entry's own name, with the value Secrets Manager holds for it — the whole value,
/// or the one JSON key the selector names.
#[tokio::test]
async fn a_reference_that_resolves_reaches_the_container() {
    let router = emulator();
    let cluster = "secretvalues";
    create_cluster(&router, cluster, "resolving references").await;
    create_log_group(&router, "resolving references").await;

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel
    // in the one emulator, so the suite waits for the longest of them rather than for their
    // sum.
    let mut launched: Vec<(&Resolved, String)> = Vec::new();
    for (index, row) in RESOLVING.iter().enumerate() {
        let case = row.case;
        let secret_arn =
            create_secret(&router, &format!("resolving-{index}"), row.secret_value, case).await;
        let family = format!("resolving{index}");
        register(&router, &family, &format!("{secret_arn}{}", row.selector), case).await;
        launched.push((row, run_task(&router, cluster, &family, case).await));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, task_arn, case).await;
        let exit_code = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            exit_code,
            Some(0),
            "case {case:?}: expected the container to exit 0, got {exit_code:?} in {task}"
        );
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop of a task that RAN, got {task}"
        );
        let events = delivered(&router, task_arn).await;
        assert_eq!(
            events,
            vec![format!("injected={}", row.injected)],
            "case {case:?}: expected the container to write injected={} to {}, got {events:?}",
            row.injected,
            stream_of(task_arn)
        );
    }
}

/// One row of the refused table: a reference that resolves to nothing, and what the agent's
/// account of it names.
struct Refused {
    case: &'static str,
    /// The value the secret holds.
    secret_value: &'static str,
    /// What is written after the secret's ARN in the `valueFrom`.
    selector: &'static str,
    /// Whether the account still holds the secret when the task is launched.
    held: bool,
    /// The clause of the agent's account that says what could not be produced.
    clause: &'static str,
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "a JSON key the document does not state",
        secret_value: "{\"USERNAME\":\"admin\"}",
        selector: ":PASSWORD::",
        held: true,
        clause: "json key PASSWORD not found in secret",
    },
    Refused {
        case: "a JSON key selected out of a value that is no document",
        secret_value: "plain-value",
        selector: ":PASSWORD::",
        held: true,
        clause: "Invalid JSON format for secret",
    },
    Refused {
        case: "a JSON key whose document the account no longer holds",
        secret_value: "{\"PASSWORD\":\"gone\"}",
        selector: ":PASSWORD::",
        held: false,
        clause: "ResourceNotFoundException",
    },
];

/// RULE, the other half: a reference that resolves to nothing launches nothing — the task is
/// stopped at the instant it would otherwise have started, with the stop of a task that never
/// ran, and the log group its configuration names holds no stream for it.
#[tokio::test]
async fn a_reference_that_resolves_to_nothing_fails_the_task_at_start() {
    let router = emulator();
    let cluster = "unresolved";
    create_cluster(&router, cluster, "unresolved references").await;
    create_log_group(&router, "unresolved references").await;

    let mut launched: Vec<(&Refused, String)> = Vec::new();
    for (index, row) in REFUSED.iter().enumerate() {
        let case = row.case;
        let name = format!("unresolved-{index}");
        let secret_arn = create_secret(&router, &name, row.secret_value, case).await;
        if !row.held {
            delete_secret(&router, &name, case).await;
        }
        let family = format!("unresolved{index}");
        register(&router, &family, &format!("{secret_arn}{}", row.selector), case).await;
        launched.push((row, run_task(&router, cluster, &family, case).await));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, task_arn, case).await;
        assert_eq!(
            task["stopCode"],
            json!(TASK_FAILED_TO_START),
            "case {case:?}: expected stopCode {TASK_FAILED_TO_START}, got {task}"
        );
        let reason = string(&task, "stoppedReason", case);
        assert!(
            reason.starts_with(RESOURCE_INITIALIZATION_ERROR),
            "case {case:?}: expected a reason beginning {RESOURCE_INITIALIZATION_ERROR:?}, got \
             {reason:?}"
        );
        assert!(
            reason.contains(row.clause),
            "case {case:?}: expected the reason to state {:?}, got {reason:?}",
            row.clause
        );
        // A task that never started carries no `startedAt` and no container status: nothing
        // ran, so there is nothing for either to report.
        assert_eq!(
            present_count(&task, "startedAt", case),
            None,
            "case {case:?}: a task that never ran carries no startedAt, got {task}"
        );
        let exit_code = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            exit_code, None,
            "case {case:?}: a container that never ran reports no exitCode, got {exit_code:?} \
             in {task}"
        );
        let streams = streams_of(&router, task_arn, case).await;
        assert!(
            streams.is_empty(),
            "case {case:?}: nothing is delivered for a container that never started, the group \
             held {streams:?}"
        );
    }
}
