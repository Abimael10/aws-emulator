// SPDX-License-Identifier: CC0-1.0

//! The deployed image-labelling estate — a DynamoDB table, an execution role, an inline
//! `nodejs20.x` function, the grant Amazon S3 needs on it, and a bucket whose
//! `NotificationConfiguration` names that function once per filtered suffix — provisioned by one
//! `CreateStack` and then driven the way a client drives it: objects written into the declared
//! bucket, and the rows the handler wrote read back through Amazon DynamoDB.
//!
//! The rule under test: **a stack create settles into a TERMINAL `StackStatus` that
//! `DescribeStacks` reports, and from then on EVERY accepted object write whose key a stored
//! configuration's key-name `Filter` admits publishes its OWN event to that configuration's
//! destination — the notification is of the write, not of the key — so a write that replaces an
//! object already at a key is published again and runs the destination function once more, each
//! delivery naming its own call; and a `Filter` rule is "the value that the filter searches for
//! in object key names" (`FilterRule$Value`), searched in the key's own text, so a key differing
//! from an admitted one only in letter case is admitted by no rule.**
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the estate and
//! projects its status, IAM answers for the execution role, Amazon S3 evaluates the stored
//! configuration and makes the invocation, AWS Lambda holds the grant and runs the handler, and
//! Amazon DynamoDB serves the table the handler writes — so the suite lives here: a crate-local
//! suite can only ever see one side of it. Its siblings reach one side each:
//! [`crate::s3_notification_invokes_its_lambda_target`] pins which key-name filters admit a
//! FIRST write, and [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] pins the grant
//! as the gate on the wiring. What only this suite reaches is the delivery COUNT: that a second
//! and a third write of one key are each published, which no suite pins.
//!
//! The rows differ along what the rule generalises over: how many times the key is written (one,
//! two, three), which of the declared suffix rules admits it, whether the written bodies differ,
//! and whether any rule admits the key at all (a key no suffix matches, and a key matching an
//! admitted suffix in every character but its case). Every row observes its effect where a
//! CLIENT does: the item DynamoDB holds for the key, whose counter the handler increments once
//! per invocation — so "never delivered" and "delivered once for a key written twice" are told
//! apart.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test holding
//! several of them across await points lays that state out on the test thread's stack and
//! overflows it as the emulator grows; the box puts each one on the heap instead.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the one it reads.
const REGION: &str = "us-east-1";

/// The terminal `StackStatus` values a create settles into, of which the success path reaches the
/// first (`smithy_query(query="shape",shape="StackStatus")` against the CloudFormation model
/// lists them beside the `*_IN_PROGRESS` values a create passes through).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const TERMINAL_CREATE_STATUSES: [&str; 4] = [
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The one stack, bucket, table and function this suite deploys.
const STACK: &str = "per-write-notification-estate";
const BUCKET: &str = "per-write-notification-images";
const TABLE: &str = "per-write-notification-labels";
const FUNCTION: &str = "per-write-notification-recorder";

/// The key attribute the declared table is keyed on, and the attributes the handler keeps beside
/// it: how many deliveries it has been handed for that key, and what the LAST one named.
const KEY_ATTRIBUTE: &str = "image_name";
const COUNT_ATTRIBUTE: &str = "deliveries";
const EVENT_ATTRIBUTE: &str = "event_name";
const REQUEST_ATTRIBUTE: &str = "last_request_id";
const SEQUENCER_ATTRIBUTE: &str = "last_sequencer";

