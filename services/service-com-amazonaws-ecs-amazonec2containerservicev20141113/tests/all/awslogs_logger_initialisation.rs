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
//! that is there with the grant in place, a driver asked to create the group itself under a
//! role that may create it, and a revision naming no execution role at all, which states no
//! principal for a policy to be read against).
//!
//! Every fact is created through the owning service's own API — the log group through
//! CloudWatch Logs' `CreateLogGroup`, the execution role and its policies through IAM's
//! `CreateRole`/`PutRolePolicy` — and every value asserted is read back through
//! `DescribeTasks` and `DescribeLogStreams`.
//!
//! A second table spans the rest of what the rule generalises over: WHICH call started the
//! task (`RunTask`, `StartTask` on a named container instance, and the scheduler of a
//! service, which places tasks with no further call), and how the grant is SCOPED (every
//! resource, the streams of the group the configuration names, the streams of another
//! group). Its rows assert the WHOLE `stoppedReason`, whose variable parts — the session the
//! execution role was assumed in, the action the driver performs and the stream ARN it
//! addressed — come from the request and from the emulator's IAM and Logs state.
//!
//! A third table spans the GROUP the driver creates where it was asked to
//! (`awslogs-create-group`): that creation is a call of the driver's too, made as the same
//! role, so a launch whose execution role may not make it is refused and the group is never
//! created — while one whose role may runs and the group appears.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, create_cluster, iam, list, ok, present_count, register_capacity,
    string, ACCOUNT, SIGNED_REGION,
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

/// The actions the `awslogs` driver performs under the execution role: it creates the stream
/// it delivers to, writes the container's lines into it, and creates the log group itself
/// when the configuration asked it to (IAM Service Authorization Reference, "Actions defined
/// by Amazon CloudWatch Logs").
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";
const CREATE_LOG_GROUP: &str = "logs:CreateLogGroup";

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
        // "awslogs-create-group": the driver creates the group as well as the stream, under a
        // role that may create it, so a group that is not there yet is not a logger that
        // cannot be initialised. The role that may NOT is the third table below.
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
        Grant::Allowed => Some(policy(
            "Allow",
            &[CREATE_LOG_STREAM, PUT_LOG_EVENTS, CREATE_LOG_GROUP],
        )),
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
    register_logging_revision(
        router,
        family,
        row.group,
        row.create_group,
        role_arn,
        ONE_LINE_PROGRAM,
        row.case,
    )
    .await
}

/// The program a container whose task is expected to STOP runs: it writes one line and
/// exits, so a task that was launched at all stops for the work it did.
const ONE_LINE_PROGRAM: &str = "echo ready";

/// Register one revision of `family` whose single container delivers through the `awslogs`
/// driver to `group` — having the driver create that group when `create_group` is set — under
/// `role_arn`, running `program`, and answer its ARN.
async fn register_logging_revision(
    router: &MultiServiceRouter,
    family: &str,
    group: &str,
    create_group: bool,
    role_arn: Option<&str>,
    program: &str,
    case: &str,
) -> String {
    let mut options = json!({
        "awslogs-group": group,
        "awslogs-region": SIGNED_REGION,
        "awslogs-stream-prefix": STREAM_PREFIX,
    });
    if create_group {
        options["awslogs-create-group"] = json!("true");
    }
    let mut request = json!({
        "family": family,
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", program],
            "logConfiguration": { "logDriver": "awslogs", "options": options },
        }],
    });
    if let Some(role_arn) = role_arn {
        request["executionRoleArn"] = json!(role_arn);
    }
    let body = ok(router, "RegisterTaskDefinition", request, case).await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// One task of `cluster`, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [arn] }),
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

/// Read one task of `cluster` again until it reports a `stopCode`, and answer it.
async fn stopped_task(
    router: &MultiServiceRouter,
    cluster: &str,
    arn: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, cluster, arn, case).await;
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
        let task = stopped_task(&router, CLUSTER, arn, case).await;
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

