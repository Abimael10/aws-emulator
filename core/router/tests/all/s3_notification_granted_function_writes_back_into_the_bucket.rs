// SPDX-License-Identifier: CC0-1.0

//! The whole leg a client walks when it wires a bucket to a function and lets the function
//! write back: the configuration refused while the destination grants nothing, the identical
//! configuration accepted once it does, and the object the handler itself writes into the very
//! bucket the event came from, read back the way the client reads it — through `HeadObject` on
//! the key the handler derives from the record.
//!
//! The rule under test: **a `NotificationConfiguration` naming a Lambda destination is admitted
//! only while that function's own resource-based policy lets the Amazon S3 service principal
//! invoke it for the configuring bucket — "in the case of Lambda destinations, Amazon S3
//! verifies that the Lambda function permissions grant Amazon S3 permission to invoke the
//! function from the Amazon S3 bucket" (`PutBucketNotificationConfiguration`'s own
//! documentation) — and from the moment it is admitted every object change the configuration
//! covers is published to that function as one asynchronous invocation carrying the `Records`
//! envelope, whatever shape the changed key has, so the objects the handler writes through
//! Amazon S3 off that record land in the bucket the event came from and are readable at the keys
//! the handler derives from it.**
//!
//! Four crates have to agree for that to hold (CloudFormation provisions the function and its
//! execution role, Lambda holds the grant and runs the handler, IAM answers for the role the
//! handler's own SDK calls are signed as, and Amazon S3 both evaluates the grant on the put and
//! takes the handler's write), so the suite lives here: a crate-local suite can only ever see
//! one side of it. Its siblings reach parts of the same wiring —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins what a key-name `Filter` admits for
//! an already granted function, [`crate::s3_notification_lambda_invoke_grant_gates_delivery`]
//! pins which GRANTS admit the configuration, with the handler's effect left in another
//! service, and [`crate::s3_deployed_notification_copies_within_its_bucket`] pins the pipeline a
//! stack declares — and what only this suite reaches is the client's own leg END TO END on one
//! unfiltered configuration: the refusal, the grant stated through `AddPermission`, the
//! acceptance, and then the handler's write back into the SAME bucket observed through
//! `HeadObject`, with the chain that write could start closed by the handler's own skip rather
//! than by a filter — so the bucket ends up holding exactly one derived object per change and
//! none derived from a derived one.
//!
//! The rows differ along what the rule generalises over: the shape of the changed key the
//! handler derives its own key from — a flat key with an extension, one several prefixes deep,
//! one carrying no extension at all, and one whose name carries dots, a dash and an underscore.
//! Each row asserts both the derived object's presence and what the handler recorded in it, so a
//! row cannot pass on a write that reached the handler without the record.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test holding
//! several of them across await points lays that state out on the test thread's stack and
//! overflows it as the emulator grows; the box puts each one on the heap instead.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources and the
/// bucket belong to. It is the Region the shared stack reader ([`wait_for_stack_status`]) signs
/// for too.
const REGION: &str = "us-east-1";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class the configuration names: "you can enable notifications for every event of a
/// type by using the wildcard", so this is every object-created event of the bucket, with no
/// key-name filter at all.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The `eventName` one `Put` of an object is published under, and the `eventSource` of every
/// record Amazon S3 publishes — both of which the handler records in the object it writes.
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";
const EVENT_SOURCE: &str = "aws:s3";

/// The prefix the handler keys the object it derives from a record under, and the suffix it
/// gives it. The handler SKIPS a record whose key already carries the prefix, which is what
/// closes the chain its own write would otherwise start on an unfiltered configuration.
const DERIVED_PREFIX: &str = "processed/";
const DERIVED_SUFFIX: &str = ".marker";

/// The error code and status Amazon S3 answers a container it could not validate with
/// ("InvalidArgument — Invalid Argument", HTTP 400, from Amazon S3's list of error codes).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The `<Message>` that refusal carries, as Amazon S3 words it. FIXED text: which destination
/// failed, and why, travel in the numbered argument series beside it.
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite looks for work the clock owes — a stack's resources coming up, a function
/// reported `Active`, the asynchronous delivery of an object event — and how often it looks while
/// waiting. Generous: no span here is one the suite asserts on, only one it refuses to wait out
/// forever.
const WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the closing read keeps looking for an object derived from an object the handler
/// itself derived — one the bucket must never hold — before concluding none came. Well past the
/// span every row above it took to leave its own object, so the absence is read against a
/// pipeline already known to deliver.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling notification suites use. Assembling
/// every service and then dispatching a CHAIN of in-process calls through it — CloudFormation
/// into Lambda and IAM, S3's delivery into Lambda, the handler's own SDK calls back into S3 —
/// nests more future than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The body every row writes, which the handler reads back off the object the record named.
const OBJECT_BODY: &str = "the object whose creation is published";

