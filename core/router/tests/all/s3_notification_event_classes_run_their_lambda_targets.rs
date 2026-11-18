// SPDX-License-Identifier: CC0-1.0

//! Which OBJECT EVENT a bucket's notification configuration runs its Lambda destinations for,
//! end to end over the assembled router — and how many destinations one event reaches.
//!
//! The rule under test: **a bucket whose notification configuration names a Lambda function
//! for an event value invokes that function once for every object event whose own value that
//! list names — literally, or through the `s3:<Type>:*` form, which stands for every event of
//! that type — to EVERY configuration naming it, and invokes nothing for an event no
//! configuration's list names.** The event values the list may carry are the published
//! `Event` vocabulary (`smithy_query(query="shape",shape="Event")`), and a record names its
//! event by that value without the `s3:` prefix, so the three calls that produce one — a put,
//! the completion of a multipart upload, and a delete — are each owed to the lists that name
//! their value and to no other.
//!
//! Three crates have to agree for that to hold (S3 evaluates the configuration against the
//! event it just produced and makes the invocation, Lambda holds the grant and RUNS the
//! handler, and CloudWatch Logs takes the handler's own output), so the suite lives here: a
//! crate-local suite can only ever see one side of it. Its siblings reach one side each —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins the key-name `Filter` half of the
//! same delivery on a put, and [`crate::s3_notification_lambda_invoke_grant_gates_delivery`]
//! pins the invoke grant as the gate on it — and what only this suite reaches is the EVENT
//! VOCABULARY half: a configuration's list judged against the event each of those three calls
//! produces, with the handler really running as the evidence, and several function
//! destinations on one bucket each taking exactly the events their own rule names.
//!
//! Every row is observed the way a client observes it: the handler logs one line per record it
//! is handed, and that line is read back out of the function's own log group
//! (`/aws/lambda/<function name>`, which is where "Lambda functions send logs" by default per
//! `LoggingConfig$LogGroup`). A row that must see NOTHING for a call states it on the same
//! group a delivered call of the SAME configuration demonstrably reaches, so an emitter that
//! delivers nothing at all cannot satisfy the negative half for the wrong reason.
//!
//! The rows differ along what the rule generalises over: which call produces the event (put,
//! multipart completion, delete), how the configuration names it (the value itself, the type
//! wildcard, a list naming two classes), and whether the produced event's value is among them.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test
//! holding several of them across await points lays that state out on the test thread's stack
//! and overflows it as the emulator grows; the box puts each one on the heap instead. Nothing
//! about what is asserted changes.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The ONE statement this crate makes of a delivered log record's form: the line break that
// closed the write belongs to the record rather than to the text it carries.
use crate::lambda_log_records_delivered_under_the_roles_logs_grant::record_body;
use crate::query_wire::{element, form_encode, outputs_of};

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the stack it
/// reports.
const REGION: &str = "us-east-1";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` a deployed stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes notifications under, and the action it performs
/// on a function destination.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// How long this suite waits for work the clock owes — the asynchronous delivery of an object
/// event and the run it starts — and how often it looks while waiting. Generous: the wait is
/// on the CONDITION, so a delivery that normally lands at once is never waited out.
const WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a call that must reach NO function keeps being looked for before concluding
/// nothing came. Stated only after a delivered call of the same configuration has already been
/// read out of the same group, so this is a margin past a delivery that already happened
/// rather than a budget for one — which is why it is short where [`WAIT`] is long.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size its siblings drive themselves on. Assembling
/// every service and then dispatching a CHAIN of in-process calls through it — S3's delivery
/// into Lambda, and Lambda's own log delivery back into CloudWatch Logs — nests more future
/// than the default stack of a test thread holds.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The marker the handler opens every line it logs with, which is what a reader of the group
/// picks its lines out by.
const EVENT_MARKER: &str = "object-event";

/// The level the handler emits that line at, which the rendered record opens with.
const LOGGED_LEVEL: &str = "INFO";

/// The body every put below writes. Small on purpose: nothing here is about object size.
const OBJECT_BODY: &str = "the object the event named";

/// The handler every function in this suite runs: for each record it is handed it logs one
/// line naming the event, the bucket and the key, through the logging facility — the shape AWS
/// documents for a Python handler. It performs no other work, so the only thing a row observes
/// is that the function RAN on the record S3 published.
const LOGGING_HANDLER: &str = r#"
import logging

LOGGER = logging.getLogger()
LOGGER.setLevel(logging.INFO)


def handler(event, context):
    seen = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = record["s3"]["object"]["key"]
        LOGGER.info("object-event %s %s %s", record["eventName"], bucket, key)
        seen.append(record["eventName"])
    return {"seen": seen}
"#;

/// The line that handler logs for one record, which a client reads out of the function's log
/// group: the marker, the event the record names, and the object it names.
fn logged_line(event_name: &str, bucket: &str, key: &str) -> String {
    format!("{EVENT_MARKER} {event_name} {bucket} {key}")
}

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread runtime
/// this suite's deliveries are made on: a notification delivery is a task of its own, so it
/// runs on this same thread and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>>
        + Send
        + 'static,
{
    ::std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The emulator as it is ASSEMBLED, on one engine: Lambda's log delivery is dispatched back
/// into this same router, so the group this suite reads is the group the run wrote.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One response of S3's REST surface: the status, the body as text, and the entity tag it
/// reported (a part upload names its part by that tag).
struct Answer {
    status: u16,
    body: String,
    etag: Option<String>,
}

/// Dispatch one request against S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
        etag,
    }
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), as its status and
/// parsed body. A group no run has created yet is a refusal this suite polls through, so the
/// status is answered rather than asserted here.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
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

/// Every line the function's log group holds right now, with the status the read answered —
/// which is a refusal while no run has created the group, and 200 from then on.
///
/// One page is the whole group: `FilterLogEvents` answers at most 10 000 events per page
/// (`FilterLogEventsRequest$limit` is modelled `range 1..10000` and defaults to that bound),
/// and the few invocations a case makes write a handful of lines each, so no `nextToken` walk
/// can hide a record from these assertions.
async fn read_log(router: &MultiServiceRouter, function: &str) -> (u16, Vec<String>) {
    let group = format!("/aws/lambda/{function}");
    let (status, answered) =
        logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    let lines = answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect();
    (status, lines)
}

/// How many of `lines` are the record the handler logged for `wanted`: a line emitted at the
/// level the handler logs at whose own MESSAGE is that text.
///
/// A rendered record is the level, the instant and the request id ahead of the message, each
/// separated by a tab, so the message is the last of those fields and is compared whole — not
/// as a suffix, which a key that ends another key would match too. A delivered record is closed
/// by the line break the write that produced it ended with, which belongs to the record rather
/// than to the message it carries, so it is taken off through the one statement of that in this
/// crate ([`record_body`]).
fn occurrences(lines: &[String], wanted: &str) -> usize {
    let opens = format!("[{LOGGED_LEVEL}]\t");
    lines
        .iter()
        .map(|line| record_body(line).unwrap_or(line))
        .filter(|line| line.starts_with(&opens) && line.split('\t').next_back() == Some(wanted))
        .count()
}

/// Wait until the function's log group holds the record the handler logged for `wanted` EXACTLY
/// once — one invocation per matching event — looking again until it does or [`WAIT`] passes.
///
/// A group no run has created yet is a refusal this polls through, so the last status the read
/// answered travels into the failure: a group that stays unreadable is then named as the read
/// failure it is rather than reported as a record that never arrived.
async fn await_one_record(router: &MultiServiceRouter, case: &str, function: &str, wanted: &str) {
    let giving_up_at = Instant::now() + WAIT;
    let (mut status, mut lines) = read_log(router, function).await;
    while occurrences(&lines, wanted) == 0 && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_INTERVAL).await;
        (status, lines) = read_log(router, function).await;
    }
    assert_eq!(
        occurrences(&lines, wanted),
        1,
        "case {case:?}: expected the group of {function:?} to hold exactly one record for \
         {wanted:?}, got {lines:?} on a read that answered {status}"
    );
}

/// Keep looking for [`QUIET_WINDOW`] and assert the function's group settles on exactly the
/// records it owes: one for each line of `delivered`, none for any line of `unwanted`.
///
/// Stated only after every line of `delivered` has already been read out of this group, so the
/// window is a margin past deliveries that happened rather than a budget for them: it is what
/// catches a record arriving for an event no configuration names, and a SECOND record for an
/// event one names only once. Each look asserts the group is still readable, so a refusal
/// cannot pass this check by answering no lines at all.
async fn assert_settled(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    delivered: &[String],
    unwanted: &[String],
) {
    let looking_until = Instant::now() + QUIET_WINDOW;
    loop {
        let (status, lines) = read_log(router, function).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group of {function:?} must stay readable, got {status}"
        );
        for wanted in delivered {
            assert_eq!(
                occurrences(&lines, wanted),
                1,
                "case {case:?}: one matching event owes {function:?} exactly one record for \
                 {wanted:?}, got {lines:?}"
            );
        }
        for wanted in unwanted {
            assert_eq!(
                occurrences(&lines, wanted),
                0,
                "case {case:?}: no configuration names that event, so {function:?} must hold no \
                 record for {wanted:?}, got {lines:?}"
            );
        }
        if Instant::now() >= looking_until {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`, which creates the object with `s3:ObjectCreated:Put`.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}/{key}"), OBJECT_BODY).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// `DELETE /<bucket>/<key>` — `DeleteObject`, which removes the object with
