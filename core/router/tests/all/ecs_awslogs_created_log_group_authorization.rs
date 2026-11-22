// SPDX-License-Identifier: CC0-1.0

//! Whether a task whose `awslogs` driver was asked to CREATE the log group it delivers into
//! can be LAUNCHED, and the stop one whose execution role may not create that group earns.
//!
//! RULE: the `awslogs` log driver a container definition's `logConfiguration` names is
//! initialised BEFORE the container it is for runs, and Amazon ECS makes every call that
//! initialisation takes AS the task's EXECUTION role ("the task execution role that grants
//! the Amazon ECS container agent permission to make Amazon Web Services API calls on your
//! behalf", `TaskDefinition$executionRoleArn`). The driver creates the STREAM first; only
//! when that finds no log group, and only when the configuration asked it to create one
//! (`awslogs-create-group`), does it create the GROUP and then the stream again. So a task
//! whose execution role may not create the group it was asked to create launches nothing: no
//! container starts, the group is never created, the task never reaches `RUNNING`, and it is
//! reported with `Task$stopCode` `TaskFailedToStart` (`smithy_query(query="shape",
//! shape="TaskStopCode")`) and a `Task$stoppedReason` of the agent's
//! `ResourceInitializationError` class carrying the refusal CloudWatch Logs gave for
//! `logs:CreateLogGroup` — naming the session the role was assumed in and the group ARN the
//! creation addressed. A task whose role MAY create it runs, and the group is there
//! afterwards.
//!
//! The rows span what the rule generalises over: WHY the group creation is refused (nothing
//! in force on the execution role allows the action, a statement on it explicitly denies it,
//! the grant names another group), and the configurations the rule does NOT decide against (a
//! role that may create the group; a group that is already there, which the driver never
//! creates, so nothing is asked about the role's permission to; a configuration that never
//! asked the driver to create one, which is refused on the stream creation instead; and a
//! revision naming no execution role, which states no principal for a policy to be read
//! against).
//!
//! It lives beside the router because the three stores the rule spans are three services':
//! the log group is CloudWatch Logs' own resource, the execution role and the documents in
//! force on it are IAM's, and the cluster and its tasks are Amazon ECS's. Every fact is
//! created through the owning service's own API, and every value asserted is read back
//! through `DescribeTasks` and `DescribeLogGroups`.

use std::collections::BTreeSet;
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

/// The cluster every task below runs in.
const CLUSTER: &str = "createlogroup";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image every container names. Nothing is fetched from it.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The `awslogs-stream-prefix` every configuration names.
const STREAM_PREFIX: &str = "ecs";

/// The program every container runs: it writes one line and exits, so a task that WAS
/// launched stops for the work it did rather than being waited on.
const CONTAINER_PROGRAM: &str = "echo ready";

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

/// `Task$lastStatus` of a task that is running its containers (`smithy_query(query="shape",
/// shape="Task")`, member `lastStatus`: "The last known status for the task").
const RUNNING: &str = "RUNNING";

/// How the agent's account of a logger it could not validate OPENS and CLOSES, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass. The step the agent retried is the stream creation, whichever of the calls that
/// creation takes is the one that failed.
const LOGGER_FAILURE_OPENING: &str =
    "ResourceInitializationError: failed to validate logger args: create stream has been \
     retried 1 times: failed to create Cloudwatch log ";
const LOGGER_FAILURE_CLOSING: &str = ": exceeded maximum number of attempts, 1";

/// The sentence CloudWatch Logs refuses a request for a log group this account does not hold
/// with (`ResourceNotFoundException`, "The specified resource does not exist.", worded for the
/// resource kind), and the class an authorization refusal carries.
const RESOURCE_NOT_FOUND: &str =
    "ResourceNotFoundException: The specified log group does not exist.";
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The reason clause IAM states for a grant nothing in force allows, and the one it states
/// for an action a document in force denies.
const NO_POLICY_ALLOWS: &str =
    "because no identity-based policy allows the logs:CreateLogGroup action";
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// A log group name no configuration below delivers into: the group a grant scoped AWAY from
/// the one a row names is written for.
const SCOPED_AWAY_GROUP: &str = "/emulator/ecs/create-group-elsewhere";

/// How often a poll re-reads the emulator, and how long one keeps trying before it gives up.
/// Generous: a task that fails to launch is stopped at the instant it would otherwise have
/// started, one that runs is stopped when the work it was placed to do is over, and this suite
/// runs beside every other test of the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(60);
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// What a row's execution role is allowed, as one identity-based document of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No execution role at all: the revision names none.
    NoRole,
    /// The driver's delivery actions AND the group creation, on every resource.
    MayCreateGroup,
    /// The delivery actions alone: nothing in force allows the group creation.
    DeliveryOnly,
    /// The delivery actions, with the group creation explicitly denied.
    CreateGroupDenied,
    /// The delivery actions everywhere, with the group creation allowed on ANOTHER group — so
    /// the group this row names is outside what the role may create.
    MayCreateAnotherGroup,
}

