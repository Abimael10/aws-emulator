// SPDX-License-Identifier: CC0-1.0

//! The diagnose-then-repair loop for a task-role permission gap, over the account shape a
//! client really stands up: a Fargate task launched by `RunTask` whose container publishes to
//! an Amazon SNS topic is refused while its task role carries no `sns:Publish`, and the SAME
//! role's next task publishes once that one statement is attached.
//!
//! RULE under test: the policies in force on the task role AT THE INSTANT a container's own
//! `aws sns publish` is made decide that call — `TaskDefinition$taskRoleArn` is "the IAM role
//! that grants containers in the task permission to call Amazon Web Services APIs on your
//! behalf" — so the same role is refused before a grant and served after it, with no
//! re-registration of anything but the revision the client happens to roll; and either way
//! the container's program RUNS TO ITS END, so `DescribeTasks` reports the task `STOPPED`
//! with the essential container's `exitCode` and `GetLogEvents` on
//! `<awslogs-stream-prefix>/<container>/<task-id>` returns the lines the program wrote.
//!
//! What this suite adds beside [`crate::ecs_task_role_publishes_to_its_topic`], which pins the
//! grant decision itself, is the ACCOUNT every one of those decisions is taken in, written the
//! way a client writes it rather than the shortest way that works:
//!
//! * the execution role is granted by ATTACHING the policy AWS publishes for one
//!   (`AmazonECSTaskExecutionRolePolicy`) rather than by an inline statement, so the
//!   `awslogs` driver's own stream creation is authorized out of IAM's catalogue of managed
//!   policies;
//! * the subscriber queue's own `Policy` admits the delivering service under a CONDITION on
//!   the topic it is delivering for (`aws:SourceArn`), which is the document the Amazon SNS
//!   Developer Guide tells a client to put on it;
//! * the tasks are placed in the account's OWN default network, read out of Amazon EC2
//!   (`default-for-az` subnets and the VPC's `default` security group) rather than written
//!   into the store, with `assignPublicIp` `ENABLED`;
//! * the repair rolls a NEW REVISION of the family whose role was refused, and it is that
//!   revision's task whose publish has to arrive — composing the body it publishes in a
//!   here-document, the form a program writes a message in when it is not one word of the
//!   launch's environment.
//!
//! Member and doc source: `smithy_query(query="operation", operation="RunTask")` for
//! `launchType`/`networkConfiguration`, `smithy_query(query="operation",
//! operation="DescribeTasks")` for `tasks`/`failures`, `smithy_query(query="shape",
//! shape="Container")` for `exitCode` ("the exit code returned from the container"),
//! `smithy_query(query="shape", shape="TaskDefinition")` for `taskRoleArn` and
//! `executionRoleArn`, and `smithy_query(query="operation", operation="Publish",
//! model_path="services/service-com-amazonaws-sns-amazonsimplenotificationservice/smithy-model/com-amazonaws-sns-amazonsimplenotificationservice.json")`
//! for the error an unauthorized publish earns (`AuthorizationErrorException`, wire code
//! `AuthorizationError`).
//!
//! Every wait below looks again for the CONDITION it waits on until a generous deadline
//! passes, so a loaded machine polls more often instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "taskrolerepair";

/// The one container of every task definition below.
const CONTAINER: &str = "Publisher";

/// The image the containers start from. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:latest";

/// The log group every container delivers its output to, created through CloudWatch Logs'
/// own API before any task is launched.
const LOG_GROUP: &str = "/emulator/ecs/task-role-repair";

/// The `awslogs-stream-prefix` every task definition configures, and so the first segment of
/// every stream name they deliver to.
const STREAM_PREFIX: &str = "repair";

/// The policy AWS publishes for a task execution role — "AmazonECSTaskExecutionRolePolicy",
/// the managed policy the Amazon ECS Developer Guide has a client attach to one — read here
/// out of IAM's own catalogue of AWS managed policies rather than restated as statements.
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The wire code Amazon SNS refuses a call no policy allows with — the `awsQueryError` code
/// of `AuthorizationErrorException`. Written out rather than read off the service's
/// constants: it is fixed by the API, so a literal here is what catches a wrong wire code.
const AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// The service principal Amazon SNS delivers to a subscribed queue as, which the queue's own
/// `Policy` has to admit.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the
/// deadline is only there so a move that never happens fails the test instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(180);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The services this decision spans, over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them. The network is the account's own default VPC, which Amazon EC2 holds from the
/// moment the account exists.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// One `X-Amz-Target` RPC call, answered with its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
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