/// `s3:ObjectRemoved:Delete` on a bucket carrying no versioning.
async fn delete_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(router, "DELETE", &format!("/{bucket}/{key}"), "").await;
    assert_eq!(
        answer.status, 204,
        "case {case:?}: DeleteObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// The multipart loop at `key`, ended by `CompleteMultipartUpload` — the call that creates the
/// object with `s3:ObjectCreated:CompleteMultipartUpload`. One part, which is the upload's LAST
/// part and so carries no minimum size ("there is no minimum size limit on the last part of
/// your multipart upload", Amazon S3 User Guide, *Amazon S3 multipart upload limits*).
async fn complete_multipart_upload(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
) {
    let initiated = s3(router, "POST", &format!("/{bucket}/{key}?uploads"), "").await;
    assert_eq!(
        initiated.status, 200,
        "case {case:?}: CreateMultipartUpload({bucket:?}, {key:?}) must succeed, got {} with {}",
        initiated.status, initiated.body
    );
    let upload_id = element(&initiated.body, "UploadId");

    let uploaded = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}?partNumber=1&uploadId={upload_id}"),
        OBJECT_BODY,
    )
    .await;
    assert_eq!(
        uploaded.status, 200,
        "case {case:?}: UploadPart({bucket:?}, {key:?}) must succeed, got {} with {}",
        uploaded.status, uploaded.body
    );
    let etag = uploaded
        .etag
        .unwrap_or_else(|| panic!("case {case:?}: a part upload must report an ETag header"));

    let payload = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{etag}</ETag></Part>\
         </CompleteMultipartUpload>"
    );
    let completed = s3(
        router,
        "POST",
        &format!("/{bucket}/{key}?uploadId={upload_id}"),
        &payload,
    )
    .await;
    assert_eq!(
        completed.status, 200,
        "case {case:?}: CompleteMultipartUpload({bucket:?}, {key:?}) must succeed, got {} with {}",
        completed.status, completed.body
    );
}