// ---------------------------------------------------------------------------------------
// The same rule, on every call that STARTS a task, and for the WHOLE reason
// ---------------------------------------------------------------------------------------

/// Which call put the task in the cluster.
///
/// The agent initialises the logger of a task Amazon ECS PLACED, so the call that placed it
/// decides nothing: `RunTask` ("Starts a new task using the specified task definition"),
/// `StartTask` ("Starts a new task from the specified task definition on the specified
/// container instance") and the scheduler of a service ("Runs and maintains your desired
/// number of tasks from a specified task definition", `CreateService`) all reach one launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartPath {
    RunTask,
    StartTask,
    Scheduler,
}

/// What a row's execution role is allowed, as one identity-based document of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathGrant {
    /// The driver's two actions on every resource of the account.
    EveryGroup,
    /// Those actions on the streams of the group the configuration NAMES, which is how a
    /// grant for one log group is written.
    NamedGroupOnly,
    /// Those actions on the streams of ANOTHER group, so the one the configuration names is
    /// outside what the role may write to.
    OtherGroupOnly,
    /// A grant about another service entirely, so nothing in force allows the driver's
    /// actions at all.
    NoLogsGrant,
}

/// Which answer CloudWatch Logs gives the driver's stream creation, for a row whose launch
/// it refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// This account holds no log group by the name the configuration carries.
    NoSuchGroup,
    /// No policy in force on the execution role permits the creation.
    NotAuthorized,
}

/// A log group name no policy or configuration below delivers to: the group a grant scoped
/// AWAY from the one a row names is written for.
const SCOPED_AWAY_GROUP: &str = "/emulator/ecs/elsewhere";

/// `Task$lastStatus` of a task that is running its containers, and of one that is over
/// (`smithy_query(query="shape",shape="Task")`, member `lastStatus`: "The last known status
/// for the task").
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The program the container of a row that LAUNCHES runs: it writes one line and then stays
/// up, so such a task is observable in the `RUNNING` state it reached.
const STAYS_UP_PROGRAM: &str = "echo ready; sleep 300";

/// One row: the call that starts the task, the group its container's `awslogs` configuration
/// names, what its execution role may do, and the launch that produces.
struct PathRow {
    case: &'static str,
    path: StartPath,
    group: &'static str,
    grant: PathGrant,
    /// The refusal the stream creation earns, and `None` for a row whose launch goes ahead.
    refusal: Option<Refusal>,
}

const PATH_ROWS: &[PathRow] = &[
    PathRow {
        case: "RunTask into a group nobody created",
        path: StartPath::RunTask,
        group: UNHELD_GROUP,
        grant: PathGrant::EveryGroup,
        refusal: Some(Refusal::NoSuchGroup),
    },
    PathRow {
        case: "RunTask with no logs grant on the execution role",
        path: StartPath::RunTask,
        group: HELD_GROUP,
        grant: PathGrant::NoLogsGrant,
        refusal: Some(Refusal::NotAuthorized),
    },
    PathRow {
        case: "RunTask with the grant scoped to the group it delivers to",
        path: StartPath::RunTask,
        group: HELD_GROUP,
        grant: PathGrant::NamedGroupOnly,
        refusal: None,
    },
    PathRow {
        case: "StartTask into a group nobody created",
        path: StartPath::StartTask,
        group: UNHELD_GROUP,
        grant: PathGrant::EveryGroup,
        refusal: Some(Refusal::NoSuchGroup),
    },
    PathRow {
        case: "StartTask with the grant scoped to another group",
        path: StartPath::StartTask,
        group: HELD_GROUP,
        grant: PathGrant::OtherGroupOnly,
        refusal: Some(Refusal::NotAuthorized),
    },
    PathRow {
        case: "StartTask with the grant scoped to the group it delivers to",
        path: StartPath::StartTask,
        group: HELD_GROUP,
        grant: PathGrant::NamedGroupOnly,
        refusal: None,
    },
    PathRow {
        case: "a service whose revision delivers to a group nobody created",
        path: StartPath::Scheduler,
        group: UNHELD_GROUP,
        grant: PathGrant::EveryGroup,
        refusal: Some(Refusal::NoSuchGroup),
    },
    PathRow {
        case: "a service whose execution role has no logs grant",
        path: StartPath::Scheduler,
        group: HELD_GROUP,
        grant: PathGrant::NoLogsGrant,
        refusal: Some(Refusal::NotAuthorized),
    },
    PathRow {
        case: "a service whose grant is scoped to the group it delivers to",
        path: StartPath::Scheduler,
        group: HELD_GROUP,
        grant: PathGrant::NamedGroupOnly,
        refusal: None,
    },
];

