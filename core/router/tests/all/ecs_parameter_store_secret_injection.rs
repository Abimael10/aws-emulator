// SPDX-License-Identifier: CC0-1.0

//! A container definition's `secrets` entry that names a SYSTEMS MANAGER PARAMETER, driven
//! end to end with Amazon ECS, Systems Manager, KMS, IAM and CloudWatch Logs on ONE shared
//! engine — so every parameter, key, role, policy and log group is created through the
//! OWNING service's own API rather than written into a store by hand.
//!
//! RULE: Amazon ECS resolves every `secrets` `valueFrom` of a task's containers BEFORE any
//! container of the task exists, as the task definition's EXECUTION role — "the task
//! execution role that grants the Amazon ECS container agent permission to make Amazon Web
//! Services API calls on your behalf" (`TaskDefinition$executionRoleArn`) — and exposes what
//! it retrieved to the container's process as the environment variable the entry's `name`
//! spells. A Parameter Store reference is read WITH DECRYPTION, because what the container is
//! given is the parameter's value and not its ciphertext, so three things decide the
//! retrieval:
//!
//! * `ssm:GetParameters` for that EXECUTION role on the parameter the reference names;
//! * `kms:Decrypt` for that same role on the key a `SecureString` is sealed under, for a key
//!   the CUSTOMER manages; an Amazon Web Services managed key carries a policy AWS manages for
//!   the service that owns it, and a `String` parameter is not encrypted at all, so neither
//!   needs a grant of the account's own;
//! * the KEY STATE of that key, which decides the retrieval AFTER its policy does — KMS tells
//!   a principal the key does not permit nothing about the key's state, so a role holding no
//!   decrypt is refused the same way whether the key is usable or not, while a role the key
//!   permits has the value it would have been given produced by nothing.
//!
//! So a launch whose execution role may retrieve the value RUNS: the container's program sees
//! the value Parameter Store holds and writes it to the `awslogs` stream its log
//! configuration names. A launch whose retrieval would be refused launches NOTHING: the task
//! never reports `RUNNING`, it reaches `STOPPED` with `Task$stopCode` `TaskFailedToStart` and
//! a `stoppedReason` of the agent's `ResourceInitializationError` class naming the Parameter
//! Store fetch and the refusal, its container reports no `exitCode` — nothing ran to produce
//! one — and the log group its configuration names receives no stream.
//!
//! The grants on the TASK role are not what decides it: that role is what the container's own
//! calls are signed with, and the value is retrieved before the container exists.
//!
//! Why this suite lives beside the router: every verdict belongs to another service. Systems
//! Manager holds the parameter and its versions, KMS holds the key, its key state and its
//! policy, IAM holds the roles and the documents in force on them, and CloudWatch Logs holds
//! what the container wrote — so all five have to agree for the rule to hold, and each
//! crate's own suites can assert only its own half.
//!
//! The rows differ along what the rule generalises over: the parameter's TYPE and which key
//! seals it (a customer managed key, the Amazon Web Services managed key, no encryption at
//! all), the FORM of the reference (the parameter's full ARN, its name, a name carrying a
//! version selector), WHICH member references it (a container's `secrets`, a log
//! configuration's `secretOptions`), WHERE the grants sit (the execution role, the task role,
//! neither), how a statement is scoped (the parameter's own ARN, a wildcard, an `Allow` beside
//! an explicit `Deny`), the key's STATE, and whether the account holds the parameter at all.

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
/// that carries traffic off the VPC, so the image fetch of a task with a public address
/// decides nothing in this suite.
const VPC: &str = "vpc-0a1b2c3d4e5f60718";
const SUBNET: &str = "subnet-0a1b2c3d4e5f60718";

/// The cluster every task below runs in.
const CLUSTER: &str = "parameterstore";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from: a reference naming a registry this emulator answers
/// nothing for, so nothing of the image is fetched and what runs is the `command` the
/// revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The log group every container delivers to, and the `awslogs-stream-prefix` every task
/// definition configures.
const LOG_GROUP: &str = "/emulator/ecs/parameter-store-secrets";
const STREAM_PREFIX: &str = "ecs";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The fixed text of the `stoppedReason` the agent words for the execution resources it could
/// not fetch from Parameter Store, spelled out here rather than imported so a wrong wording in
/// the implementation cannot make a test pass.
const RESOURCE_INITIALIZATION_ERROR: &str =
    "ResourceInitializationError: unable to pull secrets or registry auth: execution resource \
     retrieval failed: unable to retrieve secrets from ssm: service call has been retried 1 \
     time(s): ";