/// The event class the declared configurations are notified about, and the `eventName` a put is
/// published under.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The suffixes the stack declares one `LambdaConfigurations` entry each for — the three image
/// extensions this reference estate filters on.
const SUFFIXES: [&str; 3] = [".jpg", ".jpeg", ".png"];

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The provisioned capacity the declared table carries. The read side is ample because this
/// suite POLLS the table for what a delivery left, several times a second: a provisioned table
/// admits its rate plus the capacity that went unused behind it, so a poll loop faster than the
/// rate would be throttled — which is a property of the loop and not of what it waits for.
const READ_CAPACITY: u64 = 400;
const WRITE_CAPACITY: u64 = 5;

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite looks for work another service's clock owes — a stack settling, a function
/// reported `Active`, a table reported `ACTIVE`, the asynchronous delivery of an object event —
/// and how often it looks while waiting. Generous: no row asserts how long anything took, only
/// that it is not waited out forever.
const WAIT: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row that must observe NOTHING keeps looking before concluding nothing came — well
/// past the span a delivered row of the same wiring takes to leave its item.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling notification suites use. Assembling
/// every service and then dispatching a CHAIN of in-process calls through it — CloudFormation
/// into S3, Lambda, IAM and DynamoDB, S3's delivery into Lambda, the handler's own SDK call into
/// DynamoDB — nests more future than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler the stack deploys, in the `nodejs20.x` runtime it is declared with: for every
/// record it is handed it ADDS one to that key's delivery counter and records what THAT record
/// named — the call it came of and the sequence value for the key — through the AWS SDK the
/// managed Node runtime supplies.
///
/// A counter rather than a bare row, because what this suite observes is HOW MANY deliveries a
/// key received: a second write that is never published and a second write whose delivery
/// overwrote the first row look identical to a reader of a row, and differ in a counter.
const COUNTING_HANDLER: &str = r#"
const { DynamoDBClient, UpdateItemCommand } = require('@aws-sdk/client-dynamodb');

exports.handler = async (event) => {
    const ddb = new DynamoDBClient({});
    const counted = [];
    for (const record of event.Records) {
        const key = record.s3.object.key;
        await ddb.send(new UpdateItemCommand({
            TableName: process.env.TABLE_NAME,
            Key: { image_name: { S: key } },
            UpdateExpression:
                'SET event_name = :event, last_request_id = :request, ' +
                'last_sequencer = :sequencer ADD deliveries :one',
            ExpressionAttributeValues: {
                ':event': { S: record.eventName },
                ':request': { S: record.responseElements['x-amz-request-id'] },
                ':sequencer': { S: record.s3.object.sequencer },
                ':one': { N: '1' },
            },
        }));
        counted.push(key);
    }
    return { counted: counted };
};
"#;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread runtime
/// this suite's deliveries are made on: a notification delivery is a task of its own, so it runs
/// on this same thread and needs the same depth.
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

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
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

/// The emulator as it is ASSEMBLED, on one engine: S3's delivery and the handler's own SDK call
/// are dispatched back into this same router, so the function S3 invokes and the table the
/// handler writes are the ones this suite reads.
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

/// The reference estate as ONE template: the table the handler writes, the execution role, the
/// inline `nodejs20.x` function, the grant Amazon S3 needs to invoke it scoped to this account
/// and this bucket, and the bucket whose notification configuration names that function once per
/// declared suffix.
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
                "Filter": { "S3Key": { "Rules": [{ "Name": "suffix", "Value": suffix }] } },
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
                        "PolicyName": "count-the-deliveries",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "dynamodb:UpdateItem",
                                    "dynamodb:GetItem",
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
                    "Environment": { "Variables": { "TABLE_NAME": TABLE } },
                    "Code": { "ZipFile": COUNTING_HANDLER },
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

/// `PUT /<bucket>/<key>` — `PutObject`, which "adds an object to a bucket … replacing any object
/// already at that key". The write is accepted whatever the notification configuration does, so
/// every row asserts it.
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: &str) {
    let written = s3(router, "PUT", &format!("/{BUCKET}/{key}"), body).await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: expected HTTP 200 from PutObject({BUCKET:?}, {key:?}), got {} with {:?}",
        written.status,
        written.text()
    );
}