/// The identity-based document one row's execution role carries, for a row delivering to
/// `group`.
fn path_policy(grant: PathGrant, group: &str) -> String {
    let delivery = [CREATE_LOG_STREAM, PUT_LOG_EVENTS];
    match grant {
        PathGrant::EveryGroup => policy("Allow", &delivery),
        PathGrant::NamedGroupOnly => scoped_policy(&delivery, &log_group_scope(group)),
        PathGrant::OtherGroupOnly => {
            scoped_policy(&delivery, &log_group_scope(SCOPED_AWAY_GROUP))
        }
        PathGrant::NoLogsGrant => policy("Allow", &["s3:GetObject"]),
    }
}

/// An identity-based policy allowing `actions` on `resource` ALONE.
fn scoped_policy(actions: &[&str], resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// The resource a grant written for ONE log group's streams names: that group's ARN with the
/// trailing wildcard, which is what covers the streams in it.
fn log_group_scope(group: &str) -> String {
    format!("arn:aws:logs:{SIGNED_REGION}:{ACCOUNT}:log-group:{group}:*")
}

/// The ARN of the session Amazon ECS initialises the logger in: an assumed-role session of
/// the task's execution role, named after the task it is for.
fn session_arn(role_arn: &str, task_id: &str) -> String {
    let role_name = role_arn.split('/').next_back().unwrap_or(role_arn);
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{task_id}")
}

/// The ARN of the stream the driver creates for one task's container: the group's own ARN
/// carrying the stream `<prefix>/<container name>/<task id>` the options name.
fn stream_arn(group: &str, task_id: &str) -> String {
    format!(
        "arn:aws:logs:{SIGNED_REGION}:{ACCOUNT}:log-group:{group}:log-stream:\
         {STREAM_PREFIX}/{CONTAINER}/{task_id}"
    )
}

/// The WHOLE `stoppedReason` a refused row's task must carry, built the way the agent builds
/// it from values of the request and of the emulator's own IAM and Logs state: the fixed
/// opening, the answer CloudWatch Logs gave — its own sentence for a group this account does
/// not hold, or the authorization refusal naming the session the role was assumed in, the
/// action the driver performs and the stream it addressed — and the agent's account of how
/// often it tried. Spelled out here rather than imported, so a wrong wording in the
/// implementation cannot make this pass.
fn expected_reason(refusal: Refusal, role_arn: &str, group: &str, task_id: &str) -> String {
    let failure = match refusal {
        Refusal::NoSuchGroup => RESOURCE_NOT_FOUND.to_string(),
        Refusal::NotAuthorized => format!(
            "{ACCESS_DENIED}User: {session} is not authorized to perform: {CREATE_LOG_STREAM} \
             on resource: {stream} because no identity-based policy allows the \
             {CREATE_LOG_STREAM} action",
            session = session_arn(role_arn, task_id),
            stream = stream_arn(group, task_id),
        ),
    };
    format!("{LOGGER_FAILURE_PREFIX}{failure}: exceeded maximum number of attempts, 1")
}

/// What one row's launch left to read back.
struct Launched<'row> {
    row: &'row PathRow,
    /// The cluster the row runs in: one of its own, so no row's launch competes with
    /// another's for capacity.
    cluster: String,
    /// The execution role the row's revision names.
    role_arn: String,
    /// The task the starting call answered with, and `None` for a row whose task the
    /// SCHEDULER places — that task is read back off the service that placed it.
    task_arn: Option<String>,
    /// The service whose scheduler places it, for such a row.
    service_name: String,
}

