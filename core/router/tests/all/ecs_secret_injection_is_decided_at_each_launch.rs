// SPDX-License-Identifier: CC0-1.0

//! Whether a container's `secrets` can be injected is decided at EVERY LAUNCH, against the
//! facts in force at that instant — driven end to end with Amazon ECS, Systems Manager, KMS,
//! IAM and CloudWatch Logs on ONE shared engine, so every parameter, key, role, policy and log
//! group is created through the OWNING service's own API.
//!
//! RULE: Amazon ECS resolves every `secrets` `valueFrom` of a task's containers BEFORE any
//! container of the task exists, as the task definition's EXECUTION role — "the task execution
//! role that grants the Amazon ECS container agent permission to make Amazon Web Services API
//! calls on your behalf" (`TaskDefinition$executionRoleArn`) — and reads a Parameter Store
//! reference WITH DECRYPTION, because what the container is given is the parameter's value and
//! not its ciphertext. So the `ssm:GetParameters` and `kms:Decrypt` documents in force on THAT
//! role, and the KEY STATE of the key the value is sealed under, decide the launch. Nothing of
//! that verdict is carried over: every launch is decided again from the state of the moment, so
//! one revision's launch fails at start while the key it needs is unusable and runs once that
//! key is usable again, and a revision whose grants sit on the TASK role — the role the
//! container's own calls are signed with, which the retrieval does not consult — never runs
//! however often it is launched.
//!
//! A launch whose retrieval would be refused launches NOTHING: the task never reports
//! `RUNNING`, it reaches `STOPPED` with `Task$stopCode` `TaskFailedToStart` and a
//! `stoppedReason` of the agent's `ResourceInitializationError` class naming the Parameter Store
//! fetch, the refusal and the EXECUTION role's own session, its container reports no `exitCode`
//! — nothing ran to produce one — and the log group its configuration names receives no stream.
//! A launch whose retrieval is served runs and the container's program sees the value Parameter
//! Store holds.
//!
//! Why this suite lives beside the router: every verdict belongs to another service. Systems
//! Manager holds the parameter, KMS holds the key, its key state and its policy, IAM holds the
//! roles and the documents in force on them, and CloudWatch Logs holds what the container
//! wrote — so all five have to agree for the rule to hold.
//!
//! What this pins beside the suite at `core/router/tests/all/ecs_parameter_store_secret_injection.rs`,
//! which spans the shapes of one launch each in its own account: the verdict is taken PER LAUNCH
//! rather than once per revision (the rows differ along the key state each launch is made under, in both
//! directions), and the launches that run are told apart from the launches that fail at start
//! within ONE account holding ONE parameter sealed under ONE key (the rows differ along which
//! role carries the grants) — so a role that may retrieve the value cannot be what makes a role
//! that may not look healthy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account every generated emulator presents.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every request below is signed for, and so the Region every task runs in.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const SSM_TARGET_PREFIX: &str = "AmazonSSM";
const KMS_TARGET_PREFIX: &str = "TrentService";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The network every task below is placed in: one VPC with one subnet, and a main route table
/// that carries traffic off the VPC, so the image fetch of a task with a public address decides
/// nothing in this suite.
const VPC: &str = "vpc-0a1b2c3d4e5f6071a";
const SUBNET: &str = "subnet-0a1b2c3d4e5f6071a";

/// The cluster every task below runs in.
const CLUSTER: &str = "perlaunch";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from: a reference naming a registry this emulator answers
/// nothing for, so nothing of the image is fetched and what runs is the `command` the revision
/// states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The log group every container delivers to, and the `awslogs-stream-prefix` every task
/// definition configures.
const LOG_GROUP: &str = "/emulator/ecs/per-launch-secrets";
const STREAM_PREFIX: &str = "ecs";

/// The ONE parameter both tables reference, the value it holds, and the environment variable
/// the `secrets` entry exposes it under.
const PARAMETER: &str = "/emulator/ecs/per-launch/password";
const SECRET_VALUE: &str = "per-launch-s3cr3t";
const VARIABLE: &str = "DATABASE_PASSWORD";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all, and of one
/// whose essential container ran and exited
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The fixed text of the `stoppedReason` the agent words for the execution resources it could
/// not fetch from Parameter Store, spelled out here rather than imported so a wrong wording in
/// the implementation cannot make a test pass.
const RESOURCE_INITIALIZATION_ERROR: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve secrets from ssm: service call has been retried 1 \
     time(s): ";

