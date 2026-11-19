// SPDX-License-Identifier: CC0-1.0

//! An object event's DELIVERY is a hop of the chain the call that produced the event belongs
//! to — end to end over the assembled router, from a client's own write through Amazon S3's
//! delivery into the function's run and the call that run makes back.
//!
//! The rule under test: **a call one service makes on behalf of the call it is serving — the
//! asynchronous `Invoke` Amazon S3 makes to deliver an object event, and the SDK call a
//! handler's own code makes — is one HOP of the request chain the served call belongs to, so
//! it carries that chain's `X-Amzn-Trace-Id` ("for X-Ray traces, all Amazon Web Services
//! services use the X-Amzn-Trace-Id header from the HTTP request") rather than starting a
//! chain of its own; a call that arrived naming no chain carries none onward. That is what
//! lets AWS Lambda place every hop of one chain in that chain and so detect a function being
//! invoked by an event one of its own writes produced — "Lambda can detect certain types of
//! recursive loops shortly after they occur … it stops your function being invoked"
//! (`PutFunctionRecursionConfig$RecursiveLoop`) — and stop it at the invocation past the
//! number AWS publishes for the guardrail.**
//!
//! Four crates have to agree for that to hold (Amazon S3 evaluates the configuration and makes
//! the delivery, AWS Lambda places the invocation in its chain and runs the handler, IAM
//! answers for the execution role the handler's own call is signed as, and CloudWatch reports
//! the guardrail's own series), so the suite lives here.
//!
//! The rows of the first test differ along what the rule generalises over: what the producing
//! call named as its chain — a sampled chain, an unsampled one, one with no sampling decision,
//! no header at all, and a header naming nothing X-Ray reads as a chain. The second test is
//! the invariant the rule is for: a notification whose handler writes a key the same
//! configuration matches comes back round into the function, and the chain is what BOUNDS that
//! loop.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]): the assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test
//! holding several of them across await points would lay that state out on the test thread's
//! stack.

use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::MAX_REQUEST_CHAIN_INVOCATIONS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, outputs_of};
// The sibling notification suite's own harness: the router as it is ASSEMBLED, the
// `Authorization` header that routes a request, an awsQuery call, and the sized thread a chain
// of in-process calls needs.
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, driven, query,
};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// Where AWS Lambda binds the function operations this suite calls.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The header a request's X-Ray tracing context travels on.
const TRACE_HEADER: &str = "x-amzn-trace-id";

/// The service principal Amazon S3 publishes notifications under, and the action it performs
/// on a function destination.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class every configuration below names.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The `StackStatus` a deployed stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The prefix the handler keys every marker it writes under.
const MARKER_PREFIX: &str = "markers/";

/// The `Timeout` every function here is created with: generous, because nothing in this suite
/// is about how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite waits for work the clock owes — a stack's resources coming up, an
/// asynchronous delivery, and a chain running its course — and how often it looks while
/// waiting. Every wait is on a CONDITION; nothing here asserts how long one took.
const WAIT: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the bounded chain is watched for a further hop once the guardrail has fired: well
/// past the span one hop of it takes, so a count that is still growing is seen to grow.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The CloudWatch namespace AWS Lambda publishes its own series in, the series the guardrail
/// meters a stopped invocation on, the dimension those series are keyed by, and the unit the
/// metric reference gives it.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const DROPPED_SERIES: &str = "RecursiveInvocationsDropped";
const FUNCTION_DIMENSION: &str = "FunctionName";
const COUNT_UNIT: &str = "Count";

/// The window a reader asks the guardrail's series over, and the period it asks for.
const WINDOW_SECS: i64 = 900;
const PERIOD_SECS: i64 = 60;

/// The handler every function here deploys: for each record it is handed it writes ONE marker
/// object carrying the trace context its own execution environment reports, and — where its
/// configuration says to — one derived object back into the bucket the event came from, which
/// is the write that can come back round.
///
/// Both writes are made through the AWS SDK the runtime supplies, so each is a real Amazon S3
/// call made with the function's own execution-role credentials.
const MARKING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    client = boto3.client("s3")
    markers = os.environ["MARKER_BUCKET"]
    feeds_back = os.environ.get("FEED_BACK") == "yes"
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = record["s3"]["object"]["key"]
        client.put_object(
            Bucket=markers,
            Key="markers/" + key.replace("/", "_") + "--" + context.aws_request_id,
            Body=json.dumps(
                {"trace": os.environ.get("_X_AMZN_TRACE_ID", ""), "key": key}
            ).encode("utf-8"),
        )
        if feeds_back:
            client.put_object(
                Bucket=bucket,
                Key="derived/" + key.split("/")[-1],
                Body=b"derived",
            )
    return {"ok": True}
