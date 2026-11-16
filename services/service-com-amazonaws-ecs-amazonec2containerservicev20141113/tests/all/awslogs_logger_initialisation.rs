// SPDX-License-Identifier: CC0-1.0

//! Whether a task whose containers deliver through the `awslogs` log driver can be LAUNCHED,
//! and the stop one whose logger cannot be initialised earns.
//!
//! The rule under test, over the whole class rather than one configuration:
//!
//! > The `awslogs` driver is initialised before the container it is for runs — it creates the
//! > log stream that container's output will go to — and Amazon ECS makes that call as the
//! > task's EXECUTION role ("the task execution role that grants the Amazon ECS container
//! > agent permission to make Amazon Web Services API calls on your behalf",
//! > `TaskDefinition$executionRoleArn`). So a launch whose logger cannot be initialised
//! > launches nothing: the task stops with `Task$stopCode` `TaskFailedToStart` ("the stop
//! > code indicating why a task was stopped") and a `Task$stoppedReason` of the agent's
//! > `ResourceInitializationError` class carrying the answer CloudWatch Logs gave — its
//! > authorization refusal, or its own sentence for a log group this account does not hold —
//! > no container of it reports an exit code, because none ran, and no stream is created. A
//! > launch whose logger CAN be initialised runs and delivers.
//!
//! The rows span what the rule generalises over: WHY the stream creation is refused (the
//! group is not there; no policy in force on the execution role grants the action; a policy
//! explicitly denies it), and the configurations the rule does NOT decide against (a group
//! that is there with the grant in place, a driver asked to create the group itself, and a
//! revision naming no execution role at all, which states no principal for a policy to be
//! read against).
//!
//! Every fact is created through the owning service's own API — the log group through
//! CloudWatch Logs' `CreateLogGroup`, the execution role and its policies through IAM's
//! `CreateRole`/`PutRolePolicy` — and every value asserted is read back through
//! `DescribeTasks` and `DescribeLogStreams`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, iam, list, ok, present_count, string, ACCOUNT, SIGNED_REGION,
};

/// The cluster every task below runs in.
const CLUSTER: &str = "loggerinit";

/// The image every container names. Nothing is fetched from it.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The `awslogs-stream-prefix` every configuration below names, and so the first segment of
/// the stream each container delivers to.
const STREAM_PREFIX: &str = "ecs";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The log group that EXISTS in these suites, created through CloudWatch Logs' own API.
const HELD_GROUP: &str = "/emulator/ecs/logger-init";

/// A log group name no `CreateLogGroup` here was made for — the name a configuration that
/// does not match the group a client created carries.
const UNHELD_GROUP: &str = "/emulator/ecs/not-created";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The two actions the `awslogs` driver performs under the execution role: it creates the
/// stream it delivers to and writes the container's lines into it (IAM Service Authorization
/// Reference, "Actions defined by Amazon CloudWatch Logs").
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// How the agent's account of a logger it could not validate OPENS, spelled out here rather
/// than imported so a wrong constant in the implementation cannot make a test pass.
const LOGGER_FAILURE_PREFIX: &str =
    "ResourceInitializationError: failed to validate logger args: create stream has been \
     retried 1 times: failed to create Cloudwatch log stream: ";

/// The error class CloudWatch Logs answers a request for a log group it does not hold with,
/// and the sentence it answers it with (`ResourceNotFoundException`, "The specified resource
/// does not exist.", worded for the resource kind).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException: The specified log group does not exist.";

/// The class an authorization refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException: ";

/// How often a poll re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: a task that fails to launch is
/// stopped at the instant it would otherwise have started, one that runs is stopped when the
/// work it was placed to do is over, and this suite runs beside every other test of the
/// workspace.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// What a row gives its execution role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No execution role at all: the revision names none.
    NoRole,
    /// A role whose policies allow the driver's actions.
    Allowed,
    /// A role carrying a policy about another service entirely, so nothing in force on it
    /// allows the driver's actions.
    Unrelated,
    /// A role whose policy explicitly denies the stream creation.
    Denied,
}