/// The IAM actions the retrieval of a `SecureString` parameter needs: Parameter Store's own
/// batch read, which the agent retrieves a task's references through, and the decrypt of the key
/// the value is sealed under.
const GET_PARAMETERS: &str = "ssm:GetParameters";
const DECRYPT: &str = "kms:Decrypt";

/// The clause IAM's refusal carries when no document in force allows the action.
fn no_identity_policy_clause(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// How the agent reports a reference the batch read it makes produced no value for: a
/// `SecureString` whose key cannot run the decrypt is one of those names, because that read
/// reports every value it could not produce that way.
const INVALID_PARAMETERS: &str = "invalid ssm parameters";

/// The authorization refusal class a withheld grant is reported under.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. The wait ends on the CONDITION; the deadline is only there so a move
/// that never happens fails the test instead of hanging it, and it is generous so a loaded
/// machine does not fail a row that is merely slow.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// An emulator carrying the five services this rule spans, over ONE shared engine, mounted the
/// way the production router mounts them, with the account's network already held.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // A Fargate task is allocated its own elastic network interface in a subnet of the account,
    // and reads the path off that VPC before it starts.
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], &format!("{REGION}a"));
    crate::ec2_network::register_internet_route(&engine, VPC);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ssm_amazonssm::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
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
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsJson1_1 call to the service `signing_name` serves, answered with its HTTP status and
/// parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One call that has to succeed, answered with its body.
async fn ok(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let (status, answer) = rpc(router, signing_name, target_prefix, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {signing_name} {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Amazon ECS call that has to succeed.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    ok(router, "ecs", ECS_TARGET_PREFIX, operation, body, case).await
}

/// One Systems Manager call that has to succeed.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    ok(router, "ssm", SSM_TARGET_PREFIX, operation, body, case).await
}

/// One KMS call that has to succeed.
async fn kms(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    ok(router, "kms", KMS_TARGET_PREFIX, operation, body, case).await
}

/// One awsQuery call to IAM that has to succeed, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
    case: &str,
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: iam {action} must answer 200, got {status} with {text}"
    );
    text
}

/// A string member of a body, which has to be there.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// The ARN of the parameter this suite writes: a hierarchical name keeps its whole path after
/// the `parameter/` prefix.
fn parameter_arn() -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{PARAMETER}")
}

/// A trust policy that lets this service's own service principal assume the role, which is what
/// Amazon ECS has to do before it can act under it.
fn ecs_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ["ecs-tasks.amazonaws.com"] },
            "Action": ["sts:AssumeRole"],
        }],
    })
    .to_string()
}

/// An `Allow` of the Parameter Store batch read on `resource`.
fn read_statement(resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": [GET_PARAMETERS], "Resource": [resource] })
}

/// An `Allow` of the decrypt on `resource`.
fn decrypt_statement(resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": [DECRYPT], "Resource": [resource] })
}

/// Create the IAM role `name` trusting this service and carrying `statements` as its inline
/// policy — through IAM's own API — and answer its ARN.
///
/// Every role also carries the two actions the `awslogs` driver performs under the execution
/// role, so what a launch is decided by is the parameter grant the role was given and not a
/// grant the delivery needs.
async fn role(
    router: &MultiServiceRouter,
    name: &str,
    statements: &[Value],
    case: &str,
) -> String {
    iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &ecs_trust())],
        case,
    )
    .await;
    let mut all = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    all.extend(statements.iter().cloned());
    let document = json!({ "Version": "2012-10-17", "Statement": all }).to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "retrieve-parameters"),
            ("PolicyDocument", &document),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The account both tables launch into: the cluster, the log group, a customer managed key
/// created through KMS's own API, and the one `SecureString` parameter sealed under it, written
/// through Systems Manager's own API.
struct Account {
    router: MultiServiceRouter,
    /// The ARN of the key the parameter's value is sealed under, which a refusal of the decrypt
    /// names.
    key_arn: String,
}

/// Stand up [`Account`].
async fn account(case: &str) -> Account {
    let router = emulator();
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER }), case).await;
    ok(
        &router,
        "logs",
        LOGS_TARGET_PREFIX,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
        case,
    )
    .await;
    // A create that names no policy gets the account-root policy KMS writes, which DELEGATES
    // the decision to the calling principal's own identity-based policies.
    let created = kms(
        &router,
        "CreateKey",
        json!({ "Description": "the key this suite's parameter is sealed under" }),
        case,
    )
    .await;
    let key_arn = string(&created["KeyMetadata"], "Arn", case);
    ssm(
        &router,
        "PutParameter",
        json!({
            "Name": PARAMETER,
            "Value": SECRET_VALUE,
            "Type": "SecureString",
            "KeyId": key_arn,
        }),
        case,
    )
    .await;
    Account { router, key_arn }
}