/// One Lambda destination a container names: the function, the `Events` list naming when it is
/// notified, and the key-name `Filter` prefix restricting it when it carries one.
struct Configured<'a> {
    arn: &'a str,
    events: &'a [&'a str],
    prefix: Option<&'a str>,
}

/// `PUT /<bucket>?notification` with one Lambda configuration per destination, answered with
/// the status and body so a row can assert a refusal too.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    configurations: &[Configured<'_>],
) -> Answer {
    let written: String = configurations
        .iter()
        .enumerate()
        .map(|(index, destination)| {
            let named: String = destination
                .events
                .iter()
                .map(|event| format!("<Event>{event}</Event>"))
                .collect();
            let filter = destination.prefix.map_or_else(String::new, |prefix| {
                format!(
                    "<Filter><S3Key><FilterRule><Name>prefix</Name><Value>{prefix}</Value>\
                     </FilterRule></S3Key></Filter>"
                )
            });
            format!(
                "<CloudFunctionConfiguration><Id>destination-{index}</Id>\
                 <CloudFunction>{}</CloudFunction>{named}{filter}</CloudFunctionConfiguration>",
                destination.arn
            )
        })
        .collect();
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{written}</NotificationConfiguration>"
    );
    s3(router, "PUT", &format!("/{bucket}?notification"), &body).await
}

