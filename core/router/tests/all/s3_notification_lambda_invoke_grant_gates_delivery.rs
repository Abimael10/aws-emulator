// SPDX-License-Identifier: CC0-1.0

//! The Lambda INVOKE GRANT as the gate on a bucket's notification wiring: what the grant on
//! the destination function decides, both for the configuration S3 records and for the event
//! it later delivers.
//!
//! The rule under test: **a `NotificationConfiguration` naming a Lambda destination is
//! admitted only while that function's own resource-based policy lets the Amazon S3 service
//! principal perform the invoking action on it for the configuring bucket — "in the case of
//! Lambda destinations, Amazon S3 verifies that the Lambda function permissions grant Amazon
//! S3 permission to invoke the function from the Amazon S3 bucket"
//! (`PutBucketNotificationConfiguration`'s own documentation). A container whose function
//! grants nothing for this bucket is refused WHOLE — HTTP 400, `InvalidArgument`, the fixed
//! message of that class — and records nothing, so a read of the bucket reports no Lambda
//! configuration at all; the IDENTICAL container put once that policy admits the invocation
//! is accepted, is reported by a read, and every matching object change from then on reaches
//! the function as one asynchronous invocation carrying the `Records` envelope, so the work
//! the handler itself performs lands.**
//!
//! Four crates have to agree for that to hold (CloudFormation provisions the function, its
//! role and its table; Lambda holds the grant, answers for it and runs the handler; S3
//! evaluates the grant on the put and makes the invocation on the write; DynamoDB serves the
//! table the handler writes), so the suite lives here: a crate-local suite can only ever see
//! one side of it. Its siblings reach one side each —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins the delivery of an already
//! granted function, and the S3 crate's own `notification_destination_validation` suite pins
//! the refusal document of a container S3 could not validate — and what only this suite
//! reaches is the two JOINED on one function whose grant MOVES: the same put refused and then
//! accepted, with the handler's own side effect in a third service as the evidence the
//! accepted wiring really delivers.
//!
//! The rows differ along what the rule generalises over — the part of the grant the decision
//! turns on: whether the statement scopes itself to the configuring bucket, to every source,
//! or to another bucket, and whether it names the invoking action or another action of the
//! same service, and which principal it names. Each row starts from a function carrying NO
//! statement at all, so every row asserts the refusal and the acceptance (or the second
//! refusal) of one identical request, and the grant is withdrawn again at the row's end so no
//! row inherits another's.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test
//! holding several of them across await points lays that state out on the test thread's stack
//! and overflows it as the emulator grows; the box puts each one on the heap instead.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, physical_ids_of};

/// The Region every request here is signed for and every ARN below names. It is the Region the
/// shared stack reader ([`wait_for_stack_status`]) signs for too.
const REGION: &str = "us-east-1";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes notifications under, and the action it performs
/// on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The error code and status S3 answers a container it could not validate with
/// ("InvalidArgument - Invalid Argument", HTTP 400, from Amazon S3's list of error codes).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The `<Message>` that refusal carries, as S3 words it. FIXED text: which destination failed
/// travels in the numbered argument series beside it, not in the sentence.
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";

/// The event class every configuration below is notified about — the value is immaterial to the
/// rule, which is about the GRANT.
const CONFIGURED_EVENT: &str = "s3:ObjectCreated:*";

/// The `eventName` one `Put` of an object is published under, which the handler records.
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The `eventSource` of every record S3 publishes, which the handler records.
const EVENT_SOURCE: &str = "aws:s3";

