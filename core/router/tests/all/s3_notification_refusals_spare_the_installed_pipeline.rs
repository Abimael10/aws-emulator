// SPDX-License-Identifier: CC0-1.0

//! The diagnosis chain a client drives over a deployed image-labelling estate: the package the
//! control plane hands out a download URL for, the notification configuration Amazon S3 judges
//! before it stores it, and the transactional write the client follows up with.
//!
//! RULE under test: **`PutBucketNotificationConfiguration` is an atomic full replacement Amazon
//! S3 judges BEFORE it stores anything — a container naming a Lambda destination whose only
//! invoke grant is scoped to a source account other than the publishing bucket's owner, and a
//! container whose key-name rules leave two of its rules indistinguishable for one event type,
//! are each refused HTTP 400 `InvalidArgument` with the sentence S3 states for that class — so
//! the configuration already in force survives a refusal in BOTH halves of what it is: what a
//! read reports (every key-name rule under the canonical capitalised `Name`, whatever case the
//! caller sent) and what it DOES (every accepted write of a key its rules admit runs the
//! destination function once more, and a key its rules do not admit runs nothing).**
//!
//! Beside that, the two reads the same client makes of the estate it just deployed: the
//! presigned `Code.Location` `GetFunction` announces serves the stored deployment package, whose
//! bytes are the ones that same read reports the `CodeSize` and the `CodeSha256` of; and a
//! `TransactWriteItems` whose element defines a substitution token none of that element's own
//! expressions uses is refused with the service's own bare sentence, having written nothing.
//!
//! Five crates have to agree for the chain to hold — CloudFormation provisions the estate and
//! projects its status, IAM answers for the execution role, Amazon S3 validates the
//! configuration and makes the invocation, AWS Lambda holds the grant, serves the package and
//! runs the handler, and Amazon DynamoDB serves the table the handler writes — so the suite
//! lives here: a crate-local suite can only ever see one side of it. Its siblings reach one side
//! each: [`crate::s3_notification_delivers_once_per_object_write`] pins the per-write delivery of
//! a configuration a STACK declares, and [`crate::s3_notification_invokes_its_lambda_target`]
//! pins which key-name filters admit a first write. What only this suite reaches is that a
//! REFUSED put leaves the delivering pipeline in force: a client that recovers from either
//! refusal still gets every delivery the surviving configuration owes, under the rule names the
//! surviving configuration was canonicalised to.
//!
//! The rows span what the rules generalise over: which identity a download URL is announced for
//! (the unpublished function, a published version); why a container is refused (a grant naming a
//! foreign source account, two rules nothing in their filters tells apart) and which destination
//! carries the defect; which case a key-name rule's `Name` arrives in (lower, upper, mixed) and
//! which kind it is (prefix, suffix); how many times an admitted key is written (one, two) and
//! why an unadmitted one is excluded (a prefix rule, a suffix rule read in the key's own letter
//! case); and which substitution table a transacted element leaves unused, offered to which of
//! its expressions.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test holding
//! several of them across await points lays that state out on the test thread's stack and
//! overflows it as the emulator grows; the box puts each one on the heap instead.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the one it reads.
const REGION: &str = "us-east-1";

/// The `StackStatus` a create settles into on the success path.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The one stack, bucket, table and two functions this suite deploys.
const STACK: &str = "notification-refusal-estate";
const BUCKET: &str = "notification-refusal-images";
const TABLE: &str = "notification-refusal-labels";
const RECORDER: &str = "notification-refusal-recorder";
const OUTSIDER: &str = "notification-refusal-outsider";

/// The key attribute the declared table is keyed on, and the attributes the handler keeps beside
/// it: how many deliveries it has been handed for that key, and what the last one named.
const KEY_ATTRIBUTE: &str = "image_name";
const COUNT_ATTRIBUTE: &str = "deliveries";
const EVENT_ATTRIBUTE: &str = "event_name";
const REQUEST_ATTRIBUTE: &str = "last_request_id";

