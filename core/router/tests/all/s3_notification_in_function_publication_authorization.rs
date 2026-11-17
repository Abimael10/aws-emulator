// SPDX-License-Identifier: CC0-1.0

//! What decides the EventBridge publication a RUNNING function makes, when the run was started
//! by an OBJECT UPLOAD rather than by a client's own call: the identity-based policies in force
//! on the function's execution role, and nothing about which path started the run.
//!
//! RULE: a call an AWS SDK makes from inside a running function is a real signed call made as a
//! session of that function's execution role, so the service serving it decides it against the
//! policies in force on that role at the instant of the call — whatever started the run,
//! whether a client's synchronous `Invoke` or the asynchronous invocation a bucket's
//! notification configuration makes for an object write. A publication those policies allow is
//! served (`FailedEntryCount` 0 and an `EventId` per entry) and the accepted event is matched
//! against the bus's rules and delivered to the rule's target; one they do not allow is refused
//! with the AWS-common `AccessDeniedException` on HTTP 400, naming the assumed-role session,
//! the action `events:PutEvents` and the ARN of the bus the entry addressed — and NOTHING of it
//! reaches the bus, so no rule matches and no target is invoked. Either way the run itself is
//! not a failure: the handler catches the refusal and reports it.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`, and with neither an identity-based
//! nor a resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*).
//! EventBridge's `PutEvents` declares no authorization error of its own
//! (`smithy_query(query="operation", operation="PutEvents")` names `InternalException` only),
//! so the refusal is the AWS-common one; its per-entry `ErrorCode`/`ErrorMessage` channel
//! reports what is wrong with an ENTRY, which an authorization verdict is not.
//!
//! Six crates have to agree for that to hold — CloudFormation deploys the roles, the functions
//! and the grant Amazon S3 needs, S3 evaluates the notification configuration and makes the
//! invocation, IAM holds the roles and their documents, the token service mints the session the
//! handler's call is signed as, Lambda runs the handler and supplies it the SDK, EventBridge
//! takes the decision, and Amazon SQS holds what the rule delivered — so the suite lives here.
//!
//! What the neighbours pin and this one does not repeat:
//! `lambda_in_function_events_authorization` drives the TABLE of identity-document shapes one
//! in-function publication is decided by, all of them started by a client's own `Invoke`, and
//! `s3_notification_invokes_its_lambda_target` that an object write runs the function the
//! bucket notifies at all. What only this suite reaches is the two STARTING PATHS of the same
//! decision, read back through the same two places for each: what the run itself reported, and
//! what the rule's target queue holds afterwards.
//!
//! The rows differ along what the rule generalises over: WHAT started the run (an object
//! upload the bucket notifies, a client's synchronous `Invoke`) and WHAT the execution role's
//! policies say about the publication (the action allowed on every resource, nothing of the
//! service at all beyond the managed policy every execution role carries). Each row asserts
//! both halves, so a refusal that still published and a grant that answered without delivering
//! both fail here.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]), and the whole suite
//! runs on a sized thread ([`crate::s3_notification_invokes_its_lambda_target::driven`]): the
//! assembled router's dispatch future carries the whole emulator's handler state inline, and
//! this suite nests a CHAIN of them — CloudFormation into Lambda, S3's delivery into Lambda,
//! the handler's own SDK call into EventBridge, EventBridge's delivery into Amazon SQS.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};
use crate::s3_notification_invokes_its_lambda_target::driven;

/// The Region every request here is signed for, and so the Region the bus, the rule, the queue,
/// the buckets and the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where `Invoke` and the configuration read are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The bus every handler publishes onto — the account's default one.
const BUS: &str = "default";

/// The rule watching that bus for the handlers' events, and the queue it targets: the place a
/// publication EventBridge accepted is read back from.
const RULE: &str = "s3-in-fn-events-rule";
const SINK: &str = "s3-in-fn-events-sink";

