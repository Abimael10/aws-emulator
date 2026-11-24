// SPDX-License-Identifier: CC0-1.0

//! The one-shot PROBE shape a container's program is written in, across the services that
//! decide its outcome: a retry loop that leaves itself on success and reports what happened
//! with a format.
//!
//! RULE: a container's program is the shell program its launch names, and the session that
//! performs it performs the commands of the forms it performs — so a program written as
//! "try the call; on success report it and LEAVE the loop; otherwise try again, and report the
//! refusal the service gave" runs to its end: the loop is left by `break` ("exit from the
//! smallest enclosing for, while, or until loop") at the attempt that succeeded, each report
//! is written by `printf` ("the printf utility shall write formatted operands to the standard
//! output") into the container's own log stream, and `Container$exitCode` ("the exit code
//! returned from the container") is the status that program ended with — whichever way the
//! call went. What the call itself answers is the owning service's own decision: `aws sns
//! publish` "sends a message to an Amazon SNS topic", signed as the task role
//! (`TaskDefinition$taskRoleArn` is "the IAM role that grants containers in the task
//! permission to call Amazon Web Services APIs on your behalf"), so the subscribers of the
//! topic receive the message when the policies in force on that role allow the action and
//! Amazon SNS refuses it when they do not.
//!
//! That is the loop every "run a task, read its exit code and its log line" diagnostic is
//! built on, and it is why this suite lives beside the router: the role and the grant are
//! IAM's records, the session is the token service's, the authorization and the topic are
//! Amazon SNS's, what the subscriber received is Amazon SQS's, the task and its container are
//! Amazon ECS's, and the lines the container wrote are CloudWatch Logs'. Every resource is
//! created through the real operation of the service that owns it on ONE shared engine, and
//! everything asserted is read back through a real read: the container's `exitCode` through
//! `DescribeTasks`, its lines through `GetLogEvents`, and the delivery through Amazon SQS's
//! own `ReceiveMessage`.
//!
//! The arms differ along what the rule generalises over: whether the role carries the grant
//! the call needs — so whether the loop is left at its FIRST attempt or runs every attempt out
//! — and the grant is added to the SAME role between two launches, which is the repair half of
//! the cycle.
//!
//! Member and doc source: `smithy_query(query="operation", operation="Publish")` for the
//! members a publish names and the error an unauthorized one earns
//! (`AuthorizationErrorException`, wire code `AuthorizationError`),
//! `smithy_query(query="shape", shape="TaskDefinition")` for `taskRoleArn`, and
//! `smithy_query(query="shape", shape="Container")` for `exitCode`.
//!
//! Every wait below looks again for the CONDITION it is waiting on until a generous deadline
//! passes, so a loaded machine polls more often instead of failing, and nothing here asserts
//! on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "publishprobe";

/// The one container of every revision below.
const CONTAINER: &str = "Publisher";

/// The image the containers start from. Nothing is fetched from it: what runs is the `command`
/// the revision states.
const IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:latest";

/// The log group every container delivers its output to, created through CloudWatch Logs' own
/// API before any task is launched, and the `awslogs-stream-prefix` the configuration names —
/// so a container's stream is `<prefix>/<container>/<task id>`, spelled out here rather than
/// imported so a wrong naming in the implementation cannot make a test pass.
const LOG_GROUP: &str = "/emulator/ecs/publish-probe";
const STREAM_PREFIX: &str = "probe";

/// The network the Fargate tasks are placed in. The ids are fixed, because the requests and
/// the records have to name the same ones, so they are written into the account's Amazon EC2
/// records rather than minted ([`crate::ec2_network`]).
const VPC: &str = "vpc-0ec50000000000019";
const SUBNET: &str = "subnet-0ec50000000000019";

/// The IAM API version an awsQuery body carries, and Amazon SNS's.
const IAM_VERSION: &str = "2010-05-08";
const SNS_VERSION: &str = "2010-03-31";

/// The wire code Amazon SNS refuses a call no policy allows with — the `awsQueryError` code of
/// `AuthorizationErrorException`. Written out rather than read off the service's constants: it
/// is fixed by the API, so a literal here is what catches a wrong wire code.
const AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// The service principal Amazon SNS delivers to a subscribed queue as, which the queue's own
/// `Policy` has to admit, and the one Amazon ECS assumes a task's roles as.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How many attempts the probe's retry loop may take ([`PROBE`] iterates over that many
/// words), which is what a run that never succeeds reports as its attempt count.
const ATTEMPTS: u32 = 3;

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the deadline
/// is only there so a move that never happens fails the test instead of hanging it.
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

