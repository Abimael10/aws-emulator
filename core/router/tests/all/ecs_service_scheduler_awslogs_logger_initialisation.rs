// SPDX-License-Identifier: CC0-1.0

//! The tasks a SERVICE SCHEDULER places are gated on their `awslogs` logger exactly as a
//! task a client runs itself is.
//!
//! RULE: the `awslogs` log driver a container definition's `logConfiguration` names is
//! initialised BEFORE the container it is for runs — it creates the log stream that
//! container's output will go to — and Amazon ECS makes that call as the task's EXECUTION
//! role ("the task execution role that grants the Amazon ECS container agent permission to
//! make Amazon Web Services API calls on your behalf", `TaskDefinition$executionRoleArn`).
//! So a task whose logger cannot be initialised launches nothing, whoever placed it: it is
//! reported with `Task$stopCode` `TaskFailedToStart` (`smithy_query(query="shape",
//! shape="TaskStopCode")`) and a `Task$stoppedReason` of the agent's
//! `ResourceInitializationError` class carrying the answer CloudWatch Logs gave — its
//! authorization refusal, naming the session the role was assumed in, the action and the
//! stream, or its own sentence for a log group this account does not hold — no container of
//! it reports an exit code, because none ran, and no stream of that task is created. A task
//! whose logger CAN be initialised runs and delivers.
//!
//! This suite drives the half of the rule a CLIENT CALL never reaches: the task is placed by
//! the service scheduler off the clock, with no `RunTask` of its own, which is the path an
//! `AWS::ECS::Service` of a deployed stack comes up through. The same rule over the shapes a
//! single launch can take is tabled in the ECS crate's
//! `tests/all/awslogs_logger_initialisation.rs`; the rows here differ along WHY the stream
//! creation is refused — nothing in force on the execution role allows it, a statement on it
//! explicitly denies it, the group the configuration names was never created — beside the
//! row whose logger can be initialised, whose task runs and delivers.
//!
//! It lives beside the router because the three stores the rule spans are three services':
//! the log group and the stream are CloudWatch Logs' own resources, the execution role and
//! the documents in force on it are IAM's, and the service and its tasks are Amazon ECS's.
//! Every fact is created through the owning service's own API, and every value asserted is
//! read back through `DescribeServices`, `ListTasks`, `DescribeTasks` and
//! `DescribeLogStreams`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every request below is signed for, and the one the configurations name.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The cluster every service below runs in.
const CLUSTER: &str = "schedulerloggerinit";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image every container names. Nothing is fetched from it: the reference names a
/// registry this emulator answers nothing for.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The `awslogs-stream-prefix` every configuration names, and so the first segment of the
/// stream a container of it delivers to.
const STREAM_PREFIX: &str = "svc";

/// The log group that EXISTS here, created through CloudWatch Logs' own `CreateLogGroup`.
const HELD_GROUP: &str = "/emulator/ecs/scheduled-logger-init";

/// A log group name no `CreateLogGroup` here was made for — what a configuration pointed at
/// a group nobody created carries.
const UNHELD_GROUP: &str = "/emulator/ecs/scheduled-logger-missing";

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

/// How the agent's account of a logger it could not validate OPENS and CLOSES, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass.
const LOGGER_FAILURE_PREFIX: &str =
    "ResourceInitializationError: failed to validate logger args: create stream has been \
     retried 1 times: failed to create Cloudwatch log stream: ";
const LOGGER_FAILURE_SUFFIX: &str = ": exceeded maximum number of attempts, 1";

/// The sentence CloudWatch Logs refuses a request for a log group this account does not hold
/// with (`ResourceNotFoundException`, "The specified resource does not exist.", worded for
/// the resource kind).
const RESOURCE_NOT_FOUND: &str =
    "ResourceNotFoundException: The specified log group does not exist.";

/// The reason clause an authorization refusal carries when nothing in force on the role
/// allows the action, and when a statement on it explicitly denies the action.
const NO_POLICY_ALLOWS: &str = "because no identity-based policy allows the logs:CreateLogStream action";
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// How long the container's program stays up after it has written its line, so a task of a
/// converged service is one whose program is still running.
const STAY_UP_SECONDS: u32 = 600;

