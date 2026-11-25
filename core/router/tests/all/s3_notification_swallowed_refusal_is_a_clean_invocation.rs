// SPDX-License-Identifier: CC0-1.0

//! What AWS Lambda RECORDS for an invocation it made itself — the asynchronous one Amazon S3
//! makes to deliver an object-created event to a bucket's notification target — when the
//! handler's own call into another service was REFUSED and the handler caught it.
//!
//! The rule under test: **the outcome of an invocation is the HANDLER's own, not the outcome
//! of the calls the handler made: a run that returned is recorded as a successful invocation
//! and a run whose handler let an exception propagate is recorded as a function error. So a
//! call one service refused — a conditional write whose condition no longer holds, refused and
//! writing nothing — leaves, when the handler catches it and returns, a CLEAN invocation
//! record: the completed event's record goes to the `DestinationConfig$OnSuccess` destination
//! carrying `requestContext.condition` `Success` and a `responseContext` with no
//! `functionError`, and `AWS/Lambda` carries one `Invocations` observation for the run beside
//! an `Errors` observation of 0 — while the item the refused write addressed stands exactly as
//! it did before. The same refusal, left to propagate, is recorded the other way: the record
//! goes to `DestinationConfig$OnFailure` with the condition of an event Lambda gave up on and
//! a `responseContext.functionError`, and the item is still untouched.**
//!
//! Each half of that is the owning service's own documented contract:
//! `DestinationConfig$OnSuccess` is "the destination configuration for successful invocations"
//! and `$OnFailure` "the destination configuration for failed invocations"
//! (`smithy_query(query="shape",shape="DestinationConfig")`); `InvocationResponse$FunctionError`
//! holds "Unhandled" for an error the handler did not handle; `Errors` is "the number of
//! invocations that result in a function error", which are "exceptions thrown by your code and
//! exceptions thrown by the Lambda runtime" (the AWS Lambda metric reference) — so a refusal
//! the code handled is neither; and a `PutItem` whose `ConditionExpression` does not hold is
//! answered `ConditionalCheckFailedException` and writes nothing
//! (`smithy_query(query="operation",operation="PutItem")`).
//!
//! Five crates have to agree for that to hold — Amazon S3 evaluates the stored configuration
//! and makes the invocation, AWS Lambda runs the handler, records the outcome and delivers the
//! invocation record, IAM answers for the execution role the handler's call and the record's
//! delivery are made as, Amazon DynamoDB decides the conditional write, and Amazon SQS holds
//! the destinations the records land on — so the suite lives here: a crate-local suite can
//! only ever see one side of it. Its siblings reach the neighbouring halves and this one does
//! not repeat them: [`crate::s3_notification_driven_conditional_label_write`] pins the STALE
//! ROW a swallowed refusal leaves and the marker the handler wrote for what it caught,
//! [`crate::s3_notification_delivers_once_per_object_write`] pins one delivery per write, and
//! [`crate::cloudformation_provisions_lambda_event_invoke_configs`] pins the destination slots
//! over a directly invoked function. What only this suite reaches is the INVOCATION RECORD of
//! an S3-delivered event whose handler swallowed another service's refusal — the one
//! observation that tells a silently dropped write from a successful one.
//!
//! The rows differ along what the rule generalises over: whether the addressed item exists
//! before the event (so whether the handler's conditional write is served or refused), what
//! the handler does with a refusal (catches it and returns, or lets it propagate), and how
//! many times the key is written (one, two, three) — so a key is driven through "served",
//! "refused and caught" and "refused and raised" in one run of the same handler. Every row
//! observes the invocation where a client observes an asynchronous one: the record on the
//! destination queue and the `AWS/Lambda` series, beside the item Amazon DynamoDB holds.
//!
//! Every request is dispatched through a BOXED future ([`Box::pin`]). The assembled router's
//! dispatch future carries the whole emulator's handler state inline, so a test holding
//! several of them across await points lays that state out on the test thread's stack and
//! overflows it as the emulator grows; the box puts each one on the heap instead.

use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
// The sibling notification suite's own harness: the router as it is ASSEMBLED, the
// `Authorization` header that routes a request, an awsQuery call, S3's REST surface and the
// sized thread a chain of in-process calls needs.
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, driven, query, s3,
};

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the one it
/// reads.
const REGION: &str = "us-east-1";