/// [`put_notification`] where the put must be accepted.
async fn put_accepted_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    configurations: &[Configured<'_>],
) {
    let answer = put_notification(router, bucket, configurations).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must succeed, got {} with \
         {}",
        answer.status, answer.body
    );
}

/// The trust policy a Lambda execution role needs, and the grant its run's own log delivery
/// needs: "a role short of `logs:CreateLogGroup` creates no group, and one short of
/// `logs:PutLogEvents` writes no events".
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
            "Policies": [{
                "PolicyName": "write-the-run-log",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": ["logs:*"],
                        "Resource": "*",
                    }],
                },
            }],
        },
    })
}

/// A stack declaring the execution role and, for every logical id given, a function running
/// [`LOGGING_HANDLER`] with the grant that lets Amazon S3 invoke it.
///
/// The grant names no `SourceArn` ON PURPOSE: it is the configuration's `Events` list this suite
/// is about, so one deployed function serves every bucket its rows create. What a grant SCOPED
/// to the configuring bucket decides is its own rule, pinned by
/// [`crate::s3_notification_lambda_invoke_grant_gates_delivery`].
fn handler_stack(handlers: &[&str]) -> Value {
    let mut resources = json!({ "HandlerRole": role_resource() });
    let mut outputs = json!({});
    for handler in handlers {
        resources[*handler] = json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                "Timeout": 30,
                "Code": { "ZipFile": LOGGING_HANDLER },
            },
        });
        resources[format!("{handler}Permission")] = json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": [*handler, "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": S3_PRINCIPAL,
            },
        });
        outputs[format!("{handler}Arn")] = json!({ "Value": { "Fn::GetAtt": [*handler, "Arn"] } });
        outputs[format!("{handler}Name")] = json!({ "Value": { "Ref": *handler } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
}

/// Deploy the stack declaring `handlers`, wait for `CREATE_COMPLETE`, and answer each handler's
/// `(ARN, name)` in the order asked for.
async fn deployed_functions(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    handlers: &[&str],
) -> Vec<(String, String)> {
    let template = handler_stack(handlers).to_string();
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let reported = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {:?} with {}",
        reported.0, reported.1
    );
    let outputs = outputs_of(&reported.1);
    handlers
        .iter()
        .map(|handler| {
            let arn = outputs
                .get(&format!("{handler}Arn"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the stack must output {handler}'s ARN, got {outputs:?}")
                });
            let name = outputs
                .get(&format!("{handler}Name"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the stack must output {handler}'s name, got {outputs:?}")
                });
            (arn, name)
        })
        .collect()
}

/// Which call a row makes, and so which event value it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// `PutObject` — `s3:ObjectCreated:Put`.
    Put,
    /// The multipart loop ended by `CompleteMultipartUpload` —
    /// `s3:ObjectCreated:CompleteMultipartUpload`.
    Multipart,
    /// `DeleteObject` on a bucket carrying no versioning — `s3:ObjectRemoved:Delete`.
    Delete,
}

