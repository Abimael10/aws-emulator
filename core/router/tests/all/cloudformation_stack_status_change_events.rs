// SPDX-License-Identifier: CC0-1.0

//! The STACK and RESOURCE STATUS-CHANGE EVENTS CloudFormation publishes onto the account's
//! default EventBridge bus, observed where a client observes them: on the Amazon SQS queue a
//! rule on that bus delivers to.
//!
//! The rule pinned here: **every status change CloudFormation records on a stack is published
//! onto the account's default event bus with `source` `aws.cloudformation`, in the stack's own
//! account and Region, naming the stack's id both in the event's `resources` and in
//! `detail.stack-id` — the stack's own changes under `detail-type` "CloudFormation Stack
//! Status Change" and each of its resources' under "CloudFormation Resource Status Change" —
//! with the status under `detail.status-details.status`, its recorded reason under
//! `detail.status-details.status-reason`, and the logical resource, physical resource and
//! resource type the change is about beside them.** Each event states the recorded event a
//! `DescribeStackEvents` of the same instant answers from, so the stream on the bus and the
//! stream a client polls cannot disagree — and because the status is one whole string,
//! a rule whose pattern names exactly `ROLLBACK_COMPLETE` receives a failed create's
//! conclusion and nothing of a failed update's `UPDATE_ROLLBACK_COMPLETE`.
//!
//! Four crates have to agree for that to hold — CloudFormation publishes, EventBridge matches
//! the event against the rules of the bus and hands it to their targets, Amazon SQS holds what
//! was delivered, and Amazon S3 owns the resources whose deployment succeeds or fails — so the
//! suite lives here rather than in any one of them. Nothing is asserted on an emulator
//! internal: every observation is a client call.
//!
//! The rows differ along what the rule generalises over: WHICH operation the status changes
//! belong to (a create, an update, a delete), whether that operation carried through or rolled
//! back, and how many resources the stack declares. The recorded case — a create that fails
//! and rolls back to `ROLLBACK_COMPLETE`, driving a rule keyed on exactly that status — is the
//! second row, and the exactness of that keying is pinned on its own below.
//!
//! Nothing here waits on the wall clock: a stack's terminal status is reached by looking again
//! until it holds, and the queue is drained until the events a row expects are on it.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every call below is signed for, which is the Region the stacks of this suite
/// belong to and the Region their events are published in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which is the account the events carry.
const ACCOUNT: &str = "000000000000";

/// The `source` every event CloudFormation publishes carries.
const SOURCE: &str = "aws.cloudformation";

/// The `detail-type` a STACK's own status changes are published under.
const STACK_CHANGE: &str = "CloudFormation Stack Status Change";

/// The `detail-type` one RESOURCE's status changes are published under.
const RESOURCE_CHANGE: &str = "CloudFormation Resource Status Change";

/// The resource type a stack's own event is about.
const STACK_RESOURCE_TYPE: &str = "AWS::CloudFormation::Stack";

/// The resource type of every resource these templates declare.
const BUCKET_TYPE: &str = "AWS::S3::Bucket";

/// The statuses a stack operation settles in: there is nothing more to wait for once the
/// stack reports one of them.
const TERMINAL_STATUSES: &[&str] = &[
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
    "UPDATE_COMPLETE",
    "UPDATE_FAILED",
    "UPDATE_ROLLBACK_COMPLETE",
    "UPDATE_ROLLBACK_FAILED",
    "DELETE_COMPLETE",
    "DELETE_FAILED",
];

/// The event a bucket's notification configuration subscribes to. Which event it is does not
/// enter the rule: what matters is that Amazon S3 validates the DESTINATION when the
/// configuration is installed, so a configuration naming a queue it does not hold is refused
/// and the stack operation that asked for it fails.
const SUBSCRIBED_EVENT: &str = "s3:ObjectCreated:*";

/// How long a poll or a drain keeps LOOKING, and how often. Generous: the deadline is a
/// safety net, never a measurement — what is asserted is what arrived, and each loop stops
/// the instant its condition holds.
const BUDGET: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it: which service
/// claims each call — CloudFormation's own in-process `PutEvents` and EventBridge's delivery
/// into Amazon SQS included — is decided by the production mount.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation call over its awsQuery wire that must answer 200, as its body.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {text}"
    );
    text
}