/// The `source` and `detail-type` every handler publishes under, which is all the rule matches
/// on — so a message on its queue is one of these publications and nothing else.
const SOURCE: &str = "com.demo.s3-in-fn-events";
const DETAIL_TYPE: &str = "In Function Publication";

/// The stack the roles, the functions and the grant S3 needs are deployed by.
const STACK: &str = "s3-in-fn-events-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The action a publication onto a bus performs, as a statement names it and as a refusal
/// reports it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The code and status EventBridge refuses a publication the policies in force do not permit
/// with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The clause a refusal closes with when no document in force states anything about the
/// publication — IAM's implicit deny.
const NO_POLICY_ALLOWS: &str =
    "because no identity-based policy allows the events:PutEvents action";

/// The managed policy AWS owns that every Lambda execution role carries: it grants the log
/// writes a function makes and nothing of any other service, so a role carrying only it is
/// granted no publication — while its runs still leave their log where a client reads it.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The service principal the sink queue's policy admits, because a delivery to a queue is made
/// by the delivering service itself and "EventBridge relies on resource-based policies"
/// (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination — the grant the function's own policy has to carry for a
/// configuration naming it to be admitted at all.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The runtime every function is created with: a managed Python runtime, which carries the SDK
/// the handler imports.
const RUNTIME: &str = "python3.12";

/// The module and attribute the deployment package carries its handler as. CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how fast
/// an interpreter gets through one SDK call.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long work the CLOCK owes — a stack completing, a function becoming `Active`, an upload
/// reaching the function, an event reaching the rule's target — is waited for before giving up,
/// and how long between two looks. A GENEROUS ceiling rather than an expectation: every wait
/// here is on the CONDITION, and nothing asserts how long anything took.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The body every upload carries. It is never read by the handler: what the upload is for is
/// STARTING the run.
const OBJECT_BODY: &str = "the object whose write started the run";

/// What the handler prefixes its one report line with, which is how a client reading the
/// function's own log group finds it among the invocation's framing.
const REPORT_PREFIX: &str = "probe-report ";

/// The handler every function runs. It imports the SDK at MODULE scope and builds the client
/// there, publishes ONE event per marker the run is about, and reports what the publication
/// answered or the refusal it raised — both as its RETURN value, which a synchronous caller
/// reads, and through the logging facility, which is where the report of a run nobody is
/// waiting on is read from.
///
/// A run started by an object write is handed the `Records` envelope, so the marker it
/// publishes is the key that was written; a run a client invoked is handed the marker itself.
const PROBE_HANDLER: &str = r#"
import json
import logging
import os

import boto3
from botocore.exceptions import ClientError

LOGGER = logging.getLogger()
LOGGER.setLevel(logging.INFO)

EVENTS = boto3.client("events")


def markers(event):
    records = event.get("Records")
    if records:
        return [record["s3"]["object"]["key"] for record in records]
    return [event["marker"]]


def published(marker):
    try:
        answered = EVENTS.put_events(
            Entries=[
                {
                    "EventBusName": os.environ["PROBE_BUS"],
                    "Source": os.environ["PROBE_SOURCE"],
                    "DetailType": os.environ["PROBE_DETAIL_TYPE"],
                    "Detail": json.dumps({"marker": marker}),
                }
            ]
        )
    except ClientError as refused:
        return {
            "sdk": boto3.__name__,
            "marker": marker,
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "sdk": boto3.__name__,
        "marker": marker,
        "ok": True,
        "failed_count": answered["FailedEntryCount"],
        "event_ids": [entry["EventId"] for entry in answered["Entries"]],
    }


def handler(event, context):
    reports = [published(marker) for marker in markers(event)]
    for report in reports:
        LOGGER.info("probe-report %s", json.dumps(report))
    return {"reports": reports}
"#;