impl Call {
    /// The `eventName` a record of this call carries: its `Event` value without the `s3:`
    /// prefix, which is how a record names an event (Amazon S3 User Guide, *Event message
    /// structure*). The values themselves are the published `Event` vocabulary
    /// (`smithy_query(query="shape",shape="Event")`).
    fn event_name(self) -> &'static str {
        match self {
            Self::Put => "ObjectCreated:Put",
            Self::Multipart => "ObjectCreated:CompleteMultipartUpload",
            Self::Delete => "ObjectRemoved:Delete",
        }
    }
}

/// One call a row makes once its configuration is installed: which call, on which key, and
/// whether the configuration's `Events` list names the event it produces.
struct Step {
    call: Call,
    key: &'static str,
    delivered: bool,
}

/// One row: the `Events` list the bucket's single Lambda configuration carries, the keys put
/// into the bucket BEFORE that configuration is installed (so their creation is owed to
/// nothing), and the calls made after it.
struct Row {
    case: &'static str,
    events: &'static [&'static str],
    seeded: &'static [&'static str],
    steps: &'static [Step],
}

/// The table. Its first rows are over the CREATED class — the two calls that create an object
/// — and its last over removal and over a list naming a class of each. Every row ends on a call
/// the configuration DOES name, so the absences it then asserts are stated on a group the row
/// has already seen a record arrive in; a row whose event is a removal seeds the object it
/// removes before its configuration is installed, so that creation is owed to nothing.
const ROWS: &[Row] = &[
    Row {
        case: "a list naming the put event takes a put and not a completed upload",
        events: &["s3:ObjectCreated:Put"],
        seeded: &[],
        steps: &[
            Step {
                call: Call::Multipart,
                key: "events/assembled.bin",
                delivered: false,
            },
            Step {
                call: Call::Put,
                key: "events/written.txt",
                delivered: true,
            },
        ],
    },
    Row {
        case: "the created-type wildcard takes every detail of that type",
        events: &["s3:ObjectCreated:*"],
        seeded: &[],
        steps: &[
            Step {
                call: Call::Put,
                key: "events/written.txt",
                delivered: true,
            },
            Step {
                call: Call::Multipart,
                key: "events/assembled.bin",
                delivered: true,
            },
        ],
    },
    Row {
        case: "a list naming the completed upload takes it and not a put",
        events: &["s3:ObjectCreated:CompleteMultipartUpload"],
        seeded: &[],
        steps: &[
            Step {
                call: Call::Put,
                key: "events/written.txt",
                delivered: false,
            },
            Step {
                call: Call::Multipart,
                key: "events/assembled.bin",
                delivered: true,
            },
        ],
    },
    Row {
        case: "the removed-type wildcard takes a delete and no creation",
        events: &["s3:ObjectRemoved:*"],
        seeded: &["events/doomed.txt"],
        steps: &[
            Step {
                call: Call::Put,
                key: "events/written.txt",
                delivered: false,
            },
            Step {
                call: Call::Delete,
                key: "events/doomed.txt",
                delivered: true,
            },
        ],
    },
    Row {
        case: "a list naming the delete event itself takes it",
        events: &["s3:ObjectRemoved:Delete"],
        seeded: &["events/doomed.txt"],
        steps: &[Step {
            call: Call::Delete,
            key: "events/doomed.txt",
            delivered: true,
        }],
    },
    Row {
        case: "a list naming two classes takes an event of each",
        events: &["s3:ObjectCreated:Put", "s3:ObjectRemoved:*"],
        seeded: &["events/doomed.txt"],
        steps: &[
            Step {
                call: Call::Put,
                key: "events/written.txt",
                delivered: true,
            },
            Step {
                call: Call::Delete,
                key: "events/doomed.txt",
                delivered: true,
            },
        ],
    },
];

/// Every row: the function a bucket's configuration names runs exactly once for each event its
/// `Events` list names, and not at all for an event it does not name.
///
/// ONE test over the whole table, on one deployed function: a test of this target assembles the
/// router and deploys a stack of its own, so splitting the rows across tests buys nothing and
/// pays for that twice.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_configuration_runs_its_function_for_the_events_it_names() {
    driven("s3-notification-event-classes", || {
        Box::pin(rows_run_their_function(
            "notification-event-classes-stack",
            "event-classes",
            ROWS,
        ))
    });
}

