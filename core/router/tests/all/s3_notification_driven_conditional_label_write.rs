// SPDX-License-Identifier: CC0-1.0

//! The image-pipeline reference estate a client ships as ONE stack — a DynamoDB labels table, an
//! execution role, an inline `nodejs20.x` handler, the grant Amazon S3 needs on it, and a bucket
//! whose `NotificationConfiguration` names that handler once per filtered suffix — deployed by one
//! `CreateStack` and then driven the way a client drives it: objects written into the declared
//! bucket, and the label rows the handler's own conditional write left read back through Amazon
//! DynamoDB.
//!
//! The rule under test: **a stack create settles into a terminal `StackStatus` that
//! `DescribeStacks` reports, and from then on every accepted object write whose key a stored
//! configuration's key-name `Filter` admits runs the named function to COMPLETION, so each call
//! that handler makes takes effect exactly as the service owning it decides — a conditional write
//! whose condition no longer holds is REFUSED and writes nothing, so the row the served write wrote
//! stays exactly as it was however many later writes of that key are published, while the refusal
//! the handler catches is its own to record — and a key no stored rule admits runs the function for
//! none of its writes.**
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the estate and projects
//! its status, IAM answers for the execution role the handler's calls are signed as, Amazon S3
//! evaluates the stored configuration and makes the invocation, AWS Lambda holds the grant and runs
//! the handler, and Amazon DynamoDB decides the conditional write — so the suite lives here: a
//! crate-local suite can only ever see one side of it. Its siblings reach one side each:
//! [`crate::s3_notification_invokes_its_lambda_target`] pins which key-name filters admit a first
//! write, and [`crate::s3_notification_delivers_once_per_object_write`] pins that each write of an
//! admitted key is published on its own. What only this suite reaches is what a DELIVERED handler's
//! own REFUSED call then leaves behind: the stale row a re-upload cannot replace, beside the
//! refusal the handler swallowed, neither of which any suite pins.
//!
//! The rows differ along what the rule generalises over: which declared suffix rule admits the key,
//! how many times the key is written (one, two, three), the bytes each write carries — so the size
//! and entity tag the published record names differ from write to write, and the row's are the
//! FIRST write's whatever the later writes carry — and whether any stored rule admits the key at
//! all. Every row observes its effect where a CLIENT does: the item DynamoDB holds for the key, and
//! the marker item the handler writes for what it caught, each compared with what Amazon S3 itself
//! answered the writer — so "never delivered", "delivered and served" and "delivered and refused"
//! are told apart.
//!
//! Every request is dispatched through a BOXED future ([`Box::pin`]). The assembled router's
//! dispatch future carries the whole emulator's handler state inline, so a test holding several of
//! them across await points lays that state out on the test thread's stack and overflows it as the
//! emulator grows; the box puts each one on the heap instead.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{blocks_of, elements, form_encode, outputs_of};

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the one it reads.
const REGION: &str = "us-east-1";

/// The terminal `StackStatus` values a create settles into, of which the success path reaches the
/// first (`smithy_query(query="shape",shape="StackStatus")` against the CloudFormation model lists
/// them beside the `*_IN_PROGRESS` values a create passes through).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const TERMINAL_CREATE_STATUSES: [&str; 4] = [
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The one stack, bucket, table and function this suite deploys.
const STACK: &str = "conditional-write-notification-estate";
const BUCKET: &str = "conditional-write-notification-images";
const TABLE: &str = "conditional-write-notification-labels";
const FUNCTION: &str = "conditional-write-notification-recorder";

/// The key attribute the declared table is keyed on, and the attributes the handler keeps beside
/// it: what the published record named about the object, and the sequence value of the write whose
/// record was stored.
const KEY_ATTRIBUTE: &str = "image_name";
const EVENT_ATTRIBUTE: &str = "event_name";
const SIZE_ATTRIBUTE: &str = "object_size";
const ETAG_ATTRIBUTE: &str = "object_etag";
const SEQUENCER_ATTRIBUTE: &str = "sequencer";

/// The marker item the handler records what its own write was REFUSED with under: the suffix it
/// keys that item by, the class it caught, and how many refusals it has caught for the key.
const CAUGHT_SUFFIX: &str = "#caught";
const CAUGHT_ATTRIBUTE: &str = "caught";
const CATCHES_ATTRIBUTE: &str = "catches";