/// `Task$lastStatus`, or an empty string while the task reports none.
fn last_status(task: &Value) -> &str {
    task["lastStatus"].as_str().unwrap_or_default()
}

/// Read one task of `cluster` again until it SETTLES: stopped with a stop code for a row
/// whose logger could not be initialised, running for a row whose logger could.
///
/// A refused row's task is also asserted never to have been observed `RUNNING`: a launch the
/// logger failed starts no container, so no reading of it reports the state of a task that
/// is running one.
async fn settled_task(
    router: &MultiServiceRouter,
    cluster: &str,
    arn: &str,
    refusal: Option<Refusal>,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let task = describe(router, cluster, arn, case).await;
        let status = last_status(&task).to_string();
        if !status.is_empty() {
            seen.insert(status.clone());
        }
        let settled = if refusal.is_some() {
            status == STOPPED && !stop_code(&task).is_empty()
        } else {
            status == RUNNING
        };
        if settled {
            if refusal.is_some() {
                assert!(
                    !seen.contains(RUNNING),
                    "case {case:?}: a task whose logger could not be initialised runs no \
                     container, so it is never {RUNNING}, got the statuses {seen:?}"
                );
            }
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to settle (refusal {refusal:?}), got {task} \
             after the statuses {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The task the scheduler of `service` placed, read back off the service rather than off a
/// call that placed it: its ARN and the settled task itself.
async fn scheduler_task(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    refusal: Option<Refusal>,
    case: &str,
) -> (String, Value) {
    // `ListTasks` reports the tasks on their way to the status asked for, so a task the
    // launch failed is listed under the one it is on its way to.
    let desired = if refusal.is_some() { STOPPED } else { RUNNING };
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let listed = ok(
            router,
            "ListTasks",
            json!({
                "cluster": cluster,
                "serviceName": service,
                "desiredStatus": desired,
            }),
            case,
        )
        .await;
        let placed = list(&listed, "taskArns", case);
        if let Some(arn) = placed.first().and_then(Value::as_str) {
            let arn = arn.to_string();
            let task = settled_task(router, cluster, &arn, refusal, case).await;
            return (arn, task);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the scheduler to place a task on its way to {desired}, \
             got {listed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read `group` again until it holds the stream `stream` — the delivery a container that RAN
/// opens — and panic naming what the group held instead when it never does.
async fn group_holding_stream(router: &MultiServiceRouter, group: &str, stream: &str, case: &str) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = streams(router, group).await;
        if held.iter().any(|name| name == stream) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to hold the stream {stream:?} the driver creates \
             for a container that runs, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the `awslogs` destination decides whether a task's containers are launched AT ALL,
/// whichever call placed the task — and a task refused it is stopped with
/// `TaskFailedToStart` and the agent's whole `ResourceInitializationError` sentence, naming
/// the session the execution role was assumed in, the action the driver performs and the
/// stream it addressed.
#[tokio::test]
async fn an_unwritable_awslogs_destination_fails_the_task_on_every_start_path() {
    let router = emulator();
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": HELD_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group the held rows deliver into must be created, got {status} with {created}"
    );

    // Every row is started FIRST and read back afterwards: the launches are decided in the
    // one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<Launched<'_>> = Vec::new();
    for (index, row) in PATH_ROWS.iter().enumerate() {
        let case = row.case;
        let cluster = format!("loggerpath{index}");
        create_cluster(&router, &cluster, case).await;
        let instance = register_capacity(&router, &cluster, case).await;
        let role_arn = create_role(
            &router,
            &format!("loggerPathRole{index}"),
            Some(path_policy(row.grant, row.group)),
            case,
        )
        .await;
        let family = format!("loggerpath{index}");
        let definition = register_logging_revision(
            &router,
            &family,
            row.group,
            false,
            Some(&role_arn),
            STAYS_UP_PROGRAM,
            case,
        )
        .await;
        let service_name = format!("loggerpathsvc{index}");
        let task_arn = match row.path {
            StartPath::RunTask => Some(started_task_arn(
                &router,
                "RunTask",
                json!({ "cluster": cluster, "taskDefinition": definition }),
                case,
            )
            .await),
            StartPath::StartTask => Some(
                started_task_arn(
                    &router,
                    "StartTask",
                    json!({
                        "cluster": cluster,
                        "taskDefinition": definition,
                        "containerInstances": [instance],
                    }),
                    case,
                )
                .await,
            ),
            StartPath::Scheduler => {
                ok(
                    &router,
                    "CreateService",
                    json!({
                        "cluster": cluster,
                        "serviceName": service_name,
                        "taskDefinition": definition,
                        "desiredCount": 1,
                        "launchType": "EC2",
                    }),
                    case,
                )
                .await;
                None
            }
        };
        launched.push(Launched {
            row,
            cluster,
            role_arn,
            task_arn,
            service_name,
        });
    }

    for entry in &launched {
        let row = entry.row;
        let case = row.case;
        let (arn, task) = match entry.task_arn.as_deref() {
            Some(arn) => (
                arn.to_string(),
                settled_task(&router, &entry.cluster, arn, row.refusal, case).await,
            ),
            None => {
                scheduler_task(
                    &router,
                    &entry.cluster,
                    &entry.service_name,
                    row.refusal,
                    case,
                )
                .await
            }
        };
        let task_id = task_id(&arn);
        let stream = format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}");
        let Some(refusal) = row.refusal else {
            // The logger was initialised, so the containers were launched: such a task is
            // running, carries no stop and opened the stream its output goes to.
            assert_eq!(
                last_status(&task),
                RUNNING,
                "case {case:?}: a task whose logger can be initialised runs, got {task}"
            );
            assert!(
                stop_code(&task).is_empty() && stopped_reason(&task).is_empty(),
                "case {case:?}: a running task carries no stop, got {task}"
            );
            // `Task$startedAt` is "the Unix timestamp for the time when the task started",
            // which a task whose containers were launched carries.
            assert!(
                task["startedAt"].is_number(),
                "case {case:?}: a task whose containers were launched reports when it \
                 started, got {task}"
            );
            group_holding_stream(&router, row.group, &stream, case).await;
            continue;
        };
        assert_eq!(
            stop_code(&task),
            TASK_FAILED_TO_START,
            "case {case:?}: a task whose logger could not be initialised never ran, got {task}"
        );
        assert_eq!(
            last_status(&task),
            STOPPED,
            "case {case:?}: such a task is reported stopped, got {task}"
        );
        assert_eq!(
            stopped_reason(&task),
            expected_reason(refusal, &entry.role_arn, row.group, &task_id),
            "case {case:?}: expected the agent's whole account of the logger it could not \
             validate, got {task}"
        );
        // Nothing ran, whatever a poll of this task happened to catch: a task Amazon ECS
        // never started carries no `startedAt` ("the Unix timestamp for the time when the
        // task started"), and no container of it claims an exit code.
        assert!(
            task["startedAt"].is_null(),
            "case {case:?}: a task whose logger could not be initialised was never started, \
             so it reports no startedAt, got {task}"
        );
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
            !held.contains(&stream),
            "case {case:?}: nothing is delivered for a logger that could not be initialised, \
             got {held:?}"
        );
    }
}

/// Start one task through `operation` and answer the ARN it was placed under.
async fn started_task_arn(
    router: &MultiServiceRouter,
    operation: &str,
    request: Value,
    case: &str,
) -> String {
    let body = ok(router, operation, request, case).await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a task back, got {body}"));
    string(task, "taskArn", case)
}

// ---------------------------------------------------------------------------------------
// The GROUP the driver creates where it was asked to (`awslogs-create-group`)
// ---------------------------------------------------------------------------------------

/// The cluster the third table's tasks run in: its own, so its launches compete with no
/// other row's for capacity.
const CREATE_GROUP_CLUSTER: &str = "loggercreategroup";

/// The fixed OPENING of the agent's account of a logger it could not validate, up to the
/// THING the driver could not create — the attempts it reports are the stream creation's,
/// which is the call the driver retries. Spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make this pass.
const LOGGER_FAILURE_OPENING: &str =
    "ResourceInitializationError: failed to validate logger args: create stream has been \
     retried 1 times: failed to create Cloudwatch log ";

/// What the third table's execution role is allowed, as one identity-based document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateGrant {
    /// The two actions the driver performs on the STREAM and nothing about creating a group
    /// — which is exactly what AWS's own managed policy for a task execution role grants
    /// (`AmazonECSTaskExecutionRolePolicy`: `logs:CreateLogStream`, `logs:PutLogEvents`).
    DeliveryOnly,
    /// Those actions AND the creation of the group.
    DeliveryAndGroup,
    /// The creation of the group alone, so the STREAM the driver creates first is outside
    /// what the role may do.
    GroupOnly,
    /// Those actions with the group's creation explicitly DENIED.
    GroupDenied,
}

/// Which of IAM's rules withheld the grant, as the clause a refusal names it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// IAM's default deny: nothing readable allowed the action.
    NoPolicyAllows,
    /// An explicit `Deny` in one of the principal's own documents.
    ExplicitDeny,
}

/// What a third-table row's launch produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Initialised {
    /// The logger was initialised, so the task RUNS — and the group the driver was asked to
    /// create is there with this container's stream in it when `creates_group` is set.
    Runs { creates_group: bool },
    /// The GROUP creation is refused: `logs:CreateLogGroup` on the group's own ARN.
    GroupRefused(Clause),
    /// The STREAM creation is refused: `logs:CreateLogStream` on the stream's ARN.
    StreamRefused(Clause),
    /// There is no group to deliver into and the driver was not asked to create one, so what
    /// CloudWatch Logs answers is its own sentence for a group this account does not hold.
    NoGroup,
}