/// WHAT started the run whose publication a row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// An object write into the bucket whose notification configuration names the function,
    /// which Lambda runs as an asynchronous invocation nobody is waiting on.
    Upload,
    /// A client's own synchronous `Invoke` of the same function.
    Invoke,
}

/// What the publication EARNS, which follows from the role's documents alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The policies in force allow it: the entry is accepted and delivered to the rule's
    /// target.
    Served,
    /// They do not, and the refusal closes with this clause.
    Refused(&'static str),
}

/// One row: which execution role the handler runs as, what started the run, the marker that run
/// publishes, and what the publication earns.
struct Row {
    case: &'static str,
    /// The probe this row drives: [`GRANTED`] runs as a role allowing the publication, and
    /// [`UNGRANTED`] as one carrying only the managed policy every execution role carries.
    probe: usize,
    start: Start,
    marker: &'static str,
    outcome: Outcome,
}

/// The two probes, each its own execution role and its own function: the role that allows the
/// publication, and the role that allows nothing of the service.
const GRANTED: usize = 0;
const UNGRANTED: usize = 1;
const PROBES: usize = 2;

const ROWS: &[Row] = &[
    Row {
        case: "an upload the bucket notifies, run by a role that allows the publication",
        probe: GRANTED,
        start: Start::Upload,
        // The marker of an upload row is the key that was written, which is what the record the
        // handler is handed names.
        marker: "s3-in-fn-events-granted-upload.json",
        outcome: Outcome::Served,
    },
    Row {
        case: "an upload the bucket notifies, run by a role that allows nothing of the service",
        probe: UNGRANTED,
        start: Start::Upload,
        marker: "s3-in-fn-events-denied-upload.json",
        outcome: Outcome::Refused(NO_POLICY_ALLOWS),
    },
    Row {
        case: "a client's own invoke, run by a role that allows the publication",
        probe: GRANTED,
        start: Start::Invoke,
        marker: "s3-in-fn-events-granted-invoke",
        outcome: Outcome::Served,
    },
    Row {
        case: "a client's own invoke, run by a role that allows nothing of the service",
        probe: UNGRANTED,
        start: Start::Invoke,
        marker: "s3-in-fn-events-denied-invoke",
        outcome: Outcome::Refused(NO_POLICY_ALLOWS),
    },
];

/// The role one probe runs as, the function it runs in, and the bucket whose notification
/// configuration names that function — the first two named here because the refusal's own
/// sentence names both: the identity a function's call is signed as is the session
/// `assumed-role/<role>/<function>`.
fn role_name(probe: usize) -> String {
    format!("s3-in-fn-events-role-{probe}")
}
fn function_name(probe: usize) -> String {
    format!("s3-in-fn-events-probe-{probe}")
}
fn bucket_name(probe: usize) -> String {
    format!("s3-in-fn-events-source-{probe}")
}

/// The ARN of a function of this account in this Region, which is what a notification
/// configuration names its destination by.
fn function_arn(probe: usize) -> String {
    format!(
        "arn:aws:lambda:{REGION}:{ACCOUNT}:function:{}",
        function_name(probe)
    )
}

/// The ARN of an event bus of this account in this Region, which is the resource a publication
/// onto it is authorized on and the one a refusal reports.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// The ARN of a queue of this account in this Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The session a call made by an invocation of the probe's function speaks for: the session a
/// function's calls are made under is named for the FUNCTION, which is how a refusal says which
/// function was refused — and it is the same session whichever path started the run.
fn session_arn(probe: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(probe),
        function_name(probe)
    )
}

/// The sentence EventBridge refuses a publication with. Written out here rather than built from
/// the service's own helper, so the wording has something to be wrong about.
fn refusal_message(probe: usize, reason: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {PUT_EVENTS_ACTION} on resource: {} {reason}",
        session_arn(probe),
        bus_arn(BUS),
    )
}