/// What the declared table holds for `key` right now, as [`Delivered`] reads it — `None` when it
/// holds no item at all for that key.
async fn held(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Delivered> {
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
    let answered = read.json();
    let item = answered.get("Item")?.clone();
    Some(Delivered {
        count: attribute(&item, COUNT_ATTRIBUTE, "N")
            .and_then(|count| count.parse::<u64>().ok())
            .unwrap_or_default(),
        event_name: attribute(&item, EVENT_ATTRIBUTE, "S").unwrap_or_default(),
        request_id: attribute(&item, REQUEST_ATTRIBUTE, "S").unwrap_or_default(),
        sequencer: attribute(&item, SEQUENCER_ATTRIBUTE, "S").unwrap_or_default(),
        item,
    })
}

/// One attribute of an item, read as the `kind` the handler wrote it under.
fn attribute(item: &Value, name: &str, kind: &str) -> Option<String> {
    item.get(name)?
        .get(kind)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What the handler has recorded for one key: how many deliveries it counted, and what the LAST
/// one named.
#[derive(Debug, Clone)]
struct Delivered {
    count: u64,
    event_name: String,
    request_id: String,
    sequencer: String,
    item: Value,
}

/// Look for the handler's record of the `wanted`th delivery of `key` until the counter reaches it,
/// and answer the last read. The wait is on that CONDITION; no row asserts how long it took.
async fn awaited_delivery(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
    wanted: u64,
) -> Option<Delivered> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = None;
    for _ in 0..looks {
        last = held(router, case, key).await;
        if last.as_ref().is_some_and(|read| read.count >= wanted) {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// Keep looking for an item under `key` through [`QUIET_WINDOW`] and answer what was last read —
/// `None` only when nothing ever came, so a delivery that merely arrives late is still seen.
async fn quietly_absent(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
) -> Option<Delivered> {
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

/// One row: the key written into the declared bucket, how many times it is written, and how many
/// of those writes a declared configuration's key-name filter admits.
struct Row {
    case: &'static str,
    key: &'static str,
    /// The body of each write, one per write — so the rows differ in how many writes they make
    /// and in whether the writes carry the same bytes.
    bodies: &'static [&'static str],
    /// Whether any declared suffix rule admits this key, which is what decides whether each of
    /// its writes is published.
    admitted: bool,
}

/// The rows span what the rule generalises over: the number of writes of one key (one, two,
/// three), which declared suffix rule admits it, whether the written bodies differ between
/// writes, and whether any rule admits the key at all.
const ROWS: &[Row] = &[
    Row {
        case: "a key the first declared suffix rule admits, written once",
        key: "uploads/portrait.jpg",
        bodies: &["the first upload"],
        admitted: true,
    },
    Row {
        case: "the same key written twice with the same bytes",
        key: "uploads/reupload.jpg",
        bodies: &["the same bytes", "the same bytes"],
        admitted: true,
    },
    Row {
        case: "a key the second declared suffix rule admits, written twice with new bytes",
        key: "uploads/portrait.jpeg",
        bodies: &["the first bytes", "the replacing bytes"],
        admitted: true,
    },
    Row {
        case: "a key the third declared suffix rule admits, written three times",
        key: "uploads/diagram.png",
        bodies: &["one", "two and a bit", "three, longer again"],
        admitted: true,
    },
    Row {
        case: "a key no declared suffix rule admits, written twice",
        key: "uploads/notes.txt",
        bodies: &["the first note", "the replacing note"],
        admitted: false,
    },
    Row {
        case: "a key differing from an admitted one only in case, written twice",
        key: "uploads/portrait.JPG",
        bodies: &["the first upload", "the replacing upload"],
        admitted: false,
    },
];

/// The whole estate: one `CreateStack` settles into a terminal status `DescribeStacks` reports,
/// its resources exist in the services that own them, and from then on each write of an admitted
/// key runs the declared function once more — while a key no filter admits never runs it.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn every_write_of_an_admitted_key_runs_the_notified_function_again() {
    driven("s3-notification-delivers-once-per-object-write", || {
        Box::pin(every_write_of_an_admitted_key_runs_the_notified_function_again_body())
    });
}

async fn every_write_of_an_admitted_key_runs_the_notified_function_again_body() {
    let case = "the deployed image-labelling estate";
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

    // 2. The create SETTLES: the status DescribeStacks reports is one of the terminal values,
    //    and on the success path it is CREATE_COMPLETE — a deployer that polls gets a decidable
    //    answer rather than *_IN_PROGRESS for ever.
    let (reported, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert!(
        TERMINAL_CREATE_STATUSES.contains(&reported.as_str()),
        "case {case:?}: expected DescribeStacks to report one of \
         {TERMINAL_CREATE_STATUSES:?}, got {reported:?} in {described}"
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
        "case {case:?}: expected AWS Lambda to report the declared function Active, got \
         {state:?}"
    );
    let table_status = awaited_table_status(&router, TABLE).await;
    assert_eq!(
        table_status, "ACTIVE",
        "case {case:?}: expected Amazon DynamoDB to report the declared table ACTIVE, got \
         {table_status:?}"
    );
    let installed = s3(&router, "GET", &format!("/{BUCKET}?notification"), "").await;
    assert_eq!(
        installed.status,
        200,
        "case {case:?}: expected HTTP 200 from GetBucketNotificationConfiguration, got {} with \
         {:?}",
        installed.status,
        installed.text()
    );
    let reported_functions = elements(&installed.text(), "CloudFunction");
    assert_eq!(
        reported_functions.len(),
        SUFFIXES.len(),
        "case {case:?}: expected one stored Lambda configuration per declared suffix, got \
         {reported_functions:?} in {}",
        installed.text()
    );

    // 4. Every row, written one write at a time: each write of an admitted key is published on
    //    its own, and a key no filter admits is published for none of its writes.
    for row in ROWS {
        let case = row.case;
        if !row.admitted {
            // Every write first, then ONE quiet window: the handler never deletes an item, so a
            // delivery of any of these writes would still be there to be read.
            for body in row.bodies {
                put_object(&router, case, row.key, body).await;
            }
            let seen = quietly_absent(&router, case, row.key).await;
            assert!(
                seen.is_none(),
                "case {case:?}: no declared filter admits {:?}, so none of its {} writes may \
                 run a function, got the item {:?}",
                row.key,
                row.bodies.len(),
                seen.map(|read| read.item)
            );
            continue;
        }

        let mut previous: Option<Delivered> = None;
        for (index, body) in row.bodies.iter().enumerate() {
            let write = index + 1;
            let wanted = u64::try_from(write).expect("a row makes few writes");
            put_object(&router, case, row.key, body).await;
            let seen = awaited_delivery(&router, case, row.key, wanted).await;
            let seen = seen.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected write {write} of {:?} to be published, so the \
                     handler's counter must reach {wanted}, got no item at all",
                    row.key
                )
            });
            assert_eq!(
                seen.count, wanted,
                "case {case:?}: expected exactly one delivery per write, so after write \
                 {write} of {:?} the counter must be {wanted}, got {} in {}",
                row.key, seen.count, seen.item
            );
            assert_eq!(
                seen.event_name, PUT_EVENT_NAME,
                "case {case:?}: expected write {write} of {:?} to be published as \
                 {PUT_EVENT_NAME}, got {:?} in {}",
                row.key, seen.event_name, seen.item
            );
            assert!(
                !seen.request_id.is_empty() && !seen.sequencer.is_empty(),
                "case {case:?}: every published record names the call it came of and the \
                 sequence value for its key, got {} after write {write} of {:?}",
                seen.item,
                row.key
            );
            if let Some(earlier) = previous {
                assert_ne!(
                    seen.request_id, earlier.request_id,
                    "case {case:?}: write {write} of {:?} is its own call, so its record must \
                     not name the previous call {:?}",
                    row.key, earlier.request_id
                );
                // "A string representation of a hexadecimal value used to determine event
                // sequence" for one key, so the later write's is the greater.
                assert!(
                    seen.sequencer > earlier.sequencer,
                    "case {case:?}: expected write {write} of {:?} to carry a later sequencer \
                     than {:?}, got {:?}",
                    row.key,
                    earlier.sequencer,
                    seen.sequencer
                );
            }
            previous = Some(seen);
        }
    }
}