"#;

/// Run `body` to completion on a thread SIZED for a chain of in-process calls, on the
/// current-thread runtime this suite's deliveries are made on: assembling every service and
/// then dispatching a chain through it — Amazon S3's delivery into AWS Lambda, the handler's
/// own call back into Amazon S3 — nests more future than the default stack of a test thread
/// holds. It is the sibling notification suite's own harness, so neither can drift onto a
/// thread the chain does not fit on.
fn on_a_sized_thread<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> + Send + 'static,
{
    driven(name, body);
}

/// One request against Amazon S3's REST surface, path-style, carrying `headers` beside the
/// ones every call here carries.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}"), "", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, String) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One CloudWatch call over its `awsJson1_0` wire, as its parsed body.
async fn cloudwatch(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("monitoring.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("authorization", authorization("monitoring"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The stack that brings up the handler: the execution role its own Amazon S3 calls are made
/// as, and the function itself, whose inline source CloudFormation archives into its
/// deployment package.
fn handler_stack(markers: &str, feeds_back: bool) -> String {
    let mut variables = json!({ "MARKER_BUCKET": markers });
    if feeds_back {
        variables["FEED_BACK"] = json!("yes");
    }
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
                    // The handler calls Amazon S3 with temporary credentials for this role, so
                    // the role has to grant the writes it makes.
                    "Policies": [{
                        "PolicyName": "mark-the-event",
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
            },
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": variables },
                    "Code": { "ZipFile": MARKING_HANDLER },
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

/// Deploy `stack`, wait for `CREATE_COMPLETE` and answer the function's `(ARN, name)`.
async fn deployed_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> (String, String) {
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let giving_up_at = Instant::now() + WAIT;
    let mut reported = stack_status(router, stack).await;
    while reported.0 != CREATE_COMPLETE && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {:?} with {}",
        reported.0, reported.1
    );
    let outputs = outputs_of(&reported.1);
    let arn = outputs
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function's ARN"));
    let name = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function's name"));
    (arn, name)
}

/// `AddPermission`: the grant Amazon S3 reads before it records the configuration.
async fn grant_invoke(router: &MultiServiceRouter, case: &str, function: &str, bucket: &str) {
    let (status, body) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        Some(&json!({
            "StatementId": "allow-this-bucket",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{bucket}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission must succeed, got {status} with {body}"
    );
}

/// `PUT /<bucket>?notification` with one Lambda configuration naming `function_arn` for every
/// object-created event, filtered to `prefix` where a case names one.
///
/// The element is the one Amazon S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_lambda_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    function_arn: &str,
    prefix: Option<&str>,
) {
    let filter = prefix.map_or_else(String::new, |prefix| {
        format!(
            "<Filter><S3Key><FilterRule><Name>prefix</Name><Value>{prefix}</Value>\
             </FilterRule></S3Key></Filter>"
        )
    });
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>mark</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event>{filter}\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let (status, answered) = s3(
        router,
        "PUT",
        &format!("/{bucket}?notification"),
        &body,
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must succeed, got \
         {status} with {answered}"
    );
}

/// The markers `bucket` holds under `prefix` right now, read through `ListObjectsV2`.
async fn markers_under(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> Vec<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}&max-keys=1000"),
        "",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListObjectsV2({bucket:?}, {prefix:?}) must succeed, got {status} with \
         {body}"
    );
    elements(&body, "Key")
}

/// The trace context the run that left the marker `key` reported, read out of the marker the
/// handler wrote.
async fn marker_trace(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) -> String {
    let (status, body) = s3(router, "GET", &format!("/{bucket}/{key}"), "", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetObject({bucket:?}, {key:?}) must succeed, got {status} with {body}"
    );
    let marker: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the marker must be the document the handler wrote ({error}), got {body}")
    });
    marker["trace"].as_str().unwrap_or_default().to_string()
}