/// One `Allow` statement of an identity-based document.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the bus it publishes onto is the one this suite watches.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing, as in every other suite of this crate; the credential scope is
    // what routes a request. The HANDLER's own calls are signed by the runtime, with the
    // credential the invocation was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Its credential is the long-term key every emulator publishes, so every call this
/// suite makes on its own behalf is the account's and not any probe's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`, as the HTTP status and the XML it answered.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
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

/// One `awsQuery` call that must be served, as the XML it carried.
async fn served_query(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, service, version, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service} {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call, as its status and its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
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

/// One RPC call that must be served.
async fn served_rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        signing_name,
        target_prefix,
        json_version,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    served_rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    served_rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), as its status and
/// parsed body. A group the delivery has not created yet is a refusal this suite polls through,
/// so the status is answered rather than asserted here.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "logs", "Logs_20140328", "1.1", operation, body).await
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// Dispatch one request against S3's REST surface, path-style, as its status and its body.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
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
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {body}"
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`, the write that starts an upload row's run.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}/{key}"), OBJECT_BODY).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {status} with {body}"
    );
}

/// `PUT /<bucket>?notification` with one Lambda configuration naming `function_arn` for
/// `s3:ObjectCreated:*`, and the read-back that says S3 recorded it.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_lambda_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    function_arn: &str,
) {
    let document = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>probe</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>s3:ObjectCreated:*</Event>\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}?notification"),
        &document,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must succeed, got \
         {status} with {body}"
    );
    let (status, body) = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must succeed, got \
         {status} with {body}"
    );
    let recorded = elements(&body, "CloudFunction");
    assert_eq!(
        recorded,
        vec![function_arn.to_string()],
        "case {case:?}: expected the bucket to notify exactly the probe's function, got \
         {recorded:?}"
    );
}

/// The trust policy a Lambda execution role needs: this service has to be able to assume the
/// role for a function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The queue the rule targets, its policy admitting the delivering service scoped to that rule,
/// and the rule itself. Answers the queue's url.
async fn given_rule_and_sink(router: &MultiServiceRouter) -> String {
    let case = "setup";
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": SINK })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(queue_arn(SINK).as_str()),
        "setup: expected the queue ARN of this account and Region, got {attributes}"
    );

    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let rule = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a RuleArn in {rule}"))
        .to_string();

    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowRuleDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn(SINK),
            "Condition": {"ArnEquals": {"aws:SourceArn": rule_arn}},
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;

    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "sink", "Arn": queue_arn(SINK) }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "setup: expected PutTargets to accept the queue target, got {targets}"
    );
    url
}

/// The template the suite deploys: one execution role per probe — each carrying the managed
/// policy every execution role carries, and the granted one an embedded document allowing the
/// publication — one function per probe running as it, and the grant Amazon S3 needs to invoke
/// that function. The inline source is archived into the deployment package by CloudFormation,
/// which is what makes the package hold exactly ONE module and no SDK of its own.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for probe in 0..PROBES {
        let role_logical = format!("Role{probe}");
        let function_logical = format!("Probe{probe}");
        let mut properties = serde_json::Map::new();
        properties.insert("RoleName".to_string(), json!(role_name(probe)));
        properties.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        properties.insert(
            "ManagedPolicyArns".to_string(),
            json!([BASIC_EXECUTION_POLICY]),
        );
        if probe == GRANTED {
            properties.insert(
                "Policies".to_string(),
                json!([{
                    "PolicyName": "publication-grants",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [allow(PUT_EVENTS_ACTION, "*")],
                    },
                }]),
            );
        }
        resources.insert(
            role_logical.clone(),
            json!({ "Type": "AWS::IAM::Role", "Properties": Value::Object(properties) }),
        );
        resources.insert(
            function_logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(probe),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "PROBE_BUS": BUS,
                            "PROBE_SOURCE": SOURCE,
                            "PROBE_DETAIL_TYPE": DETAIL_TYPE,
                        },
                    },
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
        resources.insert(
            format!("InvokePermission{probe}"),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": {"Fn::GetAtt": [function_logical, "Arn"]},
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: the shared wait looks until it reports it, under a
    // deadline generous enough that a loaded host does not decide the outcome.
    let (reported, body) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, stack_events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&stack_events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {body} with resource failures \
         {failures:?}"
    );
    let resources = served_query(
        router,
        "setup",
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await
        .2;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {name:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `Invoke` the probe's function synchronously, asking the handler to publish `marker`, and
/// answer the one report it made.
///
/// The invocation itself must be HTTP 200 with no `FunctionError` whichever way the publication
/// went: the handler catches a refusal and reports it, so a function error here is the crash the
/// runtime must not produce.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) -> Value {
    let event = json!({ "marker": marker });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} with {payload}"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    let reports = payload["reports"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        reports.len(),
        1,
        "case {case:?}: expected one report per marker the run was about, got {payload}"
    );
    reports[0].clone()
}

/// Look in the function's own log group until the run that was handed `marker` has reported what
/// its publication answered, and answer that report — which is how the outcome of a run NOBODY
/// is waiting on is read, the way a client reads it.
async fn awaited_report(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    marker: &str,
) -> Value {
    let group = format!("/aws/lambda/{function}");
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = logs(
            router,
            "FilterLogEvents",
            &json!({ "logGroupName": group }),
        )
        .await;
        let mut lines: Vec<String> = Vec::new();
        if status == 200 {
            for event in answered["events"].as_array().cloned().unwrap_or_default() {
                let message = event["message"].as_str().unwrap_or_default().to_string();
                let Some((_, reported)) = message.split_once(REPORT_PREFIX) else {
                    continue;
                };
                let parsed: Value = serde_json::from_str(reported.trim()).unwrap_or(Value::Null);
                if parsed["marker"].as_str() == Some(marker) {
                    return parsed;
                }
                lines.push(message);
            }
        }
        // What the group holds right now, which the failure names: the report lines a run has
        // left, or the refusal a group the delivery has not created yet answers with.
        let last = if status == 200 {
            json!(lines)
        } else {
            answered
        };
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the group {group:?} to hold the run's report for \
             {marker:?}, got {last}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Every marker that has arrived on the rule's target queue, taken off it as it arrives and
/// accumulated across the whole suite — so a "nothing arrived" read is made over everything the
/// queue ever held rather than over one receive.
struct Sink {
    url: String,
    seen: Vec<String>,
}

impl Sink {
    /// Look again at the queue until `wanted` has arrived on it, or the generous deadline
    /// passes; everything received on the way is kept.
    async fn until(&mut self, router: &MultiServiceRouter, case: &str, wanted: &str) {
        let giving_up_at = Instant::now() + DEADLINE;
        loop {
            self.drain(router, case).await;
            if self.holds(wanted) {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected {wanted:?} to be delivered to the rule's queue, got {:?}",
                self.seen
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Take what the queue hands over now, keeping each delivered event's marker and deleting
    /// the message so one delivery is counted once.
    async fn drain(&mut self, router: &MultiServiceRouter, case: &str) {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": &self.url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}: expected the published source on the rule's queue, got {body}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected the published detail-type on the rule's queue, got \
                 {body}"
            );
            if let Some(found) = envelope["detail"]["marker"].as_str() {
                self.seen.push(found.to_string());
            }
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": &self.url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
    }

    /// Whether `marker` has ever arrived on that queue.
    fn holds(&self, marker: &str) -> bool {
        self.seen.iter().any(|seen| seen == marker)
    }
}

/// Every row: the publication a running function makes with the SDK its runtime supplies is
/// decided against the policies in force on its execution role, whatever started the run, and
/// what the rule's target queue holds afterwards follows that decision.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn an_in_function_publication_is_decided_by_its_role_whatever_started_the_run() {
    driven("s3-notification-in-function-publication", || {
        Box::pin(an_in_function_publication_is_decided_by_its_role_whatever_started_the_run_body())
    });
}

async fn an_in_function_publication_is_decided_by_its_role_whatever_started_the_run_body() {
    let router = build_router();
    let url = given_rule_and_sink(&router).await;
    let physical = deploy(&router).await;

    let mut functions: Vec<String> = Vec::new();
    for probe in 0..PROBES {
        let logical = format!("Probe{probe}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            function,
            function_name(probe),
            "setup: expected the stack to report the function it declared for probe {probe}"
        );
        // A configuration S3 records is validated against the function's own policy and the
        // function it names, so the function has to be there and ready first.
        wait_until_active(&router, &function).await;
        let bucket = bucket_name(probe);
        create_bucket(&router, "setup", &bucket).await;
        put_lambda_notification(&router, "setup", &bucket, &function_arn(probe)).await;
        functions.push(function);
    }

    let mut sink = Sink {
        url,
        seen: Vec::new(),
    };
    for row in ROWS {
        let case = row.case;
        let function = &functions[row.probe];
        let report = match row.start {
            Start::Upload => {
                put_object(&router, case, &bucket_name(row.probe), row.marker).await;
                awaited_report(&router, case, function, row.marker).await
            }
            Start::Invoke => invoked(&router, case, function, row.marker).await,
        };
        assert_eq!(
            report["sdk"],
            json!("boto3"),
            "case {case:?}: expected the run to report the SDK module it imported, got {report}"
        );
        match row.outcome {
            Outcome::Served => {
                assert_eq!(
                    report["ok"],
                    json!(true),
                    "case {case:?}: expected the publication to be served, got {report}"
                );
                assert_eq!(
                    report["failed_count"],
                    json!(0),
                    "case {case:?}: expected no failed entry, got {report}"
                );
                let ids = report["event_ids"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    ids.len(),
                    1,
                    "case {case:?}: expected one result entry per published entry, got {report}"
                );
                assert!(
                    ids[0].as_str().is_some_and(|id| !id.is_empty()),
                    "case {case:?}: expected EventBridge's own event id, got {report}"
                );
                // The accepted event is matched by the rule and delivered to its target, which
                // is where the publication's effect is read.
                sink.until(&router, case, row.marker).await;
            }
            Outcome::Refused(reason) => {
                assert_eq!(
                    report["ok"],
                    json!(false),
                    "case {case:?}: expected the publication to be refused, got {report}"
                );
                assert_eq!(
                    report["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected EventBridge's own refusal code, got {report}"
                );
                assert_eq!(
                    report["status"],
                    json!(ACCESS_DENIED_STATUS),
                    "case {case:?}: expected the refusal on HTTP {ACCESS_DENIED_STATUS}, got \
                     {report}"
                );
                assert_eq!(
                    report["message"],
                    json!(refusal_message(row.probe, reason)),
                    "case {case:?}: expected the refusal IAM words for it, got {report}"
                );
            }
        }
    }

    // Nothing of a refused publication ever reached the bus. The read is taken at an instant by
    // which an event planted through the SAME path as the account has already been delivered, so
    // the absence is the refusal's and not the clock's.
    let control = "an event planted by the account after the refused publications";
    let sentinel = "s3-in-fn-events-sentinel";
    let planted = events(
        &router,
        control,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": BUS,
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": sentinel }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        planted["FailedEntryCount"],
        json!(0),
        "case {control:?}: expected the account's own publication to be accepted, got {planted}"
    );
    sink.until(&router, control, sentinel).await;
    for row in ROWS {
        if row.outcome == Outcome::Served {
            continue;
        }
        assert!(
            !sink.holds(row.marker),
            "case {:?}: expected nothing of the refused publication on the rule's queue, got {:?}",
            row.case,
            sink.seen
        );
    }
}
