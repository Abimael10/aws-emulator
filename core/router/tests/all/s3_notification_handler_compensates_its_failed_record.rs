// SPDX-License-Identifier: CC0-1.0

//! What an S3-triggered handler's FAILURE PATH leaves behind, end to end over the assembled
//! router: a bucket notification runs the configured function once per matching write, and the
//! writes the handler makes while handling one record — the ones it makes before it fails and
//! the compensating one it makes afterwards — are all readable from the table it wrote them to.
//!
//! The rule under test: **a bucket whose notification configuration names a Lambda function for
//! an event class publishes every matching object write to that function as ONE asynchronous
//! invocation of its own, so the whole sequence of calls that invocation's handler performs for
//! the record — including the calls it performs after its own processing step raised — lands and
//! is readable afterwards, and neither the handler's failure on one record nor its success on
//! another changes what the other records' invocations do; a write whose key the configuration's
//! key-name `Filter` excludes is published to nothing.**
//!
//! Five crates have to agree for that to hold (S3 evaluates the configuration and makes the
//! invocation, Lambda holds the grant and RUNS the deployment package, CloudFormation archives
//! the inline handler into that package, IAM answers for the execution role the handler's own
//! call is signed as, and DynamoDB serves the table it writes), so the suite lives here: a
//! crate-local suite can only ever see one side of it. Its siblings reach the delivery of a
//! handler that SUCCEEDS — [`crate::s3_notification_invokes_its_lambda_target`] pins what the
//! `Filter` admits, [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] pins the grant
//! that decides it — and what only this suite reaches is a handler whose processing step FAILS:
//! the pre-state it had already written, the compensating write it makes instead of the
//! completion, and the other records of the same bucket carrying on regardless. Without that, a
//! client's compensating (saga) logic can never be exercised against the emulator at all.
//!
//! The rows differ along what the rule generalises over: the path the handler takes for the
//! written key (its processing step succeeds, or raises and is compensated), where that key sits
//! among the other writes into the same bucket (before a failing one, after it), and whether the
//! configuration's key-name `Filter` admits the key at all.
//!
//! Every observation is one a client makes: what S3 answered, and what the table holds
//! afterwards, read through DynamoDB's own strongly consistent `GetItem`. Nothing here asserts
//! how long a delivery took — each wait is on the CONDITION, looked for again until it holds or
//! a generous deadline passes.
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

// The stack poll and the awsQuery readers this test crate writes ONCE, rather than a second copy
// of any of them.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region the shared [`authorization`] builder
/// and the shared stack poll ([`wait_for_stack_status`]) sign for, so the stack this suite
/// creates is the stack that poll reports.
const REGION: &str = "us-east-1";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on a
/// function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class the configuration names: "you can enable notifications for every event of a
/// type by using the wildcard", so this is every object-created event.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The prefix the configuration's key-name `Filter` admits. A key outside it is how the other
/// half of the rule is observed: that write is published to nothing.
const WATCHED_PREFIX: &str = "uploads/";

/// The substring of a key the handler's own processing step treats as the failure it has to
/// compensate for. It is the handler's own rule, not S3's: what the suite pins is what the
/// handler's writes come to, whichever branch it took.
const FAILING_MARKER: &str = "fail";

/// The three statuses the handler records for one record, and the separator it keys them under —
/// the key, then the status, so every write of one record is its own item and none overwrites
/// another.
const RESERVED: &str = "reserved";
const COMPLETED: &str = "completed";
const ROLLED_BACK: &str = "rolled_back";
const STATUS_SEPARATOR: &str = "#";