/// Wait until `bucket` holds at least `wanted` markers under `prefix`, and answer the keys it
/// holds. The wait is on the CONDITION; nothing asserts how long it took.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
    wanted: usize,
) -> Vec<String> {
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let keys = markers_under(router, case, bucket, prefix).await;
        if keys.len() >= wanted {
            return keys;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected at least {wanted} marker(s) under {prefix:?}, got {keys:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One row of the first table: what the producing call named as its chain, and what the
/// invocation Amazon S3 makes for its event must then run inside.
struct Row {
    case: &'static str,
    /// The key the row writes, which is also what its marker is named after.
    key: &'static str,
    /// The tracing header the write carries, or `None` for a write that carries none.
    sent: Option<&'static str>,
    /// The chain the delivered invocation must run inside, where the row's own header names
    /// one; `None` where the row names no chain the delivery could carry, in which case the
    /// invocation runs inside a chain of AWS Lambda's own.
    continues: Option<&'static str>,
}

/// The rows: one per form a producing call's tracing header can take.
const ROWS: &[Row] = &[
    Row {
        case: "a sampled chain",
        key: "uploads/sampled.txt",
        sent: Some("Root=1-5759e988-bd862e3fe1be46a994272793;Sampled=1"),
        continues: Some("1-5759e988-bd862e3fe1be46a994272793"),
    },
    Row {
        case: "an unsampled chain",
        key: "uploads/unsampled.txt",
        sent: Some("Root=1-5759e988-aa862e3fe1be46a994272794;Sampled=0"),
        continues: Some("1-5759e988-aa862e3fe1be46a994272794"),
    },
    Row {
        case: "a chain with no sampling decision",
        key: "uploads/undecided.txt",
        sent: Some("Root=1-5759e988-bb862e3fe1be46a994272795"),
        continues: Some("1-5759e988-bb862e3fe1be46a994272795"),
    },
    Row {
        case: "no tracing header at all",
        key: "uploads/unnamed.txt",
        sent: None,
        continues: None,
    },
    Row {
        case: "a header naming nothing X-Ray reads as a chain",
        key: "uploads/malformed.txt",
        sent: Some("Root=not-a-trace-id;Sampled=1"),
        continues: None,
    },
];

/// RULE: the invocation Amazon S3 makes to deliver an object event runs inside the chain the
/// call that produced the event named, and inside a chain of AWS Lambda's own where that call
/// named none.
#[test]
fn an_object_events_delivery_runs_inside_the_producing_calls_chain() {
    on_a_sized_thread("s3-notification-chain-hop", || {
        Box::pin(an_object_events_delivery_runs_inside_the_producing_calls_chain_body())
    });
}

async fn an_object_events_delivery_runs_inside_the_producing_calls_chain_body() {
    let case = "the delivered invocation's own chain";
    let router = build_router();
    let source = "chain-hop-source";
    let markers = "chain-hop-markers";
    create_bucket(&router, case, source).await;
    create_bucket(&router, case, markers).await;
    let (function_arn, function_name) =
        deployed_function(&router, case, "chain-hop-stack", &handler_stack(markers, false)).await;
    grant_invoke(&router, case, &function_name, source).await;
    // Filtered to the prefix every row writes under, so the handler's own marker — written
    // into the other bucket — cannot produce an event of its own.
    put_lambda_notification(&router, case, source, &function_arn, Some("uploads/")).await;

    for row in ROWS {
        let case = row.case;
        let headers: Vec<(&str, &str)> = row
            .sent
            .map(|sent| vec![(TRACE_HEADER, sent)])
            .unwrap_or_default();
        let (status, body) = s3(
            &router,
            "PUT",
            &format!("/{source}/{}", row.key),
            "the object the event named",
            &headers,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutObject({:?}) must succeed, got {status} with {body}",
            row.key
        );

        let prefix = format!("{MARKER_PREFIX}{}", row.key.replace('/', "_"));
        let keys = awaited_markers(&router, case, markers, &prefix, 1).await;
        let trace = marker_trace(&router, case, markers, &keys[0]).await;
        assert!(
            trace.contains("Root=1-"),
            "case {case:?}: expected the delivered invocation to run inside a chain, got \
             {trace:?}"
        );
        match row.continues {
            Some(root) => assert!(
                trace.contains(&format!("Root={root}")),
                "case {case:?}: expected the invocation to continue the chain {root:?} the \
                 write named, got {trace:?}"
            ),
            // A write naming no chain the delivery could carry has AWS Lambda start one, so
            // what the run reports is a chain and not the text the write sent.
            None => assert!(
                !row.sent.is_some_and(|sent| trace == sent),
                "case {case:?}: expected a chain of AWS Lambda's own, got the write's own \
                 header {trace:?}"
            ),
        }
    }
}

/// RULE (the invariant): a notification whose handler writes a key the same configuration
/// matches comes back round into the function, and the chain the hops share is what bounds the
/// loop — AWS Lambda's guardrail stops the invocation past the number it publishes, and meters
/// the one it stopped.
#[test]
fn a_notification_that_comes_back_round_is_stopped_by_the_chains_guardrail() {
    on_a_sized_thread("s3-notification-chain-guardrail", || {
        Box::pin(a_notification_that_comes_back_round_is_stopped_by_the_chains_guardrail_body())
    });
}

async fn a_notification_that_comes_back_round_is_stopped_by_the_chains_guardrail_body() {
    let case = "a notification that comes back round into its own function";
    let router = build_router();
    let source = "chain-loop-source";
    let markers = "chain-loop-markers";
    create_bucket(&router, case, source).await;
    create_bucket(&router, case, markers).await;
    let (function_arn, function_name) = deployed_function(
        &router,
        case,
        "chain-loop-stack",
        &handler_stack(markers, true),
    )
    .await;
    grant_invoke(&router, case, &function_name, source).await;
    // UNFILTERED, so the derived object the handler writes back into this same bucket is an
    // event of the same configuration: the chain comes back round into the function.
    put_lambda_notification(&router, case, source, &function_arn, None).await;

    // The write that starts the chain names it, so every hop of the loop is a hop of ONE
    // chain and the count the guardrail keeps is this chain's from the first delivery on.
    let root = "1-5759e988-cc862e3fe1be46a994272796";
    let (status, body) = s3(
        &router,
        "PUT",
        &format!("/{source}/start.txt"),
        "the object the event named",
        &[(TRACE_HEADER, &format!("Root={root};Sampled=0"))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutObject must succeed, got {status} with {body}"
    );

    // The guardrail firing is the condition this waits on: it is what says the loop has been
    // detected and stopped, rather than a count that has merely paused.
    let giving_up_at = Instant::now() + WAIT;
    let mut dropped = 0.0;
    while dropped < 1.0 {
        dropped = dropped_invocations(&router, case, &function_name).await;
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected AWS Lambda to stop an invocation of the loop and meter it \
             on {DROPPED_SERIES}, got a sum of {dropped} with {} marker(s)",
            markers_under(&router, case, markers, MARKER_PREFIX).await.len()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    let keys = markers_under(&router, case, markers, MARKER_PREFIX).await;
    let ran = keys.len();
    assert!(
        ran > 1,
        "case {case:?}: expected the handler's own write to come back round into the function, \
         got {ran} invocation(s)"
    );
    let allowed = usize::try_from(MAX_REQUEST_CHAIN_INVOCATIONS).unwrap_or(usize::MAX);
    assert!(
        ran <= allowed,
        "case {case:?}: expected the chain to be stopped at {allowed} invocation(s) of the \
         function, got {ran}"
    );
    // Every hop belongs to the ONE chain the first write named, which is why the guardrail
    // could count them at all.
    for key in &keys {
        let trace = marker_trace(&router, case, markers, key).await;
        assert!(
            trace.contains(&format!("Root={root}")),
            "case {case:?}: expected every hop of the loop to run inside the chain {root:?}, \
             got {trace:?} for {key:?}"
        );
    }
    // And the loop is OVER: a stopped invocation queues nothing, so no further hop appears
    // however long this keeps looking.
    let giving_up_at = Instant::now() + QUIET_WINDOW;
    while Instant::now() < giving_up_at {
        let later = markers_under(&router, case, markers, MARKER_PREFIX).await.len();
        assert_eq!(
            later, ran,
            "case {case:?}: expected the stopped chain to make no further invocation, got \
             {later} after {ran}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// How many invocations of `function` AWS Lambda's guardrail is reported to have stopped: the
/// SUM of its own series over a window covering the whole case, which is the magnitude a
/// client alarms on. A series with no datapoint at all sums to zero.
async fn dropped_invocations(router: &MultiServiceRouter, case: &str, function: &str) -> f64 {
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
            "MetricName": DROPPED_SERIES,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": function }],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum"],
            "Unit": COUNT_UNIT,
        }),
    )
    .await;
    stats["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}