/// One row: the configuration a container delivers through, what its execution role may do,
/// and the launch that produces.
struct Row {
    case: &'static str,
    /// The log group the `awslogs-group` option names.
    group: &'static str,
    /// Whether the driver is asked to create that group (`awslogs-create-group`).
    create_group: bool,
    /// What the row's execution role is given.
    grant: Grant,
    /// The failure the task's `stoppedReason` must carry after the fixed opening
    /// ([`LOGGER_FAILURE_PREFIX`]), and `None` for a row whose task launches.
    failure: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the group is there and the role may create the stream",
        group: HELD_GROUP,
        create_group: false,
        grant: Grant::Allowed,
        failure: None,
    },
    Row {
        case: "the configuration names a group nobody created",
        group: UNHELD_GROUP,
        create_group: false,
        grant: Grant::Allowed,
        failure: Some(RESOURCE_NOT_FOUND),
    },
    Row {
        case: "no policy in force on the execution role allows the stream creation",
        group: HELD_GROUP,
        create_group: false,
        grant: Grant::Unrelated,
        failure: Some(ACCESS_DENIED),
    },
    Row {
        case: "a policy on the execution role denies the stream creation",
        group: HELD_GROUP,
        create_group: false,
        grant: Grant::Denied,
        failure: Some(ACCESS_DENIED),
    },
    Row {
        // "awslogs-create-group": the driver creates the group as well as the stream, so a
        // group that is not there yet is not a logger that cannot be initialised.
        case: "the driver is asked to create the group it delivers to",
        group: UNHELD_GROUP,
        create_group: true,
        grant: Grant::Allowed,
        failure: None,
    },
    Row {
        // A revision naming no execution role states no principal for a policy to be read
        // against, so nothing is decided about it and the launch goes ahead.
        case: "the revision names no execution role at all",
        group: HELD_GROUP,
        create_group: false,
        grant: Grant::NoRole,
        failure: None,
    },
];

/// A router carrying Amazon ECS, IAM — which holds the execution role the launch is decided
/// against — and CloudWatch Logs, which owns the log group and the stream, over ONE shared
/// engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
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

/// One CloudWatch Logs call, the way a client makes it.
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
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create the role `name` with a trust policy that lets Amazon ECS assume it and the inline
/// policy `document`, when the row gives one — each step through IAM's OWN API — and answer
/// its ARN.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    document: Option<String>,
    case: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [ECS_TASKS_PRINCIPAL] },
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    if let Some(document) = document {
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "logger".to_string()),
                ("PolicyDocument", document),
            ],
            case,
        )
        .await;
    }
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The execution role one row names, created for it, and `None` for a row that names none.
async fn execution_role(router: &MultiServiceRouter, index: usize, row: &Row) -> Option<String> {
    let name = format!("loggerRole{index}");
    let document = match row.grant {
        Grant::NoRole => return None,
        Grant::Allowed => Some(policy("Allow", &[CREATE_LOG_STREAM, PUT_LOG_EVENTS])),
        // A grant about another service entirely: the role carries a document, and nothing
        // in it allows the driver's actions.
        Grant::Unrelated => Some(policy("Allow", &["s3:GetObject"])),
        Grant::Denied => Some(policy("Deny", &[CREATE_LOG_STREAM])),
    };
    Some(create_role(router, &name, document, row.case).await)
}

/// An identity-based policy with one statement: `effect` on `actions`, for every resource.
fn policy(effect: &str, actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Register one revision of `family` delivering through `row`'s configuration under
/// `role_arn`, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    row: &Row,
    role_arn: Option<&str>,
) -> String {
    let mut options = json!({
        "awslogs-group": row.group,
        "awslogs-region": SIGNED_REGION,
        "awslogs-stream-prefix": STREAM_PREFIX,
    });
    if row.create_group {
        options["awslogs-create-group"] = json!("true");
    }
    let mut request = json!({
        "family": family,
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", "echo ready"],
            "logConfiguration": { "logDriver": "awslogs", "options": options },
        }],
    });
    if let Some(role_arn) = role_arn {
        request["executionRoleArn"] = json!(role_arn);
    }
    let body = ok(router, "RegisterTaskDefinition", request, row.case).await;
    string(&body["taskDefinition"], "taskDefinitionArn", row.case)
}

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
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

