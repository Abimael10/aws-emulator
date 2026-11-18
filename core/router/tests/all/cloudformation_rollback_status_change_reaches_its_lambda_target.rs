// SPDX-License-Identifier: CC0-1.0

//! The TERMINAL ROLLBACK status of a stack operation, observed where an operator observes it:
//! on the Lambda function an EventBridge rule on the account's default bus invokes, which
//! writes what it was handed into Amazon DynamoDB.
//!
//! The rule pinned here: **a stack operation that fails and rolls back publishes, onto the
//! account's default event bus, one `aws.cloudformation` / "CloudFormation Stack Status Change"
//! event whose `detail.status-details.status` is EXACTLY the terminal rollback status the stack
//! reached — `ROLLBACK_COMPLETE` for a create that failed, `UPDATE_ROLLBACK_COMPLETE` for an
//! update that failed — naming that stack's id in `detail.stack-id`; EventBridge matches the
//! event by its CONTENT, so a rule whose pattern pins that member to one whole status is handed
//! that status's event and NO event of another status, and the function target of such a rule,
//! admitted by its own resource policy, really runs on the envelope it was handed.** A string
//! operand of a pattern matches the whole value, so neither status is a match for the other
//! even though one spells a suffix of the other.
//!
//! Six crates have to agree for that to hold — CloudFormation runs the operation and publishes
//! the status it reached, the service that OWNS each declared resource is what refuses the one
//! that cannot be deployed, EventBridge matches and hands over, Lambda decides the invocation
//! against the function's policy and then RUNS it, DynamoDB serves the table the handler
//! writes, and IAM answers for the execution role — so the suite lives here rather than in any
//! one of them, and it runs over the whole `build_router` service set the binary serves.
//!
//! The rows differ along what the rule generalises over: WHICH terminal rollback status the
//! operation reaches, WHICH stack operation reached it (a create, an update), and WHICH service
//! refuses the declaration that fails — Amazon S3 on a notification destination it cannot find,
//! AWS Systems Manager on a parameter value its own `AllowedPattern` does not accept, and
//! Amazon DynamoDB on a table name already taken — so the rule is asserted over three
//! different owning services rather than over one failure mode. Each row carries its own
//! POSITIVE CONTROL: the exact-status rule for the status the row's stack really reached is what
//! says the event was published and routed at all, and the OTHER exact-status rule holding
//! nothing for that same stack is the near-miss being withheld rather than a look taken too
//! early.
//!
//! What is not asserted here, because a sibling suite asserts it:
//! [`crate::cloudformation_stack_status_change_events`] pins the whole published `detail`
//! against what a `DescribeStackEvents` of the same instant reports, through QUEUE targets.
//! What only this suite reaches is the chain an operator alarms on a failed deployment with: a
//! rollback's own event running a function whose write a later read reports.
//!
//! Nothing here waits on the wall clock: a stack's terminal status is reached by looking again
//! until it holds, and the table is read again until the item the delivery owes is in it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element, outputs_of};

/// The Region every call below is signed for, which is the Region the stacks belong to and the
/// Region their events are published in.
const REGION: &str = "us-east-1";

/// The account this emulator serves.
const ACCOUNT: &str = "000000000000";

/// The `source` and `detail-type` a stack's own status change is published under.
const SOURCE: &str = "aws.cloudformation";
const STACK_CHANGE: &str = "CloudFormation Stack Status Change";

/// The two terminal rollback statuses this suite is about, as `StackStatus` spells them.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The status a deployment that carried through reaches, which every row's first stage that is
/// an update has to reach before the update is asked for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The statuses a stack operation settles in: there is nothing more to wait for once the stack
/// reports one of them.
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

/// The names this suite's shared resources are created under.
const TABLE: &str = "cfn-rollback-landings";
const ROLE_NAME: &str = "cfn-rollback-writer-role";
const WRITERS_STACK: &str = "cfn-rollback-writers";

/// The two functions, and the MARK each writes its items behind: one per exact-status rule, so
/// WHICH rule's delivery wrote an item is readable from the item's own key.
const ROLLBACK_FUNCTION: &str = "cfn-rollback-complete-writer";
const UPDATE_ROLLBACK_FUNCTION: &str = "cfn-update-rollback-complete-writer";