/// The key state one launch is made under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyState {
    /// A key that runs cryptographic operations.
    Enabled,
    /// A key KMS runs no cryptographic operation under, so nothing can open a value sealed
    /// with it.
    Disabled,
}

impl KeyState {
    /// `KeyMetadata$KeyState` as KMS reports it.
    fn reported(self) -> &'static str {
        match self {
            Self::Enabled => "Enabled",
            Self::Disabled => "Disabled",
        }
    }

    /// The operation that leaves a key in this state.
    fn operation(self) -> &'static str {
        match self {
            Self::Enabled => "EnableKey",
            Self::Disabled => "DisableKey",
        }
    }
}

/// Leave the key in `state` through KMS's OWN API, and confirm it through that service's own
/// read — the way a client confirms it before launching.
async fn put_key_in_state(
    router: &MultiServiceRouter,
    key_arn: &str,
    state: KeyState,
    case: &str,
) {
    kms(router, state.operation(), json!({ "KeyId": key_arn }), case).await;
    let described = kms(router, "DescribeKey", json!({ "KeyId": key_arn }), case).await;
    assert_eq!(
        described["KeyMetadata"]["KeyState"],
        json!(state.reported()),
        "case {case:?}: expected the key to report KeyState {:?}, got {described}",
        state.reported()
    );
}

/// Register one revision whose single container takes the parameter as a `secrets` entry and
/// echoes it, under `execution_role_arn` and `task_role_arn`.
async fn register_definition(
    router: &MultiServiceRouter,
    family: &str,
    execution_role_arn: &str,
    task_role_arn: &str,
    case: &str,
) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            // A documented Fargate task size: 256 CPU units, 512 MiB.
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role_arn,
            "taskRoleArn": task_role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "command": ["sh", "-c", format!("echo \"injected=${VARIABLE}\"")],
                "secrets": [{ "name": VARIABLE, "valueFrom": parameter_arn() }],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
        case,
    )
    .await;
}

/// Launch `family` once, answered with the ARN of the task that was placed.
async fn run_task(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let placed = ecs(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": { "subnets": [SUBNET], "assignPublicIp": "ENABLED" }
            },
        }),
        case,
    )
    .await;
    let tasks = placed["tasks"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {placed}"
    );
    string(&tasks[0], "taskArn", case)
}

/// The task id: the last segment of a task's ARN.
fn task_id_of(task_arn: &str) -> String {
    task_arn.rsplit('/').next().unwrap_or_default().to_string()
}

/// The session Amazon ECS retrieves one task's execution resources in: an assumed-role session
/// of the EXECUTION role named after the task it is for, which is the principal a refusal names.
fn session_arn(execution_role_name: &str, task_arn: &str) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{execution_role_name}/{}",
        task_id_of(task_arn)
    )
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming that driver's own
/// `awslogs-stream-prefix` documentation states.
fn stream_of(task_arn: &str) -> String {
    format!("{STREAM_PREFIX}/{CONTAINER}/{}", task_id_of(task_arn))
}

/// The task as `DescribeTasks` reports it now.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    body["tasks"][0].clone()
}