/// The modelled DynamoDB refusal of a write whose condition does not hold, which is the class the
/// handler catches (`smithy_query(query="operation",operation="PutItem")`).
const CONDITION_FAILED: &str = "ConditionalCheckFailedException";

/// The event class the declared configurations are notified about, and the `eventName` a put is
/// published under.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The suffixes the stack declares one `LambdaConfigurations` entry each for — the three image
/// extensions this reference estate filters on.
const SUFFIXES: [&str; 3] = [".jpg", ".jpeg", ".png"];

/// The key-name filter rule kind each declared entry is DECLARED with, which is one of the two
/// names `FilterRuleName` publishes (`smithy_query(query="shape",shape="FilterRuleName")`), and the
/// canonical capitalised spelling Amazon S3 keeps a rule under and reports it to every reader by,
/// whatever spelling it was written in.
const SUFFIX_RULE: &str = "suffix";
/// See [`SUFFIX_RULE`].
const REPORTED_SUFFIX_RULE: &str = "Suffix";

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The provisioned capacity the declared table carries, as the reference estate declares it.
const READ_CAPACITY: u64 = 5;
const WRITE_CAPACITY: u64 = 5;

/// How long one invocation of the handler is given, as the function's own `Timeout`. Nothing here
/// asserts how long a run took: what this suite asserts is what a run LEFT, so the bound is
/// generous — a run that imports a bundled SDK client, on a host running every other suite of this
/// target at the same time, must not be ended by a limit this test is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite looks for work another service's clock owes — a stack settling, a function
/// reported `Active`, a table reported `ACTIVE`, the asynchronous delivery of an object event and
/// the handler's own call — and how often it looks while waiting. Generous: no row asserts how long
/// anything took, only that it is not waited out forever.
const WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row that must observe NOTHING keeps looking before concluding nothing came — well
/// past the span a delivered row of the same wiring takes to leave its item.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling notification suites use. Assembling
/// every service and then dispatching a CHAIN of in-process calls through it — CloudFormation into
/// S3, Lambda, IAM and DynamoDB, S3's delivery into Lambda, the handler's own call into DynamoDB —
/// nests more future than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler the stack deploys, in the `nodejs20.x` runtime it is declared with: for every record
/// it is handed it records what that record named about the object CONDITIONALLY — only where the
/// key carries no row yet — inside a try/catch, and ends by returning. The catch block records the
/// CLASS it caught and what THAT record named under a marker key of its own, so the refusal the
/// handler swallowed is readable through the store rather than only through the run's log.
const RECORDING_HANDLER: &str = r#"
const {
    DynamoDBClient,
    PutItemCommand,
    UpdateItemCommand,
} = require('@aws-sdk/client-dynamodb');

const ITEMS = new DynamoDBClient({});

exports.handler = async (event) => {
    const recorded = [];
    for (const record of event.Records) {
        const key = record.s3.object.key;
        const size = String(record.s3.object.size);
        const etag = record.s3.object.eTag;
        console.log('recording ' + record.s3.bucket.name + '/' + key);
        try {
            await ITEMS.send(new PutItemCommand({
                TableName: process.env.TABLE_NAME,
                Item: {
                    image_name: { S: key },
                    event_name: { S: record.eventName },
                    object_size: { N: size },
                    object_etag: { S: etag },
                    sequencer: { S: record.s3.object.sequencer },
                },
                ConditionExpression: 'attribute_not_exists(image_name)',
            }));
            recorded.push(key);
        } catch (refused) {
            console.log(refused);
            await ITEMS.send(new UpdateItemCommand({
                TableName: process.env.TABLE_NAME,
                Key: { image_name: { S: key + process.env.CAUGHT_SUFFIX } },
                UpdateExpression:
                    'SET #caught = :class, #size = :size, #etag = :etag ADD #catches :one',
                ExpressionAttributeNames: {
                    '#caught': 'caught',
                    '#size': 'object_size',
                    '#etag': 'object_etag',
                    '#catches': 'catches',
                },
                ExpressionAttributeValues: {
                    ':class': { S: refused.name },
                    ':size': { N: size },
                    ':etag': { S: etag },
                    ':one': { N: '1' },
                },
            }));
        }
    }
    return { recorded: recorded };
};
"#;