/// The service principal EventBridge invokes a resource-policy-governed target as, and the
/// action such an invocation performs ("for Lambda functions, Amazon SNS topics and Amazon SQS
/// queues, EventBridge relies on resource-based policies", `PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// How long the handler is allowed to run, as `Timeout` carries it.
const FUNCTION_TIMEOUT: i64 = 10;

/// The event a bucket's notification configuration subscribes to. Which event it is does not
/// enter the rule: what matters is that Amazon S3 validates the DESTINATION when the
/// configuration is installed, so a configuration naming a queue it does not hold is refused
/// and the operation that asked for it fails.
const SUBSCRIBED_EVENT: &str = "s3:ObjectCreated:*";

/// How long an observation of work the clock owes waits before giving up, and how long between
/// two looks. Generous: the deadline is a safety net, never a measurement — each loop stops the
/// instant its condition holds.
const BUDGET: Duration = Duration::from_secs(120);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler both functions run: for the status-change event it is handed it writes one item
/// keyed by its own MARK, the status the event reports and the stack that reached it, carrying
/// the envelope members the delivery brought and ADDING to a counter, so a chain that handed the
/// same event over twice is readable too.
const WRITER_SOURCE: &str = r##"
import os

import boto3

CLIENT = boto3.client("dynamodb")
MARK = os.environ["MARK"]
TABLE = os.environ["TABLE"]


def handler(event, context):
    detail = event["detail"]
    status = detail["status-details"]["status"]
    key = MARK + "#" + status + "#" + detail["stack-id"]
    CLIENT.update_item(
        TableName=TABLE,
        Key={"pk": {"S": key}},
        UpdateExpression="SET #s = :s, #k = :k, #d = :d, #o = :o, #e = :e ADD #n :one",
        ExpressionAttributeNames={
            "#s": "status",
            "#k": "stack",
            "#d": "detail_type",
            "#o": "source",
            "#e": "event_id",
            "#n": "deliveries",
        },
        ExpressionAttributeValues={
            ":s": {"S": status},
            ":k": {"S": detail["stack-id"]},
            ":d": {"S": event["detail-type"]},
            ":o": {"S": event["source"]},
            ":e": {"S": event["id"]},
            ":one": {"N": "1"},
        },
    )
    return {"stored": key}
"##;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the table it writes is the table this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the service it belongs to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire, with whatever status it answered.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, Bytes) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), body)
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
    let (status, answered) = dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).into_owned();
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

/// One AWS Lambda call over its `restJson1` wire, with whatever status it answered.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let (status, answered) = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
    };
    (status, parsed)
}

/// One awsQuery call of `signing_name` at `host` that must answer 200, as its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with {text}"
    );
    text
}