/// Look again until the task reports `STOPPED`, and answer that record with every status the
/// poll observed on the way — which is how a row says a task never reported `RUNNING`.
async fn poll_to_stopped(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
) -> (Value, Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let task = describe(router, task_arn, case).await;
        let status = string(&task, "lastStatus", case);
        if !seen.contains(&status) {
            seen.push(status.clone());
        }
        if status == STOPPED {
            return (task, seen);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {STOPPED}, it was still {status:?} after \
             {seen:?}: {task}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The lines CloudWatch Logs holds in the stream one task's container delivers to, oldest
/// first. An unknown stream reads as no lines, which is what a stream nothing was delivered to
/// holds.
async fn delivered(router: &MultiServiceRouter, task_arn: &str) -> Vec<String> {
    let (status, body) = rpc(
        router,
        "logs",
        LOGS_TARGET_PREFIX,
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

/// Look again until the stream this task's container delivers to holds a line.
async fn poll_for_output(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let lines = delivered(router, task_arn).await;
        if !lines.is_empty() {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a line in {}, the stream held none",
            stream_of(task_arn)
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The streams the log group holds for one task's container — none for a task whose container
/// never started.
async fn streams_of(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Vec<String> {
    let body = ok(
        router,
        "logs",
        LOGS_TARGET_PREFIX,
        "DescribeLogStreams",
        json!({ "logGroupName": LOG_GROUP, "logStreamNamePrefix": stream_of(task_arn) }),
        case,
    )
    .await;
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

/// What one launch of the rule produced, asserted as the shape of a launch that RAN or of one
/// that failed at start, and answered as the stopped record so a row can read the refusal it
/// carries.
async fn settled(
    router: &MultiServiceRouter,
    task_arn: &str,
    runs: bool,
    case: &str,
) -> Value {
    let (task, seen) = poll_to_stopped(router, task_arn, case).await;
    if runs {
        assert!(
            seen.contains(&RUNNING.to_string()),
            "case {case:?}: expected the task to report {RUNNING} on its way, saw {seen:?}"
        );
        assert_eq!(
            task["stopCode"],
            json!(ESSENTIAL_CONTAINER_EXITED),
            "case {case:?}: expected the stop of a task that ran, got {task}"
        );
        assert_eq!(
            task["containers"][0]["exitCode"],
            json!(0),
            "case {case:?}: expected exitCode 0, got {task}"
        );
        let lines = poll_for_output(router, task_arn, case).await;
        assert_eq!(
            lines,
            vec![format!("injected={SECRET_VALUE}")],
            "case {case:?}: expected the container to write the parameter's value to {}, got \
             {lines:?}",
            stream_of(task_arn)
        );
    } else {
        assert!(
            !seen.contains(&RUNNING.to_string()),
            "case {case:?}: a task whose secrets could not be retrieved never runs, saw {seen:?}"
        );
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
        // No container started, so none reports a status it exited with, and the driver creates
        // the stream when the container starts.
        assert_eq!(
            task["containers"][0]["exitCode"],
            Value::Null,
            "case {case:?}: a container that never ran reports no exitCode, got {task}"
        );
        let streams = streams_of(router, task_arn, case).await;
        assert!(
            streams.is_empty(),
            "case {case:?}: expected no stream in {LOG_GROUP} for this task, got {streams:?}"
        );
    }
    task
}

/// The launches ONE revision is put through, in order: the key state each is made under, and
/// whether the task runs. The rows differ along that state in BOTH directions, which is what
/// says the verdict is taken per launch rather than once per revision.
const KEY_STATE_ROWS: &[(&str, KeyState, bool)] = &[
    ("the first launch, with the key enabled", KeyState::Enabled, true),
    (
        "the same revision launched again once the key is disabled",
        KeyState::Disabled,
        false,
    ),
    (
        "the same revision launched again once that key is enabled",
        KeyState::Enabled,
        true,
    ),
    (
        "the same revision launched again once it is disabled a second time",
        KeyState::Disabled,
        false,
    ),
];

/// RULE, the key-state half: the key state of the key a `secrets` reference's value is sealed
/// under decides EACH LAUNCH of a revision at the instant that launch is made, in both
/// directions — so one revision whose execution role holds both grants runs while its key is
/// usable, fails at start while it is not, and runs again once it is, with the refusal of a
/// launch the key could not serve reported as a reference the batch read produced no value for.
#[tokio::test]
async fn the_key_state_decides_each_launch_of_one_revision() {
    let case = "one revision whose execution role holds both grants";
    let Account { router, key_arn } = account(case).await;
    let execution_role = "perlaunchexec";
    let execution_role_arn = role(
        &router,
        execution_role,
        &[read_statement(&parameter_arn()), decrypt_statement(&key_arn)],
        case,
    )
    .await;
    // The task role carries no grant of its own: it is not what the retrieval is made as.
    let task_role_arn = role(&router, "perlaunchtask", &[], case).await;
    let family = "perlaunchfamily";
    register_definition(&router, family, &execution_role_arn, &task_role_arn, case).await;

    for (case, state, runs) in KEY_STATE_ROWS {
        put_key_in_state(&router, &key_arn, *state, case).await;
        let task_arn = run_task(&router, family, case).await;
        let task = settled(&router, &task_arn, *runs, case).await;
        if *runs {
            continue;
        }
        // A role the key DOES permit is refused by the key's state, which the batch read the
        // agent makes reports among the names it produced no value for — by the reference the
        // revision carried, and with nothing of the key's own account of the state.
        let reason = string(&task, "stoppedReason", case);
        let expected = format!("{INVALID_PARAMETERS}: {}", parameter_arn());
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the reason to carry {expected:?}, got {reason:?}"
        );
        assert!(
            !reason.contains(ACCESS_DENIED),
            "case {case:?}: a role the key permits is not refused for want of permission, got \
             {reason:?}"
        );
    }
}

/// WHERE the grants the retrieval needs sit for one row of [`GRANT_SITE_ROWS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantSite {
    /// Both grants on the EXECUTION role: the role the retrieval is made as.
    Execution,
    /// The Parameter Store read on the EXECUTION role and the decrypt on the TASK role alone.
    DecryptOnTaskRole,
    /// Both grants on the TASK role alone — the role the container's own calls are signed with,
    /// which the retrieval does not consult.
    TaskRoleOnly,
}

impl GrantSite {
    /// `(the execution role's statements, the task role's statements)`.
    fn documents(self, parameter_arn: &str, key_arn: &str) -> (Vec<Value>, Vec<Value>) {
        let read = read_statement(parameter_arn);
        let decrypt = decrypt_statement(key_arn);
        match self {
            Self::Execution => (vec![read, decrypt], Vec::new()),
            Self::DecryptOnTaskRole => (vec![read.clone()], vec![read, decrypt]),
            Self::TaskRoleOnly => (Vec::new(), vec![read, decrypt]),
        }
    }
}

/// What the `stoppedReason` of a refused row has to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withheld {
    /// Nothing is withheld: the row runs.
    Nothing,
    /// No document in force on the execution role allows the Parameter Store read.
    ParameterRead,
    /// No document in force on it allows the decrypt of the key the value is sealed under.
    Decrypt,
}

/// The launches ONE account holding ONE parameter under ONE usable key is put through: which
/// role carries the grants, whether the task runs, and what the refusal of one that does not
/// has to name.
const GRANT_SITE_ROWS: &[(&str, GrantSite, bool, Withheld)] = &[
    (
        "both grants on the execution role",
        GrantSite::Execution,
        true,
        Withheld::Nothing,
    ),
    (
        "the Parameter Store read on the execution role and the decrypt on the task role",
        GrantSite::DecryptOnTaskRole,
        false,
        Withheld::Decrypt,
    ),
    (
        "both grants on the task role alone",
        GrantSite::TaskRoleOnly,
        false,
        Withheld::ParameterRead,
    ),
];

/// RULE, the role half: the retrieval is made as the EXECUTION role, so within one account
/// holding one parameter sealed under one usable key the launches that run and the launches that
/// fail at start are told apart by which role carries the grants alone — a grant that sits on
/// the TASK role authorizes nothing, and the refusal names the execution role's own session and
/// never the task role.
#[tokio::test]
async fn where_the_grants_sit_decides_whether_a_launch_runs() {
    let setup = "one account holding one parameter under one usable key";
    let Account { router, key_arn } = account(setup).await;
    let parameter_arn = parameter_arn();

    for (index, (case, site, runs, withheld)) in GRANT_SITE_ROWS.iter().enumerate() {
        let (execution_statements, task_statements) = site.documents(&parameter_arn, &key_arn);
        let execution_role = format!("grantsiteexec{index}");
        let task_role = format!("grantsitetask{index}");
        let execution_role_arn =
            role(&router, &execution_role, &execution_statements, case).await;
        let task_role_arn = role(&router, &task_role, &task_statements, case).await;
        let family = format!("grantsitefamily{index}");
        register_definition(&router, &family, &execution_role_arn, &task_role_arn, case).await;

        let task_arn = run_task(&router, &family, case).await;
        let task = settled(&router, &task_arn, *runs, case).await;
        let expected = match withheld {
            Withheld::Nothing => continue,
            Withheld::ParameterRead => format!(
                "User: {} is not authorized to perform: {GET_PARAMETERS} on resource: \
                 {parameter_arn} {}",
                session_arn(&execution_role, &task_arn),
                no_identity_policy_clause(GET_PARAMETERS)
            ),
            Withheld::Decrypt => format!(
                "User: {} is not authorized to perform: {DECRYPT} on resource: {key_arn} {}",
                session_arn(&execution_role, &task_arn),
                no_identity_policy_clause(DECRYPT)
            ),
        };
        let reason = string(&task, "stoppedReason", case);
        assert!(
            reason.contains(ACCESS_DENIED),
            "case {case:?}: expected the reason to name {ACCESS_DENIED}, got {reason:?}"
        );
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the reason to carry {expected:?}, got {reason:?}"
        );
        // The role the container's own calls are signed with is not what the retrieval was made
        // as, so it is not what the refusal names either.
        assert!(
            !reason.contains(&task_role_arn) && !reason.contains(task_role.as_str()),
            "case {case:?}: expected the refusal to name the execution role and not \
             {task_role_arn:?}, got {reason:?}"
        );
    }
}