/// One row: whether the driver is asked to create the group it delivers to, whether that
/// group is there already, what the execution role may do, and the launch that produces.
struct CreateGroupRow {
    case: &'static str,
    /// `awslogs-create-group`.
    create_group: bool,
    /// Whether the suite creates the group through CloudWatch Logs' own API first.
    held: bool,
    grant: CreateGrant,
    outcome: Initialised,
}

/// The rows differ along what the rule generalises over: whether the driver was asked to
/// create its group, whether there is one already, and which of the driver's two creations
/// the execution role may make.
const CREATE_GROUP_ROWS: &[CreateGroupRow] = &[
    CreateGroupRow {
        case: "the driver must create the group and nothing allows the role to",
        create_group: true,
        held: false,
        grant: CreateGrant::DeliveryOnly,
        outcome: Initialised::GroupRefused(Clause::NoPolicyAllows),
    },
    CreateGroupRow {
        case: "the driver must create the group and a policy denies that creation",
        create_group: true,
        held: false,
        grant: CreateGrant::GroupDenied,
        outcome: Initialised::GroupRefused(Clause::ExplicitDeny),
    },
    CreateGroupRow {
        case: "the driver must create the group and the role may",
        create_group: true,
        held: false,
        grant: CreateGrant::DeliveryAndGroup,
        outcome: Initialised::Runs {
            creates_group: true,
        },
    },
    CreateGroupRow {
        // Nothing is created where the group is already there, so a role that may not create
        // one launches all the same.
        case: "the group is there already and the role may not create one",
        create_group: true,
        held: true,
        grant: CreateGrant::DeliveryOnly,
        outcome: Initialised::Runs {
            creates_group: false,
        },
    },
    CreateGroupRow {
        // A driver that was NOT asked to create its group never makes that call, so the
        // grant for it decides nothing: what such a launch meets is the missing group.
        case: "the driver is not asked to create the group, and there is none",
        create_group: false,
        held: false,
        grant: CreateGrant::DeliveryAndGroup,
        outcome: Initialised::NoGroup,
    },
    CreateGroupRow {
        // The STREAM is the creation the driver makes first, and the group is created only
        // because that one found no group to land in: a role allowed the group alone is
        // refused the stream.
        case: "the role may create the group but not the stream",
        create_group: true,
        held: false,
        grant: CreateGrant::GroupOnly,
        outcome: Initialised::StreamRefused(Clause::NoPolicyAllows),
    },
];