/// The `stopCode` one task reports, or an empty string while it reports none.
fn stop_code(task: &Value) -> &str {
    task["stopCode"].as_str().unwrap_or_default()
}

/// The `stoppedReason` one task reports, or an empty string while it reports none.
fn stopped_reason(task: &Value) -> &str {
    task["stoppedReason"].as_str().unwrap_or_default()
}

/// Read one task again until it reports a `stopCode`, and answer it.
async fn stopped_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, arn, case).await;
        if !stop_code(&task).is_empty() {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to stop with a stop code, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The names of every stream of `group`, and an empty list for a group this account does not
/// hold at all.
async fn streams(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": group }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
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

/// RULE: a task whose `awslogs` logger cannot be initialised fails to start, saying why in
/// the agent's own words; one whose logger can be initialised runs.
#[tokio::test]
async fn a_task_whose_awslogs_logger_cannot_be_initialised_fails_to_start() {
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, "logger init").await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": HELD_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group every row delivers into must be created, got {status} with {created}"
    );

    // Every row's task is launched FIRST and polled afterwards: the launches are decided in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("loggerinit{index}");
        let role_arn = execution_role(&router, index, row).await;
        let definition = register(&router, &family, row, role_arn.as_deref()).await;
        let body = ok(
            &router,
            "RunTask",
            json!({ "cluster": CLUSTER, "taskDefinition": definition }),
            row.case,
        )
        .await;
        let tasks = list(&body, "tasks", row.case);
        let task = tasks
            .first()
            .unwrap_or_else(|| panic!("case {:?}: expected a task back, got {body}", row.case));
        launched.push((row, string(task, "taskArn", row.case)));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, arn, case).await;
        let reason = stopped_reason(&task).to_string();
        let Some(failure) = row.failure else {
            // The logger was initialised, so the task RAN: it stops for the work it did,
            // not for a resource it could not bring up.
            assert_ne!(
                stop_code(&task),
                TASK_FAILED_TO_START,
                "case {case:?}: this task's logger can be initialised, so it must not fail \
                 to start, got {task}"
            );
            assert!(
                !reason.starts_with("ResourceInitializationError"),
                "case {case:?}: expected no resource-initialisation failure, got {reason:?}"
            );
            continue;
        };
        assert_eq!(
            stop_code(&task),
            TASK_FAILED_TO_START,
            "case {case:?}: a task whose logger could not be initialised never ran, got {task}"
        );
        assert!(
            reason.starts_with(LOGGER_FAILURE_PREFIX),
            "case {case:?}: expected a reason opening {LOGGER_FAILURE_PREFIX:?}, got {reason:?}"
        );
        assert!(
            reason.contains(failure),
            "case {case:?}: expected the reason to carry {failure:?}, got {reason:?}"
        );
        assert!(
            reason.ends_with("exceeded maximum number of attempts, 1"),
            "case {case:?}: expected the agent's account of its attempts, got {reason:?}"
        );
        // Nothing ran: no container of such a task claims an exit code, and nothing was
        // delivered to the group the configuration named.
        for container in list(&task, "containers", case) {
            assert_eq!(
                present_count(&container, "exitCode", case),
                None,
                "case {case:?}: a container that never started reports no exit code, got \
                 {container}"
            );
        }
        let held = streams(&router, row.group).await;
        assert!(
            held.iter().all(|stream| !stream.contains(&task_id(arn))),
            "case {case:?}: no stream is created for a logger that could not be \
             initialised, got {held:?}"
        );
    }
}

/// The task id one task ARN names, which is the last segment of the stream the driver would
/// have delivered that task's container to (`<prefix>/<container name>/<task id>`).
fn task_id(task_arn: &str) -> String {
    task_arn
        .rsplit('/')
        .next()
        .unwrap_or(task_arn)
        .to_string()
}