/// Every service below is created with the DEPLOYMENT CIRCUIT BREAKER on, which is what a
/// rollout whose tasks keep failing to start reaches a terminal state through: "A deployment
/// in `FAILED` state doesn't launch any new tasks" (`Deployment$rolloutState`). The rule
/// under test is about the FIRST task the scheduler places, so the breaker only bounds how
/// many replacements follow it.
fn deployment_configuration() -> Value {
    json!({
        "deploymentCircuitBreaker": { "enable": true, "rollback": false },
    })
}

/// The line the container writes, which is the one event its stream must hold.
const WRITTEN_LINE: &str = "scheduled-ready";

/// How often a poll re-reads the emulator.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: every placement the scheduler
/// makes is owed by the clock, a failing one is stopped at the instant it would have started,
/// and this suite runs beside every other test of the workspace.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// What one row gives its execution role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// A role whose policy allows the driver's actions.
    Allowed,
    /// A role carrying a policy about another service entirely, so nothing in force on it
    /// allows the driver's actions.
    Unrelated,
    /// A role whose policy explicitly denies the stream creation.
    Denied,
}

/// Why a row's stream creation is refused, which decides the clause the refusal carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The logger can be initialised: the task runs and delivers.
    Launches,
    /// The creation is refused on authorization, with this reason clause.
    NotAuthorized(&'static str),
    /// There is no group to create the stream in.
    NotFound,
}

/// One row: the service the scheduler is given, the group its container delivers to, what
/// its execution role may do, and the launch that produces.
struct Row {
    case: &'static str,
    service_name: &'static str,
    family: &'static str,
    role_name: &'static str,
    group: &'static str,
    grant: Grant,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        case: "the group is there and the role may create the stream",
        service_name: "logger-allowed",
        family: "schedloggerallowed",
        role_name: "schedLoggerAllowed",
        group: HELD_GROUP,
        grant: Grant::Allowed,
        outcome: Outcome::Launches,
    },
    Row {
        case: "no policy in force on the execution role allows the stream creation",
        service_name: "logger-unrelated",
        family: "schedloggerunrelated",
        role_name: "schedLoggerUnrelated",
        group: HELD_GROUP,
        grant: Grant::Unrelated,
        outcome: Outcome::NotAuthorized(NO_POLICY_ALLOWS),
    },
    Row {
        case: "a statement on the execution role denies the stream creation",
        service_name: "logger-denied",
        family: "schedloggerdenied",
        role_name: "schedLoggerDenied",
        group: HELD_GROUP,
        grant: Grant::Denied,
        outcome: Outcome::NotAuthorized(EXPLICIT_DENY),
    },
    Row {
        case: "the configuration names a group nobody created",
        service_name: "logger-missing-group",
        family: "schedloggermissing",
        role_name: "schedLoggerMissing",
        group: UNHELD_GROUP,
        grant: Grant::Allowed,
        outcome: Outcome::NotFound,
    },
];