/// The IAM actions the retrieval of a `SecureString` parameter needs: Parameter Store's own
/// batch read, which the agent retrieves a task's references through, and the decrypt of the
/// key the value is sealed under.
const GET_PARAMETERS: &str = "ssm:GetParameters";
const DECRYPT: &str = "kms:Decrypt";

/// The clause IAM's refusal carries when no document in force allows the action, and the one
/// it carries when a document denies it outright.
fn no_identity_policy_clause(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}
const EXPLICIT_DENY_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. The wait ends on the CONDITION; the deadline is only there so a move
/// that never happens fails the test instead of hanging it, and it is generous so a loaded
/// machine does not fail a row that is merely slow.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// An emulator carrying the five services this rule spans, over ONE shared engine, mounted
/// the way the production router mounts them, with the account's network already held.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // A Fargate task is allocated its own elastic network interface in a subnet of the
    // account, and reads the path off that VPC before it starts.
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

/// WHICH key seals the parameter's value, which is what decides whether the retrieval needs a
/// grant on a key at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sealing {
    /// `Type=SecureString` with `KeyId` naming a key the account created: a customer managed
    /// key, whose own policy and the role's documents decide the decrypt.
    CustomerKey,
    /// `Type=SecureString` naming no `KeyId`, so the value is sealed under the Amazon Web
    /// Services managed key of this service, whose policy AWS manages.
    AwsManagedKey,
    /// `Type=String`: "This flag is ignored for String and StringList parameter types"
    /// (`GetParameterRequest$WithDecryption`), so no key decides the read.
    Plaintext,
}

/// WHERE the grants the retrieval needs sit, and how their statements are scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Both grants on the EXECUTION role, each scoped to the one resource it is about.
    Execution,
    /// Both on the EXECUTION role, scoped to every resource — how AWS's own task-execution
    /// policy states a grant that is not narrowed to one parameter.
    ExecutionWildcard,
    /// The Parameter Store read on the EXECUTION role, with no grant on the key.
    ExecutionWithoutDecrypt,
    /// Both grants, with an explicit `Deny` of the read beside the `Allow`.
    ExecutionWithDeny,
    /// Both grants on the TASK role alone — the misconfiguration the rule is about.
    TaskRoleOnly,
    /// On neither role.
    Neither,
}

impl Grant {
    /// The statements each role carries under this grant, beside the log-delivery grant every
    /// execution role needs: `(the execution role's, the task role's)`.
    fn documents(self, parameter_arn: &str, key_arn: &str) -> (Vec<Value>, Vec<Value>) {
        let read = |resource: &str| {
            json!({ "Effect": "Allow", "Action": [GET_PARAMETERS], "Resource": [resource] })
        };
        let decrypt = |resource: &str| {
            json!({ "Effect": "Allow", "Action": [DECRYPT], "Resource": [resource] })
        };
        let both = vec![read(parameter_arn), decrypt(key_arn)];
        match self {
            Self::Execution => (both, Vec::new()),
            Self::ExecutionWildcard => (vec![read("*"), decrypt("*")], Vec::new()),
            Self::ExecutionWithoutDecrypt => (vec![read(parameter_arn)], Vec::new()),
            Self::ExecutionWithDeny => (
                vec![
                    read(parameter_arn),
                    decrypt(key_arn),
                    json!({
                        "Effect": "Deny",
                        "Action": [GET_PARAMETERS],
                        "Resource": [parameter_arn],
                    }),
                ],
                Vec::new(),
            ),
            Self::TaskRoleOnly => (Vec::new(), both),
            Self::Neither => (Vec::new(), Vec::new()),
        }
    }
}

/// WHICH member of the revision references the parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    /// `ContainerDefinition$secrets`: the value is exposed to the container's process as an
    /// environment variable.
    ContainerSecrets,
    /// `LogConfiguration$secretOptions`: "the secrets to pass to the log configuration",
    /// retrieved under the same role before the container starts.
    LogSecretOptions,
}