/// The account every ARN written here names: the one this emulator mints resources under.
const ACCOUNT: &str = "000000000000";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` the handler stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where AWS Lambda binds the function operations this suite calls, and where it binds the
/// asynchronous-invocation configuration
/// (`smithy_query(query="operation",operation="PutFunctionEventInvokeConfig")`:
/// `PUT /2019-09-25/functions/{FunctionName}/event-invoke-config`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The qualifier the configuration below belongs to and the record names: the unpublished
/// version, which is what an unqualified invocation runs.
const UNPUBLISHED: &str = "$LATEST";

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class the stored configurations are notified about, and the `eventName` a put is
/// published under.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The one bucket, table and stack this suite brings up, and the two functions it deploys:
/// one that CATCHES the refusal its conditional write earns, one that lets it propagate.
const BUCKET: &str = "clean-invocation-images";
const TABLE: &str = "clean-invocation-labels";
const STACK: &str = "clean-invocation-estate";
const SWALLOWING_FUNCTION: &str = "clean-invocation-catcher";
const RAISING_FUNCTION: &str = "clean-invocation-raiser";

/// The key prefix each stored configuration admits — one per function, so one bucket drives
/// both handlers and a key says which one its writes reach.
const SWALLOW_PREFIX: &str = "swallow/";
const RAISE_PREFIX: &str = "raise/";

/// The queues the two functions' asynchronous-invocation configurations name, one pair each.
const SWALLOW_SUCCESS_QUEUE: &str = "clean-invocation-catcher-success";
const SWALLOW_FAILURE_QUEUE: &str = "clean-invocation-catcher-failure";
const RAISE_SUCCESS_QUEUE: &str = "clean-invocation-raiser-success";
const RAISE_FAILURE_QUEUE: &str = "clean-invocation-raiser-failure";

/// The key attribute the table is keyed on, and the attributes the handler writes beside it.
const KEY_ATTRIBUTE: &str = "image_name";
const EVENT_ATTRIBUTE: &str = "event_name";
const SIZE_ATTRIBUTE: &str = "object_size";

/// The modelled DynamoDB refusal of a write whose condition does not hold, which is the class
/// the handler catches (`smithy_query(query="operation",operation="PutItem")`).
const CONDITION_FAILED: &str = "ConditionalCheckFailedException";

/// The `requestContext.condition` an invocation record carries for an event whose handler
/// RETURNED, and for one Lambda gave up on because every attempt it was allowed failed — the
/// two conditions the documented record of a completed event carries.
const CONDITION_SUCCESS: &str = "Success";
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The value `FunctionError` carries for an error the handler did not handle
/// (`InvocationResponse$FunctionError`: "Unhandled" means "the function had an error").
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// The status an invocation that EXECUTED reports in a record's `responseContext`, whether the
/// handler returned or raised.
const EXECUTED_STATUS: i64 = 200;

/// How many delivery attempts each event here gets: one. `MaximumRetryAttempts` is `@range(min:
/// 0, max: 2)` (`smithy_query(query="operation",operation="PutFunctionEventInvokeConfig")`), and
/// a row that asserts what ONE run left needs the count the record reports to be a known
/// number rather than however many retries a failure happened to take.
const RETRY_ATTEMPTS: i64 = 0;
const INVOKE_COUNT: i64 = 1;

/// The CloudWatch namespace AWS Lambda publishes its own series in, the two series an
/// invocation's own outcome is read from, the dimension they are keyed by and the unit the
/// metric reference gives both.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const INVOCATIONS_SERIES: &str = "Invocations";
const ERRORS_SERIES: &str = "Errors";
const FUNCTION_DIMENSION: &str = "FunctionName";
const COUNT_UNIT: &str = "Count";

/// The window a reader asks those series over, and the period it asks for: wide enough that
/// every observation of this suite falls inside one read, so nothing here depends on which
/// minute a run landed in.
const WINDOW_SECS: i64 = 900;
const PERIOD_SECS: i64 = 60;

/// How long one invocation of a handler is given, as the function's own `Timeout`: generous,
/// because nothing here asserts how long a run took — only what it left.
const FUNCTION_TIMEOUT: u64 = 120;

/// The provisioned capacity the table is created with. The read side is ample because this
/// suite POLLS the table for what a delivery left, several times a second: a provisioned table
/// admits its rate plus the capacity that went unused behind it, so a poll loop faster than the
/// rate would be throttled — which is a property of the loop and not of what it waits for.
const READ_CAPACITY: u64 = 400;
const WRITE_CAPACITY: u64 = 5;

/// How long this suite looks for work another service's clock owes — a stack settling, a
/// function reported `Active`, a table reported `ACTIVE`, the asynchronous delivery of an
/// object event, the handler's own call and the invocation record that follows it — and how
/// often it looks while waiting. Every wait is on a CONDITION; nothing asserts how long one
/// took.
const WAIT: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long a check that must see NOTHING FURTHER keeps looking: well past the span a
/// delivered row of the same wiring takes to leave its record.
const QUIET_WINDOW: Duration = Duration::from_secs(8);

/// The handler both functions deploy, in the `nodejs20.x` runtime they are declared with: for
/// every record it is handed it writes what that record named about the object CONDITIONALLY —
/// only where the key carries no row yet — inside a try/catch, and ends by returning what it
/// recorded and what it caught, so the invocation record's own `responsePayload` says which
/// branch the run took. The catch block RE-RAISES where the function's environment says to,
/// which is the only difference between the two functions this suite deploys.
const CONDITIONAL_HANDLER: &str = r#"
const { DynamoDBClient, PutItemCommand } = require('@aws-sdk/client-dynamodb');

const ITEMS = new DynamoDBClient({});

exports.handler = async (event) => {
    const recorded = [];
    const caught = [];
    for (const record of event.Records) {
        const key = record.s3.object.key;
        console.log('recording ' + record.s3.bucket.name + '/' + key);
        try {
            await ITEMS.send(new PutItemCommand({
                TableName: process.env.TABLE_NAME,
                Item: {
                    image_name: { S: key },
                    event_name: { S: record.eventName },
                    object_size: { N: String(record.s3.object.size) },
                },
                ConditionExpression: 'attribute_not_exists(image_name)',
            }));
            recorded.push(key);
        } catch (refused) {
            console.log('caught ' + refused.name + ' for ' + key);
            caught.push(refused.name);
            if (process.env.RETHROW === 'yes') {
                throw refused;
            }
        }
    }
    return { recorded: recorded, caught: caught };
};
"#;

// ---------------------------------------------------------------------------
// The wires this suite dispatches on, beside the shared S3 and awsQuery ones
// ---------------------------------------------------------------------------

/// One JSON-RPC call of `signing_name`, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "dynamodb",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        &format!("DynamoDB_20120810.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
    .await
}