/// The body every row writes into its bucket. Its content is immaterial: what the row observes
/// is the ITEM the handler wrote for the record it was handed.
const OBJECT_BODY: &str = "an object whose creation is notified";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite looks for work the clock owes — a stack's resources coming up, a
/// function reported `Active`, a table reported `ACTIVE`, the asynchronous delivery of an
/// object event — and how often it looks while waiting. Generous: nothing here is a span the
/// suite asserts on, only one it refuses to wait out forever.
const WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row that must observe NOTHING keeps looking before concluding nothing came —
/// well past the span a delivered row takes to leave its item, so a row whose grant was
/// refused is read against rows of the same wiring that do deliver.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling notification suites use.
/// Assembling every service and then dispatching a CHAIN of in-process calls through it —
/// CloudFormation into Lambda and DynamoDB, S3's delivery into Lambda, the handler's own SDK
/// call into DynamoDB — nests more future than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler the stack deploys: for every record it is handed it writes one DynamoDB item
/// keyed by the object key, carrying what the record said about the change — so the item is
/// evidence the handler really ran on the envelope S3 published, and not merely that something
/// wrote to the table.
const RECORDING_HANDLER: &str = r#"
import os

import boto3


def handler(event, context):
    client = boto3.client("dynamodb")
    written = []
    for record in event["Records"]:
        key = record["s3"]["object"]["key"]
        client.put_item(
            TableName=os.environ["TABLE"],
            Item={
                "pk": {"S": key},
                "bucket": {"S": record["s3"]["bucket"]["name"]},
                "eventName": {"S": record["eventName"]},
                "eventSource": {"S": record["eventSource"]},
            },
        )
        written.append(key)
    return {"written": written}
"#;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread runtime
/// this suite's deliveries are made on: a notification delivery is a task of its own, so it
/// runs on this same thread and needs the same depth.
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

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
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

/// The ARN of a bucket, which is how a grant on the destination function names the source of
/// the invocation ("the ARN of the Amazon Web Services resource that invokes the function",
/// `AddPermission$SourceArn`). A bucket is global, so the ARN names no Region and no account.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateBucket({bucket:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`. The write is accepted whatever the notification
/// configuration does, so every row asserts it.
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
/// [`CONFIGURED_EVENT`] — the identical request every row issues twice.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_lambda_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    function_arn: &str,
) -> Answered {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>record-the-event</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{CONFIGURED_EVENT}</Event>\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    s3(router, "PUT", &format!("/{bucket}?notification"), &body).await
}

/// The function ARNs `GetBucketNotificationConfiguration` reports for `bucket`, in the order it
/// reports them — empty for a bucket carrying no Lambda configuration.
async fn reported_functions(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
) -> Vec<String> {
    let answered = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from GetBucketNotificationConfiguration({bucket:?}), \
         got {} with {:?}",
        answered.status,
        answered.text()
    );
    elements(&answered.text(), "CloudFunction")
}