/// One emulator carrying Amazon ECS, IAM — which holds the execution role a launch is decided
/// against — and CloudWatch Logs, which owns the log group and the streams, over ONE shared
/// engine, as the production router mounts them.
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsJson1_1` call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    host: &str,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = json_call(
        router,
        "ecs",
        "ecs",
        &format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, answered with its HTTP status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(
        router,
        "logs",
        "logs",
        &format!("Logs_20140328.{operation}"),
        body,
    )
    .await
}

/// One awsQuery call to IAM that must succeed. The role and the policies in force on it are
/// created through IAM's OWN API, never written into its store.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, String)], case: &str) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
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

/// Create one row's execution role through IAM's own API — a trust policy that lets Amazon
/// ECS assume it, and the inline document the row's grant calls for — and answer its ARN.
async fn create_execution_role(router: &MultiServiceRouter, row: &Row) -> String {
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
            ("RoleName", row.role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        row.case,
    )
    .await;
    let document = match row.grant {
        Grant::Allowed => policy("Allow", &[CREATE_LOG_STREAM, PUT_LOG_EVENTS]),
        // A grant about another service entirely: the role carries a document, and nothing
        // in it allows the driver's actions.
        Grant::Unrelated => policy("Allow", &["s3:GetObject"]),
        Grant::Denied => policy("Deny", &[CREATE_LOG_STREAM]),
    };
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", row.role_name.to_string()),
            ("PolicyName", "logger".to_string()),
            ("PolicyDocument", document),
        ],
        row.case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{}", row.role_name)
}

/// An EC2 instance identity document, as the agent reads it from the instance metadata
/// service, and a base64 signature beside it: a registration presents both.
fn identity_document(instance_id: &str) -> String {
    json!({
        "accountId": ACCOUNT,
        "instanceId": instance_id,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string()
}

/// Register one revision of `row`'s family: a single container that writes one line and stays
/// up, delivering its output through the `awslogs` driver to the row's group under the row's
/// execution role.
async fn register_definition(router: &MultiServiceRouter, row: &Row, role_arn: &str) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "bridge",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": [
                    "sh",
                    "-c",
                    format!("echo {WRITTEN_LINE}; sleep {STAY_UP_SECONDS}"),
                ],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": row.group,
                        "awslogs-region": REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
        row.case,
    )
    .await;
}

/// The task ARNs of one row's service with the desired status `desired`.
async fn task_arns(router: &MultiServiceRouter, row: &Row, desired: &str) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "desiredStatus": desired,
        }),
        row.case,
    )
    .await;
    listed["taskArns"]
        .as_array()
        .map(|arns| {
            arns.iter()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One task of one row's service, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, row: &Row, arn: &str) -> Value {
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        row.case,
    )
    .await;
    described["tasks"][0].clone()
}

/// Poll `probe` until it answers `Some`, or the budget runs out. The wait is for the
/// CONDITION: every transition below is owed by the clock, not by a call.
async fn poll_until<T, F, Fut>(probe: F, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        if let Some(answer) = probe().await {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{description}: not observed within {POLL_BUDGET:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The stream the driver delivers one task's container to: `<prefix>/<container>/<task id>`,
/// the naming `awslogs-stream-prefix` documents.
fn stream_of(task_id: &str) -> String {
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// What `DescribeLogStreams` answers for one group: the HTTP status and the parsed body,
/// both asserted on by the caller — a read that was refused is not evidence about what the
/// group holds.
async fn described_streams(router: &MultiServiceRouter, group: &str) -> (u16, Value) {
    logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": group }),
    )
    .await
}

/// The stream names one `DescribeLogStreams` answer lists.
fn stream_names(body: &Value) -> Vec<String> {
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

/// The messages one stream of one group holds, oldest first. The group exists wherever this
/// is called, and the stream is created by the delivery itself, so a refusal here is only
/// ever the stream not being there yet — which is what the caller polls through.
async fn delivered(router: &MultiServiceRouter, group: &str, stream: &str) -> Vec<String> {
    let (_, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": group,
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

/// The WHOLE `stoppedReason` one row's failed launch must carry, composed the way the agent
/// composes it: the fixed opening, the answer CloudWatch Logs gave for THIS task's stream,
/// and the fixed close. Every variable part comes from the request and the emulator's own
/// records — the session the execution role was assumed in for this task, the action, and the
/// stream the configuration addressed.
fn expected_reason(row: &Row, task_id: &str) -> String {
    let failure = match row.outcome {
        Outcome::Launches => unreachable!("a row that launches has no stopped reason"),
        Outcome::NotFound => RESOURCE_NOT_FOUND.to_string(),
        Outcome::NotAuthorized(clause) => format!(
            "AccessDeniedException: User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{task_id} is \
             not authorized to perform: {CREATE_LOG_STREAM} on resource: \
             arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{}:log-stream:{} {clause}",
            row.role_name,
            row.group,
            stream_of(task_id)
        ),
    };
    format!("{LOGGER_FAILURE_PREFIX}{failure}{LOGGER_FAILURE_SUFFIX}")
}

/// The task id one task ARN names: the last segment of it.
fn task_id_of(task_arn: &str) -> String {
    task_arn
        .rsplit('/')
        .next()
        .unwrap_or(task_arn)
        .to_string()
}

#[tokio::test]
async fn a_scheduled_task_whose_awslogs_logger_cannot_be_initialised_fails_to_start() {
    let router = emulator();
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "the cluster every row's service runs in",
    )
    .await;
    // The scheduler places these tasks on capacity registered to the cluster: "The EC2 launch
    // type runs your tasks on Amazon EC2 instances registered to your cluster."
    ecs(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity_document("i-0c0ffee0c0ffee0c0"),
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
        "the capacity the scheduler places on",
    )
    .await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": HELD_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group these rows deliver into must be created, got {status} with {created}"
    );

    // Every row's service is created up front, so the clock-owed placements of all of them
    // fall due at once rather than one row's budget after another's.
    for row in ROWS {
        let role_arn = create_execution_role(&router, row).await;
        register_definition(&router, row, &role_arn).await;
        let created = ecs(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name,
                "taskDefinition": row.family,
                "desiredCount": 1,
                "launchType": "EC2",
                "deploymentConfiguration": deployment_configuration(),
            }),
            row.case,
        )
        .await;
        assert_eq!(
            created["service"]["runningCount"].as_i64(),
            Some(0),
            "case {:?}: nothing runs at the instant the service is created, got {created}",
            row.case
        );
    }

    for row in ROWS {
        let case = row.case;
        if row.outcome == Outcome::Launches {
            // The logger can be initialised, so the scheduler's task RUNS and its container's
            // line reaches the stream the configuration named.
            let arn = poll_until(
                || async {
                    let running = task_arns(&router, row, "RUNNING").await;
                    let arn = running.first()?.clone();
                    let task = describe_task(&router, row, &arn).await;
                    (task["lastStatus"].as_str() == Some("RUNNING")).then_some(arn)
                },
                &format!("case {case:?}: a running task of {}", row.service_name),
            )
            .await;
            let task = describe_task(&router, row, &arn).await;
            assert_eq!(
                task["stopCode"].as_str(),
                None,
                "case {case:?}: a task that started carries no stop code, got {task}"
            );
            let stream = stream_of(&task_id_of(&arn));
            let events = poll_until(
                || async {
                    let events = delivered(&router, row.group, &stream).await;
                    (!events.is_empty()).then_some(events)
                },
                &format!("case {case:?}: the line delivered to {stream}"),
            )
            .await;
            assert_eq!(
                events,
                vec![WRITTEN_LINE.to_string()],
                "case {case:?}: expected the container's line in {stream}, got {events:?}"
            );
            // The group itself lists that stream: the delivery created it there, which is the
            // creation the negative rows are refused.
            let (status, listed) = described_streams(&router, row.group).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the group the configuration named must be readable, got \
                 {status} with {listed}"
            );
            assert!(
                stream_names(&listed).contains(&stream),
                "case {case:?}: expected the group to hold {stream}, got {listed}"
            );
            continue;
        }

        // The logger cannot be initialised, so the task the SCHEDULER placed is stopped at the
        // instant it would otherwise have started.
        let arn = poll_until(
            || async {
                let stopped = task_arns(&router, row, "STOPPED").await;
                let arn = stopped.first()?.clone();
                let task = describe_task(&router, row, &arn).await;
                task["stopCode"].as_str().is_some().then_some(arn)
            },
            &format!("case {case:?}: a stopped task of {}", row.service_name),
        )
        .await;
        let task = describe_task(&router, row, &arn).await;
        assert_eq!(
            task["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose logger could not be initialised never ran, got {task}"
        );
        let expected = expected_reason(row, &task_id_of(&arn));
        assert_eq!(
            task["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {task}"
        );
        // Nothing ran: no container of such a task claims an exit code, and no stream of it
        // was created in the group the configuration named.
        let containers = task["containers"].as_array().cloned().unwrap_or_default();
        assert!(
            !containers.is_empty(),
            "case {case:?}: the stopped task reports its containers, got {task}"
        );
        for container in containers {
            assert_eq!(
                container["exitCode"].as_i64(),
                None,
                "case {case:?}: a container that never started reports no exit code, got \
                 {container}"
            );
        }
        let stream = stream_of(&task_id_of(&arn));
        let (status, listed) = described_streams(&router, row.group).await;
        if row.group == UNHELD_GROUP {
            // Nothing created this group, so CloudWatch Logs refuses the read of it in its
            // own words: there is no group for a stream of this task to have been created in.
            assert_ne!(
                status, 200,
                "case {case:?}: a group nobody created is not readable, got {listed}"
            );
            assert!(
                listed.to_string().contains("ResourceNotFoundException"),
                "case {case:?}: expected the log service's own not-found refusal, got {listed}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: the group the configuration named must be readable, got \
                 {status} with {listed}"
            );
            let held = stream_names(&listed);
            assert!(
                !held.contains(&stream),
                "case {case:?}: no stream is created for a logger that could not be \
                 initialised, got {held:?}"
            );
        }
    }
}