/// The event class every configuration below is notified about, and the `eventName` a put is
/// published under.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The account the OUTSIDER's grant names as the owner of the publishing bucket: a 12-digit
/// account that is not the one this emulator serves, so every part of that grant fits except the
/// account the notification really comes from (`AddPermission$SourceAccount` is "for Amazon Web
/// Services service principals, the ID of the Amazon Web Services account that owns the
/// resource").
const FOREIGN_ACCOUNT: &str = "111122223333";

/// The error code and status Amazon S3 refuses a container it will not store with
/// ("InvalidArgument — Invalid Argument", HTTP 400, from Amazon S3's list of error codes).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The fixed sentence of each class a refused container falls in: a destination S3 could not
/// validate, and two rules nothing in their key-name filters tells apart.
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";
const AMBIGUOUS: &str = "Configuration is ambiguously defined. Cannot have overlapping suffixes \
                         in two rules if the prefixes are overlapping for the same event type.";

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
/// that it is not waited out for ever.
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
/// named, through the AWS SDK the managed Node runtime supplies.
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
                'SET event_name = :event, last_request_id = :request ADD deliveries :one',
            ExpressionAttributeValues: {
                ':event': { S: record.eventName },
                ':request': { S: record.responseElements['x-amz-request-id'] },
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
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(body.to_owned()),
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

/// Follow a URL a service ANNOUNCED, the way a client follows a presigned one: at the host the
/// URL names and carrying no `Authorization` header at all.
async fn followed(router: &MultiServiceRouter, url: &str) -> Answered {
    let host = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    let request = http::Request::builder()
        .method("GET")
        .uri(url)
        .header("host", host)
        .body(Bytes::new())
        .expect("the download request must build");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// The `__type` code and the message of an awsJson refusal.
fn json_refusal(answered: &Answered) -> (String, String) {
    let body = answered.json();
    let declared = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let code = declared.split('#').next_back().unwrap_or_default().to_string();
    let message = body
        .get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (code, message)
}

/// The `<Code>` and `<Message>` of an S3 error document.
fn s3_refusal(answered: &Answered) -> (String, String) {
    let body = answered.text();
    (
        opt_element(&body, "Code").unwrap_or_default(),
        opt_element(&body, "Message").unwrap_or_default(),
    )
}

/// The estate as ONE template: the table the handler writes, the execution role, the inline
/// `nodejs20.x` handler, a second function of the same shape, the grant Amazon S3 needs on each
/// — the recorder's naming THIS account as the owner of the publishing bucket and the outsider's
/// naming [`FOREIGN_ACCOUNT`] — and the bucket itself, which the template leaves with no
/// notification configuration at all so every configuration below is one this suite PUT.
fn estate_template() -> String {
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
                    "FunctionName": RECORDER,
                    "Runtime": "nodejs20.x",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["RecorderRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { "TABLE_NAME": TABLE } },
                    "Code": { "ZipFile": COUNTING_HANDLER },
                },
            },
            "Outsider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": OUTSIDER,
                    "Runtime": "nodejs20.x",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["RecorderRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { "TABLE_NAME": TABLE } },
                    "Code": { "ZipFile": COUNTING_HANDLER },
                },
            },
            "RecorderPermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Recorder", "Arn"] },
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                    "SourceAccount": { "Ref": "AWS::AccountId" },
                    "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
                },
            },
            // Every part of this grant fits the publication except the account it names as the
            // owner of the publishing bucket.
            "OutsiderPermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Outsider", "Arn"] },
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                    "SourceAccount": FOREIGN_ACCOUNT,
                    "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
                },
            },
            "ImageBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": BUCKET },
            },
        },
        "Outputs": {
            "BucketName": { "Value": { "Ref": "ImageBucket" } },
            "TableName": { "Value": { "Ref": "LabelsTable" } },
            "RecorderArn": { "Value": { "Fn::GetAtt": ["Recorder", "Arn"] } },
            "OutsiderArn": { "Value": { "Fn::GetAtt": ["Outsider", "Arn"] } },
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
            "",
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

/// One key-name `FilterRule` as a request's XML carries it: the `Name` in the CASE the caller
/// sent, and the value it searches for in object key names.
struct SentRule {
    name: &'static str,
    value: &'static str,
}

/// One Lambda configuration of a container: its id, the destination it publishes to, and the
/// key-name rules it filters by.
struct SentConfiguration<'a> {
    id: &'static str,
    function_arn: &'a str,
    rules: &'a [SentRule],
}