/// HOW the `valueFrom` spells the parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The parameter's full ARN.
    Arn,
    /// Its name — "if the SSM Parameter Store parameter exists in the same Region as the task
    /// you're launching, then you can use either the full ARN or name of the parameter"
    /// (`ContainerDefinition$secrets`).
    Name,
}

/// One row of either table.
struct Row {
    case: &'static str,
    sealing: Sealing,
    grant: Grant,
    member: Member,
    form: Form,
    /// The environment variable, or log-driver option, the entry exposes the value under.
    variable: &'static str,
    /// The value `PutParameter` stores.
    value: &'static str,
    /// Whether the account holds the parameter at all.
    held: bool,
    /// What is written after the parameter in the `valueFrom`: the `:selector` naming a
    /// version or a label, empty for a reference to the latest version.
    selector: &'static str,
    /// Whether the key the value is sealed under is DISABLED before the task is launched.
    key_disabled: bool,
}

/// Everything one row's launch produced, for the assertions to read back through.
struct Launched {
    router: MultiServiceRouter,
    task_arn: String,
    /// The session Amazon ECS retrieves the task's resources in: an assumed-role session of
    /// the EXECUTION role named after the task it is for, which is the principal a refusal
    /// names.
    session_arn: String,
    /// The ARN of the parameter the reference names, which a refusal of the read names.
    parameter_arn: String,
    /// The ARN of the key a `SecureString`'s value is sealed under, which a refusal of the
    /// decrypt names; empty for a row whose value no customer managed key seals.
    key_arn: String,
    /// The reference the revision carried, as a refusal that resolved it to nothing quotes it
    /// back.
    reference: String,
}

/// The parameter one row writes, named after the row so no two rows share one.
fn parameter_name(index: usize) -> String {
    format!("/emulator/ecs/injected-{index}")
}

/// The ARN of the parameter `name` names in [`REGION`]: a hierarchical name keeps its whole
/// path after the `parameter/` prefix.
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{}",
        if name.starts_with('/') {
            name.to_string()
        } else {
            format!("/{name}")
        }
    )
}

/// A trust policy that lets this service's own service principal assume the role, which is
/// what Amazon ECS has to do before it can act under it.
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

/// Create the IAM role `name` trusting this service and carrying `statements` as its inline
/// policy — through IAM's own API — and answer its ARN.
///
/// Every role also carries the two actions the `awslogs` driver performs under the execution
/// role, so what a row is decided by is the parameter grant it was given and not a grant the
/// delivery needs.
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

/// Run one row's scenario up to and including its `RunTask`.
async fn launch(row: &Row, index: usize) -> Launched {
    let case = row.case;
    let router = emulator();
    ecs(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        case,
    )
    .await;
    ok(
        &router,
        "logs",
        LOGS_TARGET_PREFIX,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
        case,
    )
    .await;

    // The key a `SecureString` is sealed under, created through KMS's OWN API: a create that
    // names no policy gets the account-root policy KMS writes, which DELEGATES the decision
    // to the calling principal's own identity-based policies.
    let key_arn = if row.sealing == Sealing::CustomerKey {
        let created = kms(
            &router,
            "CreateKey",
            json!({ "Description": "the key this row's parameter is sealed under" }),
            case,
        )
        .await;
        string(&created["KeyMetadata"], "Arn", case)
    } else {
        String::new()
    };

    // The parameter itself, written through Systems Manager's OWN API.
    let name = parameter_name(index);
    if row.held {
        let mut request = json!({
            "Name": name,
            "Value": row.value,
            "Type": match row.sealing {
                Sealing::Plaintext => "String",
                _ => "SecureString",
            },
        });
        if row.sealing == Sealing::CustomerKey {
            request["KeyId"] = json!(key_arn);
        }
        ssm(&router, "PutParameter", request, case).await;
    }
    // A key whose state no longer runs a cryptographic operation, disabled through KMS's own
    // API after the value was sealed under it — and confirmed through that service's own read,
    // the way a client confirms it.
    if row.key_disabled {
        kms(&router, "DisableKey", json!({ "KeyId": key_arn }), case).await;
        let described = kms(&router, "DescribeKey", json!({ "KeyId": key_arn }), case).await;
        assert_eq!(
            described["KeyMetadata"]["KeyState"],
            json!("Disabled"),
            "case {case:?}: DisableKey must leave the key disabled, got {described}"
        );
    }

    let arn = parameter_arn(&name);
    let (execution_statements, task_statements) = row.grant.documents(&arn, &key_arn);
    let execution_role_name = format!("paramexec{index}");
    let execution_role_arn = role(&router, &execution_role_name, &execution_statements, case).await;
    let task_role_arn = role(&router, &format!("paramtask{index}"), &task_statements, case).await;

    let reference = format!(
        "{}{}",
        match row.form {
            Form::Arn => arn.clone(),
            Form::Name => name.clone(),
        },
        row.selector
    );
    let entry = json!({ "name": row.variable, "valueFrom": reference });
    let mut logging = json!({
        "logDriver": "awslogs",
        "options": {
            "awslogs-group": LOG_GROUP,
            "awslogs-region": REGION,
            "awslogs-stream-prefix": STREAM_PREFIX,
        },
    });
    let variable = row.variable;
    let mut container = json!({
        "name": CONTAINER,
        "image": IMAGE,
        "essential": true,
        "command": ["sh", "-c", format!("echo \"injected=${variable}\"")],
    });
    match row.member {
        Member::ContainerSecrets => container["secrets"] = json!([entry]),
        Member::LogSecretOptions => logging["secretOptions"] = json!([entry]),
    }
    container["logConfiguration"] = logging;
    let family = format!("paramstore{index}");
    ecs(
        &router,
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
            "containerDefinitions": [container],
        }),
        case,
    )
    .await;
    let placed = ecs(
        &router,
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
    let task_arn = string(&tasks[0], "taskArn", case);
    let session_arn = format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{execution_role_name}/{}",
        task_id_of(&task_arn)
    );
    Launched {
        router,
        task_arn,
        session_arn,
        parameter_arn: arn,
        key_arn,
        reference,
    }
}