/// The SAM template this suite deploys ONCE: the handler above, and the table it writes, named
/// to the function through the environment variable the transform carries through. The grant S3
/// needs is NOT declared here — it is what each row moves.
fn template() -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Recorder": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "InlineCode": RECORDING_HANDLER,
                    "Environment": { "Variables": { "TABLE": { "Ref": "Records" } } },
                    "Policies": [
                        { "DynamoDBCrudPolicy": { "TableName": { "Ref": "Records" } } }
                    ]
                }
            },
            "Records": {
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

/// The item `table` holds under `key` right now, or [`Value::Null`] when it holds none.
async fn item_under(router: &MultiServiceRouter, case: &str, table: &str, key: &str) -> Value {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": table,
            "Key": { "pk": { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetItem({table:?}, {key:?}), got {} with {:?}",
        read.status,
        read.text()
    );
    read.json().get("Item").cloned().unwrap_or(Value::Null)
}

/// Look for the item the handler owes under `key` until it is there, or — when `expected` is
/// false — through [`QUIET_WINDOW`], and answer what was last read. The wait is on the
/// CONDITION; no row asserts how long it took.
async fn awaited_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    expected: bool,
) -> Value {
    let wait = if expected { WAIT } else { QUIET_WINDOW };
    let looks = wait.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = Value::Null;
    for _ in 0..looks {
        last = item_under(router, case, table, key).await;
        if expected && last != Value::Null {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// How the one statement a row puts on the function's resource policy is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The invoking action for the Amazon S3 principal, scoped to the row's OWN bucket.
    ThisBucket,
    /// The invoking action for the Amazon S3 principal, scoped to no source at all, which
    /// admits every bucket.
    EverySource,
    /// The invoking action for the Amazon S3 principal, scoped to a bucket that is not the one
    /// configuring the notification.
    AnotherBucket,
    /// Another action of the same service, for the Amazon S3 principal, scoped to the row's own
    /// bucket.
    AnotherAction,
    /// The invoking action scoped to the row's own bucket, for another service's principal.
    AnotherPrincipal,
}

/// One row: the grant it puts on the shared function, the key it writes, and whether the
/// configuration naming that function is admitted — which is also whether the write is
/// delivered.
struct Row {
    case: &'static str,
    grant: Grant,
    key: &'static str,
    admitted: bool,
}

/// The rows span the grant: each part of the statement the decision turns on, on both sides of
/// it. The keys differ only so each row reads its own item out of the shared table.
const ROWS: &[Row] = &[
    Row {
        case: "the invoking action for Amazon S3, scoped to the configuring bucket",
        grant: Grant::ThisBucket,
        key: "authorized.txt",
        admitted: true,
    },
    Row {
        case: "the invoking action for Amazon S3, scoped to no source at all",
        grant: Grant::EverySource,
        key: "uploads/unscoped.json",
        admitted: true,
    },
    Row {
        case: "the invoking action for Amazon S3, scoped to another bucket",
        grant: Grant::AnotherBucket,
        key: "other-source.txt",
        admitted: false,
    },
    Row {
        case: "another action of the same service, scoped to the configuring bucket",
        grant: Grant::AnotherAction,
        key: "other-action.txt",
        admitted: false,
    },
    Row {
        case: "the invoking action scoped to the configuring bucket, for another principal",
        grant: Grant::AnotherPrincipal,
        key: "other-principal.txt",
        admitted: false,
    },
];

/// The bucket a grant scoped away from the configuring bucket names instead. It is never
/// created: `aws:SourceArn` is a value the statement is written against, not a resource the
/// grant resolves.
const UNRELATED_BUCKET: &str = "some-other-bucket";

/// Another action of the same service, which is not the one S3 performs on a function
/// destination. Its name is the one `AddPermission$Action` admits ("the action that the
/// principal can use on the function", `lambda:<Operation>`).
const READ_ACTION: &str = "lambda:GetFunction";

/// Another service's principal, which is not the one Amazon S3 publishes notifications under.
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

impl Grant {
    /// The `AddPermission` request body this grant is stated as, for `statement` on the function
    /// configured by `bucket`.
    fn stated(self, statement: &str, bucket: &str) -> Value {
        let mut body = json!({
            "StatementId": statement,
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": bucket_arn(bucket),
        });
        match self {
            Self::ThisBucket => {}
            Self::EverySource => {
                body.as_object_mut()
                    .expect("the request body is an object")
                    .remove("SourceArn");
            }
            Self::AnotherBucket => body["SourceArn"] = json!(bucket_arn(UNRELATED_BUCKET)),
            Self::AnotherAction => body["Action"] = json!(READ_ACTION),
            Self::AnotherPrincipal => body["Principal"] = json!(OTHER_PRINCIPAL),
        }
        body
    }
}

/// Every row: the identical put is refused while the function grants nothing, is admitted
/// exactly when the row's grant admits the invocation, and the object write that follows an
/// admitted put really runs the handler — whose item carries what the record said.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn the_functions_invoke_grant_gates_both_the_configuration_and_its_delivery() {
    driven("s3-notification-lambda-invoke-grant", || {
        Box::pin(the_functions_invoke_grant_gates_both_the_configuration_and_its_delivery_body())
    });
}

async fn the_functions_invoke_grant_gates_both_the_configuration_and_its_delivery_body() {
    let router = emulator();

    // The shared wiring: ONE function and ONE table for every row, so what moves between rows
    // is only the grant.
    let stack = "notification-grant-stack";
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
        .get("Recorder")
        .cloned()
        .expect("setup: no physical id for Recorder");
    let table = physical
        .get("Records")
        .cloned()
        .expect("setup: no physical id for Records");
    let configuration = active_function(&router, &function_name).await;
    await_active_table(&router, &table).await;
    let function_arn = configuration
        .get("FunctionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: no FunctionArn in {configuration}"))
        .to_string();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = format!("notification-grant-{index}");
        create_bucket(&router, case, &bucket).await;

        // 1. With no statement on the function at all, the container is refused WHOLE and
        //    records nothing.
        let refused = put_lambda_notification(&router, &bucket, &function_arn).await;
        assert_eq!(
            refused.status,
            INVALID_ARGUMENT_STATUS,
            "case {case:?}: an ungranted function destination must be refused HTTP \
             {INVALID_ARGUMENT_STATUS}, got {} with {:?}",
            refused.status,
            refused.text()
        );
        let reported = refused.text();
        assert_eq!(
            element(&reported, "Code"),
            INVALID_ARGUMENT,
            "case {case:?}: expected the error code {INVALID_ARGUMENT:?}, got {reported}"
        );
        assert_eq!(
            element(&reported, "Message"),
            UNABLE_TO_VALIDATE,
            "case {case:?}: expected the message {UNABLE_TO_VALIDATE:?}, got {reported}"
        );
        let after_refusal = reported_functions(&router, case, &bucket).await;
        assert!(
            after_refusal.is_empty(),
            "case {case:?}: a refused put records nothing, so the bucket must report no Lambda \
             configuration, got {after_refusal:?}"
        );

        // 2. The row's grant, stated on the function's own resource policy.
        let statement = format!("grant-{index}");
        let granted = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function_name}/policy"),
            Some(row.grant.stated(&statement, &bucket)),
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "case {case:?}: expected HTTP 201 from AddPermission, got {} with {:?}",
            granted.status,
            granted.text()
        );

        // 3. The IDENTICAL put, which the grant alone decides.
        let answered = put_lambda_notification(&router, &bucket, &function_arn).await;
        let expected_status = if row.admitted {
            200
        } else {
            INVALID_ARGUMENT_STATUS
        };
        assert_eq!(
            answered.status,
            expected_status,
            "case {case:?}: expected HTTP {expected_status} from the identical put once the \
             grant was stated, got {} with {:?}",
            answered.status,
            answered.text()
        );
        let listed = reported_functions(&router, case, &bucket).await;
        let expected_listing: Vec<String> = if row.admitted {
            vec![function_arn.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            listed, expected_listing,
            "case {case:?}: expected the bucket to report {expected_listing:?} as its Lambda \
             destination(s), got {listed:?}"
        );

        // 4. The write, which is accepted either way, and the handler's own work, which only an
        //    admitted configuration causes.
        put_object(&router, case, &bucket, row.key).await;
        let item = awaited_item(&router, case, &table, row.key, row.admitted).await;
        if row.admitted {
            assert_eq!(
                item,
                json!({
                    "pk": { "S": row.key },
                    "bucket": { "S": bucket },
                    "eventName": { "S": PUT_EVENT_NAME },
                    "eventSource": { "S": EVENT_SOURCE },
                }),
                "case {case:?}: expected the handler's item for the record of {:?}, got {item}",
                row.key
            );
        } else {
            assert_eq!(
                item,
                Value::Null,
                "case {case:?}: the grant admits no invocation, so no handler may run for \
                 {:?}, got {item}",
                row.key
            );
        }

        // 5. The grant is withdrawn, so the next row starts from a function granting nothing.
        let removed = lambda(
            &router,
            "DELETE",
            &format!("/2015-03-31/functions/{function_name}/policy/{statement}"),
            None,
        )
        .await;
        assert_eq!(
            removed.status,
            204,
            "case {case:?}: expected HTTP 204 from RemovePermission, got {} with {:?}",
            removed.status,
            removed.text()
        );
    }
}