/// Which of IAM's rules withheld a refused group creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    /// Nothing in force allows the action.
    NothingAllows,
    /// A document in force denies it.
    Denies,
}

/// What a row's launch produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The logger is initialised, so the task runs and the group it delivers into is there.
    Launches,
    /// The role may not create the group the driver was asked to create: the launch is
    /// refused, with this reason clause after the group the creation addressed.
    GroupRefused(Clause),
    /// There is no group and the driver was not asked to create one: the launch is refused on
    /// the stream creation, in CloudWatch Logs' own words for a group it does not hold.
    NoSuchGroup,
}

/// One row: the configuration a container delivers through, whether the group is already
/// there, what the execution role may do, and the launch that produces.
struct Row {
    case: &'static str,
    /// Whether the configuration asks the driver to create the group
    /// (`awslogs-create-group`).
    create_group: bool,
    /// Whether a client created the group before the task was launched.
    group_exists: bool,
    grant: Grant,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        case: "the driver creates the group it was asked to create, as a role that may",
        create_group: true,
        group_exists: false,
        grant: Grant::MayCreateGroup,
        outcome: Outcome::Launches,
    },
    Row {
        case: "no policy in force on the execution role allows the group creation",
        create_group: true,
        group_exists: false,
        grant: Grant::DeliveryOnly,
        outcome: Outcome::GroupRefused(Clause::NothingAllows),
    },
    Row {
        case: "a statement on the execution role denies the group creation",
        create_group: true,
        group_exists: false,
        grant: Grant::CreateGroupDenied,
        outcome: Outcome::GroupRefused(Clause::Denies),
    },
    Row {
        case: "the role's grant to create a group names another group",
        create_group: true,
        group_exists: false,
        grant: Grant::MayCreateAnotherGroup,
        outcome: Outcome::GroupRefused(Clause::NothingAllows),
    },
    Row {
        // The driver creates the stream first and only creates a group when that finds none,
        // so a group that is already there is never created and the role's permission to
        // create one is never asked about.
        case: "the group the driver was asked to create is already there",
        create_group: true,
        group_exists: true,
        grant: Grant::DeliveryOnly,
        outcome: Outcome::Launches,
    },
    Row {
        // A grant to create the group does not stand in for a group: a driver that was not
        // asked to create one finds none, which is the stream creation's own refusal.
        case: "the driver was not asked to create the group it delivers into",
        create_group: false,
        group_exists: false,
        grant: Grant::MayCreateGroup,
        outcome: Outcome::NoSuchGroup,
    },
    Row {
        // A revision naming no execution role states no principal for a policy to be read
        // against, so nothing is decided about it and the launch goes ahead.
        case: "the revision names no execution role at all",
        create_group: true,
        group_exists: false,
        grant: Grant::NoRole,
        outcome: Outcome::Launches,
    },
];

/// The log group one row's configuration names: one of its own, so no row's delivery meets
/// another's.
fn group_of(index: usize) -> String {
    format!("/emulator/ecs/create-group-{index}")
}

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

/// One statement of an identity-based policy: `effect` on `actions`, for `resource`.
fn statement(effect: &str, actions: &[&str], resource: &str) -> Value {
    json!({
        "Effect": effect,
        "Action": actions,
        "Resource": resource,
    })
}

/// The identity-based document one row's execution role carries.
fn policy_document(grant: Grant) -> String {
    let delivery = [CREATE_LOG_STREAM, PUT_LOG_EVENTS];
    let everything = [CREATE_LOG_STREAM, PUT_LOG_EVENTS, CREATE_LOG_GROUP];
    let statements = match grant {
        // No role is created for a row that names none, so this arm describes no document.
        Grant::NoRole => vec![],
        Grant::MayCreateGroup => vec![statement("Allow", &everything, "*")],
        Grant::DeliveryOnly => vec![statement("Allow", &delivery, "*")],
        Grant::CreateGroupDenied => vec![
            statement("Allow", &everything, "*"),
            statement("Deny", &[CREATE_LOG_GROUP], "*"),
        ],
        Grant::MayCreateAnotherGroup => vec![
            statement("Allow", &delivery, "*"),
            statement("Allow", &[CREATE_LOG_GROUP], &group_arn(SCOPED_AWAY_GROUP)),
        ],
    };
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// Create one row's execution role through IAM's own API — a trust policy that lets Amazon
/// ECS assume it, and the inline document the row's grant calls for — and answer its ARN.
/// `None` for a row that names no role.
async fn execution_role(
    router: &MultiServiceRouter,
    index: usize,
    row: &Row,
) -> Option<(String, String)> {
    if row.grant == Grant::NoRole {
        return None;
    }
    let name = format!("createGroupRole{index}");
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
            ("RoleName", name.clone()),
            ("AssumeRolePolicyDocument", trust),
        ],
        row.case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.clone()),
            ("PolicyName", "logger".to_string()),
            ("PolicyDocument", policy_document(row.grant)),
        ],
        row.case,
    )
    .await;
    let arn = format!("arn:aws:iam::{ACCOUNT}:role/{name}");
    Some((name, arn))
}