/// The task id: the last segment of a task's ARN.
fn task_id_of(task_arn: &str) -> String {
    task_arn.rsplit('/').next().unwrap_or_default().to_string()
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

/// The rows whose execution role MAY retrieve the parameter, so the task runs and the value
/// reaches the container.
const RUNS: &[Row] = &[
    Row {
        case: "a SecureString under a customer managed key, referenced by its ARN",
        sealing: Sealing::CustomerKey,
        grant: Grant::Execution,
        member: Member::ContainerSecrets,
        form: Form::Arn,
        variable: "DATABASE_PASSWORD",
        value: "s3cr3t-value",
        held: true,
        selector: "",
        key_disabled: false,
    },
    Row {
        case: "the same parameter referenced by its NAME in the task's Region",
        sealing: Sealing::CustomerKey,
        grant: Grant::Execution,
        member: Member::ContainerSecrets,
        form: Form::Name,
        variable: "API_TOKEN",
        value: "named-reference-value",
        held: true,
        selector: "",
        key_disabled: false,
    },
    Row {
        case: "wildcard-scoped grants on the execution role",
        sealing: Sealing::CustomerKey,
        grant: Grant::ExecutionWildcard,
        member: Member::ContainerSecrets,
        form: Form::Arn,
        variable: "WILDCARD_GRANTED",
        value: "{\"json\":\"is not a selector here\"}",
        held: true,
        selector: "",
        key_disabled: false,
    },
    Row {
        case: "a SecureString under the Amazon Web Services managed key needs no key grant",
        sealing: Sealing::AwsManagedKey,
        grant: Grant::ExecutionWithoutDecrypt,
        member: Member::ContainerSecrets,
        form: Form::Arn,
        variable: "SERVICE_KEY_SEALED",
        value: "aws-managed-key-value",
        held: true,
        selector: "",
        key_disabled: false,
    },
    Row {
        case: "a String parameter is not encrypted, so no key decides it",
        sealing: Sealing::Plaintext,
        grant: Grant::ExecutionWithoutDecrypt,
        member: Member::ContainerSecrets,
        form: Form::Name,
        variable: "PLAIN_VALUE",
        value: "not-encrypted-at-all",
        held: true,
        selector: "",
        key_disabled: false,
    },
    Row {
        case: "a reference carrying the version selector of the only version",
        sealing: Sealing::CustomerKey,
        grant: Grant::Execution,
        member: Member::ContainerSecrets,
        form: Form::Arn,
        variable: "SELECTED_VERSION",
        value: "first-version-value",
        held: true,
        selector: ":1",
        key_disabled: false,
    },
];

/// RULE, the running half: a `secrets` entry naming a parameter the EXECUTION role may
/// retrieve — and whose key, where one seals it, would open it for that role — is exposed to
/// the container's process under the entry's own name, with the value Parameter Store holds.
#[tokio::test]
async fn a_parameter_the_execution_role_may_retrieve_reaches_the_container() {
    for (index, row) in RUNS.iter().enumerate() {
        let case = row.case;
        let launched = launch(row, index).await;
        let router = &launched.router;

        // The container ran, and what it wrote is the value the parameter holds.
        let lines = poll_for_output(router, &launched.task_arn, case).await;
        assert_eq!(
            lines,
            vec![format!("injected={}", row.value)],
            "case {case:?}: expected the container to write the parameter's value to {}, got \
             {lines:?}",
            stream_of(&launched.task_arn)
        );

        // The task came up and ended as a task whose container RAN ends.
        let (task, seen) = poll_to_stopped(router, &launched.task_arn, case).await;
        assert!(
            seen.contains(&RUNNING.to_string()),
            "case {case:?}: expected the task to report {RUNNING} on its way, saw {seen:?}"
        );
        assert_eq!(
            task["containers"][0]["exitCode"],
            json!(0),
            "case {case:?}: expected exitCode 0, got {task}"
        );
        assert_eq!(
            task["stopCode"],
            json!("EssentialContainerExited"),
            "case {case:?}: expected the stop of a task that ran, got {task}"
        );
    }
}

/// One refusing row: the launch, plus what the `stoppedReason` has to carry.
struct Refused {
    row: Row,
    /// The error class the reason carries.
    class: &'static str,
    /// Which refusal the reason has to spell out, when a policy or a key state is what
    /// withheld the value.
    refusal: Withheld,
}

/// What the `stoppedReason` of a refused row has to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withheld {
    /// No document in force on the execution role allows the Parameter Store read.
    ParameterRead,
    /// A document in force denies that read outright.
    DeniedParameterRead,
    /// No document in force allows the decrypt of the key the value is sealed under.
    Decrypt,
    /// The read produced no value for the reference at all — nothing resolves it, or the key
    /// the value is sealed under cannot run the decrypt — which the batch read the agent makes
    /// reports among the names it could not resolve.
    Unresolved,
}