/// The handler the stack deploys: for every record it is handed it reads the object the record
/// named and writes one object of its own back into the SAME bucket, keyed by the record's key,
/// carrying what the record said about the change — so the written object is evidence the
/// handler ran on the envelope Amazon S3 published and resolved the object it named.
///
/// A record's key "is URL encoded" (Amazon S3 User Guide, *Event message structure*), so it is
/// decoded the way that guide's own handler decodes it before being used as a key again. A
/// record whose key already carries the derived prefix is SKIPPED, which is what keeps the
/// handler idempotent on a configuration that publishes every key of the bucket.
const DERIVING_HANDLER: &str = r#"
import json
import urllib.parse

import boto3

DERIVED_PREFIX = "processed/"
DERIVED_SUFFIX = ".marker"


def handler(event, context):
    objects = boto3.client("s3")
    written = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        if key.startswith(DERIVED_PREFIX):
            continue
        body = objects.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        derived = {
            "eventSource": record["eventSource"],
            "eventName": record["eventName"],
            "bucket": bucket,
            "key": key,
            "body": body,
        }
        objects.put_object(
            Bucket=bucket,
            Key=DERIVED_PREFIX + key + DERIVED_SUFFIX,
            Body=json.dumps(derived).encode("utf-8"),
        )
        written.append(key)
    return {"written": written}
"#;

/// One row: the key the client writes, whose shape is what the rule generalises over.
struct Row {
    case: &'static str,
    key: &'static str,
}

/// The rows span the shape of the changed key, which is the value the handler derives its own
/// key from — so a derived key built from anything but the record's own key fails one of them.
const ROWS: &[Row] = &[
    Row {
        case: "a flat key carrying an extension",
        key: "trigger.txt",
    },
    Row {
        case: "a key several prefixes deep",
        key: "incoming/2026/03/report.json",
    },
    Row {
        case: "a key carrying no extension at all",
        key: "raw-upload",
    },
    Row {
        case: "a key whose name carries dots, a dash and an underscore",
        key: "logs/app.2026-03-01_part.1.log",
    },
];

/// The key the handler owes for `key`.
fn derived_key(key: &str) -> String {
    format!("{DERIVED_PREFIX}{key}{DERIVED_SUFFIX}")
}

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

/// The emulator as it is ASSEMBLED, on one engine: S3's delivery and the handler's own SDK calls
/// are dispatched back into this same router, so the function S3 invokes and the bucket the
/// handler writes are the ones this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
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

/// One call on Amazon S3's REST surface, path-style.
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
/// [`OBJECT_CREATED`] and no key-name filter at all — the identical request this suite issues
/// twice, once on either side of the grant.
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
         <Id>derive-from-the-record</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event>\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    s3(router, "PUT", &format!("/{bucket}?notification"), &body).await
}

/// The function ARNs `GetBucketNotificationConfiguration` reports for `bucket`, in the order it
/// reports them — empty for a bucket carrying no Lambda configuration.
async fn reported_functions(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
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

/// The keys `bucket` holds under `prefix` right now, read through `ListObjectsV2`.
async fn keys_under(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let answered = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        "",
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from ListObjectsV2({bucket:?}, {prefix:?}), got {} \
         with {:?}",
        answered.status,
        answered.text()
    );
    elements(&answered.text(), "Key")
}

/// Look for the object the handler owes at `key` until `HeadObject` reports it, and answer the
/// last status that read gave. The wait is on the CONDITION; nothing asserts how long it took.
async fn awaited_head(router: &MultiServiceRouter, bucket: &str, key: &str) -> u16 {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = 0;
    for _ in 0..looks {
        last = s3(router, "HEAD", &format!("/{bucket}/{key}"), "")
            .await
            .status;
        if last == 200 {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    last
}

/// The trust policy a Lambda execution role needs, and the grants the handler's own calls need.
fn role_resource() -> Value {
    json!({
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
            // The handler calls Amazon S3 with temporary credentials for this role, so the role
            // has to grant the read and the write it makes.
            "Policies": [{
                "PolicyName": "derive-from-the-record",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": ["s3:GetObject", "s3:PutObject", "logs:*"],
                        "Resource": "*",
                    }],
                },
            }],
        },
    })
}