// ---------------------------------------------------------------------------
// The emulator and the wires this suite dispatches on
// ---------------------------------------------------------------------------

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread runtime
/// this suite's deliveries are made on: a notification delivery is a task of its own, so it runs on
/// this same thread and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// One answered request: its status, the entity tag it reported (empty where it reported none) and
/// its body verbatim.
struct Answered {
    status: u16,
    etag: String,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: S3's delivery and the handler's own call are
/// dispatched back into this same router, so the function S3 invokes and the table the handler
/// writes are the ones this suite deploys and reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, and the signature itself is never checked.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response =
        Box::pin(router.dispatch(builder.body(body).expect("a well-formed request"))).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        // An `ETag` is "returned within double quotation marks" on the wire, while a published
        // record names it unquoted, so the quotes come off here once.
        etag: parts
            .headers
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .trim_matches('"')
            .to_string(),
        body,
    }
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One call on S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from(body.to_owned()),
    )
    .await
}

// ---------------------------------------------------------------------------
// The estate
// ---------------------------------------------------------------------------

/// The reference estate as ONE template: the table the handler writes, the execution role its call
/// is signed as, the inline `nodejs20.x` function, the grant Amazon S3 needs to invoke it scoped to
/// this account and this bucket, and the bucket whose notification configuration names that
/// function once per declared suffix.
///
/// The bucket `DependsOn` the grant, which is the order the configuration's own destination
/// validation needs.
fn estate_template() -> String {
    let configurations: Vec<Value> = SUFFIXES
        .iter()
        .map(|suffix| {
            json!({
                "Event": OBJECT_CREATED,
                "Function": { "Fn::GetAtt": ["Recorder", "Arn"] },
                "Filter": {
                    "S3Key": { "Rules": [{ "Name": SUFFIX_RULE, "Value": suffix }] },
                },
            })
        })
        .collect();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "LabelsTable": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": TABLE,
                    "AttributeDefinitions": [
                        { "AttributeName": KEY_ATTRIBUTE, "AttributeType": "S" },
                    ],
                    "KeySchema": [{ "AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH" }],
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": READ_CAPACITY,
                        "WriteCapacityUnits": WRITE_CAPACITY,
                    },
                },
            },
            "RecorderRole": {
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
                    "Policies": [{
                        "PolicyName": "record-what-was-written",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "dynamodb:PutItem",
                                    "dynamodb:UpdateItem",
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
            "Recorder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "nodejs20.x",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["RecorderRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "TABLE_NAME": TABLE,
                            "CAUGHT_SUFFIX": CAUGHT_SUFFIX,
                        },
                    },
                    "Code": { "ZipFile": RECORDING_HANDLER },
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Recorder", "Arn"] },
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                    "SourceAccount": { "Ref": "AWS::AccountId" },
                    "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
                },
            },
            "ImageBucket": {
                "Type": "AWS::S3::Bucket",
                "DependsOn": "InvokePermission",
                "Properties": {
                    "BucketName": BUCKET,
                    "NotificationConfiguration": { "LambdaConfigurations": configurations },
                },
            },
        },
        "Outputs": {
            "BucketName": { "Value": { "Ref": "ImageBucket" } },
            "TableName": { "Value": { "Ref": "LabelsTable" } },
            "FunctionName": { "Value": { "Ref": "Recorder" } },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until AWS Lambda first reports the function `Active` — "you
/// cannot invoke … while the function is in the Pending state" — and answer the state it reported
/// last. The span is that service's own, so nothing here sleeps a fixed span on its behalf.
async fn awaited_function_state(router: &MultiServiceRouter, function: &str) -> String {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = String::new();
    for _ in 0..looks {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
        )
        .await;
        last = reported
            .json()
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == "Active" {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// Poll `DescribeTable` until Amazon DynamoDB first reports the table `ACTIVE` — "you can perform
/// read and write operations only on an ACTIVE table" — and answer the status it reported last.
async fn awaited_table_status(router: &MultiServiceRouter, table: &str) -> String {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = String::new();
    for _ in 0..looks {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        last = described
            .json()
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == "ACTIVE" {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// What one write of a key left: the size of the bytes it carried and the entity tag Amazon S3
/// answered the writer with, which is what the record published for that write has to name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Written {
    size: String,
    etag: String,
}

/// `PUT /<bucket>/<key>` — `PutObject`, which "adds an object to a bucket … replacing any object
/// already at that key". The write is accepted whatever the notification configuration does, so
/// every row asserts it, and answers what the write left.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &str) -> Written {
    let written = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: expected HTTP 200 from PutObject({BUCKET:?}, {key:?}), got {} with {:?}",
        written.status,
        written.text()
    );
    assert!(
        !written.etag.is_empty(),
        "case {case:?}: expected PutObject({BUCKET:?}, {key:?}) to answer an entity tag, got {:?}",
        written.text()
    );
    Written {
        size: body.len().to_string(),
        etag: written.etag,
    }
}

/// What the declared table holds under `key` right now, as the item a client reads — `None` when it
/// holds no item at all for that key.
async fn held(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Value> {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": TABLE,
            "Key": { KEY_ATTRIBUTE: { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetItem({TABLE:?}, {key:?}), got {} with {:?}",
        read.status,
        read.text()
    );
    read.json().get("Item").cloned()
}

/// One attribute of an item, read as the `kind` the handler wrote it under.
fn attribute(item: &Value, name: &str, kind: &str) -> String {
    item.get(name)
        .and_then(|value| value.get(kind))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// What the object members of an item name about the object: the pair one write left.
fn named(item: &Value) -> Written {
    Written {
        size: attribute(item, SIZE_ATTRIBUTE, "N"),
        etag: attribute(item, ETAG_ATTRIBUTE, "S"),
    }
}

/// How many refusals the marker item has counted.
fn catches(item: &Value) -> u64 {
    attribute(item, CATCHES_ATTRIBUTE, "N")
        .parse::<u64>()
        .unwrap_or_default()
}

/// Look for an item under `key` until one is there, and answer the last read. The wait is on that
/// CONDITION; no row asserts how long it took.
async fn awaited_item(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Value> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = None;
    for _ in 0..looks {
        last = held(router, case, key).await;
        if last.is_some() {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// Look for the marker item under `key` until it counts `wanted` caught refusals, and answer the
/// last read.
async fn awaited_catches(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
    wanted: u64,
) -> Option<Value> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = None;
    for _ in 0..looks {
        last = held(router, case, key).await;
        if last.as_ref().is_some_and(|item| catches(item) >= wanted) {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// Keep looking for an item under `key` through [`QUIET_WINDOW`] and answer what was last read —
/// `None` only when nothing ever came, so an item that merely arrives late is still seen.
async fn quietly_absent(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Value> {
    let looks = QUIET_WINDOW.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = None;
    for _ in 0..looks {
        last = held(router, case, key).await;
        if last.is_some() {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// One row: the key written into the declared bucket, the bytes each write of it carries, and
/// whether any stored configuration's filter admits the key.
struct Row {
    case: &'static str,
    key: &'static str,
    /// The body of each write, one per write — each of its own length, so the record published for
    /// a write names a size and an entity tag no other write of the row shares.
    bodies: &'static [&'static str],
    /// Whether any declared suffix rule admits this key, which is what decides whether each of its
    /// writes is published at all.
    admitted: bool,
}

/// The rows span what the rule generalises over: which declared suffix rule admits the key, how
/// many times it is written (one, two, three), the bytes each write carries, and whether any stored
/// rule admits the key at all.
const ROWS: &[Row] = &[
    Row {
        case: "the first suffix rule's key, written once and then re-uploaded twice",
        key: "uploads/portrait.jpg",
        bodies: &[
            "the first upload",
            "the second upload, longer",
            "the third upload, longer again",
        ],
        admitted: true,
    },
    Row {
        case: "the second suffix rule's key, written once and then re-uploaded",
        key: "uploads/landscape.jpeg",
        bodies: &["one", "two and a bit"],
        admitted: true,
    },
    Row {
        case: "the third suffix rule's key, written once",
        key: "uploads/diagram.png",
        bodies: &["the only upload"],
        admitted: true,
    },
    Row {
        case: "a key no declared suffix rule admits, written twice",
        key: "uploads/notes.txt",
        bodies: &["the first note", "the replacing note, longer"],
        admitted: false,
    },
];

/// The whole estate: one `CreateStack` settles into a terminal status `DescribeStacks` reports, the
/// three declared suffix-filtered configurations are installed, and from then on each write of an
/// admitted key RUNS the declared handler to completion — the first write's record stored under the
/// key, every later write's conditional record refused and caught, and the stored row left exactly
/// as it was — while a key no filter admits runs the handler for none of its writes.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_delivered_conditional_write_keeps_the_first_served_row() {
    driven("s3-notification-driven-conditional-write", || {
        Box::pin(a_delivered_conditional_write_keeps_the_first_served_row_body())
    });
}

async fn a_delivered_conditional_write_keeps_the_first_served_row_body() {
    let case = "the deployed image-pipeline estate";
    let router = emulator();

    // 1. The estate, provisioned by ONE CreateStack.
    let template = estate_template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );

    // 2. The create SETTLES: the status DescribeStacks reports is one of the terminal values, and
    //    on the success path it is CREATE_COMPLETE — a deployer that polls gets a decidable answer
    //    rather than *_IN_PROGRESS for ever.
    let (reported, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert!(
        TERMINAL_CREATE_STATUSES.contains(&reported.as_str()),
        "case {case:?}: expected DescribeStacks to report one of {TERMINAL_CREATE_STATUSES:?}, \
         got {reported:?} in {described}"
    );
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the declared estate to provision, got {reported:?} in \
         {described}"
    );
    let outputs = outputs_of(&described);
    assert_eq!(
        (
            outputs.get("BucketName").map(String::as_str),
            outputs.get("TableName").map(String::as_str),
            outputs.get("FunctionName").map(String::as_str),
        ),
        (Some(BUCKET), Some(TABLE), Some(FUNCTION)),
        "case {case:?}: expected the settled stack to publish the names it declared, got \
         {outputs:?}"
    );

    // 3. Each declared resource exists in the service that OWNS it, and is ready to be used.
    let state = awaited_function_state(&router, FUNCTION).await;
    assert_eq!(
        state, "Active",
        "case {case:?}: expected AWS Lambda to report the declared function Active, got {state:?}"
    );
    let table_status = awaited_table_status(&router, TABLE).await;
    assert_eq!(
        table_status, "ACTIVE",
        "case {case:?}: expected Amazon DynamoDB to report the declared table ACTIVE, got \
         {table_status:?}"
    );

    // 4. The three declared configurations are INSTALLED on the bucket, each filtered on its own
    //    suffix and each naming the declared function.
    let installed = s3(&router, "GET", &format!("/{BUCKET}?notification"), "").await;
    assert_eq!(
        installed.status,
        200,
        "case {case:?}: expected HTTP 200 from GetBucketNotificationConfiguration, got {} with \
         {:?}",
        installed.status,
        installed.text()
    );
    let document = installed.text();
    let configurations = blocks_of(&document, "CloudFunctionConfiguration");
    assert_eq!(
        configurations.len(),
        SUFFIXES.len(),
        "case {case:?}: expected one stored Lambda configuration per declared suffix, got \
         {configurations:?} in {document}"
    );
    let mut stored: Vec<(String, String, bool)> = configurations
        .iter()
        .map(|configuration| {
            let names = elements(configuration, "Name");
            let values = elements(configuration, "Value");
            let destination = elements(configuration, "CloudFunction");
            (
                names.first().cloned().unwrap_or_default(),
                values.first().cloned().unwrap_or_default(),
                destination
                    .first()
                    .is_some_and(|arn| arn.ends_with(&format!(":function:{FUNCTION}"))),
            )
        })
        .collect();
    stored.sort();
    let mut expected: Vec<(String, String, bool)> = SUFFIXES
        .iter()
        .map(|suffix| (REPORTED_SUFFIX_RULE.to_string(), (*suffix).to_string(), true))
        .collect();
    expected.sort();
    assert_eq!(
        stored, expected,
        "case {case:?}: expected the declared suffix rules installed against the declared \
         function, got {stored:?} in {document}"
    );

    // 5. Every row, written one write at a time.
    for row in ROWS {
        let case = row.case;
        let caught_key = format!("{}{CAUGHT_SUFFIX}", row.key);

        if !row.admitted {
            // Every write first, then ONE quiet window: the handler never deletes an item, so an
            // invocation for any of these writes would still be there to be read.
            for body in row.bodies {
                put_object(&router, case, row.key, body).await;
            }
            let seen = quietly_absent(&router, case, row.key).await;
            assert!(
                seen.is_none(),
                "case {case:?}: no stored filter admits {:?}, so none of its {} writes may run \
                 the function, got the item {seen:?}",
                row.key,
                row.bodies.len()
            );
            let marker = held(&router, case, &caught_key).await;
            assert!(
                marker.is_none(),
                "case {case:?}: a key no filter admits reaches no handler, so nothing may have \
                 been caught for {:?}, got {marker:?}",
                row.key
            );
            continue;
        }

        // The FIRST write is the one whose conditional record is served: what the record published
        // for it named, stored under the key.
        let first = put_object(&router, case, row.key, row.bodies[0]).await;
        let recorded = awaited_item(&router, case, row.key).await.unwrap_or_else(|| {
            panic!(
                "case {case:?}: a stored filter admits {:?}, so its first write must run the \
                 handler and record a row, got no item at all",
                row.key
            )
        });
        assert_eq!(
            named(&recorded),
            first,
            "case {case:?}: expected the row to name what Amazon S3 answered the first write of \
             {:?}, got {recorded}",
            row.key
        );
        assert_eq!(
            attribute(&recorded, EVENT_ATTRIBUTE, "S"),
            PUT_EVENT_NAME,
            "case {case:?}: expected the first write of {:?} to be published as \
             {PUT_EVENT_NAME}, got {recorded}",
            row.key
        );
        let sequencer = attribute(&recorded, SEQUENCER_ATTRIBUTE, "S");
        assert!(
            !sequencer.is_empty(),
            "case {case:?}: every published record names the sequence value for its key, got \
             {recorded} for {:?}",
            row.key
        );

        if row.bodies.len() == 1 {
            // One write, so nothing was ever refused: no marker item may appear at all.
            let marker = quietly_absent(&router, case, &caught_key).await;
            assert!(
                marker.is_none(),
                "case {case:?}: the only write of {:?} was recorded, so nothing may have been \
                 caught, got {marker:?}",
                row.key
            );
            continue;
        }

        // Each LATER write is published too, and the handler's conditional record of it cannot
        // hold: it is refused, caught, and leaves the stored row exactly as it was.
        for (index, body) in row.bodies.iter().enumerate().skip(1) {
            let write = index + 1;
            let wanted = u64::try_from(index).expect("a row makes few writes");
            let replacing = put_object(&router, case, row.key, body).await;
            assert_ne!(
                replacing, first,
                "case {case:?}: write {write} of {:?} carries its own bytes, so Amazon S3 must \
                 answer it something other than the first write's {first:?}",
                row.key
            );
            let marker = awaited_catches(&router, case, &caught_key, wanted)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: write {write} of {:?} is published too, so the handler \
                         must catch its refused record, got no marker item at all",
                        row.key
                    )
                });
            assert_eq!(
                catches(&marker),
                wanted,
                "case {case:?}: expected one caught refusal per re-upload, so after write \
                 {write} of {:?} the marker must count {wanted}, got {marker}",
                row.key
            );
            assert_eq!(
                attribute(&marker, CAUGHT_ATTRIBUTE, "S"),
                CONDITION_FAILED,
                "case {case:?}: expected the refusal of a write whose condition does not hold \
                 after write {write} of {:?}, got {marker}",
                row.key
            );
            assert_eq!(
                named(&marker),
                replacing,
                "case {case:?}: the refused record still named what Amazon S3 answered write \
                 {write} of {:?}, got {marker}",
                row.key
            );

            let after = held(&router, case, row.key).await.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a refused write deletes nothing, so the row recorded for \
                     {:?} must still be there after write {write}",
                    row.key
                )
            });
            assert_eq!(
                (named(&after), attribute(&after, SEQUENCER_ATTRIBUTE, "S")),
                (first.clone(), sequencer.clone()),
                "case {case:?}: write {write} of {:?} was published and its record refused, so \
                 the row must still be the first write's, got {after}",
                row.key
            );
        }
    }
}