const REFUSALS: &[Refused] = &[
    Refused {
        row: Row {
            case: "the Parameter Store read granted but not the decrypt",
            sealing: Sealing::CustomerKey,
            grant: Grant::ExecutionWithoutDecrypt,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "DATABASE_PASSWORD",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: false,
        },
        class: "AccessDeniedException",
        refusal: Withheld::Decrypt,
    },
    Refused {
        row: Row {
            case: "both grants on the task role instead of the execution role",
            sealing: Sealing::CustomerKey,
            grant: Grant::TaskRoleOnly,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "API_TOKEN",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: false,
        },
        class: "AccessDeniedException",
        refusal: Withheld::ParameterRead,
    },
    Refused {
        row: Row {
            case: "no grant on either role",
            sealing: Sealing::CustomerKey,
            grant: Grant::Neither,
            member: Member::ContainerSecrets,
            form: Form::Name,
            variable: "NO_GRANT",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: false,
        },
        class: "AccessDeniedException",
        refusal: Withheld::ParameterRead,
    },
    Refused {
        row: Row {
            case: "an explicit deny beside the allow on the execution role",
            sealing: Sealing::CustomerKey,
            grant: Grant::ExecutionWithDeny,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "DENIED_VALUE",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: false,
        },
        class: "AccessDeniedException",
        refusal: Withheld::DeniedParameterRead,
    },
    Refused {
        row: Row {
            case: "the key disabled, with both grants on the execution role",
            sealing: Sealing::CustomerKey,
            grant: Grant::Execution,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "DISABLED_KEY",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: true,
        },
        class: "invalid ssm parameters",
        refusal: Withheld::Unresolved,
    },
    Refused {
        row: Row {
            case: "the key disabled and the decrypt not granted either",
            sealing: Sealing::CustomerKey,
            grant: Grant::ExecutionWithoutDecrypt,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "DISABLED_AND_UNGRANTED",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: true,
        },
        // The key's policy is read BEFORE its state, so a role the key does not permit is
        // refused the same way whatever state the key is in.
        class: "AccessDeniedException",
        refusal: Withheld::Decrypt,
    },
    Refused {
        row: Row {
            case: "a parameter the account does not hold",
            sealing: Sealing::Plaintext,
            grant: Grant::ExecutionWildcard,
            member: Member::ContainerSecrets,
            form: Form::Arn,
            variable: "ABSENT_PARAMETER",
            value: "never-written",
            held: false,
            selector: "",
            key_disabled: false,
        },
        class: "invalid ssm parameters",
        refusal: Withheld::Unresolved,
    },
    Refused {
        row: Row {
            case: "a selector naming no version of the parameter",
            sealing: Sealing::Plaintext,
            grant: Grant::ExecutionWildcard,
            member: Member::ContainerSecrets,
            form: Form::Name,
            variable: "ABSENT_VERSION",
            value: "only-version-one",
            held: true,
            selector: ":7",
            key_disabled: false,
        },
        class: "invalid ssm parameters",
        refusal: Withheld::Unresolved,
    },
    Refused {
        row: Row {
            case: "a log configuration's secretOptions the execution role may not retrieve",
            sealing: Sealing::CustomerKey,
            grant: Grant::TaskRoleOnly,
            member: Member::LogSecretOptions,
            form: Form::Arn,
            variable: "log-token",
            value: "never-injected",
            held: true,
            selector: "",
            key_disabled: false,
        },
        class: "AccessDeniedException",
        refusal: Withheld::ParameterRead,
    },
];