/// The body every row writes. Its content is immaterial: what a row observes is the items the
/// handler wrote for the record it was handed.
const OBJECT_BODY: &str = "an upload whose processing is notified";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite looks for work the clock owes — the stack's resources coming up, the
/// function reported `Active`, the table reported `ACTIVE`, the asynchronous delivery of an
/// object event and the writes its handler makes — and how often it looks while waiting.
/// Generous: no row asserts a span, only that it does not wait one out forever.
const WAIT: Duration = Duration::from_secs(90);
/// See [`WAIT`].
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long the row that must observe NOTHING keeps looking before concluding nothing came. It is
/// read only after every delivered row has left its items, so the pipeline is known to be live by
/// then.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling notification suites use. Assembling
/// every service and then dispatching a CHAIN of in-process calls through it — CloudFormation
/// into Lambda and DynamoDB, S3's delivery into Lambda, the handler's own SDK call into
/// DynamoDB — nests more future than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler the stack deploys — the shape a compensating (saga) pipeline has: for each record
/// it is handed it records the pre-state, runs its processing step, and records either the
/// completion or, where that step raised, the compensating rollback. A record's key "is URL
/// encoded" (Amazon S3 User Guide, *Event message structure*), so it is decoded the way that
/// guide's own handler decodes it before being recorded.
const SAGA_HANDLER: &str = r##"
import os
import urllib.parse

import boto3

FAILING_MARKER = "fail"
RESERVED = "reserved"
COMPLETED = "completed"
ROLLED_BACK = "rolled_back"
STATUS_SEPARATOR = "#"


def record_status(client, table, bucket, key, status):
    client.put_item(
        TableName=table,
        Item={
            "pk": {"S": key + STATUS_SEPARATOR + status},
            "bucket": {"S": bucket},
            "objectKey": {"S": key},
            "status": {"S": status},
        },
    )


def handler(event, context):
    table = os.environ["TABLE"]
    client = boto3.client("dynamodb")
    handled = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        record_status(client, table, bucket, key, RESERVED)
        try:
            if FAILING_MARKER in key:
                raise RuntimeError("the processing step failed for " + key)
            record_status(client, table, bucket, key, COMPLETED)
        except RuntimeError:
            record_status(client, table, bucket, key, ROLLED_BACK)
        handled.append(key)
    return {"handled": handled}
"##;

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
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
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

/// The ARN of a bucket, which is how a grant on the destination function names the source of the
/// invocation ("the ARN of the Amazon Web Services resource that invokes the function",
/// `AddPermission$SourceArn`). A bucket is global, so the ARN names no Region and no account.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "setup: expected HTTP 200 from CreateBucket({bucket:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`. The write is accepted whatever the configuration goes on
/// to publish, so every row asserts it.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}/{key}"), OBJECT_BODY).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from PutObject({bucket:?}, {key:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// `PUT /<bucket>?notification` with ONE Lambda configuration naming `function_arn` for
/// [`OBJECT_CREATED`], filtered to [`WATCHED_PREFIX`].
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_lambda_notification(router: &MultiServiceRouter, bucket: &str, function_arn: &str) {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>process-the-upload</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event>\
         <Filter><S3Key><FilterRule><Name>prefix</Name><Value>{WATCHED_PREFIX}</Value>\
         </FilterRule></S3Key></Filter>\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let answered = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answered.status,
        200,
        "setup: expected HTTP 200 from PutBucketNotificationConfiguration({bucket:?}), got {} \
         with {:?}",
        answered.status,
        answered.text()
    );
    // The configuration a later read reports is the destination the writes below are published
    // to, so the wiring is confirmed before any object is written.
    let read = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        read.status,
        200,
        "setup: expected HTTP 200 from GetBucketNotificationConfiguration({bucket:?}), got {} \
         with {:?}",
        read.status,
        read.text()
    );
    let reported = elements(&read.text(), "CloudFunction");
    assert_eq!(
        reported,
        vec![function_arn.to_string()],
        "setup: expected the bucket to report {function_arn:?} as its Lambda destination, got \
         {reported:?}"
    );
}