/// The identity-based document one third-table row's execution role carries.
fn create_group_policy(grant: CreateGrant) -> String {
    let delivery = [CREATE_LOG_STREAM, PUT_LOG_EVENTS];
    match grant {
        CreateGrant::DeliveryOnly => policy("Allow", &delivery),
        CreateGrant::DeliveryAndGroup => policy(
            "Allow",
            &[CREATE_LOG_GROUP, CREATE_LOG_STREAM, PUT_LOG_EVENTS],
        ),
        CreateGrant::GroupOnly => policy("Allow", &[CREATE_LOG_GROUP]),
        CreateGrant::GroupDenied => json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": [CREATE_LOG_GROUP, CREATE_LOG_STREAM, PUT_LOG_EVENTS],
                    "Resource": "*",
                },
                { "Effect": "Deny", "Action": [CREATE_LOG_GROUP], "Resource": "*" },
            ],
        })
        .to_string(),
    }
}

/// The ARN of the log GROUP itself — the resource a `logs:CreateLogGroup` grant names, which
/// carries no stream and no trailing wildcard, since that call names a group and no stream.
fn group_arn(group: &str) -> String {
    format!("arn:aws:logs:{SIGNED_REGION}:{ACCOUNT}:log-group:{group}")
}

/// The authorization refusal CloudWatch Logs answers one of the driver's creations with:
/// the session the execution role was assumed in, the action the driver performs, the
/// resource it addressed and the clause naming which of IAM's rules withheld the grant.
fn not_authorized(clause: Clause, action: &str, session: &str, resource: &str) -> String {
    let reason = match clause {
        Clause::NoPolicyAllows => {
            format!("because no identity-based policy allows the {action} action")
        }
        Clause::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
    };
    format!(
        "{ACCESS_DENIED}User: {session} is not authorized to perform: {action} on resource: \
         {resource} {reason}"
    )
}