/// RULE, the refusing half: a `secrets` entry whose parameter the EXECUTION role may not
/// retrieve — because no document in force on it allows the read, because the key the value is
/// sealed under does not permit it the decrypt, because that key's state runs no cryptographic
/// operation, or because nothing resolves the reference at all — launches no container: the
/// task never reports `RUNNING`, it stops under `TaskFailedToStart` with the agent's
/// `ResourceInitializationError`, its container reports no `exitCode`, and the log group its
/// configuration names receives no stream.
#[tokio::test]
async fn a_parameter_the_execution_role_may_not_retrieve_fails_the_task_at_start() {
    for (index, refused) in REFUSALS.iter().enumerate() {
        let row = &refused.row;
        let case = row.case;
        // Offset so no row of this table shares a parameter name with a row of the other.
        let launched = launch(row, index + RUNS.len()).await;
        let router = &launched.router;

        let (task, seen) = poll_to_stopped(router, &launched.task_arn, case).await;
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
        assert!(
            reason.contains(refused.class),
            "case {case:?}: expected the reason to name {}, got {reason:?}",
            refused.class
        );
        // The refusal itself: the principal the retrieval was made by — a session of the
        // EXECUTION role, named after the task it was made for — the action, the resource and
        // the rule that withheld it.
        let expected = match refused.refusal {
            Withheld::ParameterRead => format!(
                "User: {} is not authorized to perform: {GET_PARAMETERS} on resource: {} {}",
                launched.session_arn,
                launched.parameter_arn,
                no_identity_policy_clause(GET_PARAMETERS)
            ),
            Withheld::DeniedParameterRead => format!(
                "User: {} is not authorized to perform: {GET_PARAMETERS} on resource: {} \
                 {EXPLICIT_DENY_CLAUSE}",
                launched.session_arn, launched.parameter_arn
            ),
            Withheld::Decrypt => format!(
                "User: {} is not authorized to perform: {DECRYPT} on resource: {} {}",
                launched.session_arn,
                launched.key_arn,
                no_identity_policy_clause(DECRYPT)
            ),
            // A value the read produced nothing for is reported by the reference the revision
            // carried, the way the batch read the agent makes reports one.
            Withheld::Unresolved => launched.reference.clone(),
        };
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the reason to carry {expected:?}, got {reason:?}"
        );
        // No container started, so none reports a status it exited with.
        assert_eq!(
            task["containers"][0]["exitCode"],
            Value::Null,
            "case {case:?}: a container that never ran reports no exitCode, got {task}"
        );
        // And nothing was delivered: the driver creates the stream when the container starts,
        // and this container never did.
        let streams = streams_of(router, &launched.task_arn, case).await;
        assert!(
            streams.is_empty(),
            "case {case:?}: expected no stream in {LOG_GROUP} for this task, got {streams:?}"
        );
    }
}