/// The stack this suite deploys ONCE: the execution role and the function, whose inline code
/// CloudFormation archives into its deployment package. The grant Amazon S3 needs is NOT
/// declared here — it is what the leg below states through Lambda's own API, the way a client
/// that forgot it adds it afterwards.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": role_resource(),
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": DERIVING_HANDLER },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Handler", "Arn"] } },
            "FunctionName": { "Value": { "Ref": "Handler" } },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`). The span is that service's
/// own, so nothing here sleeps a fixed span on its behalf.
async fn await_active_function(router: &MultiServiceRouter, name: &str) {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.json().get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// The leg a client walks: the configuration refused while the function grants nothing, the
/// grant, the identical configuration accepted, and then one derived object per change — read
/// through `HeadObject` and `GetObject` — with no object derived from a derived one.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_granted_function_derives_one_object_per_published_change() {
    driven("s3-notification-derived-object", || {
        Box::pin(a_granted_function_derives_one_object_per_published_change_body())
    });
}

async fn a_granted_function_derives_one_object_per_published_change_body() {
    let router = emulator();
    let bucket = "notification-derived-objects";
    create_bucket(&router, bucket).await;

    // The shared wiring: ONE function for every row, carrying no grant for Amazon S3 yet.
    let stack = "notification-derived-objects-stack";
    let body = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
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
    let outputs = outputs_of(&described);
    let function_arn = outputs
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionArn output in {described}"));
    let function_name = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionName output in {described}"));
    await_active_function(&router, &function_name).await;

    // 1. While the function's resource policy grants nothing, the container is refused WHOLE and
    //    records nothing — the refusal naming the destination that failed and why, beside the
    //    fixed sentence of its class.
    let case = "the destination function grants Amazon S3 nothing";
    let refused = put_lambda_notification(&router, bucket, &function_arn).await;
    assert_eq!(
        refused.status,
        INVALID_ARGUMENT_STATUS,
        "case {case:?}: an ungranted function destination must be refused HTTP \
         {INVALID_ARGUMENT_STATUS}, got {} with {:?}",
        refused.status,
        refused.text()
    );
    let document = refused.text();
    assert_eq!(
        element(&document, "Code"),
        INVALID_ARGUMENT,
        "case {case:?}: expected the error code {INVALID_ARGUMENT:?}, got {document}"
    );
    assert_eq!(
        element(&document, "Message"),
        UNABLE_TO_VALIDATE,
        "case {case:?}: expected the message {UNABLE_TO_VALIDATE:?}, got {document}"
    );
    assert_eq!(
        elements(&document, "ArgumentValue1"),
        vec![format!("Not authorized to invoke function [{function_arn}]")],
        "case {case:?}: expected the refusal to say why that destination failed, got {document}"
    );
    let after_refusal = reported_functions(&router, case, bucket).await;
    assert!(
        after_refusal.is_empty(),
        "case {case:?}: a refused put records nothing, so the bucket must report no Lambda \
         configuration, got {after_refusal:?}"
    );

    // 2. The grant, stated on the function's own resource policy for this bucket.
    let case = "the invoking action for Amazon S3, scoped to the configuring bucket";
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function_name}/policy"),
        Some(json!({
            "StatementId": "allow-this-bucket",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": bucket_arn(bucket),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {} with {:?}",
        granted.status,
        granted.text()
    );

    // 3. The IDENTICAL container, which the grant alone decides, and the read that reports it.
    let accepted = put_lambda_notification(&router, bucket, &function_arn).await;
    assert_eq!(
        accepted.status,
        200,
        "case {case:?}: expected HTTP 200 from the identical put once the grant was stated, got \
         {} with {:?}",
        accepted.status,
        accepted.text()
    );
    let listed = reported_functions(&router, case, bucket).await;
    assert_eq!(
        listed,
        vec![function_arn.clone()],
        "case {case:?}: expected the bucket to report {function_arn:?} as its Lambda \
         destination, got {listed:?}"
    );

    // 4. Every row's change, and the object the handler derives from the record it was handed.
    for row in ROWS {
        let case = row.case;
        put_object(&router, case, bucket, row.key).await;
        let derived = derived_key(row.key);
        let head = awaited_head(&router, bucket, &derived).await;
        assert_eq!(
            head, 200,
            "case {case:?}: expected HeadObject({bucket:?}, {derived:?}) to report the object \
             the invoked handler owes for {:?}, got HTTP {head}",
            row.key
        );
        let read = s3(&router, "GET", &format!("/{bucket}/{derived}"), "").await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetObject({bucket:?}, {derived:?}), got {} \
             with {:?}",
            read.status,
            read.text()
        );
        let recorded: Value = serde_json::from_slice(&read.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the derived object must carry the document the handler wrote \
                 ({error}), got {:?}",
                read.text()
            )
        });
        assert_eq!(
            recorded,
            json!({
                "eventSource": EVENT_SOURCE,
                "eventName": PUT_EVENT_NAME,
                "bucket": bucket,
                "key": row.key,
                "body": OBJECT_BODY,
            }),
            "case {case:?}: expected the handler's record of the published change of {:?}, got \
             {recorded}",
            row.key
        );
    }

    // 5. The chain the handler's own writes could start: each derived object's own creation is
    //    published too, and the handler skips it — so the bucket holds exactly one derived
    //    object per change and none derived from a derived one, however long this keeps looking.
    let case = "the handler's own write is published and skipped";
    let mut expected: Vec<String> = ROWS.iter().map(|row| derived_key(row.key)).collect();
    expected.sort();
    let looks = QUIET_WINDOW.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let mut held = keys_under(&router, case, bucket, DERIVED_PREFIX).await;
        held.sort();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the bucket to hold exactly one derived object per change, \
             got {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}