/// An EC2 instance identity document, as the agent reads it from the instance metadata
/// service: a registration presents it with a signature beside it.
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

/// Register one revision of `family` whose single container delivers through the `awslogs`
/// driver into `group` — having the driver create that group when the row asks it to — under
/// `role_arn`, and answer its ARN.
async fn register_definition(
    router: &MultiServiceRouter,
    family: &str,
    row: &Row,
    group: &str,
    role_arn: Option<&str>,
) -> String {
    let mut options = json!({
        "awslogs-group": group,
        "awslogs-region": REGION,
        "awslogs-stream-prefix": STREAM_PREFIX,
    });
    if row.create_group {
        options["awslogs-create-group"] = json!("true");
    }
    let mut request = json!({
        "family": family,
        "networkMode": "bridge",
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "command": ["sh", "-c", CONTAINER_PROGRAM],
            "logConfiguration": { "logDriver": "awslogs", "options": options },
        }],
    });
    if let Some(role_arn) = role_arn {
        request["executionRoleArn"] = json!(role_arn);
    }
    let registered = ecs(router, "RegisterTaskDefinition", request, row.case).await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the revision's ARN, got {registered}",
                row.case
            )
        })
        .to_string()
}

/// One task of the cluster, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        case,
    )
    .await;
    described["tasks"][0].clone()
}

/// Whether this account holds a log group named `group`, read back through
/// `DescribeLogGroups`.
async fn holds_group(router: &MultiServiceRouter, group: &str) -> bool {
    let (status, body) = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": group }),
    )
    .await;
    if status != 200 {
        return false;
    }
    body["logGroups"].as_array().is_some_and(|groups| {
        groups
            .iter()
            .any(|held| held["logGroupName"].as_str() == Some(group))
    })
}

/// The task id one task ARN names: the last segment of it.
fn task_id_of(task_arn: &str) -> &str {
    task_arn.rsplit('/').next().unwrap_or(task_arn)
}

/// The ARN of the session Amazon ECS initialises the logger in: an assumed-role session of
/// the task's execution role, named after the task it is for.
fn session_arn(role_name: &str, task_id: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{task_id}")
}

/// A log group's own ARN, which is the resource a `CreateLogGroup` addresses.
fn group_arn(group: &str) -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{group}")
}

/// The WHOLE `stoppedReason` one row's refused launch must carry, composed the way the agent
/// composes it: the fixed opening, the call of the driver's own creation that failed, the
/// answer CloudWatch Logs gave for THIS task, and the fixed close. Every variable part comes
/// from the request and from the emulator's own records — the session the execution role was
/// assumed in for this task, the action the driver performs and the group ARN it addressed.
fn expected_reason(row: &Row, role_name: Option<&str>, group: &str, task_id: &str) -> String {
    let (resource_kind, failure) = match row.outcome {
        Outcome::Launches => unreachable!("a row that launches has no stopped reason"),
        Outcome::NoSuchGroup => ("stream", RESOURCE_NOT_FOUND.to_string()),
        Outcome::GroupRefused(clause) => {
            let reason = match clause {
                Clause::NothingAllows => NO_POLICY_ALLOWS,
                Clause::Denies => EXPLICIT_DENY,
            };
            let session = session_arn(
                role_name.expect("a refused group creation is made as a role"),
                task_id,
            );
            (
                "group",
                format!(
                    "{ACCESS_DENIED}: User: {session} is not authorized to perform: \
                     {CREATE_LOG_GROUP} on resource: {} {reason}",
                    group_arn(group)
                ),
            )
        }
    };
    format!("{LOGGER_FAILURE_OPENING}{resource_kind}: {failure}{LOGGER_FAILURE_CLOSING}")
}