/// One Amazon ECS call that has to succeed.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonEC2ContainerServiceV20141113.{operation}");
    let (status, parsed) = json_call(router, "ecs", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that has to succeed.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let (status, parsed) = json_call(router, "sqs", &target, "1.0", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call, answered with its HTTP status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let target = format!("Logs_20140328.{operation}");
    json_call(router, "logs", &target, "1.1", body).await
}

/// One awsQuery call that has to succeed, answered with its XML body.
async fn query_call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {signing_name} {action}, got {status} with {text}"
    );
    text
}

/// One IAM action that has to succeed.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    query_call(
        router,
        case,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await
}

/// One Amazon SNS action that has to succeed.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    query_call(
        router,
        case,
        "sns",
        &format!("sns.{REGION}.amazonaws.com"),
        "2010-03-31",
        action,
        members,
    )
    .await
}

/// One Amazon EC2 action that has to succeed.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    query_call(
        router,
        case,
        "ec2",
        &format!("ec2.{REGION}.amazonaws.com"),
        "2016-11-15",
        action,
        members,
    )
    .await
}

/// The network a placement names, read out of the account's own Amazon EC2 records: one
/// default subnet and the `default` security group of the VPC it belongs to.
struct Network {
    /// The subnet the task's elastic network interface is created in.
    subnet: String,
    /// The security group the interface carries.
    security_group: String,
}

/// Read that network, the way a client that was given no network of its own reads it.
async fn given_the_default_network(router: &MultiServiceRouter, case: &str) -> Network {
    let described = ec2(
        router,
        case,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    )
    .await;
    let subnets = crate::query_wire::elements(&described, "subnetId");
    let subnet = subnets
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the account must hold a default subnet"));
    let vpc = crate::query_wire::element(&described, "vpcId");
    let groups = ec2(
        router,
        case,
        "DescribeSecurityGroups",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", &vpc),
            ("Filter.2.Name", "group-name"),
            ("Filter.2.Value.1", "default"),
        ],
    )
    .await;
    let security_group = crate::query_wire::elements(&groups, "groupId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the VPC must hold its default group"));
    Network {
        subnet,
        security_group,
    }
}

/// The trust policy a role Amazon ECS assumes for a task carries.
fn ecs_tasks_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create the role `name`, trusted by this service's principal, through IAM's own API; answer
/// its ARN.
async fn role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &ecs_tasks_trust()),
        ],
    )
    .await;
    crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {created}"))
}

/// Attach an inline policy to `role_name` allowing `actions` on `resource`, through IAM's own
/// `PutRolePolicy` — the call a client makes to repair a missing grant.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_name: &str,
    actions: &[&str],
    resource: &str,
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": resource,
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", policy_name),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// Register one revision whose single container runs `program` under `task_role_arn`, with
/// `environment` on it, and answer the revision's ARN.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    execution_role_arn: &str,
    task_role_arn: &str,
    program: &str,
    environment: &[(&str, &str)],
) -> String {
    let environment: Vec<Value> = environment
        .iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    let registered = ecs(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role_arn,
            "taskRoleArn": task_role_arn,
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "entryPoint": ["bash", "-c"],
                "command": [program],
                "environment": environment,
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
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no revision ARN in {registered}"))
        .to_string()
}

/// `RunTask` one Fargate task of `task_definition` in `network`, and answer its ARN.
async fn run_task(
    router: &MultiServiceRouter,
    case: &str,
    task_definition: &str,
    network: &Network,
) -> String {
    let answer = ecs(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": task_definition,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnet],
                    "securityGroups": [network.security_group],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    answer["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {answer}"))
        .to_string()
}

/// Look again until the task `task_arn` reports `STOPPED`, then answer the task as
/// `DescribeTasks` reports it and the lines its container delivered to its own `awslogs`
/// stream.
async fn stopped_task(
    router: &MultiServiceRouter,
    case: &str,
    task_arn: &str,
) -> (Value, Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    let task = loop {
        let body = ecs(
            router,
            case,
            "DescribeTasks",
            &json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        )
        .await;
        let task = body["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some("STOPPED") {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never stopped, last read {task}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    };
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    let (status, read) = logs(
        router,
        "GetLogEvents",
        &json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}"),
            "startFromHead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetLogEvents must answer 200, got {status} with {read}"
    );
    let lines = read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (task, lines)
}

/// The value a line `NAME=<value>` carries, and `None` when no line names it.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// Every message body waiting on the queue at `url`, deleted as it is read.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let handle = message["ReceiptHandle"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a ReceiptHandle, got {message}"));
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        )
        .await;
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or(Value::Null));
    }
    envelopes
}