/// One `X-Amz-Target` RPC call that must answer 200, as its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One EventBridge call that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One Amazon DynamoDB call that must answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "dynamodb",
        "DynamoDB_20120810",
        "1.0",
        operation,
        body,
    )
    .await
}

/// One `(key, value)` request member, owned so a caller can build names like `Parameters.*`.
fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// A queue whose own policy admits the delivering service, and a rule on the default bus that
/// delivers every event matching `pattern` to it: the queue's url.
///
/// "For Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`), so the queue carries the statement that admits
/// `events.amazonaws.com`; the rule is created BEFORE the stack, since an event published
/// while the bus carries no matching rule reaches no target.
async fn wired_queue(router: &MultiServiceRouter, case: &str, name: &str, pattern: &Value)
-> String {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": format!("{name}-rule"), "EventPattern": pattern.to_string() }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": format!("{name}-rule"),
            "Targets": [{ "Id": "queue", "Arn": arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// The pattern of the rule that observes every event of one `detail-type` this service
/// publishes.
fn every_change_of(detail_type: &str) -> Value {
    json!({ "source": [SOURCE], "detail-type": [detail_type] })
}

/// The pattern of the rule that observes a stack status change reporting EXACTLY `status` —
/// the operator's "tell me when a deployment rolled back" rule.
fn only_the_status(status: &str) -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [STACK_CHANGE],
        "detail": { "status-details": { "status": [status] } },
    })
}

/// Every event body waiting on the queue at `url`, parsed, and taken off the queue.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message["Body"].as_str())
        .map(|body| serde_json::from_str(body).unwrap_or(Value::Null))
        .collect()
}

/// Drain the queue at `url` into `seen` until `enough` is satisfied or [`BUDGET`] runs out.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut Vec<Value>,
    enough: impl Fn(&[Value]) -> bool,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        seen.extend(drain(router, case, url).await);
        if enough(seen) || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The status one event reports.
fn status_of(event: &Value) -> &str {
    event["detail"]["status-details"]["status"]
        .as_str()
        .unwrap_or_default()
}

/// The statuses `seen` reports, in the order they arrived.
fn statuses_of(seen: &[Value]) -> Vec<String> {
    seen.iter().map(|event| status_of(event).to_string()).collect()
}

