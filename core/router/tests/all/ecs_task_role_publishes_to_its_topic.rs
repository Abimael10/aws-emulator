// SPDX-License-Identifier: CC0-1.0

//! What a container's own `aws sns publish` does, across the services that decide it.
//!
//! RULE under test: an AWS CLI invocation a container's program makes PERFORMS the call that
//! command names against the service that owns the resource, signed with a session of the task
//! role the launch resolved (`TaskDefinition$taskRoleArn` is "the IAM role that grants
//! containers in the task permission to call Amazon Web Services APIs on your behalf") — so
//! `aws sns publish`, which "sends a message to an Amazon SNS topic" (the `aws sns publish`
//! command reference), reaches the subscribers of the topic the command names when the policies
//! in force on that role allow the action, is refused by Amazon SNS when they do not, and the
//! command reports the service's own answer either way: the response document and the status of
//! a successful command, or the refusal in the CLI's words ("An error occurred (<code>) when
//! calling the <operation> operation: <message>") with the status it answers a service error
//! with. The container's own `exitCode` is the status its PROGRAM ended with, whichever way the
//! call went — which is what makes a one-shot task's outcome readable at all.
//!
//! Why this suite lives beside the router: nothing about the decision belongs to one service.
//! The role and the grant are IAM's records, the session is the token service's, the
//! authorization and the topic are Amazon SNS's, what the subscriber received is Amazon SQS's,
//! the task and its container are Amazon ECS's, and what the container WROTE is CloudWatch
//! Logs' — so all of them have to agree for the rule to hold. Every resource below is created
//! through the real operation of the service that owns it on ONE shared engine, and everything
//! asserted is read back through a real read: the container's `exitCode` through
//! `DescribeTasks`, its lines through `GetLogEvents`, and what was delivered through Amazon
//! SQS's own `ReceiveMessage`.
//!
//! Member and doc source: `smithy_query(query="operation", operation="Publish")` for the
//! members a publish names (`TopicArn`, `Message`, `Subject`) and the error an unauthorized one
//! earns (`AuthorizationErrorException`, wire code `AuthorizationError`, HTTP 403),
//! `smithy_query(query="shape", shape="TaskDefinition")` for `taskRoleArn`, and
//! `smithy_query(query="shape", shape="Container")` for `exitCode` ("the exit code returned from
//! the container").
//!
//! Every wait below looks again for the CONDITION it is waiting on until a generous deadline
//! passes, so a loaded machine polls more often instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "taskrolepublish";

/// The one container of every task definition below.
const CONTAINER: &str = "Publisher";

/// The image the containers start from. Nothing is fetched from it: what runs is the `command`
/// the revision states.
const IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:latest";

/// The log group every container delivers its output to, created through CloudWatch Logs' own
/// API before any task is launched.
const LOG_GROUP: &str = "/emulator/ecs/task-role-publish";

/// The `awslogs-stream-prefix` every task definition configures, and so the first segment of
/// every stream name they deliver to.
const STREAM_PREFIX: &str = "publish";

/// The network the Fargate tasks are placed in. The ids are fixed, because the requests and the
/// records have to name the same ones, so they are written into the account's Amazon EC2
/// records rather than minted ([`crate::ec2_network`]).
const VPC: &str = "vpc-0ec50000000000009";
const SUBNET: &str = "subnet-0ec50000000000009";

/// The wire code Amazon SNS refuses a call no policy allows with — the `awsQueryError` code of
/// `AuthorizationErrorException`. Written out rather than read off the service's constants: it
/// is fixed by the API, so a literal here is what catches a wrong wire code.
const AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// The service principal Amazon SNS delivers to a subscribed queue as, which the queue's own
/// `Policy` has to admit ("you must ensure that the Amazon SQS queue policy allows the Amazon
/// SNS topic to send messages to the queue").
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the deadline
/// is only there so a move that never happens fails the test instead of hanging it.
const DEADLINE: Duration = Duration::from_secs(120);
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