/// One Amazon SQS call over its `awsJson1_0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "sqs",
        &format!("sqs.{REGION}.amazonaws.com"),
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
    .await
}

/// One CloudWatch call over its `awsJson1_0` wire.
async fn cloudwatch(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "monitoring",
        &format!("monitoring.{REGION}.amazonaws.com"),
        &format!("GraniteServiceVersion20100801.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

// ---------------------------------------------------------------------------
// The estate
// ---------------------------------------------------------------------------

/// The ARN a queue of `name` carries in this suite's Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN a function of `name` carries.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// Create one queue and answer the URL a reader receives from.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue({name:?}) must succeed, got {status} with {created}"
    );
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue({name:?}) must answer a URL"))
        .to_string()
}

/// The template the handler stack deploys: the execution role the handler's own call and the
/// delivery of its invocation record are made as, and the two inline `nodejs20.x` functions
/// whose source CloudFormation archives into a deployment package this emulator can run.
fn handler_stack() -> String {
    let function = |name: &str, rethrows: bool| {
        let mut variables = json!({ "TABLE_NAME": TABLE });
        if rethrows {
            variables["RETHROW"] = json!("yes");
        }
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "nodejs20.x",
                "Handler": "index.handler",
                "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": { "Variables": variables },
                "Code": { "ZipFile": CONDITIONAL_HANDLER },
            },
        })
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The handler writes the table with temporary credentials for this role,
                    // and AWS Lambda delivers each invocation record ACTING AS the same role,
                    // so the role has to grant both writes.
                    "Policies": [{
                        "PolicyName": "record-and-report",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "dynamodb:PutItem",
                                    "sqs:SendMessage",
                                    "logs:CreateLogGroup",
                                    "logs:CreateLogStream",
                                    "logs:PutLogEvents",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Catcher": function(SWALLOWING_FUNCTION, false),
            "Raiser": function(RAISING_FUNCTION, true),
        },
        "Outputs": {
            "CatcherArn": { "Value": { "Fn::GetAtt": ["Catcher", "Arn"] } },
            "RaiserArn": { "Value": { "Fn::GetAtt": ["Raiser", "Arn"] } },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until AWS Lambda first reports `function` `Active` — "you
/// cannot invoke … while the function is in the Pending state" — and answer the state it
/// reported last. The span is that service's own, so nothing here sleeps on its behalf.
async fn awaited_function_state(router: &MultiServiceRouter, function: &str) -> String {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await;
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == "Active" || Instant::now() >= giving_up_at {
            return state;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Poll `DescribeTable` until Amazon DynamoDB first reports `table` `ACTIVE` — "you can perform
/// read and write operations only on an ACTIVE table" — and answer the status it reported last.
async fn awaited_table_status(router: &MultiServiceRouter, table: &str) -> String {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let (_, described) = dynamodb(router, "DescribeTable", &json!({ "TableName": table })).await;
        let status = described["Table"]["TableStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" || Instant::now() >= giving_up_at {
            return status;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// `AddPermission`: the grant Amazon S3 reads before it records a configuration naming
/// `function`, scoped to the one bucket whose events it publishes.
async fn grant_invoke(router: &MultiServiceRouter, case: &str, function: &str) {
    let (status, body) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        Some(&json!({
            "StatementId": "allow-this-bucket",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission({function:?}) must succeed, got {status} with {body}"
    );
}

/// `PutFunctionEventInvokeConfig`: the asynchronous-invocation configuration whose destinations
/// the invocation records of this function's completed events are delivered to.
async fn configure_destinations(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_success: &str,
    on_failure: &str,
) {
    let (status, body) = lambda(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config"),
        Some(&json!({
            "DestinationConfig": {
                "OnSuccess": { "Destination": on_success },
                "OnFailure": { "Destination": on_failure },
            },
            "MaximumRetryAttempts": RETRY_ATTEMPTS,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionEventInvokeConfig({function:?}) must succeed, got {status} \
         with {body}"
    );
}

/// `PUT /<bucket>?notification` with one Lambda configuration per function, each admitting its
/// own key prefix.
///
/// The elements are the ones Amazon S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_notification_configuration(router: &MultiServiceRouter, case: &str) {
    let configuration = |id: &str, arn: String, prefix: &str| {
        format!(
            "<CloudFunctionConfiguration><Id>{id}</Id><CloudFunction>{arn}</CloudFunction>\
             <Event>{OBJECT_CREATED}</Event><Filter><S3Key><FilterRule><Name>prefix</Name>\
             <Value>{prefix}</Value></FilterRule></S3Key></Filter>\
             </CloudFunctionConfiguration>"
        )
    };
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{}{}</NotificationConfiguration>",
        configuration(
            "catch-the-refusal",
            function_arn(SWALLOWING_FUNCTION),
            SWALLOW_PREFIX
        ),
        configuration("raise-the-refusal", function_arn(RAISING_FUNCTION), RAISE_PREFIX),
    );
    let answer = s3(router, "PUT", &format!("/{BUCKET}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({BUCKET:?}) must succeed, got {} \
         with {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`, accepted whatever the notification configuration then
/// does with the event.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &str) {
    let answer = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({BUCKET:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// What the table holds under `key` right now, as the item a client reads — `None` when it
/// holds no item at all for that key.
async fn held(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Value> {
    let (status, read) = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { KEY_ATTRIBUTE: { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetItem({TABLE:?}, {key:?}) must succeed, got {status} with {read}"
    );
    read.get("Item").cloned()
}

/// Write the item a row pre-seeds: the row a client put there BEFORE any event, whose presence
/// is what the handler's own conditional write then falls foul of.
async fn seed_item(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let item = json!({
        KEY_ATTRIBUTE: { "S": key },
        EVENT_ATTRIBUTE: { "S": "seeded-before-any-event" },
        SIZE_ATTRIBUTE: { "N": "0" },
    });
    let (status, written) = dynamodb(
        router,
        "PutItem",
        &json!({ "TableName": TABLE, "Item": item }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutItem({TABLE:?}, {key:?}) must succeed, got {status} with {written}"
    );
    held(router, case, key)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the seeded row for {key:?} must be readable"))
}

/// The item the handler's own served write leaves for `key`: what the published record named
/// about the object that was written.
fn handler_row(key: &str, body: &str) -> Value {
    json!({
        KEY_ATTRIBUTE: { "S": key },
        EVENT_ATTRIBUTE: { "S": PUT_EVENT_NAME },
        SIZE_ATTRIBUTE: { "N": body.len().to_string() },
    })
}

// ---------------------------------------------------------------------------
// The invocation records, read off the destination queues
// ---------------------------------------------------------------------------

/// Take every message the queue `url` holds right now off it, parsed as the invocation records
/// they are, and delete each one so a later read cannot count it twice.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage({url:?}) must succeed, got {status} with {received}"
    );
    let mut records = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage({url:?}) must succeed, got {status} with {deleted}"
            );
        }
        let body = message["Body"].as_str().unwrap_or_default();
        records.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: an invocation record must be a document ({error}), got {body:?}")
        }));
    }
    records
}

/// The key the event an invocation record is about named, read out of the record's own
/// `requestPayload` — which is the `Records` envelope Amazon S3 delivered.
fn record_key(record: &Value) -> String {
    record["requestPayload"]["Records"][0]["s3"]["object"]["key"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look at `url` until it has delivered a record about `key`, taking everything it holds off it
/// into `seen` as it goes, and answer that record. The wait is on that CONDITION; nothing
/// asserts how long it took.
async fn awaited_record(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut Vec<Value>,
    key: &str,
) -> Value {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        seen.extend(drained(router, case, url).await);
        if let Some(position) = seen.iter().position(|record| record_key(record) == key) {
            return seen.remove(position);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected an invocation record about {key:?} on {url:?}, got \
             {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Keep looking at `url` through [`QUIET_WINDOW`] and answer every record it delivered in it.
async fn quietly_drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let giving_up_at = Instant::now() + QUIET_WINDOW;
    let mut seen = Vec::new();
    while Instant::now() < giving_up_at {
        seen.extend(drained(router, case, url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

/// What one series of `function` holds over the whole window: the sum of its datapoints, and
/// how many observations they were folded from.
///
/// A series with no datapoint at all sums to zero and reports no observation, which is how
/// "this function was never invoked" reads — told apart from "invoked and failed nothing",
/// which reports observations summing to zero.
async fn series(router: &MultiServiceRouter, case: &str, function: &str, metric: &str) -> (f64, f64) {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default(),
    )
    .unwrap_or_default();
    let stats = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": LAMBDA_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": function }],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount"],
            "Unit": COUNT_UNIT,
        }),
    )
    .await;
    let datapoints = stats["Datapoints"].as_array().cloned().unwrap_or_default();
    (
        datapoints
            .iter()
            .filter_map(|point| point["Sum"].as_f64())
            .sum(),
        datapoints
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum(),
    )
}

/// Look at a series until it has folded at least `wanted` observations, and answer what it
/// then holds.
async fn awaited_series(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    metric: &str,
    wanted: f64,
) -> (f64, f64) {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let held = series(router, case, function, metric).await;
        if held.1 >= wanted || Instant::now() >= giving_up_at {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// One row: the key written into the bucket, the bytes each write of it carries, whether the
/// table already holds a row for that key, and which handler the key's prefix reaches.
struct Row {
    case: &'static str,
    key: &'static str,
    /// The body of each write, one per write — each of its own length, so the record published
    /// for a write names a size no other write of the row shares.
    bodies: &'static [&'static str],
    /// Whether a client seeded a row for this key before any event was published.
    seeded: bool,
    /// Whether the handler the key's prefix reaches RE-RAISES the refusal it catches.
    rethrows: bool,
}

impl Row {
    /// The function this row's writes run, which is the one its key prefix admits.
    fn function(&self) -> &'static str {
        if self.rethrows {
            RAISING_FUNCTION
        } else {
            SWALLOWING_FUNCTION
        }
    }
}

/// The rows span what the rule generalises over: whether the addressed item exists before the
/// event, what the handler does with a refusal, and how many times the key is written — so one
/// run of each handler covers a conditional write that is SERVED, one that is REFUSED and
/// caught, and one that is REFUSED and raised.
const ROWS: &[Row] = &[
    Row {
        case: "a pre-existing row, the handler catching the refusal, written once",
        key: "swallow/portrait.jpg",
        bodies: &["the first upload"],
        seeded: true,
        rethrows: false,
    },
    Row {
        case: "a pre-existing row, the handler catching the refusal, written three times",
        key: "swallow/landscape.jpeg",
        bodies: &[
            "the first upload",
            "the second upload, longer",
            "the third upload, longer again",
        ],
        seeded: true,
        rethrows: false,
    },
    Row {
        case: "no pre-existing row, so the first write is served and the second refused and \
               caught",
        key: "swallow/diagram.png",
        bodies: &["the only served upload", "the refused re-upload, longer"],
        seeded: false,
        rethrows: false,
    },
    Row {
        case: "a pre-existing row, the handler raising the refusal, written once",
        key: "raise/portrait.jpg",
        bodies: &["the first upload"],
        seeded: true,
        rethrows: true,
    },
    Row {
        case: "no pre-existing row, so the first write is served and the second raises",
        key: "raise/diagram.png",
        bodies: &["the only served upload", "the raising re-upload, longer"],
        seeded: false,
        rethrows: true,
    },
];

/// RULE: the outcome AWS Lambda records for an asynchronous invocation it made to deliver an
/// object event is the HANDLER's own — a refusal the handler caught leaves a clean invocation
/// record and the refused write's item exactly as it was, while the same refusal left to
/// propagate is recorded as a function error and still writes nothing.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_swallowed_refusal_leaves_a_clean_invocation_and_an_untouched_item() {
    driven("s3-notification-swallowed-refusal", || {
        Box::pin(a_swallowed_refusal_leaves_a_clean_invocation_and_an_untouched_item_body())
    });
}

async fn a_swallowed_refusal_leaves_a_clean_invocation_and_an_untouched_item_body() {
    let case = "the notification-driven conditional-write estate";
    let router = build_router();

    // 1. The destinations the invocation records are delivered to, one pair per function.
    let swallow_success = given_queue(&router, case, SWALLOW_SUCCESS_QUEUE).await;
    let swallow_failure = given_queue(&router, case, SWALLOW_FAILURE_QUEUE).await;
    let raise_success = given_queue(&router, case, RAISE_SUCCESS_QUEUE).await;
    let raise_failure = given_queue(&router, case, RAISE_FAILURE_QUEUE).await;

    // 2. The table the handler writes, ready before anything is published to it.
    let (status, created) = dynamodb(
        &router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
            "ProvisionedThroughput": {
                "ReadCapacityUnits": READ_CAPACITY,
                "WriteCapacityUnits": WRITE_CAPACITY,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTable({TABLE:?}) must succeed, got {status} with {created}"
    );
    let table_status = awaited_table_status(&router, TABLE).await;
    assert_eq!(
        table_status, "ACTIVE",
        "case {case:?}: expected Amazon DynamoDB to report {TABLE:?} ACTIVE, got \
         {table_status:?}"
    );

    // 3. The two handlers, whose inline source one stack archives into runnable packages.
    let template = handler_stack();
    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let (reported, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the handler stack to deploy, got {reported:?} in {described}"
    );
    let outputs = outputs_of(&described);
    assert_eq!(
        (
            outputs.get("CatcherArn").map(String::as_str),
            outputs.get("RaiserArn").map(String::as_str),
        ),
        (
            Some(function_arn(SWALLOWING_FUNCTION).as_str()),
            Some(function_arn(RAISING_FUNCTION).as_str()),
        ),
        "case {case:?}: expected the stack to publish both function ARNs, got {outputs:?}"
    );
    for function in [SWALLOWING_FUNCTION, RAISING_FUNCTION] {
        let state = awaited_function_state(&router, function).await;
        assert_eq!(
            state, "Active",
            "case {case:?}: expected AWS Lambda to report {function:?} Active, got {state:?}"
        );
    }

    // 4. Where each function's completed events report what became of them, and the grant
    //    Amazon S3 needs before it will record a configuration naming them.
    configure_destinations(
        &router,
        case,
        SWALLOWING_FUNCTION,
        &queue_arn(SWALLOW_SUCCESS_QUEUE),
        &queue_arn(SWALLOW_FAILURE_QUEUE),
    )
    .await;
    configure_destinations(
        &router,
        case,
        RAISING_FUNCTION,
        &queue_arn(RAISE_SUCCESS_QUEUE),
        &queue_arn(RAISE_FAILURE_QUEUE),
    )
    .await;
    grant_invoke(&router, case, SWALLOWING_FUNCTION).await;
    grant_invoke(&router, case, RAISING_FUNCTION).await;

    // 5. The bucket, and the configuration that names one function per key prefix.
    create_bucket(&router, case, BUCKET).await;
    put_notification_configuration(&router, case).await;

    // Every row's pre-existing row is written BEFORE any event is published, so no handler can
    // have put it there.
    let mut seeded: Vec<(&str, Value)> = Vec::new();
    for row in ROWS.iter().filter(|row| row.seeded) {
        seeded.push((row.key, seed_item(&router, row.case, row.key).await));
    }

    // What has been read off each queue but belongs to another row's key, kept so a record
    // delivered early is never lost to a later read.
    let mut pending_success: Vec<Value> = Vec::new();
    let mut pending_failure: Vec<Value> = Vec::new();
    // How many invocations each function has had, and how many of them came to a function
    // error: what its own series must then report.
    let mut invocations = [0.0_f64, 0.0_f64];
    let mut errors = [0.0_f64, 0.0_f64];

    for row in ROWS {
        let case = row.case;
        let function = row.function();
        let slot = usize::from(row.rethrows);
        let (success, failure) = if row.rethrows {
            (&raise_success, &raise_failure)
        } else {
            (&swallow_success, &swallow_failure)
        };

        for (index, body) in row.bodies.iter().enumerate() {
            let write = index + 1;
            // A key with no row yet has its FIRST write served; every later write of any key,
            // and every write of a seeded key, finds a row already there.
            let refused = row.seeded || index > 0;
            let failing = refused && row.rethrows;
            let before = held(&router, case, row.key).await;

            put_object(&router, case, row.key, body).await;

            let (url, pending) = if failing {
                (failure, &mut pending_failure)
            } else {
                (success, &mut pending_success)
            };
            let record = awaited_record(&router, case, url, pending, row.key).await;

            // The record is about THIS invocation of THIS function, and it reports the event
            // Amazon S3 delivered: one record of the written key.
            let stated: &[(&str, Value)] = &[
                (
                    "/requestContext/functionArn",
                    json!(format!("{}:{UNPUBLISHED}", function_arn(function))),
                ),
                (
                    "/requestContext/condition",
                    json!(if failing {
                        CONDITION_RETRIES_EXHAUSTED
                    } else {
                        CONDITION_SUCCESS
                    }),
                ),
                ("/requestContext/approximateInvokeCount", json!(INVOKE_COUNT)),
                ("/requestPayload/Records/0/eventName", json!(PUT_EVENT_NAME)),
                (
                    "/requestPayload/Records/0/s3/bucket/name",
                    json!(BUCKET),
                ),
                (
                    "/requestPayload/Records/0/s3/object/size",
                    json!(body.len()),
                ),
                ("/responseContext/statusCode", json!(EXECUTED_STATUS)),
            ];
            for (pointer, expected) in stated {
                let reported = record.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    &reported, expected,
                    "case {case:?}: write {write} of {:?}: expected {pointer} to be {expected}, \
                     got {record}",
                    row.key
                );
            }
            assert!(
                record
                    .pointer("/requestContext/requestId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty()),
                "case {case:?}: write {write} of {:?}: a record names the invocation it is \
                 about, got {record}",
                row.key
            );

            let function_error = record
                .pointer("/responseContext/functionError")
                .and_then(Value::as_str);
            if failing {
                // The refusal was left to propagate, so the run is recorded as a FUNCTION
                // ERROR, naming the class the handler did not handle.
                assert_eq!(
                    function_error,
                    Some(FUNCTION_ERROR_UNHANDLED),
                    "case {case:?}: write {write} of {:?}: a handler that raised is recorded \
                     with a function error, got {record}",
                    row.key
                );
                assert_eq!(
                    record.pointer("/responsePayload/errorType").and_then(Value::as_str),
                    Some(CONDITION_FAILED),
                    "case {case:?}: write {write} of {:?}: the error the run answered with is \
                     the refusal of its conditional write, got {record}",
                    row.key
                );
            } else {
                // The run RETURNED, so nothing about it is a function error — whatever the
                // services it called answered it.
                assert_eq!(
                    function_error, None,
                    "case {case:?}: write {write} of {:?}: a handler that returned is recorded \
                     clean, got {record}",
                    row.key
                );
                // And the handler's own answer says which branch it took: the key recorded
                // where its conditional write was served, the class it caught where it was
                // refused.
                let expected_payload = if refused {
                    json!({ "recorded": [], "caught": [CONDITION_FAILED] })
                } else {
                    json!({ "recorded": [row.key], "caught": [] })
                };
                assert_eq!(
                    record.pointer("/responsePayload").cloned().unwrap_or(Value::Null),
                    expected_payload,
                    "case {case:?}: write {write} of {:?}: expected the handler to answer \
                     {expected_payload}, got {record}",
                    row.key
                );
            }

            // The item: unchanged where the write was refused, and the handler's own row where
            // it was served.
            let after = held(&router, case, row.key).await;
            if refused {
                assert_eq!(
                    after, before,
                    "case {case:?}: write {write} of {:?} was refused, so it wrote nothing and \
                     the item must stand exactly as it was",
                    row.key
                );
            } else {
                assert_eq!(
                    after,
                    Some(handler_row(row.key, body)),
                    "case {case:?}: write {write} of {:?} was served, so the item must be what \
                     the published record named",
                    row.key
                );
            }

            // The series a client reads the same outcome from: one observation of
            // `Invocations` per run, and one of `Errors` per run carrying 1 only for a run
            // that came to a function error.
            invocations[slot] += 1.0;
            if failing {
                errors[slot] += 1.0;
            }
            let (invoked_sum, invoked_samples) =
                awaited_series(&router, case, function, INVOCATIONS_SERIES, invocations[slot])
                    .await;
            assert_eq!(
                (invoked_sum, invoked_samples),
                (invocations[slot], invocations[slot]),
                "case {case:?}: write {write} of {:?}: expected {} invocation(s) of \
                 {function:?} on {INVOCATIONS_SERIES}, got a sum of {invoked_sum} over \
                 {invoked_samples} observation(s)",
                row.key,
                invocations[slot]
            );
            let (error_sum, error_samples) =
                awaited_series(&router, case, function, ERRORS_SERIES, invocations[slot]).await;
            assert_eq!(
                (error_sum, error_samples),
                (errors[slot], invocations[slot]),
                "case {case:?}: write {write} of {:?}: expected {} of {}'s {function:?} \
                 invocation(s) to be recorded as a function error on {ERRORS_SERIES}, got a sum \
                 of {error_sum} over {error_samples} observation(s)",
                row.key,
                errors[slot],
                invocations[slot]
            );
        }
    }

    // Nothing further comes: each write was published once, so no queue holds a record for a
    // write that was not made — a second delivery of any of them would land here.
    for (url, what) in [
        (&swallow_success, "the catching handler's successes"),
        (&swallow_failure, "the catching handler's failures"),
        (&raise_success, "the raising handler's successes"),
        (&raise_failure, "the raising handler's failures"),
    ] {
        let extra = quietly_drained(&router, case, url).await;
        assert!(
            extra.is_empty(),
            "case {case:?}: every published write has been accounted for, so {what} must \
             deliver no further record, got {extra:?}"
        );
    }

    // And the rows a client seeded are still the rows a client seeded, whatever each handler
    // then did with its refusal.
    for (key, seeded_item) in &seeded {
        let now_held = held(&router, case, key).await;
        assert_eq!(
            now_held.as_ref(),
            Some(seeded_item),
            "case {case:?}: every conditional write addressed to {key:?} was refused, so the \
             seeded row must be untouched"
        );
    }
}