/// The WHOLE `stoppedReason` a refused third-table row's task must carry: the fixed opening,
/// WHICH of the driver's creations failed, the answer CloudWatch Logs gave and the agent's
/// account of how often it tried.
fn created_group_reason(
    outcome: Initialised,
    role_arn: &str,
    group: &str,
    task_id: &str,
    case: &str,
) -> String {
    let session = session_arn(role_arn, task_id);
    let (created, failure) = match outcome {
        Initialised::GroupRefused(clause) => (
            "group",
            not_authorized(clause, CREATE_LOG_GROUP, &session, &group_arn(group)),
        ),
        Initialised::StreamRefused(clause) => (
            "stream",
            not_authorized(
                clause,
                CREATE_LOG_STREAM,
                &session,
                &stream_arn(group, task_id),
            ),
        ),
        Initialised::NoGroup => ("stream", RESOURCE_NOT_FOUND.to_string()),
        Initialised::Runs { .. } => {
            panic!("case {case:?}: this row's logger is initialised, so it carries no reason")
        }
    };
    format!("{LOGGER_FAILURE_OPENING}{created}: {failure}: exceeded maximum number of attempts, 1")
}

/// The names of every log group whose name begins with `prefix`, through CloudWatch Logs'
/// own `DescribeLogGroups` — empty when this account holds none.
async fn group_names(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": prefix }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLogGroups must answer the groups of this account, got {status} with {body}"
    );
    body["logGroups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| group["logGroupName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Read a group's name again until this account holds it, and panic naming what it held
/// instead when it never does.
async fn group_created(router: &MultiServiceRouter, group: &str, case: &str) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = group_names(router, group).await;
        if held.iter().any(|name| name == group) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the driver to create the group {group}, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the GROUP a driver configured to create the one it delivers to has to create is a
/// call of that driver's, made as the task's execution role — so a launch whose role may not
/// make it is never started, the group it named is never created, and the stop says which of
/// the driver's creations failed; a launch whose role may runs, and the group appears.
#[tokio::test]
async fn a_driver_asked_to_create_its_log_group_launches_only_where_the_role_may_create_it() {
    let router = emulator();
    cluster_with_capacity(&router, CREATE_GROUP_CLUSTER, "created group").await;

    // Every row is started FIRST and read back afterwards: the launches are decided in the
    // one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&CreateGroupRow, String, String, String)> = Vec::new();
    for (index, row) in CREATE_GROUP_ROWS.iter().enumerate() {
        let case = row.case;
        // One group name per row, so a group a row creates is not a group another row finds.
        let group = format!("/emulator/ecs/created-group-{index}");
        if row.held {
            let (status, created) = logs(
                &router,
                "CreateLogGroup",
                json!({ "logGroupName": group.clone() }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the group this row delivers into must be created, got {status} \
                 with {created}"
            );
        }
        let role_arn = create_role(
            &router,
            &format!("createGroupRole{index}"),
            Some(create_group_policy(row.grant)),
            case,
        )
        .await;
        let definition = register_logging_revision(
            &router,
            &format!("creategroup{index}"),
            &group,
            row.create_group,
            Some(&role_arn),
            STAYS_UP_PROGRAM,
            case,
        )
        .await;
        let task_arn = started_task_arn(
            &router,
            "RunTask",
            json!({ "cluster": CREATE_GROUP_CLUSTER, "taskDefinition": definition }),
            case,
        )
        .await;
        launched.push((row, group, role_arn, task_arn));
    }

    for (row, group, role_arn, arn) in &launched {
        let case = row.case;
        let refused = !matches!(row.outcome, Initialised::Runs { .. });
        let refusal = refused.then_some(Refusal::NotAuthorized);
        let task = settled_task(&router, CREATE_GROUP_CLUSTER, arn, refusal, case).await;
        let task_id = task_id(arn);
        let stream = format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}");
        if let Initialised::Runs { creates_group } = row.outcome {
            assert_eq!(
                last_status(&task),
                RUNNING,
                "case {case:?}: a task whose logger can be initialised runs, got {task}"
            );
            assert!(
                stop_code(&task).is_empty() && stopped_reason(&task).is_empty(),
                "case {case:?}: a running task carries no stop, got {task}"
            );
            if creates_group {
                group_created(&router, group, case).await;
            }
            group_holding_stream(&router, group, &stream, case).await;
            continue;
        }
        assert_eq!(
            stop_code(&task),
            TASK_FAILED_TO_START,
            "case {case:?}: a task whose logger could not be initialised never ran, got {task}"
        );
        assert_eq!(
            stopped_reason(&task),
            created_group_reason(row.outcome, role_arn, group, &task_id, case),
            "case {case:?}: expected the agent's whole account of the logger it could not \
             validate, got {task}"
        );
        // Nothing ran, and nothing was created: a task Amazon ECS never started carries no
        // `startedAt`, no container of it claims an exit code, and the group the driver would
        // have created is not there.
        assert!(
            task["startedAt"].is_null(),
            "case {case:?}: a task whose logger could not be initialised was never started, \
             so it reports no startedAt, got {task}"
        );
        for container in list(&task, "containers", case) {
            assert_eq!(
                present_count(&container, "exitCode", case),
                None,
                "case {case:?}: a container that never started reports no exit code, got \
                 {container}"
            );
        }
        let held = group_names(&router, group).await;
        assert!(
            held.is_empty(),
            "case {case:?}: the log group of a logger that could not be initialised is never \
             created, got {held:?}"
        );
    }
}