/// The trust policy a role Amazon ECS assumes for a task carries: the service principal it
/// assumes a task's roles as.
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
/// `environment` on it — the values a client writes a publish's destination and body into.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    execution_role_arn: &str,
    task_role_arn: &str,
    program: &str,
    environment: &[(&str, &str)],
) {
    let environment: Vec<Value> = environment
        .iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
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

/// Every message body waiting on the queue at `url`, deleted as it is read, so one stage's
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
///
/// The queue is polled rather than read once: the hand-over to a subscribed queue is Amazon
/// SNS's own call, made as the publish is served.
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
/// it: the log group the containers deliver to, the cluster they run in, the EXECUTION role the
/// agent acts as (which a Fargate task with the `awslogs` driver needs), and the topic with an
/// SQS queue subscribed to it.
struct Account {
    /// The execution role every revision names.
    execution: String,
    /// The topic the containers publish to.
    topic: String,
    /// The queue subscribed to it, as a client reads it.
    queue_url: String,
}

/// Stand that account up, under the resource names `suffix` distinguishes.
async fn given_the_account(router: &MultiServiceRouter, case: &str, suffix: &str) -> Account {
    let (status, created) = logs(router, "CreateLogGroup", &json!({ "logGroupName": LOG_GROUP })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLogGroup must answer 200, got {status} with {created}"
    );
    ecs(router, case, "CreateCluster", &json!({ "clusterName": CLUSTER })).await;
    let execution_role_name = format!("publishexec{suffix}");
    let execution = role(router, case, &execution_role_name).await;
    grant(
        router,
        case,
        &execution_role_name,
        "deliver-container-logs",
        &["logs:CreateLogStream", "logs:PutLogEvents"],
        "*",
    )
    .await;
    let queue = sqs(
        router,
        case,
        "CreateQueue",
        &json!({ "QueueName": format!("publish-subscriber-{suffix}") }),
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
    let created = sns(
        router,
        case,
        "CreateTopic",
        &[("Name", &format!("publish-topic-{suffix}"))],
    )
    .await;
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

/// The program every arm runs, written the way a client writes a one-shot probe of a task
/// role's grant: the publish's own output captured, the outcome reported from it, and the
/// program ending successfully whichever way the call went.
///
/// `arguments` is what follows `aws sns publish`, so a row can vary the FORM of the invocation
/// without changing the probe around it.
fn program(arguments: &str) -> String {
    format!(
        "if out=$(aws sns publish {arguments} 2>&1); then echo \"OUTCOME=OK BODY=$BODY\"; \
         else echo \"OUTCOME=DENIED BODY=$BODY\"; echo \"DETAIL=$out\"; fi; exit 0"
    )
}

/// RULE: the policies in force on the TASK ROLE decide whether a container's own
/// `aws sns publish` reaches the topic, and the program reports the service's own answer either
/// way — while the container's `exitCode` is the status its program ended with, whichever way
/// the call went.
///
/// The arms differ along what the rule generalises over: whether the role carries the grant the
/// call needs (none at all, the one `Allow` statement attached to the SAME role between two
/// launches), and a role granted ANOTHER action of the same service, which is not this one.
#[tokio::test]
async fn the_task_roles_grant_decides_whether_a_containers_publish_reaches_its_topic() {
    let case = "the task role's grant decides the container's publish";
    let router = emulator();
    let account = given_the_account(&router, case, "grant").await;
    let task_role_name = "publishtaskrole";
    let task_role = role(&router, case, task_role_name).await;
    let other_role_name = "publishotherrole";
    let other_role = role(&router, case, other_role_name).await;
    // A grant of another action of the same service, which is not the action this call performs.
    grant(
        &router,
        case,
        other_role_name,
        "subscribe-only",
        &["sns:Subscribe"],
        &account.topic,
    )
    .await;

    // (family, the role the launch resolves, the body the publish carries)
    let arms: [(&str, &str, &str); 3] = [
        ("publishungranted", task_role.as_str(), "body-ungranted"),
        ("publishgranted", task_role.as_str(), "body-granted"),
        ("publishotheraction", other_role.as_str(), "body-other"),
    ];
    for (family, role_arn, body) in arms {
        register(
            &router,
            case,
            family,
            &account.execution,
            role_arn,
            &program("--topic-arn $TOPIC_ARN --message $BODY"),
            &[("TOPIC_ARN", &account.topic), ("BODY", body)],
        )
        .await;
    }

    // ARM 1 — the role carries no grant at all: Amazon SNS refuses the call, the program reports
    // the refusal it captured, the container still ends with the status its program chose, and
    // nothing reaches the subscriber.
    let arm = "no grant on the role";
    let ungranted = run_task(&router, case, "publishungranted").await;
    let (task, lines) = stopped_task(&router, case, &ungranted).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        reported(&lines, "OUTCOME").as_deref(),
        Some("DENIED BODY=body-ungranted"),
        "case {case:?} arm {arm:?}: expected the program to report the refusal, got {lines:?}"
    );
    let detail = reported(&lines, "DETAIL").unwrap_or_default();
    let task_id = ungranted.rsplit('/').next().unwrap_or_default();
    for expected in [
        AUTHORIZATION_ERROR,
        "calling the Publish operation",
        &format!(":assumed-role/{task_role_name}/{task_id}"),
        &format!("is not authorized to perform: SNS:Publish on resource: {}", account.topic),
        "because no identity-based policy allows the SNS:Publish action",
    ] {
        assert!(
            detail.contains(expected),
            "case {case:?} arm {arm:?}: expected {expected:?} in the captured refusal, got \
             {detail:?}"
        );
    }
    assert!(
        drain(&router, case, &account.queue_url).await.is_empty(),
        "case {case:?} arm {arm:?}: a refused publish must reach no subscriber"
    );

    // ARM 2 — the one `Allow` statement attached to the SAME role: the next task's call is made,
    // the program reports the document it answered, and the message arrives at the subscriber.
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
    let granted = run_task(&router, case, "publishgranted").await;
    let (task, lines) = stopped_task(&router, case, &granted).await;
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
    assert_eq!(
        envelope["TopicArn"].as_str(),
        Some(account.topic.as_str()),
        "case {case:?} arm {arm:?}: expected the envelope of the topic it published to, got \
         {envelope}"
    );

    // ARM 3 — a role granted another action of the same service: that is not this action, so the
    // call is refused exactly as an un-granted one is.
    let arm = "a grant of another action";
    let other = run_task(&router, case, "publishotheraction").await;
    let (task, lines) = stopped_task(&router, case, &other).await;
    assert_eq!(
        task["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?} arm {arm:?}: expected the status the program ended with, got {task}"
    );
    assert_eq!(
        reported(&lines, "OUTCOME").as_deref(),
        Some("DENIED BODY=body-other"),
        "case {case:?} arm {arm:?}: expected the program to report the refusal, got {lines:?}"
    );
    assert!(
        reported(&lines, "DETAIL")
            .unwrap_or_default()
            .contains(AUTHORIZATION_ERROR),
        "case {case:?} arm {arm:?}: expected the service's own refusal, got {lines:?}"
    );
    assert!(
        drain(&router, case, &account.queue_url).await.is_empty(),
        "case {case:?} arm {arm:?}: a refused publish must reach no subscriber"
    );
}

/// One row of the FORMS table: how the program writes the invocation, and what the subscriber is
/// to receive from it.
struct Form {
    /// The case this row is reported under.
    case: &'static str,
    /// The family its revision is registered under.
    family: &'static str,
    /// What follows `aws sns publish` in the program, over the environment the launch states.
    arguments: &'static str,
    /// The body the publish carries, which the envelope's `Message` is to equal.
    body: &'static str,
    /// The `Subject` the envelope is to carry, and `None` for a row naming none.
    subject: Option<&'static str>,
}

/// RULE, over the FORMS of the one command: whichever way a container's program writes a
/// publish it performs nothing differently — the call carries the members the line named and the
/// topic's subscribers receive the message — because what the line asks for is read as the CLI
/// reads it rather than matched against one spelling.
///
/// The rows differ along the member the line names (a message alone, a subject beside it, the
/// FIFO-free members absent), the form a parameter's value is written in (`--name value` and
/// `--name=value`), the shape of the body (one word, text with spaces that the program quotes),
/// and the output format the line asks the answer in (the default document, and the
/// tab-separated one).
#[tokio::test]
async fn every_form_of_the_publish_a_containers_program_writes_reaches_the_topic() {
    let case = "the forms of a container's publish";
    let router = emulator();
    let account = given_the_account(&router, case, "forms").await;
    let task_role_name = "publishformsrole";
    let task_role = role(&router, case, task_role_name).await;
    grant(
        &router,
        case,
        task_role_name,
        "publish-to-the-topic",
        &["sns:Publish"],
        &account.topic,
    )
    .await;

    let rows: [Form; 5] = [
        Form {
            case: "the destination and the message alone",
            family: "publishformplain",
            arguments: "--topic-arn $TOPIC_ARN --message $BODY",
            body: "form-plain",
            subject: None,
        },
        Form {
            case: "a subject beside the message",
            family: "publishformsubject",
            arguments: "--topic-arn $TOPIC_ARN --message $BODY --subject Status",
            body: "form-subject",
            subject: Some("Status"),
        },
        Form {
            case: "the values written with equals signs",
            family: "publishformequals",
            arguments: "--topic-arn=$TOPIC_ARN --message=$BODY",
            body: "form-equals",
            subject: None,
        },
        Form {
            case: "a body of several words, quoted by the program",
            family: "publishformquoted",
            arguments: "--topic-arn $TOPIC_ARN --message \"$BODY\"",
            body: "form quoted body",
            subject: None,
        },
        Form {
            case: "the tab-separated output format",
            family: "publishformtext",
            arguments: "--topic-arn $TOPIC_ARN --message $BODY --output text",
            body: "form-text",
            subject: None,
        },
    ];
    for row in &rows {
        register(
            &router,
            case,
            row.family,
            &account.execution,
            &task_role,
            &program(row.arguments),
            &[("TOPIC_ARN", &account.topic), ("BODY", row.body)],
        )
        .await;
    }

    for row in &rows {
        let arm = row.case;
        let task_arn = run_task(&router, case, row.family).await;
        let (task, lines) = stopped_task(&router, case, &task_arn).await;
        assert_eq!(
            task["containers"][0]["exitCode"].as_i64(),
            Some(0),
            "case {case:?} row {arm:?}: expected the status the program ended with, got {task}"
        );
        assert_eq!(
            reported(&lines, "OUTCOME").as_deref(),
            Some(format!("OK BODY={}", row.body).as_str()),
            "case {case:?} row {arm:?}: expected the program to report the publish, got {lines:?}"
        );
        let envelope = delivered(&router, case, &account.queue_url)
            .await
            .unwrap_or_else(|| {
                panic!("case {case:?} row {arm:?}: nothing reached the subscriber")
            });
        assert_eq!(
            envelope["Message"].as_str(),
            Some(row.body),
            "case {case:?} row {arm:?}: expected the body the container published, got {envelope}"
        );
        assert_eq!(
            envelope["Subject"].as_str(),
            row.subject,
            "case {case:?} row {arm:?}: expected the subject the line named, got {envelope}"
        );
    }
}