/// One IAM call that must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)])
-> String {
    query(
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

/// One CloudFormation call that must answer 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    query(
        router,
        case,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// The table both handlers write, with one string hash key; answers the ARN DynamoDB reports.
async fn given_table(router: &MultiServiceRouter) -> String {
    let created = dynamodb(
        router,
        "setup",
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let arn = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateTable reports no TableArn, got {created}"))
        .to_string();
    // "You can perform read and write operations only on an ACTIVE table."
    let deadline = Instant::now() + BUDGET;
    loop {
        let described =
            dynamodb(router, "setup", "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: DynamoDB never reported {TABLE} ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The execution role both functions run as: trusted by Lambda, granting the one write their
/// handler makes. Answers its ARN as IAM reports it.
async fn given_role(router: &MultiServiceRouter, table_arn: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "setup",
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "dynamodb:UpdateItem",
            "Resource": table_arn,
        }],
    })
    .to_string();
    iam(
        router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-the-landings"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    arn
}

/// Deploy both writers in one stack, whose inline code CloudFormation archives into the
/// deployment package Lambda runs, and answer `(rollback writer arn, update rollback writer
/// arn)` once Lambda reports each function invocable.
async fn given_functions(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let function = |name: &str, mark: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": { "Variables": { "TABLE": TABLE, "MARK": mark } },
                "Code": { "ZipFile": WRITER_SOURCE },
            },
        })
    };
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "RollbackWriter": function(ROLLBACK_FUNCTION, ROLLBACK_COMPLETE),
            "UpdateRollbackWriter": function(UPDATE_ROLLBACK_FUNCTION, UPDATE_ROLLBACK_COMPLETE),
        },
        "Outputs": {
            "RollbackArn": { "Value": { "Fn::GetAtt": ["RollbackWriter", "Arn"] } },
            "UpdateRollbackArn": {
                "Value": { "Fn::GetAtt": ["UpdateRollbackWriter", "Arn"] },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "setup",
        "CreateStack",
        &[("StackName", WRITERS_STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = settled(router, "setup", WRITERS_STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the writers' stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let outputs = outputs_of(&described);
    for name in [ROLLBACK_FUNCTION, UPDATE_ROLLBACK_FUNCTION] {
        await_function_active(router, name).await;
    }
    let arn = |key: &str| {
        outputs
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the writers' stack publishes no {key}, {described}"))
    };
    (arn("RollbackArn"), arn("UpdateRollbackArn"))
}

/// Wait until Lambda first reports `name` `Active` — the state an invocation needs.
async fn await_function_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {name} Active, last read {status} {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// An enabled rule on the default bus whose pattern pins a stack status change to EXACTLY
/// `status`, with `function_arn` as its one target, and the statement on that FUNCTION that
/// admits the invocation the rule makes.
async fn given_exact_status_rule(
    router: &MultiServiceRouter,
    status: &str,
    function: &str,
    function_arn: &str,
) {
    let name = format!("cfn-exact-{}", status.to_ascii_lowercase());
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [STACK_CHANGE],
        "detail": { "status-details": { "status": [status] } },
    });
    let rule = events(
        router,
        "setup",
        "PutRule",
        &json!({
            "Name": &name,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: PutRule {name:?} reports no RuleArn, got {rule}"))
        .to_string();
    let wired = events(
        router,
        "setup",
        "PutTargets",
        &json!({
            "Rule": &name,
            "Targets": [{ "Id": "writer", "Arn": function_arn }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"].as_i64(),
        Some(0),
        "setup: the function target of {name:?} must be accepted, got {wired}"
    );
    let (status_code, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(&json!({
            "StatementId": format!("{name}-invoke"),
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        })),
    )
    .await;
    assert_eq!(
        status_code, 201,
        "setup: expected HTTP 201 from AddPermission for {name:?}, got {status_code} with {granted}"
    );
}

/// Look at the stack `name` until it reports a terminal status, and answer that status beside
/// the `DescribeStacks` answer that reported it.
///
/// The CONDITION is what is waited for, not a span: each look is a `DescribeStacks`, which is
/// also what materialises the transitions the stack's own clock owes and publishes what they owe
/// the bus.
async fn settled(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = describe(router, case, name).await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return (status, described);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack {name:?} never settled, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One `DescribeStacks` of `name`.
async fn describe(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    cloudformation(router, case, "DescribeStacks", &[("StackName", name)]).await
}

/// The item the table holds under `key` right now, through DynamoDB's own strongly consistent
/// read — `Value::Null` where it holds none.
async fn item(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let read = dynamodb(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { "pk": { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

/// The item the delivery of `status`'s event to the writer marked `mark` owes, looked for until
/// it is there.
///
/// Each look also asks CloudFormation about the stack, which is what drives the transitions the
/// stack's clock owes and the publication they owe the bus — so the wait is for the CONDITION
/// rather than for a span.
async fn await_item(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    mark: &str,
    status: &str,
    stack_id: &str,
) -> Value {
    let key = item_key(mark, status, stack_id);
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = item(router, case, &key).await;
        if held.is_object() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the {status} rule's Lambda target never wrote the item {key:?} the \
             published status change owes"
        );
        describe(router, case, stack).await;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The key one delivery writes its item under: the writer's own MARK, the status the event
/// reported and the stack that reached it.
fn item_key(mark: &str, status: &str, stack_id: &str) -> String {
    format!("{mark}#{status}#{stack_id}")
}

/// A template declaring one plain bucket named after `prefix`.
fn healthy_bucket(prefix: &str) -> String {
    json!({
        "Resources": {
            "Bucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": format!("{prefix}-0") },
            },
        },
    })
    .to_string()
}

/// A template whose bucket's notification names a queue Amazon SQS does not hold, so Amazon S3
/// refuses the configuration and the operation that asked for it fails.
///
/// "After Amazon S3 receives this request, it first verifies that any Amazon Simple Notification
/// Service (Amazon SNS) or Amazon Simple Queue Service (Amazon SQS) destination exists, and that
/// the bucket owner has permission to publish to it"
/// (`PutBucketNotificationConfiguration`).
fn bucket_with_a_missing_destination(prefix: &str) -> String {
    json!({
        "Resources": {
            "Bucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {
                    "BucketName": format!("{prefix}-0"),
                    "NotificationConfiguration": {
                        "QueueConfigurations": [{
                            "Queue": format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{prefix}-no-queue"),
                            "Event": SUBSCRIBED_EVENT,
                        }],
                    },
                },
            },
        },
    })
    .to_string()
}

/// A template declaring one parameter whose own `AllowedPattern` admits `value` or does not:
/// "AllowedPattern — a regular expression used to validate the parameter value"
/// (`PutParameter$AllowedPattern`), which Systems Manager itself is the judge of.
fn parameter_valued(prefix: &str, value: &str) -> String {
    json!({
        "Resources": {
            "Threshold": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": format!("/{prefix}/threshold"),
                    "Type": "String",
                    "Value": value,
                    "AllowedPattern": "^[0-9]+$",
                },
            },
        },
    })
    .to_string()
}

/// A template declaring one table by `table`: the smallest table Amazon DynamoDB accepts, so a
/// declaration of a name that service already holds is one it refuses to create.
fn table_named(table: &str) -> String {
    json!({
        "Resources": {
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": table,
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                },
            },
        },
    })
    .to_string()
}

/// How one row's deployment is made to fail — which service refuses the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failing {
    /// `CreateStack` of a bucket whose notification destination Amazon S3 cannot find.
    CreateRefusedByS3,
    /// `CreateStack` of a parameter Systems Manager accepts, then `UpdateStack` to a value its
    /// own `AllowedPattern` does not admit.
    UpdateRefusedBySystemsManager,
    /// `CreateStack` of a plain bucket, then `UpdateStack` adding a table whose name Amazon
    /// DynamoDB already holds.
    UpdateRefusedByDynamoDb,
}

/// One row: the stack, how its deployment fails, and the terminal rollback status that leaves it
/// in.
struct Row {
    case: &'static str,
    stack: &'static str,
    failing: Failing,
    rolls_back_to: &'static str,
}

/// The rows: one per way a rollback is reached — which operation fails, which service refuses it
/// and which terminal rollback status that is. The recorded cases are the first two.
const ROWS: &[Row] = &[
    Row {
        case: "a create Amazon S3 refuses, which rolls the stack back",
        stack: "cfn-rollback-created",
        failing: Failing::CreateRefusedByS3,
        rolls_back_to: ROLLBACK_COMPLETE,
    },
    Row {
        case: "an update Systems Manager refuses on the parameter value its pattern rejects",
        stack: "cfn-rollback-parameter",
        failing: Failing::UpdateRefusedBySystemsManager,
        rolls_back_to: UPDATE_ROLLBACK_COMPLETE,
    },
    Row {
        case: "an update Amazon DynamoDB refuses on a table name already taken",
        stack: "cfn-rollback-table",
        failing: Failing::UpdateRefusedByDynamoDb,
        rolls_back_to: UPDATE_ROLLBACK_COMPLETE,
    },
];

/// Run the row's deployment and answer `(the terminal status it settled in, the stack's id)`.
async fn run(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let case = row.case;
    match row.failing {
        Failing::CreateRefusedByS3 => {
            let template = bucket_with_a_missing_destination(row.stack);
            cloudformation(
                router,
                case,
                "CreateStack",
                &[("StackName", row.stack), ("TemplateBody", &template)],
            )
            .await;
        }
        Failing::UpdateRefusedBySystemsManager => {
            let accepted = parameter_valued(row.stack, "7");
            cloudformation(
                router,
                case,
                "CreateStack",
                &[("StackName", row.stack), ("TemplateBody", &accepted)],
            )
            .await;
            let (created, described) = settled(router, case, row.stack).await;
            assert_eq!(
                created, CREATE_COMPLETE,
                "case {case:?}: expected the parameter's stack to deploy, got {described}"
            );
            let refused = parameter_valued(row.stack, "seven");
            cloudformation(
                router,
                case,
                "UpdateStack",
                &[("StackName", row.stack), ("TemplateBody", &refused)],
            )
            .await;
        }
        Failing::UpdateRefusedByDynamoDb => {
            let template = healthy_bucket(row.stack);
            cloudformation(
                router,
                case,
                "CreateStack",
                &[("StackName", row.stack), ("TemplateBody", &template)],
            )
            .await;
            let (created, described) = settled(router, case, row.stack).await;
            assert_eq!(
                created, CREATE_COMPLETE,
                "case {case:?}: expected the bucket's stack to deploy, got {described}"
            );
            // The name is taken OUTSIDE the stack first, so the table the update declares is one
            // Amazon DynamoDB refuses to create.
            let table = format!("{}-taken", row.stack);
            dynamodb(
                router,
                case,
                "CreateTable",
                &json!({
                    "TableName": &table,
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                }),
            )
            .await;
            let refused = table_named(&table);
            cloudformation(
                router,
                case,
                "UpdateStack",
                &[("StackName", row.stack), ("TemplateBody", &refused)],
            )
            .await;
        }
    }
    let (status, described) = settled(router, case, row.stack).await;
    (status, element(&described, "StackId"))
}

/// Every terminal rollback status reaches the function of the rule that names EXACTLY it, and
/// the function of the rule that names the other rollback status is handed nothing.
#[tokio::test]
async fn a_rollback_status_change_runs_the_exact_status_rules_function() {
    let router = emulator();
    let table_arn = given_table(&router).await;
    let role_arn = given_role(&router, &table_arn).await;
    let (rollback_arn, update_rollback_arn) = given_functions(&router, &role_arn).await;
    given_exact_status_rule(&router, ROLLBACK_COMPLETE, ROLLBACK_FUNCTION, &rollback_arn).await;
    given_exact_status_rule(
        &router,
        UPDATE_ROLLBACK_COMPLETE,
        UPDATE_ROLLBACK_FUNCTION,
        &update_rollback_arn,
    )
    .await;

    for row in ROWS {
        let case = row.case;
        let (reached, stack_id) = run(&router, row).await;
        assert_eq!(
            reached, row.rolls_back_to,
            "case {case:?}: expected the operation to roll back to {}, got {reached}",
            row.rolls_back_to
        );

        // The rule that names exactly the status the stack reached: its function ran, on the
        // envelope this stack's change was published as.
        let written = await_item(
            &router,
            case,
            row.stack,
            row.rolls_back_to,
            row.rolls_back_to,
            &stack_id,
        )
        .await;
        for (member, expected) in [
            ("status", row.rolls_back_to),
            ("stack", stack_id.as_str()),
            ("detail_type", STACK_CHANGE),
            ("source", SOURCE),
        ] {
            let actual = written[member]["S"].as_str().unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the delivered {member} to be {expected:?}, got \
                 {actual:?} in {written}"
            );
        }
        let event_id = written["event_id"]["S"].as_str().unwrap_or_default();
        assert!(
            !event_id.is_empty(),
            "case {case:?}: the delivered envelope must carry an id, got {written}"
        );
        let deliveries = written["deliveries"]["N"].as_str().unwrap_or_default();
        assert_eq!(
            deliveries, "1",
            "case {case:?}: the matching rule hands the event over once, got {deliveries:?} in \
             {written}"
        );

        // ...and the rule that names the OTHER rollback status was handed nothing about this
        // stack: a string operand matches the whole value, so neither status matches the other.
        let other = if row.rolls_back_to == ROLLBACK_COMPLETE {
            UPDATE_ROLLBACK_COMPLETE
        } else {
            ROLLBACK_COMPLETE
        };
        for mark in [ROLLBACK_COMPLETE, UPDATE_ROLLBACK_COMPLETE] {
            if mark == row.rolls_back_to {
                continue;
            }
            let near_miss = item(&router, case, &item_key(mark, row.rolls_back_to, &stack_id)).await;
            assert_eq!(
                near_miss,
                Value::Null,
                "case {case:?}: the rule pinned to {mark} must not take a {} event, got \
                 {near_miss}",
                row.rolls_back_to
            );
        }
        let unreached = item(&router, case, &item_key(other, other, &stack_id)).await;
        assert_eq!(
            unreached,
            Value::Null,
            "case {case:?}: this stack never reached {other}, so nothing may be written for it, \
             got {unreached}"
        );
    }
}