/// The services this decision spans over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them, with the network the Fargate placements name in the account's Amazon EC2 records.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], &format!("{REGION}a"));
    crate::ec2_network::register_internet_route(&engine, VPC);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
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
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon ECS call that has to succeed.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonEC2ContainerServiceV20141113.{operation}");
    let (status, parsed) = json_call(router, "ecs", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that has to succeed.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let (status, parsed) = json_call(router, "sqs", &target, "1.0", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call that has to succeed.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("Logs_20140328.{operation}");
    let (status, parsed) = json_call(router, "logs", &target, "1.1", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery action that has to succeed, answered with its XML body.
async fn query_call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    host: &str,
    action: &str,
    version: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
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
        "case {case:?}: {action} must answer 200, got {status} with {text}"
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
        action,
        IAM_VERSION,
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
        action,
        SNS_VERSION,
        members,
    )
    .await
}

/// The trust policy a role Amazon ECS assumes for a task carries.
fn ecs_tasks_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
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

/// The probe every launch below runs, written the way a client writes a one-shot probe of a
/// permission: a bounded retry loop ([`ATTEMPTS`] words) that LEAVES itself at the attempt
/// that succeeded, and a formatted report of what happened either way — the outcome, the body
/// it published, the attempt it ended on and, for a refusal, the account the service gave of
/// it.
///
/// Nothing in it waits for a span: the attempts are the loop's own, so what the program reports
/// depends on the service's answer and never on how fast the host runs.
const PROBE: &str = "OK=no; A=0; \
     for I in 1 2 3; do \
       A=$((A + 1)); \
       if OUT=$(aws sns publish --topic-arn \"$TOPIC_ARN\" --message \"$BODY\" 2>&1); then \
         OK=yes; break; \
       fi; \
     done; \
     if [ \"$OK\" = yes ]; then \
       printf 'OUTCOME=OK BODY=%s ATTEMPT=%d\\n' \"$BODY\" $A; \
     else \
       printf 'OUTCOME=DENIED BODY=%s ATTEMPT=%d DETAIL=%s\\n' \"$BODY\" $A \"$OUT\"; \
     fi; \
     exit 0";

/// Register one revision whose single container runs the probe under `task_role_arn`, with the
/// destination and the body it publishes in its environment.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    execution_role_arn: &str,
    task_role_arn: &str,
    topic: &str,
    body: &str,
) {
    ecs(
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
                "command": [PROBE],
                "environment": [
                    { "name": "TOPIC_ARN", "value": topic },
                    { "name": "BODY", "value": body },
                ],
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
}

/// `RunTask` one Fargate task of `family`, and answer its ARN.
async fn run_task(router: &MultiServiceRouter, case: &str, family: &str) -> String {
    let answer = ecs(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": { "subnets": [SUBNET], "assignPublicIp": "ENABLED" }
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
///
/// The CONDITION is polled rather than a span slept for: the task's own end is what makes its
/// container's run certainly over, so the lines read afterwards are the whole of what the
/// program wrote.
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
    let read = logs(
        router,
        case,
        "GetLogEvents",
        &json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}"),
            "startFromHead": true,
        }),
    )
    .await;
    let lines = read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (task, lines)
}

/// The one line the probe reported, which is the whole of what it wrote.
fn reported(lines: &[String], case: &str) -> String {
    assert_eq!(
        lines.len(),
        1,
        "case {case:?}: expected the probe's one report, got {lines:?}"
    );
    lines.first().cloned().unwrap_or_default()
}

/// Every message body waiting on the queue at `url`, deleted as it is read, so one arm's
/// delivery never comes back into view for the next.
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
/// `None` once the deadline passes with nothing on it, which is what a delivery that never
/// happened leaves.
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
/// it: the log group the containers deliver to, the cluster they run in, the EXECUTION role
/// the agent acts as, and the topic with an Amazon SQS queue subscribed to it.
struct Account {
    /// The execution role every revision names.
    execution: String,
    /// The topic the containers publish to.
    topic: String,
    /// The queue subscribed to it, as a client reads it.
    queue_url: String,
}