/// Look again until the subscriber queue at `url` holds something, and answer what it held —
/// `None` once the deadline passes with nothing on it.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str) -> Option<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut envelopes = drain(router, case, url).await;
        if !envelopes.is_empty() {
            assert_eq!(
                envelopes.len(),
                1,
                "case {case:?}: expected one delivery, got {envelopes:?}"
            );
            return envelopes.pop();
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Everything the launches below need, each created through the API of the service that owns
/// it.
struct Account {
    /// The execution role every revision names, granted by the attached managed policy.
    execution: String,
    /// The topic the containers publish to.
    topic: String,
    /// The queue subscribed to it, as a client reads it.
    queue_url: String,
}

/// Stand that account up.
async fn given_the_account(router: &MultiServiceRouter, case: &str) -> Account {
    let (status, created) = logs(
        router,
        "CreateLogGroup",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLogGroup must answer 200, got {status} with {created}"
    );
    ecs(
        router,
        case,
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;
    // The EXECUTION role, granted the way the Developer Guide grants one: the published
    // managed policy ATTACHED, with no statement of the client's own.
    let execution_role_name = "repairexecrole";
    let execution = role(router, case, execution_role_name).await;
    iam(
        router,
        case,
        "AttachRolePolicy",
        &[
            ("RoleName", execution_role_name),
            ("PolicyArn", EXECUTION_ROLE_POLICY),
        ],
    )
    .await;
    let queue = sqs(
        router,
        case,
        "CreateQueue",
        &json!({ "QueueName": "repair-subscriber" }),
    )
    .await;
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {queue}"))
        .to_string();
    sqs(
        router,
        case,
        "TagQueue",
        &json!({ "QueueUrl": queue_url, "Tags": { "suite": "task-role-repair" } }),
    )
    .await;
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    let created = sns(router, case, "CreateTopic", &[("Name", "repair-topic")]).await;
    let topic = crate::query_wire::opt_element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no <TopicArn> in {created}"));
    // The queue takes the hand-over only where its own document admits the delivering
    // service's principal — here under the CONDITION that the delivery is for THIS topic.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic } },
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": policy } }),
    )
    .await;
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    Account {
        execution,
        topic,
        queue_url,
    }
}

/// The program every revision runs, written the way a client writes a one-shot probe of a
/// task role's grant: the publish's own output captured, the outcome reported from its LAST
/// line, and the program ending successfully whichever way the call went.
///
/// `composed` writes the message the publish carries as a HERE-DOCUMENT — the form a program
/// composes a body in when it is not one word of the launch's environment — rather than
/// naming the environment variable directly, so one arm drives that form through the whole
/// chain while the probe around it stays the same.
fn program(composed: bool) -> String {
    let body = if composed {
        "MESSAGE=$(cat <<EOF\n$BODY\nEOF\n); "
    } else {
        "MESSAGE=\"$BODY\"; "
    };
    format!(
        "set -u; {body}\
         if out=$(aws sns publish --topic-arn \"$TOPIC_ARN\" --message \"$MESSAGE\" 2>&1); then \
         echo \"OUTCOME=OK BODY=$MESSAGE\"; \
         else echo \"OUTCOME=DENIED BODY=$MESSAGE DETAIL=$(echo \"$out\" | tail -n 1)\"; fi; \
         exit 0"
    )
}

/// RULE: the policies in force on the TASK ROLE at the instant the container's call is made
/// decide it, so the same role is refused before its grant and served after it — and either
/// way the container's program runs to its end, so the task settles to `STOPPED` with the
/// essential container's `exitCode` and its lines are readable from its own log stream.
///
/// The arms differ along what the rule generalises over: what the role carries at the instant
/// of the call (nothing, the one `Allow` statement, the statement attached between two
/// launches of the same role), whether the revision the task runs was registered before or
/// after the grant, and the FORM the program writes the message in (one word of the launch's
/// environment, a document the program composes).
#[tokio::test]
async fn a_task_roles_sns_publish_denial_is_repaired_by_attaching_the_one_statement() {
    let case = "the task role's denial and its repair";
    let router = emulator();
    let network = given_the_default_network(&router, case).await;
    let account = given_the_account(&router, case).await;

    let denied_role_name = "repairdeniedrole";
    let denied_role = role(&router, case, denied_role_name).await;
    let granted_role_name = "repairgrantedrole";
    let granted_role = role(&router, case, granted_role_name).await;
    grant(
        &router,
        case,
        granted_role_name,
        "publish-to-the-topic",
        &["sns:Publish"],
        &account.topic,
    )
    .await;

    let denied_family = "repairdenied";
    let denied_revision = register(
        &router,
        case,
        denied_family,
        &account.execution,
        &denied_role,
        &program(false),
        &[("TOPIC_ARN", &account.topic), ("BODY", "body-denied")],
    )
    .await;
    let granted_revision = register(
        &router,
        case,
        "repairgranted",
        &account.execution,
        &granted_role,
        &program(false),
        &[("TOPIC_ARN", &account.topic), ("BODY", "body-granted")],
    )
    .await;

    // ARM 1 — the role carries no statement at all: Amazon SNS refuses the call, the program
    // reports the refusal it captured, the container still ends with the status its program
    // chose, and nothing reaches the subscriber.
    let arm = "no statement on the role";
    let task_arn = run_task(&router, case, &denied_revision, &network).await;
    let (task, lines) = stopped_task(&router, case, &task_arn).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    let outcome = reported(&lines, "OUTCOME").unwrap_or_default();
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    for expected in [
        "DENIED BODY=body-denied",
        AUTHORIZATION_ERROR,
        "calling the Publish operation",
        &format!(":assumed-role/{denied_role_name}/{task_id}"),
        &format!(
            "is not authorized to perform: SNS:Publish on resource: {}",
            account.topic
        ),
        "because no identity-based policy allows the SNS:Publish action",
    ] {
        assert!(
            outcome.contains(expected),
            "case {case:?} arm {arm:?}: expected {expected:?} in the reported outcome, got \
             {outcome:?}"
        );
    }
    assert!(
        drain(&router, case, &account.queue_url).await.is_empty(),
        "case {case:?} arm {arm:?}: a refused publish must reach no subscriber"
    );

    // ARM 2 — a role that carries the statement: the call is made and the message arrives.
    let arm = "the statement already on the role";
    let task_arn = run_task(&router, case, &granted_revision, &network).await;
    let (task, lines) = stopped_task(&router, case, &task_arn).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        reported(&lines, "OUTCOME").as_deref(),
        Some("OK BODY=body-granted"),
        "case {case:?} arm {arm:?}: expected the program to report the publish, got {lines:?}"
    );
    let envelope = delivered(&router, case, &account.queue_url)
        .await
        .unwrap_or_else(|| panic!("case {case:?} arm {arm:?}: nothing reached the subscriber"));
    assert_eq!(
        envelope["Message"].as_str(),
        Some("body-granted"),
        "case {case:?} arm {arm:?}: expected the body the container published, got {envelope}"
    );

    // ARM 3 — the REPAIR: the one missing statement is attached to the role that was refused,
    // a new revision of its own family is rolled, and that revision's task publishes.
    let arm = "the repaired role's next revision";
    grant(
        &router,
        case,
        denied_role_name,
        "publish-to-the-topic",
        &["sns:Publish"],
        &account.topic,
    )
    .await;
    let repaired_revision = register(
        &router,
        case,
        denied_family,
        &account.execution,
        &denied_role,
        // The repaired revision composes the body it publishes in a here-document, so the
        // form a program writes its message in is driven through the same chain.
        &program(true),
        &[("TOPIC_ARN", &account.topic), ("BODY", "body-repaired")],
    )
    .await;
    assert_ne!(
        repaired_revision, denied_revision,
        "case {case:?} arm {arm:?}: the repair must roll a new revision"
    );
    let task_arn = run_task(&router, case, &repaired_revision, &network).await;
    let (task, lines) = stopped_task(&router, case, &task_arn).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        reported(&lines, "OUTCOME").as_deref(),
        Some("OK BODY=body-repaired"),
        "case {case:?} arm {arm:?}: expected the program to report the publish, got {lines:?}"
    );
    let envelope = delivered(&router, case, &account.queue_url)
        .await
        .unwrap_or_else(|| panic!("case {case:?} arm {arm:?}: nothing reached the subscriber"));
    assert_eq!(
        envelope["Message"].as_str(),
        Some("body-repaired"),
        "case {case:?} arm {arm:?}: expected the body the container published, got {envelope}"
    );
}