/// A `NotificationConfiguration` document naming one Lambda configuration per entry, for
/// [`OBJECT_CREATED`].
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`: `LambdaFunctionConfigurations`
/// is bound as `CloudFunctionConfiguration`, whose ARN member is bound as `CloudFunction`).
fn notification_document(configurations: &[SentConfiguration<'_>]) -> String {
    let written: String = configurations
        .iter()
        .map(|configuration| {
            let rules: String = configuration
                .rules
                .iter()
                .map(|rule| {
                    format!(
                        "<FilterRule><Name>{}</Name><Value>{}</Value></FilterRule>",
                        rule.name, rule.value
                    )
                })
                .collect();
            let filter = if rules.is_empty() {
                String::new()
            } else {
                format!("<Filter><S3Key>{rules}</S3Key></Filter>")
            };
            format!(
                "<CloudFunctionConfiguration><Id>{}</Id><CloudFunction>{}</CloudFunction>\
                 <Event>{OBJECT_CREATED}</Event>{filter}</CloudFunctionConfiguration>",
                configuration.id, configuration.function_arn
            )
        })
        .collect();
    format!("<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{written}</NotificationConfiguration>")
}

/// `PUT /<bucket>?notification`.
async fn put_notification(router: &MultiServiceRouter, document: &str) -> Answered {
    s3(router, "PUT", &format!("/{BUCKET}?notification"), document).await
}

/// `GET /<bucket>?notification`, as the document the service reports it.
async fn installed_notification(router: &MultiServiceRouter, case: &str) -> String {
    let read = s3(router, "GET", &format!("/{BUCKET}?notification"), "").await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetBucketNotificationConfiguration, got {} with \
         {:?}",
        read.status,
        read.text()
    );
    read.text()
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

/// What the handler has recorded for one key: how many deliveries it counted, what the last one
/// named, and the item as it stands.
#[derive(Debug, Clone)]
struct Delivered {
    count: u64,
    event_name: String,
    request_id: String,
    item: Value,
}

/// One attribute of an item, read as the `kind` the handler wrote it under.
fn attribute(item: &Value, name: &str, kind: &str) -> Option<String> {
    item.get(name)?
        .get(kind)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What the declared table holds for `key` right now — `None` when it holds no item at all.
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
        item,
    })
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

/// The key-name rules the surviving configuration is installed with, in the CASE the caller sends
/// them and in the capitalised spelling a read must report.
///
/// The vocabulary is the model's: `FilterRuleName`
/// (`smithy_query(query="shape",shape="FilterRuleName")`) publishes `prefix` and `suffix`, and
/// the spelling a read reports is the capitalised one — so each row below sends one published
/// name in a case of its own.
const RULE_SPELLINGS: &[(&str, &str)] = &[
    ("suffix", "Suffix"),
    ("SUFFIX", "Suffix"),
    ("PreFix", "Prefix"),
    ("Suffix", "Suffix"),
];

/// One delivery row: the key written into the declared bucket, how many times, whether the
/// surviving configuration's rules admit it, and why.
struct DeliveryRow {
    case: &'static str,
    key: &'static str,
    /// One body per write, so a row differs in how many writes it makes and in whether the
    /// writes carry the same bytes.
    bodies: &'static [&'static str],
    admitted: bool,
}

/// The rows span what the delivery half of the rule generalises over: which of the installed
/// rules admits the key (the rule sent lower-case, the one sent upper-case, the prefix sent in
/// mixed case beside a canonical suffix), how many times the key is written (one, two), whether
/// the writes carry the same bytes, and why an unadmitted key is excluded (a prefix rule, and a
/// suffix rule read in the key's own letter case).
const DELIVERY_ROWS: &[DeliveryRow] = &[
    DeliveryRow {
        case: "a key the rule sent lower-case admits, written once",
        key: "uploads/portrait.jpg",
        bodies: &["the first upload"],
        admitted: true,
    },
    DeliveryRow {
        case: "a key the rule sent lower-case admits, written twice with the same bytes",
        key: "uploads/reupload.jpg",
        bodies: &["the same bytes", "the same bytes"],
        admitted: true,
    },
    DeliveryRow {
        case: "a key the rule sent upper-case admits, written twice with new bytes",
        key: "uploads/portrait.jpeg",
        bodies: &["the first bytes", "the replacing bytes"],
        admitted: true,
    },
    DeliveryRow {
        case: "a key the mixed-case prefix rule and its suffix rule both admit",
        key: "uploads/diagram.png",
        bodies: &["a diagram"],
        admitted: true,
    },
    DeliveryRow {
        case: "a key the mixed-case prefix rule excludes",
        key: "archive/diagram.png",
        bodies: &["an archived diagram"],
        admitted: false,
    },
    DeliveryRow {
        case: "a key differing from an admitted one only in letter case, written twice",
        key: "uploads/portrait.JPG",
        bodies: &["the first upload", "the replacing upload"],
        admitted: false,
    },
];

/// The whole chain, over one deployed estate. Driven on a sized thread; see [`driven`].
#[test]
fn a_refused_notification_container_leaves_the_installed_pipeline_in_force() {
    driven("s3-notification-refusals-spare-the-installed-pipeline", || {
        Box::pin(a_refused_notification_container_leaves_the_installed_pipeline_in_force_body())
    });
}

async fn a_refused_notification_container_leaves_the_installed_pipeline_in_force_body() {
    let case = "the deployed image-labelling estate";
    let router = emulator();

    // 1. The estate, provisioned by ONE CreateStack that settles into CREATE_COMPLETE.
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
    let (reported, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the declared estate to provision, got {reported:?} in \
         {described}"
    );
    let outputs = outputs_of(&described);
    let recorder_arn = outputs
        .get("RecorderArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the handler's ARN"));
    let outsider_arn = outputs
        .get("OutsiderArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the outsider's ARN"));
    assert_eq!(
        (
            outputs.get("BucketName").map(String::as_str),
            outputs.get("TableName").map(String::as_str),
        ),
        (Some(BUCKET), Some(TABLE)),
        "case {case:?}: expected the settled stack to publish the names it declared, got \
         {outputs:?}"
    );

    // 2. Each declared resource exists in the service that OWNS it, and is ready to be used.
    for function in [RECORDER, OUTSIDER] {
        let state = awaited_function_state(&router, function).await;
        assert_eq!(
            state, "Active",
            "case {case:?}: expected AWS Lambda to report {function:?} Active, got {state:?}"
        );
    }
    let table_status = awaited_table_status(&router, TABLE).await;
    assert_eq!(
        table_status, "ACTIVE",
        "case {case:?}: expected Amazon DynamoDB to report the declared table ACTIVE, got \
         {table_status:?}"
    );
    let bucket = s3(&router, "HEAD", &format!("/{BUCKET}"), "").await;
    assert_eq!(
        bucket.status,
        200,
        "case {case:?}: expected HTTP 200 from HeadBucket({BUCKET:?}), got {}",
        bucket.status
    );

    // 3. The deployment package the estate declared is SERVED by the presigned URL the read
    //    announces for it, for each identity that read reports one for: the unpublished function,
    //    and a published version, whose package is frozen at the revision it was cut from.
    let published = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{RECORDER}/versions"),
        "{}",
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "case {case:?}: expected HTTP 201 from PublishVersion, got {} with {:?}",
        published.status,
        published.text()
    );
    let version = published
        .json()
        .get("Version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !version.is_empty(),
        "case {case:?}: PublishVersion must report the version it cut, got {:?}",
        published.text()
    );
    for (identity, path) in [
        ("the unpublished function", format!("/2015-03-31/functions/{RECORDER}")),
        (
            "the published version",
            format!("/2015-03-31/functions/{RECORDER}?Qualifier={version}"),
        ),
    ] {
        let read = lambda(&router, "GET", &path, "").await;
        assert_eq!(
            read.status,
            200,
            "case {identity:?}: expected HTTP 200 from GetFunction, got {} with {:?}",
            read.status,
            read.text()
        );
        let answered = read.json();
        let location = answered
            .pointer("/Code/Location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            location.starts_with("https://"),
            "case {identity:?}: expected GetFunction to announce a download URL, got \
             {location:?}"
        );
        let fetched = followed(&router, &location).await;
        assert_eq!(
            fetched.status,
            200,
            "case {identity:?}: expected HTTP 200 following {location:?}, got {} with {:?}",
            fetched.status,
            fetched.text()
        );
        // The bytes answered are the ones the same read reports the size and the digest of:
        // `CodeSha256` is "the SHA256 hash of the function's deployment package", which AWS
        // reports base64-encoded, and `CodeSize` is "the size of the function's deployment
        // package, in bytes".
        let reported_size = answered
            .pointer("/Configuration/CodeSize")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let reported_digest = answered
            .pointer("/Configuration/CodeSha256")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            u64::try_from(fetched.body.len()).unwrap_or_default(),
            reported_size,
            "case {identity:?}: expected the announced URL to serve the {reported_size}-byte \
             package the read reports, got {} bytes",
            fetched.body.len()
        );
        let served_digest = aws_smithy_types::base64::encode(Sha256::digest(&fetched.body));
        assert_eq!(
            served_digest, reported_digest,
            "case {identity:?}: expected the served bytes to digest to the reported \
             {reported_digest:?}, got {served_digest:?}"
        );
        assert!(
            !fetched.body.is_empty(),
            "case {identity:?}: a deployment package is not empty, got no bytes at all"
        );
    }

    // 4. The configuration the client installs: one Lambda configuration per suffix it filters
    //    on, each rule's `Name` sent in a case of its own. The read reports every one of them
    //    under the capitalised spelling, and none under the case it was sent in.
    let installed_document = notification_document(&[
        SentConfiguration {
            id: "sent-lower-case",
            function_arn: &recorder_arn,
            rules: &[SentRule {
                name: RULE_SPELLINGS[0].0,
                value: ".jpg",
            }],
        },
        SentConfiguration {
            id: "sent-upper-case",
            function_arn: &recorder_arn,
            rules: &[SentRule {
                name: RULE_SPELLINGS[1].0,
                value: ".jpeg",
            }],
        },
        SentConfiguration {
            id: "sent-mixed-case",
            function_arn: &recorder_arn,
            rules: &[
                SentRule {
                    name: RULE_SPELLINGS[2].0,
                    value: "uploads/",
                },
                SentRule {
                    name: RULE_SPELLINGS[3].0,
                    value: ".png",
                },
            ],
        },
    ]);
    let stored = put_notification(&router, &installed_document).await;
    assert_eq!(
        stored.status,
        200,
        "case {case:?}: expected HTTP 200 from PutBucketNotificationConfiguration, got {} with \
         {:?}",
        stored.status,
        stored.text()
    );
    let installed = installed_notification(&router, case).await;
    for (sent, canonical) in RULE_SPELLINGS {
        assert!(
            installed.contains(&format!("<Name>{canonical}</Name>")),
            "case {sent:?}: expected the read to report the rule under {canonical:?}, got \
             {installed}"
        );
        if sent != canonical {
            assert!(
                !installed.contains(&format!("<Name>{sent}</Name>")),
                "case {sent:?}: expected the read to report no rule under the case it was sent \
                 in, got {installed}"
            );
        }
    }
    for (rule, value) in [
        ("Suffix", ".jpg"),
        ("Suffix", ".jpeg"),
        ("Prefix", "uploads/"),
        ("Suffix", ".png"),
    ] {
        let expected = format!("<FilterRule><Name>{rule}</Name><Value>{value}</Value></FilterRule>");
        assert!(
            installed.contains(&expected),
            "case {case:?}: expected the read to report {expected}, got {installed}"
        );
    }

    // 5. Each container S3 refuses, one at a time: the status, the code and the sentence of its
    //    class, and the installed configuration unchanged after it — the put being atomic, a
    //    refused container replaces nothing.
    let refusals: Vec<(&str, String, &str)> = vec![
        (
            "a destination whose only grant names a foreign source account",
            notification_document(&[SentConfiguration {
                id: "outsider",
                function_arn: &outsider_arn,
                rules: &[SentRule {
                    name: "suffix",
                    value: ".jpg",
                }],
            }]),
            UNABLE_TO_VALIDATE,
        ),
        (
            "two rules sharing a suffix for one event type",
            notification_document(&[
                SentConfiguration {
                    id: "first-of-the-pair",
                    function_arn: &recorder_arn,
                    rules: &[SentRule {
                        name: "suffix",
                        value: ".jpg",
                    }],
                },
                SentConfiguration {
                    id: "second-of-the-pair",
                    function_arn: &recorder_arn,
                    rules: &[SentRule {
                        name: "suffix",
                        value: ".jpg",
                    }],
                },
            ]),
            AMBIGUOUS,
        ),
    ];
    for (refused_case, document, sentence) in refusals {
        let answered = put_notification(&router, &document).await;
        let (code, message) = s3_refusal(&answered);
        assert_eq!(
            (answered.status, code.as_str(), message.as_str()),
            (INVALID_ARGUMENT_STATUS, INVALID_ARGUMENT, sentence),
            "case {refused_case:?}: expected HTTP {INVALID_ARGUMENT_STATUS} \
             {INVALID_ARGUMENT} {sentence:?}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        let after = installed_notification(&router, refused_case).await;
        assert_eq!(
            after, installed,
            "case {refused_case:?}: a refused container replaces nothing, so the installed \
             configuration must read back unchanged"
        );
    }

    // 6. And the installed configuration still DOES what it reports: every accepted write of a
    //    key its rules admit runs the destination function once more, and a key its rules do not
    //    admit runs nothing.
    for row in DELIVERY_ROWS {
        let row_case = row.case;
        if !row.admitted {
            // Every write first, then ONE quiet window: the handler never deletes an item, so a
            // delivery of any of these writes would still be there to be read.
            for body in row.bodies {
                put_object(&router, row_case, row.key, body).await;
            }
            let seen = quietly_absent(&router, row_case, row.key).await;
            assert!(
                seen.is_none(),
                "case {row_case:?}: no installed rule admits {:?}, so none of its {} writes may \
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
            put_object(&router, row_case, row.key, body).await;
            let seen = awaited_delivery(&router, row_case, row.key, wanted).await;
            let seen = seen.unwrap_or_else(|| {
                panic!(
                    "case {row_case:?}: expected write {write} of {:?} to be published, so the \
                     handler's counter must reach {wanted}, got no item at all",
                    row.key
                )
            });
            assert_eq!(
                seen.count, wanted,
                "case {row_case:?}: expected exactly one delivery per write, so after write \
                 {write} of {:?} the counter must be {wanted}, got {} in {}",
                row.key, seen.count, seen.item
            );
            assert_eq!(
                seen.event_name, PUT_EVENT_NAME,
                "case {row_case:?}: expected write {write} of {:?} to be published as \
                 {PUT_EVENT_NAME}, got {:?} in {}",
                row.key, seen.event_name, seen.item
            );
            if let Some(earlier) = previous {
                assert_ne!(
                    seen.request_id, earlier.request_id,
                    "case {row_case:?}: write {write} of {:?} is its own call, so its record \
                     must not name the previous call {:?}",
                    row.key, earlier.request_id
                );
            }
            previous = Some(seen);
        }
    }

    // 7. The follow-up write the client makes against the declared table: an element that
    //    defines a substitution token none of THAT element's expressions uses is refused with
    //    the service's own bare sentence — the `<N> validation error(s) detected: ` envelope
    //    reports violations of the REQUEST's own members, and an element's expression members
    //    are members of that element — and the transaction writes nothing.
    let unused_probe = "transacted-probe";
    let transacted: Vec<(&str, Value, String)> = vec![
        (
            "a name a transacted put's condition never references",
            json!({"TransactItems": [{"Put": {
                "TableName": TABLE,
                "Item": {KEY_ATTRIBUTE: {"S": unused_probe}, "labels": {"S": "cat"}},
                "ConditionExpression": format!("attribute_not_exists({KEY_ATTRIBUTE})"),
                "ExpressionAttributeNames": {"#unused": "labels"}}}]}),
            "Value provided in ExpressionAttributeNames unused in expressions: keys: {#unused}"
                .to_string(),
        ),
        (
            "two values a transacted update's expression never references",
            json!({"TransactItems": [{"Update": {
                "TableName": TABLE,
                "Key": {KEY_ATTRIBUTE: {"S": unused_probe}},
                "UpdateExpression": "SET labels = :kept",
                "ExpressionAttributeValues": {":kept": {"S": "cat"},
                                              ":spare": {"S": "dog"},
                                              ":other": {"S": "bird"}}}}]}),
            "Value provided in ExpressionAttributeValues unused in expressions: keys: \
             {:other, :spare}"
                .to_string(),
        ),
    ];
    for (transact_case, request, sentence) in transacted {
        let answered = dynamodb(&router, "TransactWriteItems", request).await;
        let (code, message) = json_refusal(&answered);
        assert_eq!(
            (answered.status, code.as_str(), message.as_str()),
            (400, "ValidationException", sentence.as_str()),
            "case {transact_case:?}: expected HTTP 400 ValidationException {sentence:?}, got {} \
             with {:?}",
            answered.status,
            answered.text()
        );
        let after = held(&router, transact_case, unused_probe).await;
        assert!(
            after.is_none(),
            "case {transact_case:?}: a refused transaction writes nothing, got the item {:?}",
            after.map(|read| read.item)
        );
    }

    // The BOUNDARY the bare sentence is stated against: the same defect in the REQUEST's own
    // members keeps the envelope, so its absence above says what it says.
    let single = dynamodb(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {KEY_ATTRIBUTE: {"S": unused_probe}},
               "ConditionExpression": format!("attribute_not_exists({KEY_ATTRIBUTE})"),
               "ExpressionAttributeNames": {"#unused": "labels"}}),
    )
    .await;
    let (code, message) = json_refusal(&single);
    assert_eq!(
        (single.status, code.as_str()),
        (400, "ValidationException"),
        "case {case:?}: the same defect in the request's own members is a 400 \
         ValidationException, got {} with {:?}",
        single.status,
        single.text()
    );
    assert_eq!(
        message,
        "1 validation error detected: Value provided in ExpressionAttributeNames unused in \
         expressions: keys: {#unused}",
        "case {case:?}: a violation of the REQUEST's own members is stated inside the envelope, \
         got {message:?}"
    );
}