/// The SAM template this suite deploys ONCE: the handler above, and the table it writes, named to
/// the function through the environment variable the transform carries through. `InlineCode` is
/// what archives handler SOURCE into a deployment package this emulator can run.
fn template() -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Processor": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "InlineCode": SAGA_HANDLER,
                    "Environment": { "Variables": { "TABLE": { "Ref": "Progress" } } },
                    "Policies": [
                        { "DynamoDBCrudPolicy": { "TableName": { "Ref": "Progress" } } }
                    ]
                }
            },
            "Progress": {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": "pk", "Type": "String" } }
            }
        }
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and answer
/// the configuration it reported then. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn active_function(router: &MultiServiceRouter, name: &str) -> Value {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        let configuration = reported.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE` — "you can perform read
/// and write operations only on an ACTIVE table".
async fn await_active_table(router: &MultiServiceRouter, table: &str) {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        if described
            .json()
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            == Some("ACTIVE")
        {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: DynamoDB never reported {table} ACTIVE");
}

/// How the handler keys the item it writes for one status of one record.
fn item_key(key: &str, status: &str) -> String {
    format!("{key}{STATUS_SEPARATOR}{status}")
}

/// The item `table` holds under `pk` right now, or [`Value::Null`] when it holds none.
async fn item_under(router: &MultiServiceRouter, case: &str, table: &str, pk: &str) -> Value {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": table,
            "Key": { "pk": { "S": pk } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetItem({table:?}, {pk:?}), got {} with {:?}",
        read.status,
        read.text()
    );
    read.json().get("Item").cloned().unwrap_or(Value::Null)
}

/// Look for the item the handler owes under `pk` until it is there, or — when `expected` is
/// false — through [`QUIET_WINDOW`], and answer what was last read. The wait is on the CONDITION;
/// no row asserts how long it took.
async fn awaited_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    pk: &str,
    expected: bool,
) -> Value {
    let wait = if expected { WAIT } else { QUIET_WINDOW };
    let looks = wait.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = Value::Null;
    for _ in 0..looks {
        last = item_under(router, case, table, pk).await;
        if expected && last != Value::Null {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// One row: the key written into the shared bucket, and what the handler's invocation for it must
/// come to.
struct Row {
    case: &'static str,
    key: &'static str,
    /// Whether the configuration's key-name `Filter` admits the key, and so whether the write is
    /// published to the function at all.
    delivered: bool,
    /// The terminal status the handler records for the record, where it is handed one.
    terminal: &'static str,
}

/// The rows span the handler's own path and the filter's verdict: a key whose processing step
/// succeeds before the failing one, the failing one itself, a key whose step succeeds after it,
/// and a key the filter excludes. They are written in this order into one bucket, so a failure
/// that stopped the pipeline — or leaked into another record — fails here.
const ROWS: &[Row] = &[
    Row {
        case: "a key whose processing step succeeds, written before the failing one",
        key: "uploads/order-1.json",
        delivered: true,
        terminal: COMPLETED,
    },
    Row {
        case: "a key whose processing step raises, which the handler compensates for",
        key: "uploads/order-2-fail.json",
        delivered: true,
        terminal: ROLLED_BACK,
    },
    Row {
        case: "a key whose processing step succeeds, written after the failing one",
        key: "uploads/nested/order-3.json",
        delivered: true,
        terminal: COMPLETED,
    },
    Row {
        case: "a key the configuration's prefix filter excludes",
        key: "archive/order-4.json",
        delivered: false,
        terminal: COMPLETED,
    },
];

/// RULE: each matching write is published as its own asynchronous invocation, so every write the
/// handler makes for that record lands — the pre-state, and either the completion or the
/// compensating rollback — while a key the filter excludes reaches no function at all.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_notified_handlers_failure_path_leaves_its_compensating_write() {
    driven("s3-notification-saga-compensation", || {
        Box::pin(a_notified_handlers_failure_path_leaves_its_compensating_write_body())
    });
}

async fn a_notified_handlers_failure_path_leaves_its_compensating_write_body() {
    let router = emulator();

    // The shared wiring: ONE function and ONE table, so what differs between rows is only the key
    // written and the path the handler takes for it.
    let stack = "notification-saga-stack";
    let body = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let resources = cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical = physical_ids_of(&resources.text());
    let function_name = physical
        .get("Processor")
        .cloned()
        .expect("setup: no physical id for Processor");
    let table = physical
        .get("Progress")
        .cloned()
        .expect("setup: no physical id for Progress");
    let configuration = active_function(&router, &function_name).await;
    await_active_table(&router, &table).await;
    let function_arn = configuration
        .get("FunctionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: no FunctionArn in {configuration}"))
        .to_string();

    // The bucket, the grant S3 needs on the destination function, and the configuration that
    // names it — the order a client wires this in, because the put is admitted only once the
    // function's own policy lets Amazon S3 invoke it for this bucket.
    let bucket = "notification-saga-uploads";
    create_bucket(&router, bucket).await;
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function_name}/policy"),
        Some(json!({
            "StatementId": "s3-invoke",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": bucket_arn(bucket),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: expected HTTP 201 from AddPermission, got {} with {:?}",
        granted.status,
        granted.text()
    );
    put_lambda_notification(&router, bucket, &function_arn).await;

    // Every write first, in the rows' own order, so the records are handled as a client's uploads
    // are: one stream of writes into one bucket, one of which fails the handler.
    for row in ROWS {
        put_object(&router, row.case, bucket, row.key).await;
    }

    for row in ROWS {
        let case = row.case;
        // The row table and the handler agree on which key fails: the handler raises exactly for
        // a key carrying the marker, so a row expecting the compensating write has to carry it
        // and a row expecting the completion must not.
        assert_eq!(
            row.key.contains(FAILING_MARKER),
            row.terminal == ROLLED_BACK && row.delivered,
            "case {case:?}: the row's expected terminal status {:?} must follow from whether \
             {:?} carries {FAILING_MARKER:?}",
            row.terminal,
            row.key
        );
        let reserved = item_key(row.key, RESERVED);
        let held = awaited_item(&router, case, &table, &reserved, row.delivered).await;
        if !row.delivered {
            assert_eq!(
                held,
                Value::Null,
                "case {case:?}: the filter excludes {:?}, so no invocation and no item may \
                 follow its write, got {held}",
                row.key
            );
            continue;
        }
        assert_eq!(
            held,
            json!({
                "pk": { "S": reserved },
                "bucket": { "S": bucket },
                "objectKey": { "S": row.key },
                "status": { "S": RESERVED },
            }),
            "case {case:?}: expected the pre-state the handler wrote for the record of {:?}, got \
             {held}",
            row.key
        );

        // The terminal write of the same record: the completion, or the compensating rollback the
        // handler makes because its processing step raised.
        let terminal = item_key(row.key, row.terminal);
        let settled = awaited_item(&router, case, &table, &terminal, true).await;
        assert_eq!(
            settled,
            json!({
                "pk": { "S": terminal },
                "bucket": { "S": bucket },
                "objectKey": { "S": row.key },
                "status": { "S": row.terminal },
            }),
            "case {case:?}: expected the handler's {:?} write for the record of {:?}, got \
             {settled}",
            row.terminal,
            row.key
        );

        // And only that one: the branch the handler did not take wrote nothing. One read is
        // enough, because the handler makes the terminal write above AFTER the point the other
        // branch would have been written at.
        let other = if row.terminal == COMPLETED {
            ROLLED_BACK
        } else {
            COMPLETED
        };
        let unwritten = item_under(&router, case, &table, &item_key(row.key, other)).await;
        assert_eq!(
            unwritten,
            Value::Null,
            "case {case:?}: the handler took the {:?} branch for {:?}, so it may leave no {other:?} \
             item, got {unwritten}",
            row.terminal,
            row.key
        );
    }
}