/// What one row's launch left to read back.
struct Launched<'row> {
    row: &'row Row,
    /// The log group the row's configuration names.
    group: String,
    /// The NAME of the execution role the revision names, which a refusal names the session
    /// of; `None` for a row that names no role.
    role_name: Option<String>,
    task_arn: String,
}

#[tokio::test]
async fn a_task_whose_driver_may_not_create_its_log_group_fails_to_start() {
    let router = emulator();
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "the cluster every row's task runs in",
    )
    .await;
    // These tasks run on capacity registered to the cluster: "The EC2 launch type runs your
    // tasks on Amazon EC2 instances registered to your cluster."
    ecs(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity_document("i-0c0ffee0c0ffee0c0"),
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
        "the capacity every row's task is placed on",
    )
    .await;

    // Every row's task is launched FIRST and polled afterwards: the launches are decided in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<Launched<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let group = group_of(index);
        if row.group_exists {
            let (status, created) =
                logs(&router, "CreateLogGroup", json!({ "logGroupName": &group })).await;
            assert_eq!(
                status, 200,
                "case {:?}: the group a client creates must be created, got {status} with \
                 {created}",
                row.case
            );
        }
        let role = execution_role(&router, index, row).await;
        let family = format!("creategroup{index}");
        let definition = register_definition(
            &router,
            &family,
            row,
            &group,
            role.as_ref().map(|(_, arn)| arn.as_str()),
        )
        .await;
        let run = ecs(
            &router,
            "RunTask",
            json!({ "cluster": CLUSTER, "taskDefinition": definition }),
            row.case,
        )
        .await;
        let task_arn = run["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: expected a task back, got {run}", row.case))
            .to_string();
        launched.push(Launched {
            row,
            group,
            role_name: role.map(|(name, _)| name),
            task_arn,
        });
    }

    // Every task is read again until it carries a stop code, and every `lastStatus` seen on
    // the way is kept: a task whose logger could not be initialised never ran, so `RUNNING`
    // is never among them.
    let mut observed: Vec<BTreeSet<String>> = vec![BTreeSet::new(); launched.len()];
    let mut stopped: Vec<Option<Value>> = vec![None; launched.len()];
    let mut pending: Vec<usize> = (0..launched.len()).collect();
    let deadline = Instant::now() + POLL_BUDGET;
    while !pending.is_empty() {
        let mut still = Vec::new();
        for index in pending {
            let entry = &launched[index];
            let task = describe_task(&router, &entry.task_arn, entry.row.case).await;
            observed[index].insert(task["lastStatus"].as_str().unwrap_or_default().to_string());
            if task["stopCode"].as_str().is_some() {
                stopped[index] = Some(task);
            } else {
                still.push(index);
            }
        }
        pending = still;
        if pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected every task to stop with a stop code within {POLL_BUDGET:?}, still \
             waiting on {pending:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, entry) in launched.iter().enumerate() {
        let case = entry.row.case;
        let task = stopped[index]
            .as_ref()
            .unwrap_or_else(|| panic!("case {case:?}: expected a stopped task"));
        let statuses = &observed[index];
        if entry.row.outcome == Outcome::Launches {
            // The logger was initialised, so the task RAN: it stops for the work it did, not
            // for a resource it could not bring up, and the group it delivers into is there.
            assert_ne!(
                task["stopCode"].as_str(),
                Some(TASK_FAILED_TO_START),
                "case {case:?}: this task's logger can be initialised, so it must not fail to \
                 start, got {task}"
            );
            assert!(
                !task["stoppedReason"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("ResourceInitializationError"),
                "case {case:?}: expected no resource-initialisation failure, got {task}"
            );
            assert!(
                holds_group(&router, &entry.group).await,
                "case {case:?}: expected this account to hold the group {:?} the driver \
                 delivers into",
                entry.group
            );
            continue;
        }
        assert_eq!(
            task["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose logger could not be initialised never ran, got {task}"
        );
        let expected = expected_reason(
            entry.row,
            entry.role_name.as_deref(),
            &entry.group,
            task_id_of(&entry.task_arn),
        );
        assert_eq!(
            task["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {task}"
        );
        assert!(
            !statuses.contains(RUNNING),
            "case {case:?}: a task whose logger could not be initialised never reaches \
             {RUNNING}, observed {statuses:?}"
        );
        // Nothing ran: no container of such a task claims an exit code, and the group the
        // driver was asked to create was never created.
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
        assert!(
            !holds_group(&router, &entry.group).await,
            "case {case:?}: no log group is created for a logger that could not be \
             initialised, got {:?} held",
            entry.group
        );
    }
}