/// Drive `rows` against one function of a stack of its own, each row on a bucket of its own.
async fn rows_run_their_function(stack: &str, buckets: &str, rows: &'static [Row]) {
    let router = build_router();
    let deployed = deployed_functions(&router, "the shared wiring", stack, &["Handler"]).await;
    let (function_arn, function_name) = deployed[0].clone();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let bucket = format!("{buckets}-{index}");
        create_bucket(&router, case, &bucket).await;

        // Seeded BEFORE the configuration is installed, so the creation of a key a later
        // delete removes is owed to no destination at all.
        for key in row.seeded {
            put_object(&router, case, &bucket, key).await;
        }
        put_accepted_notification(
            &router,
            case,
            &bucket,
            &[Configured {
                arn: &function_arn,
                events: row.events,
                prefix: None,
            }],
        )
        .await;

        let mut absent: Vec<String> = row
            .seeded
            .iter()
            .map(|key| logged_line(Call::Put.event_name(), &bucket, key))
            .collect();
        let mut delivered = Vec::new();
        for step in row.steps {
            match step.call {
                Call::Put => put_object(&router, case, &bucket, step.key).await,
                Call::Multipart => {
                    complete_multipart_upload(&router, case, &bucket, step.key).await
                }
                Call::Delete => delete_object(&router, case, &bucket, step.key).await,
            }
            let line = logged_line(step.call.event_name(), &bucket, step.key);
            if step.delivered {
                await_one_record(&router, case, &function_name, &line).await;
                delivered.push(line);
            } else {
                absent.push(line);
            }
        }
        assert_settled(&router, case, &function_name, &delivered, &absent).await;
    }
}

/// SEVERAL function destinations on one bucket: each object event runs exactly the functions
/// whose own configuration names its event and whose key-name `Filter` admits its key, and no
/// other — which is what makes a bucket fan its events out to destinations of their own.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn an_event_reaches_exactly_the_destinations_that_name_it() {
    driven("s3-notification-event-fan-out", || {
        Box::pin(an_event_reaches_exactly_the_destinations_that_name_it_body())
    });
}

async fn an_event_reaches_exactly_the_destinations_that_name_it_body() {
    let case = "three function destinations on one bucket";
    let router = build_router();
    let deployed = deployed_functions(
        &router,
        case,
        "notification-fan-out-stack",
        &["First", "Second", "Third"],
    )
    .await;
    let (first_arn, first_name) = deployed[0].clone();
    let (second_arn, second_name) = deployed[1].clone();
    let (third_arn, third_name) = deployed[2].clone();

    let bucket = "event-classes-fan-out";
    create_bucket(&router, case, bucket).await;
    // Two destinations take the SAME event class under disjoint key prefixes, and a third
    // takes another class over the whole bucket: no two of these rules are reached by one
    // object event, which is what the container has to satisfy to be recorded at all
    // ("configurations on the same bucket cannot share a common event type").
    put_accepted_notification(
        &router,
        case,
        bucket,
        &[
            Configured {
                arn: &first_arn,
                events: &["s3:ObjectCreated:*"],
                prefix: Some("first/"),
            },
            Configured {
                arn: &second_arn,
                events: &["s3:ObjectCreated:*"],
                prefix: Some("second/"),
            },
            Configured {
                arn: &third_arn,
                events: &["s3:ObjectRemoved:*"],
                prefix: None,
            },
        ],
    )
    .await;

    let first_key = "first/written.txt";
    let second_key = "second/written.txt";
    put_object(&router, case, bucket, first_key).await;
    put_object(&router, case, bucket, second_key).await;
    delete_object(&router, case, bucket, first_key).await;

    let created_first = logged_line(Call::Put.event_name(), bucket, first_key);
    let created_second = logged_line(Call::Put.event_name(), bucket, second_key);
    let removed_first = logged_line(Call::Delete.event_name(), bucket, first_key);

    // Each destination ran once for the event its own rule names …
    await_one_record(&router, case, &first_name, &created_first).await;
    await_one_record(&router, case, &second_name, &created_second).await;
    await_one_record(&router, case, &third_name, &removed_first).await;

    // … and for nothing else, stated on groups a record has already arrived in, which is also
    // where a second record for an event named once would show.
    assert_settled(
        &router,
        case,
        &first_name,
        std::slice::from_ref(&created_first),
        &[created_second.clone(), removed_first.clone()],
    )
    .await;
    assert_settled(
        &router,
        case,
        &second_name,
        std::slice::from_ref(&created_second),
        &[created_first.clone(), removed_first.clone()],
    )
    .await;
    assert_settled(
        &router,
        case,
        &third_name,
        &[removed_first],
        &[created_first, created_second],
    )
    .await;
}