/// Stand that account up.
async fn given_the_account(router: &MultiServiceRouter, case: &str) -> Account {
    logs(
        router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    ecs(
        router,
        case,
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;
    let execution_role_name = "probeexecutionrole";
    let execution = role(router, case, execution_role_name).await;
    grant(
        router,
        case,
        execution_role_name,
        "deliver-container-logs",
        &["logs:CreateLogStream", "logs:PutLogEvents"],
        "*",
    )
    .await;
    let queue = sqs(
        router,
        case,
        "CreateQueue",
        &json!({ "QueueName": "publish-probe-subscriber" }),
    )
    .await;
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {queue}"))
        .to_string();
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
    let created = sns(router, case, "CreateTopic", &[("Name", "publish-probe")]).await;
    let topic = crate::query_wire::opt_element(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: no <TopicArn> in {created}"));
    // The queue takes the subscription's hand-over only where its own document admits the
    // delivering service's principal.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
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

/// RULE: the probe a container runs — a retry loop left on success, reporting with a format —
/// runs to its end either way, so a client reads the task's own outcome: the refusal while the
/// task role carries no grant, every attempt taken, and the publish once the one missing
/// statement is attached to that same role, at the first attempt, with the message at the
/// subscriber.
#[tokio::test]
async fn a_retry_probe_reports_the_refusal_and_then_the_publish() {
    let case = "the task role's grant decides what the probe reports";
    let router = emulator();
    let account = given_the_account(&router, case).await;
    let task_role_name = "probetaskrole";
    let task_role = role(&router, case, task_role_name).await;

    let ungranted_body = "probe-body-ungranted";
    let granted_body = "probe-body-granted";
    for (family, body) in [
        ("probeungranted", ungranted_body),
        ("probegranted", granted_body),
    ] {
        register(
            &router,
            case,
            family,
            &account.execution,
            &task_role,
            &account.topic,
            body,
        )
        .await;
    }

    // ARM 1 — the role carries no grant at all: every attempt of the loop is refused, so the
    // loop runs out rather than being left, and the report carries the service's own account of
    // the refusal. The container still ends with the status its program chose.
    let arm = "no grant on the role";
    let ungranted = run_task(&router, case, "probeungranted").await;
    let (task, lines) = stopped_task(&router, case, &ungranted).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        task["stopCode"].as_str(),
        Some(ESSENTIAL_CONTAINER_EXITED),
        "case {case:?} arm {arm:?}: expected the stop of a task whose container ran, got {task}"
    );
    let report = reported(&lines, case);
    let task_id = ungranted.rsplit('/').next().unwrap_or_default();
    for expected in [
        format!("OUTCOME=DENIED BODY={ungranted_body} ATTEMPT={ATTEMPTS} DETAIL="),
        format!("An error occurred ({AUTHORIZATION_ERROR}) when calling the Publish operation:"),
        format!(":assumed-role/{task_role_name}/{task_id}"),
        format!(
            "is not authorized to perform: SNS:Publish on resource: {}",
            account.topic
        ),
        "because no identity-based policy allows the SNS:Publish action".to_string(),
    ] {
        assert!(
            report.contains(&expected),
            "case {case:?} arm {arm:?}: expected {expected:?} in the probe's report, got \
             {report:?}"
        );
    }
    assert!(
        drain(&router, case, &account.queue_url).await.is_empty(),
        "case {case:?} arm {arm:?}: a refused publish must reach no subscriber"
    );

    // ARM 2 — the one `Allow` statement attached to the SAME role: the next task's first
    // attempt is served, so the loop is LEFT at that attempt, and the message arrives at the
    // subscriber.
    let arm = "the grant attached to the same role";
    grant(
        &router,
        case,
        task_role_name,
        "publish-to-the-topic",
        &["sns:Publish"],
        &account.topic,
    )
    .await;
    let granted = run_task(&router, case, "probegranted").await;
    let (task, lines) = stopped_task(&router, case, &granted).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        reported(&lines, case),
        format!("OUTCOME=OK BODY={granted_body} ATTEMPT=1"),
        "case {case:?} arm {arm:?}: expected the probe to report the publish at its first \
         attempt, got {lines:?}"
    );
    let envelope = delivered(&router, case, &account.queue_url)
        .await
        .unwrap_or_else(|| panic!("case {case:?} arm {arm:?}: nothing reached the subscriber"));
    assert_eq!(
        envelope["Message"].as_str(),
        Some(granted_body),
        "case {case:?} arm {arm:?}: expected the body the container published, got {envelope}"
    );
    assert_eq!(
        envelope["TopicArn"].as_str(),
        Some(account.topic.as_str()),
        "case {case:?} arm {arm:?}: expected the envelope of the topic it published to, got \
         {envelope}"
    );
}