/// Look at the stack `name` until it reports a terminal status, and answer that status.
///
/// The CONDITION is what is waited for, not a span: each look is a `DescribeStacks`, which is
/// also what materialises the transitions the stack's own clock owes.
async fn settled(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described =
            cloudformation(router, case, "DescribeStacks", &[member("StackName", name)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&reported.as_str()) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never reached a terminal status; it reports {reported:?} \
             in {described}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The stack id `DescribeStacks` reports for the stack `name`.
async fn stack_id_of(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let described =
        cloudformation(router, case, "DescribeStacks", &[member("StackName", name)]).await;
    opt_element(&described, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {described}"))
}

/// One recorded stack event, as the members these published events are built from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedEvent {
    resource_type: String,
    logical_resource_id: String,
    status: String,
    status_reason: String,
}

/// Every event `DescribeStackEvents` reports for `stack`, oldest first.
///
/// `DescribeStackEvents` answers newest first, so the list is reversed into the order the
/// changes happened — which is the order they were published in.
async fn recorded_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<RecordedEvent> {
    let body =
        cloudformation(router, case, "DescribeStackEvents", &[member("StackName", stack)]).await;
    let mut recorded: Vec<RecordedEvent> = members_of(&body)
        .into_iter()
        .map(|entry| RecordedEvent {
            resource_type: elements(entry, "ResourceType").into_iter().next().unwrap_or_default(),
            logical_resource_id: elements(entry, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default(),
            status: elements(entry, "ResourceStatus").into_iter().next().unwrap_or_default(),
            status_reason: elements(entry, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default(),
        })
        // A pre-deployment validation failure is a statement about a resource NOTHING was
        // done to, so it carries no status and is no status change.
        .filter(|event| !event.status.is_empty())
        .collect();
    recorded.reverse();
    recorded
}

/// A template declaring `count` plain buckets named after `prefix`.
fn healthy_template(prefix: &str, count: usize) -> String {
    let mut resources = ::serde_json::Map::new();
    for index in 0..count {
        resources.insert(
            format!("Bucket{index}"),
            json!({
                "Type": BUCKET_TYPE,
                "Properties": { "BucketName": format!("{prefix}-{index}") },
            }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// A template whose last bucket's notification names a queue Amazon SQS does not hold, so
/// Amazon S3 refuses the configuration and the operation that asked for it fails.
///
/// "After Amazon S3 receives this request, it first verifies that any Amazon Simple
/// Notification Service (Amazon SNS) or Amazon Simple Queue Service (Amazon SQS) destination
/// exists, and that the bucket owner has permission to publish to it"
/// (`PutBucketNotificationConfiguration`).
fn broken_template(prefix: &str, count: usize) -> String {
    let mut resources = ::serde_json::Map::new();
    for index in 0..count {
        let mut properties = json!({ "BucketName": format!("{prefix}-{index}") });
        if index + 1 == count {
            properties["NotificationConfiguration"] = json!({
                "QueueConfigurations": [{
                    "Queue": format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{prefix}-no-such-queue"),
                    "Event": SUBSCRIBED_EVENT,
                }],
            });
        }
        resources.insert(
            format!("Bucket{index}"),
            json!({ "Type": BUCKET_TYPE, "Properties": properties }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// What one row does to its stack, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// `CreateStack` with a template every owning service accepts.
    CreateHealthy,
    /// `CreateStack` with a template Amazon S3 refuses part of.
    CreateBroken,
    /// `UpdateStack` to another template every owning service accepts.
    UpdateHealthy,
    /// `UpdateStack` to a template Amazon S3 refuses part of.
    UpdateBroken,
    /// `UpdateStack` to a template declaring an Amazon DynamoDB table whose name is already
    /// taken, which that service refuses — so the failure is another service's, on another
    /// resource type.
    UpdateTakenTable,
    /// `DeleteStack`.
    Delete,
}

/// What one Amazon DynamoDB table is declared as, by the name `table`: the smallest table
/// that service accepts, so what the row varies is WHO refuses the change and on which
/// resource type.
fn table_declaration(table: &str) -> Value {
    json!({
        "Type": "AWS::DynamoDB::Table",
        "Properties": {
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
        },
    })
}

/// A template declaring `count` plain buckets and one Amazon DynamoDB table called `table`.
fn table_template(prefix: &str, count: usize, table: &str) -> String {
    let mut resources = ::serde_json::Map::new();
    for index in 0..count {
        resources.insert(
            format!("Bucket{index}"),
            json!({
                "Type": BUCKET_TYPE,
                "Properties": { "BucketName": format!("{prefix}-{index}") },
            }),
        );
    }
    resources.insert("Table".to_string(), table_declaration(table));
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// One row: the stack, what is done to it, how many buckets its templates declare, and the
/// terminal status each stage must settle in.
struct Row {
    case: &'static str,
    stack: &'static str,
    resources: usize,
    stages: &'static [Stage],
    /// The terminal status of each stage, in the same order.
    settles_in: &'static [&'static str],
}

/// The class the rule covers: each kind of stack operation, carried through and rolled back.
const ROWS: &[Row] = &[
    Row {
        case: "a create that completes",
        stack: "cfnsse-create-ok",
        resources: 1,
        stages: &[Stage::CreateHealthy],
        settles_in: &["CREATE_COMPLETE"],
    },
    Row {
        case: "a create that fails and rolls back",
        stack: "cfnsse-create-rolled-back",
        resources: 2,
        stages: &[Stage::CreateBroken],
        settles_in: &["ROLLBACK_COMPLETE"],
    },
    Row {
        case: "a create that completes, then an update that completes",
        stack: "cfnsse-update-ok",
        resources: 2,
        stages: &[Stage::CreateHealthy, Stage::UpdateHealthy],
        settles_in: &["CREATE_COMPLETE", "UPDATE_COMPLETE"],
    },
    Row {
        case: "a create that completes, then an update that fails and rolls back",
        stack: "cfnsse-update-rolled-back",
        resources: 1,
        stages: &[Stage::CreateHealthy, Stage::UpdateBroken],
        settles_in: &["CREATE_COMPLETE", "UPDATE_ROLLBACK_COMPLETE"],
    },
    Row {
        case: "a create that completes, then a delete",
        stack: "cfnsse-delete",
        resources: 1,
        stages: &[Stage::CreateHealthy, Stage::Delete],
        settles_in: &["CREATE_COMPLETE", "DELETE_COMPLETE"],
    },
    Row {
        case: "a create that completes, then an update another service refuses on another \
               resource type",
        stack: "cfnsse-update-table-rolled-back",
        resources: 1,
        stages: &[Stage::CreateHealthy, Stage::UpdateTakenTable],
        settles_in: &["CREATE_COMPLETE", "UPDATE_ROLLBACK_COMPLETE"],
    },
];

/// Run one stage of `row` against its stack, and answer the terminal status it settled in.
async fn run_stage(router: &MultiServiceRouter, row: &Row, stage: Stage) -> String {
    let case = row.case;
    match stage {
        Stage::CreateHealthy | Stage::CreateBroken => {
            let template = if stage == Stage::CreateHealthy {
                healthy_template(row.stack, row.resources)
            } else {
                broken_template(row.stack, row.resources)
            };
            cloudformation(
                router,
                case,
                "CreateStack",
                &[
                    member("StackName", row.stack),
                    member("TemplateBody", &template),
                ],
            )
            .await;
        }
        Stage::UpdateHealthy | Stage::UpdateBroken => {
            let template = if stage == Stage::UpdateHealthy {
                healthy_template(row.stack, row.resources + 1)
            } else {
                broken_template(row.stack, row.resources)
            };
            cloudformation(
                router,
                case,
                "UpdateStack",
                &[
                    member("StackName", row.stack),
                    member("TemplateBody", &template),
                ],
            )
            .await;
        }
        Stage::UpdateTakenTable => {
            // The name is taken OUTSIDE the stack first, so the table the update declares is
            // one Amazon DynamoDB refuses to create.
            let table = format!("{}-taken", row.stack);
            dynamodb(
                router,
                case,
                "CreateTable",
                &json!({
                    "TableName": table,
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                }),
            )
            .await;
            let template = table_template(row.stack, row.resources, &table);
            cloudformation(
                router,
                case,
                "UpdateStack",
                &[
                    member("StackName", row.stack),
                    member("TemplateBody", &template),
                ],
            )
            .await;
        }
        Stage::Delete => {
            cloudformation(
                router,
                case,
                "DeleteStack",
                &[member("StackName", row.stack)],
            )
            .await;
            // A deleted stack is no longer addressable by NAME, so its conclusion is polled
            // by the id its create answered — which is how the row holds its stack id.
            return String::new();
        }
    }
    settled(router, case, row.stack).await
}

/// Every status change of a stack is published onto the default bus, reporting exactly what a
/// `DescribeStackEvents` of the same moment reports.
#[tokio::test]
async fn every_stack_status_change_reaches_the_rule_that_watches_the_bus() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so a bucket or stack name one row spells cannot decide
        // another row's deployment.
        let router = router();
        let url = wired_queue(
            &router,
            case,
            &format!("{}-bus", row.stack),
            &every_change_of(STACK_CHANGE),
        )
        .await;

        let mut stack_id = String::new();
        for (index, stage) in row.stages.iter().enumerate() {
            let settled_in = run_stage(&router, row, *stage).await;
            if stack_id.is_empty() {
                stack_id = stack_id_of(&router, case, row.stack).await;
            }
            let expected = row.settles_in[index];
            if *stage == Stage::Delete {
                // The delete's conclusion is read off the stack's own id: a deleted stack is
                // addressable by id, not by name.
                let reached = settled(&router, case, &stack_id).await;
                assert_eq!(
                    reached, expected,
                    "case {case:?}: stage {index} must settle in {expected:?}, got {reached:?}"
                );
            } else {
                assert_eq!(
                    settled_in, expected,
                    "case {case:?}: stage {index} must settle in {expected:?}, got \
                     {settled_in:?}"
                );
            }
        }

        // What the bus received, collected until the last status the row expects is on it.
        let last = *row.settles_in.last().expect("every row has a stage");
        let mut seen: Vec<Value> = Vec::new();
        collect_until(&router, case, &url, &mut seen, |seen| {
            seen.iter().any(|event| status_of(event) == last)
        })
        .await;

        // The stream on the bus IS the stack's own recorded stream: same statuses, same
        // order, nothing added and nothing missing.
        let recorded = recorded_events(&router, case, &stack_id).await;
        let expected_statuses: Vec<String> = recorded
            .iter()
            .filter(|event| event.resource_type == STACK_RESOURCE_TYPE)
            .map(|event| event.status.clone())
            .collect();
        let published_statuses = statuses_of(&seen);
        assert_eq!(
            published_statuses, expected_statuses,
            "case {case:?}: the bus must carry exactly the stack's own recorded statuses, in \
             order; got {published_statuses:?} against {expected_statuses:?}"
        );
        assert!(
            published_statuses.iter().any(|status| status == last),
            "case {case:?}: the terminal status {last:?} must be published, got \
             {published_statuses:?}"
        );

        // ...and every event carries the envelope and the detail a rule filters on.
        for event in &seen {
            let status = status_of(event).to_string();
            for (part, expected) in [
                ("source", json!(SOURCE)),
                ("detail-type", json!(STACK_CHANGE)),
                ("account", json!(ACCOUNT)),
                ("region", json!(REGION)),
                ("resources", json!([stack_id.clone()])),
            ] {
                assert_eq!(
                    event[part], expected,
                    "case {case:?}: the {status:?} event's {part} must be {expected}, got {event}"
                );
            }
            assert_eq!(
                event["detail"]["stack-id"],
                json!(stack_id.clone()),
                "case {case:?}: the {status:?} event must name its stack, got {event}"
            );
            assert_eq!(
                event["detail"]["logical-resource-id"],
                json!(row.stack),
                "case {case:?}: a stack's own event names the stack, got {event}"
            );
            assert_eq!(
                event["detail"]["resource-type"],
                json!(STACK_RESOURCE_TYPE),
                "case {case:?}: a stack's own event reports the stack's type, got {event}"
            );
            // The reason is the one the stack's own event recorded, empty where it recorded
            // none — so a subscriber reads the same sentence a poller does.
            let recorded_reason = recorded
                .iter()
                .find(|recorded| {
                    recorded.resource_type == STACK_RESOURCE_TYPE && recorded.status == status
                })
                .map(|recorded| recorded.status_reason.clone())
                .unwrap_or_default();
            assert_eq!(
                event["detail"]["status-details"]["status-reason"],
                json!(recorded_reason),
                "case {case:?}: the {status:?} event must carry the recorded reason \
                 {recorded_reason:?}, got {event}"
            );
        }
    }
}

/// A rule keyed on one exact status receives that status and no other — the recorded case,
/// where a failed create's `ROLLBACK_COMPLETE` fires the rule and a failed update's
/// `UPDATE_ROLLBACK_COMPLETE` does not.
#[tokio::test]
async fn an_exact_status_rule_takes_that_status_and_no_other() {
    let case = "an exact ROLLBACK_COMPLETE rule beside an exact UPDATE_ROLLBACK_COMPLETE one";
    let router = router();
    let rolled_back = wired_queue(
        &router,
        case,
        "cfnsse-exact-rollback",
        &only_the_status("ROLLBACK_COMPLETE"),
    )
    .await;
    let update_rolled_back = wired_queue(
        &router,
        case,
        "cfnsse-exact-update-rollback",
        &only_the_status("UPDATE_ROLLBACK_COMPLETE"),
    )
    .await;

    // A create that fails, so the stack rolls back to ROLLBACK_COMPLETE.
    let failed_create = Row {
        case,
        stack: "cfnsse-exact-created",
        resources: 1,
        stages: &[Stage::CreateBroken],
        settles_in: &["ROLLBACK_COMPLETE"],
    };
    let reached = run_stage(&router, &failed_create, Stage::CreateBroken).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: the failed create must roll back, got {reached:?}"
    );

    // ...and an update that fails on a stack that was created, so it rolls back to
    // UPDATE_ROLLBACK_COMPLETE.
    let failed_update = Row {
        case,
        stack: "cfnsse-exact-updated",
        resources: 1,
        stages: &[Stage::CreateHealthy, Stage::UpdateBroken],
        settles_in: &["CREATE_COMPLETE", "UPDATE_ROLLBACK_COMPLETE"],
    };
    let created = run_stage(&router, &failed_update, Stage::CreateHealthy).await;
    assert_eq!(
        created, "CREATE_COMPLETE",
        "case {case:?}: the stack under update must be created first, got {created:?}"
    );
    let updated = run_stage(&router, &failed_update, Stage::UpdateBroken).await;
    assert_eq!(
        updated, "UPDATE_ROLLBACK_COMPLETE",
        "case {case:?}: the failed update must roll back, got {updated:?}"
    );

    // Each rule took its OWN status: one event each, and nothing of the other's.
    let mut on_rollback: Vec<Value> = Vec::new();
    collect_until(&router, case, &rolled_back, &mut on_rollback, |seen| {
        !seen.is_empty()
    })
    .await;
    let mut on_update_rollback: Vec<Value> = Vec::new();
    collect_until(
        &router,
        case,
        &update_rolled_back,
        &mut on_update_rollback,
        |seen| !seen.is_empty(),
    )
    .await;

    assert_eq!(
        statuses_of(&on_rollback),
        vec!["ROLLBACK_COMPLETE".to_string()],
        "case {case:?}: the exact ROLLBACK_COMPLETE rule takes that status alone, got \
         {on_rollback:?}"
    );
    assert_eq!(
        statuses_of(&on_update_rollback),
        vec!["UPDATE_ROLLBACK_COMPLETE".to_string()],
        "case {case:?}: the exact UPDATE_ROLLBACK_COMPLETE rule takes that status alone, got \
         {on_update_rollback:?}"
    );
    let rolled_back_stack = stack_id_of(&router, case, failed_create.stack).await;
    let updated_stack = stack_id_of(&router, case, failed_update.stack).await;
    assert_eq!(
        on_rollback[0]["detail"]["stack-id"],
        json!(rolled_back_stack),
        "case {case:?}: the ROLLBACK_COMPLETE event is the failed create's, got {:?}",
        on_rollback[0]
    );
    assert_eq!(
        on_update_rollback[0]["detail"]["stack-id"],
        json!(updated_stack),
        "case {case:?}: the UPDATE_ROLLBACK_COMPLETE event is the failed update's, got {:?}",
        on_update_rollback[0]
    );
}

/// Each RESOURCE's status changes are published too, under their own `detail-type`, naming
/// the resource the change is about.
#[tokio::test]
async fn every_resource_status_change_reaches_its_own_detail_type() {
    let case = "a create that completes, watched through the resource-change rule";
    let router = router();
    let url = wired_queue(
        &router,
        case,
        "cfnsse-resources",
        &every_change_of(RESOURCE_CHANGE),
    )
    .await;

    let row = Row {
        case,
        stack: "cfnsse-resource-changes",
        resources: 2,
        stages: &[Stage::CreateHealthy],
        settles_in: &["CREATE_COMPLETE"],
    };
    let reached = run_stage(&router, &row, Stage::CreateHealthy).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: the create must complete, got {reached:?}"
    );
    let stack_id = stack_id_of(&router, case, row.stack).await;

    // Both buckets have to report their own CREATE_COMPLETE before the stream is compared.
    let mut seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &url, &mut seen, |seen| {
        seen.iter()
            .filter(|event| status_of(event) == "CREATE_COMPLETE")
            .count()
            >= row.resources
    })
    .await;

    let recorded = recorded_events(&router, case, &stack_id).await;
    let expected: Vec<(String, String)> = recorded
        .iter()
        .filter(|event| event.resource_type != STACK_RESOURCE_TYPE)
        .map(|event| (event.logical_resource_id.clone(), event.status.clone()))
        .collect();
    let published: Vec<(String, String)> = seen
        .iter()
        .map(|event| {
            (
                event["detail"]["logical-resource-id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                status_of(event).to_string(),
            )
        })
        .collect();
    assert_eq!(
        published, expected,
        "case {case:?}: the bus must carry exactly the resources' own recorded transitions, in \
         order; got {published:?} against {expected:?}"
    );
    assert!(
        !published.is_empty(),
        "case {case:?}: a create of {} resources publishes their transitions",
        row.resources
    );
    for event in &seen {
        assert_eq!(
            event["detail-type"],
            json!(RESOURCE_CHANGE),
            "case {case:?}: a resource's change carries its own detail-type, got {event}"
        );
        assert_eq!(
            event["detail"]["resource-type"],
            json!(BUCKET_TYPE),
            "case {case:?}: a resource's change reports that resource's type, got {event}"
        );
        assert_eq!(
            event["detail"]["stack-id"],
            json!(stack_id.clone()),
            "case {case:?}: a resource's change names the stack it belongs to, got {event}"
        );
    }
}