/// A configuration naming a function that does NOT exist is refused WHOLE, so nothing of it is
/// installed and no later write reaches even the function it named beside the missing one.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_configuration_naming_a_missing_function_delivers_nothing() {
    driven("s3-notification-missing-function", || {
        Box::pin(a_configuration_naming_a_missing_function_delivers_nothing_body())
    });
}

async fn a_configuration_naming_a_missing_function_delivers_nothing_body() {
    let case = "a container naming a function that does not exist";
    let router = build_router();
    let deployed = deployed_functions(
        &router,
        case,
        "notification-missing-function-stack",
        &["Handler"],
    )
    .await;
    let (function_arn, function_name) = deployed[0].clone();

    let bucket = "event-classes-missing-function";
    create_bucket(&router, case, bucket).await;

    // The ARN is well formed and names no function of this account: the ARN shape is the one
    // every function ARN of this emulator carries, with a name nothing created. It is named
    // beside a function that DOES exist, under another event class so that no two of the
    // container's rules are reached by one event.
    let missing_arn = function_arn.replace(&function_name, "no-such-handler");
    let refused = put_notification(
        &router,
        bucket,
        &[
            Configured {
                arn: &function_arn,
                events: &["s3:ObjectCreated:*"],
                prefix: None,
            },
            Configured {
                arn: &missing_arn,
                events: &["s3:ObjectRemoved:*"],
                prefix: None,
            },
        ],
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: the put must be refused, got {} with {}",
        refused.status, refused.body
    );
    assert!(
        refused.body.contains("<Code>InvalidArgument</Code>")
            && refused
                .body
                .contains("Unable to validate the following destination configurations"),
        "case {case:?}: expected the InvalidArgument refusal of a destination that does not \
         validate, got {}",
        refused.body
    );

    // The put is atomic, so the bucket carries no Lambda configuration at all …
    let read = s3(&router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration must succeed, got {} with {}",
        read.status, read.body
    );
    assert!(
        !read.body.contains("<CloudFunction>"),
        "case {case:?}: a refused put records nothing, so the read must report no function \
         destination, got {}",
        read.body
    );

    // … and so a write into the bucket reaches neither the missing function nor the one named
    // beside it. The same function is then shown to be reachable on a configuration S3 DID
    // accept, so this absence is not an unreachable function's.
    let unnotified = "events/unnotified.txt";
    put_object(&router, case, bucket, unnotified).await;
    let accepted = "events/notified.txt";
    put_accepted_notification(
        &router,
        case,
        bucket,
        &[Configured {
            arn: &function_arn,
            events: &["s3:ObjectCreated:*"],
            prefix: None,
        }],
    )
    .await;
    put_object(&router, case, bucket, accepted).await;
    let notified = logged_line(Call::Put.event_name(), bucket, accepted);
    await_one_record(&router, case, &function_name, &notified).await;
    assert_settled(
        &router,
        case,
        &function_name,
        &[notified],
        &[logged_line(Call::Put.event_name(), bucket, unnotified)],
    )
    .await;
}
