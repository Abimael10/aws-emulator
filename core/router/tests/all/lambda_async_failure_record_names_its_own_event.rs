// SPDX-License-Identifier: CC0-1.0

//! What a FAILED asynchronous invocation leaves behind for a client: the invocation record on
//! the queue `PutFunctionEventInvokeConfig` named as the on-failure destination, and the
//! refusal of a destination no record could ever reach.
//!
//! The rules under test:
//!
//! 1. **An accepted asynchronous event whose handler fails every delivery attempt its
//!    qualifier's configuration allows delivers ONE invocation record to that configuration's
//!    `DestinationConfig$OnFailure` destination, and that record is the EVENT's own** — the
//!    request id it was accepted under, the payload it carried, the condition it ended in and
//!    the answer the handler gave for it. So a client reading the destination queue tells one
//!    failed event from another rather than learning only that something failed.
//! 2. **A destination the function's execution role may not write is refused by the call that
//!    CONFIGURES it**, with Lambda's own `InvalidParameterValueException` and nothing
//!    persisted: Lambda delivers an invocation record acting as that role, so such a
//!    configuration names a destination no record could ever reach.
//!
//! Four crates have to agree for that to hold (Lambda serves the configuration, judges the
//! role and runs the asynchronous invocation, Amazon SQS holds the destination, IAM answers
//! for the role's policies, and CloudFormation packages the handler's source into the
//! function's deployment package), so the suite lives here rather than in any one crate. Its
//! sibling [`crate::cloudformation_provisions_lambda_event_invoke_configs`] drives the same
//! wiring from a DECLARED `AWS::Lambda::EventInvokeConfig`; what this one adds is the
//! configuration put through Lambda's OWN API and several events in flight against one
//! configuration at once, which is what makes "the record is the event's own" observable.
//!
//! The rows of the first table differ along what rule 1 generalises over: the FORM of the
//! event payload — an object, a nested object, an array, a bare string and a bare number, each
//! a document `Invoke$Payload` carries — with one record owed per event. The rows of the
//! second differ along what the role is granted and over the KIND of destination the write is
//! judged for.
//!
//! Every expected value is computed here from what the row states, never read back out of the
//! answer being checked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region the shared stack reader
/// ([`wait_for_stack_status`]) polls in — and so the Region every resource below belongs to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `StackStatus` the one stack of this suite must reach before its function is invocable.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where `PutFunctionEventInvokeConfig` and `GetFunctionEventInvokeConfig` are bound:
/// `/2019-09-25/functions/{FunctionName}/event-invoke-config`
/// (`smithy_query(query="operation",operation="PutFunctionEventInvokeConfig")`).
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// Where `CreateFunction` and `Invoke` are bound (`/2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The header `Invoke$InvocationType` is bound to, and the value that QUEUES the event instead
/// of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The qualifier an invocation that names none is addressed to, which is the qualifier the
/// configuration below belongs to.
const UNPUBLISHED: &str = "$LATEST";

/// The `requestContext.condition` of the record of an event Lambda gave up on after every
/// attempt its configuration allowed.
const RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `responseContext.functionError` a run whose handler raised reports.
const UNHANDLED: &str = "Unhandled";

/// The wire error code `PutFunctionEventInvokeConfig` refuses an unreachable destination with
/// (`@httpError(400)`), and the header `restJson1` names an error shape in.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";
/// See [`INVALID_PARAMETER_VALUE`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// How long a wait for work the CLOCK owes goes on — the stack settling, an event's delivery
/// attempt and the record that follows it.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until the thing it waits for is there or this deadline passes, so a slow or loaded host is
/// waited for and only something that never arrives at all fails a row.
const WAIT_DEADLINE: Duration = Duration::from_secs(120);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the deployed function runs: it RAISES for every event, naming the event it was
/// handed in the message it raises with, so the error document the record carries is the
/// handler's own account of THAT event. The event is rendered with no space after a separator,
/// which is the compact form this suite computes the same message in.
const RAISING_HANDLER: &str = r#"
import json


def handler(event, context):
    raise RuntimeError("the handler refused " + json.dumps(event, separators=(",", ":")))
"#;

/// The fixed part of the sentence that handler raises with; the rest is the event itself.
const REFUSED: &str = "the handler refused ";

/// A deployment package for the function the refusal rows configure. Those rows are decided
/// before any invocation, so the bytes are never run: what they have to be is a package
/// `CreateFunction` accepts.
const UNRUN_PACKAGE: &str = "the refusal rows never run this package";

/// The four services this suite exercises, mounted on ONE engine. It needs no other service:
/// Lambda serves the configuration and runs the invocation, Amazon SQS holds the destination,
/// IAM answers for the role, and CloudFormation packages the handler's source — so only those
/// four are mounted, which keeps the chain this suite drives shallow. Request authentication is
/// off, as in every other suite here; the calls this emulator makes to itself sign themselves
/// either way.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call (CloudFormation, IAM) over its own wire.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
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
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// One IAM call. IAM is global; the request is signed for this suite's Region only because
/// that is what routes it.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await
}

/// One awsJson1_0 RPC call to Amazon SQS.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// What one AWS Lambda call answered: the status, the error shape a refusal names in
/// [`ERROR_TYPE_HEADER`], and the body as a document.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

/// One AWS Lambda call over its restJson1 wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Answer {
    let payload = match body {
        Some(document) => serde_json::to_vec(document).expect("the request body serializes"),
        None => Vec::new(),
    };
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer {
        status,
        error_type,
        body,
    }
}

/// The URI that addresses the asynchronous-invocation configuration of one function's
/// unpublished version.
fn config_uri(function: &str) -> String {
    format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config")
}

/// The ARN a queue of `name` carries in this suite's Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN a topic of `name` carries, which one refusal row names as its destination.
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN a function of `name` carries.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The ARN a role of `name` carries. IAM is global, so the ARN carries no Region.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A trust policy that lets AWS Lambda assume the role, which is what an execution role has to
/// carry for Lambda to take it.
fn lambda_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create one queue and answer its URL and its ARN.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the queue {name:?} to be created, got {status} with body \
         {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    (url, queue_arn(name))
}

/// Create one execution role in IAM, granting `granted` on `resources` where a row asks for a
/// grant at all, and answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    granted: &[&str],
    resources: &[String],
) -> String {
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &lambda_trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the execution role to be created in IAM, got {status} with \
         body {body}"
    );
    if !granted.is_empty() {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": granted,
                "Resource": resources,
            }]
        })
        .to_string();
        let (status, body) = iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "destination-writes"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the inline policy to be written, got {status} with body \
             {body}"
        );
    }
    role_arn(name)
}

/// Deploy the one stack this suite's delivery rows share: a function on `role` whose inline
/// source CloudFormation archives into its deployment package, so the handler really runs.
async fn given_deployed_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
    role: &str,
) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Fn": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role,
                    "Timeout": 30,
                    "Code": { "ZipFile": RAISING_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy the function, got {described}"
    );
}

/// Every DISTINCT message on the queue `url` addresses, read off it until there are at least
/// `wanted` of them or [`WAIT_DEADLINE`] passes.
///
/// Each message received is deleted and kept under its own `MessageId`, so a message the queue
/// makes visible again while this poll is waiting is not counted twice: what is being asserted
/// is one record per event, which only distinct records can answer.
async fn awaited(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    loop {
        let (status, received) = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ReceiveMessage, got {status} with body \
             {received}"
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let id = message["MessageId"].as_str().unwrap_or_default().to_string();
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                let (status, answered) = sqs(
                    router,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from DeleteMessage, got {status} with body \
                     {answered}"
                );
            }
            held.insert(id, message);
        }
        if held.len() >= wanted {
            return held.into_values().collect();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {wanted} invocation record(s) on {url} within \
             {WAIT_DEADLINE:?}, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 1: each failed event's record is that event's own
// ---------------------------------------------------------------------------

/// One row of the delivery table: the event one `Invoke` carries, and the text the handler's
/// own error document has to name it by.
struct EventRow {
    case: &'static str,
    /// The payload the row invokes the function with.
    event: Value,
    /// The text that payload puts in the handler's message, computed from the payload here.
    named_by: String,
}

/// The rows differ along the FORM of the payload an asynchronous `Invoke` carries — every form
/// a JSON document takes — because the rule is about the record being the EVENT's own whatever
/// the event is.
fn event_rows() -> Vec<EventRow> {
    [
        (
            "an event that is an object naming itself by an id",
            json!({ "id": "c1" }),
        ),
        (
            "an event that is an object carrying a nested document",
            json!({ "detail": { "id": "c2" } }),
        ),
        ("an event that is an array", json!(["c3", 3])),
        ("an event that is a bare string", json!("c4")),
        ("an event that is a bare number", json!(5)),
    ]
    .into_iter()
    .map(|(case, event)| EventRow {
        case,
        named_by: serde_json::to_string(&event).expect("the payload serializes"),
        event,
    })
    .collect()
}

#[tokio::test]
async fn each_failed_event_delivers_its_own_record_to_the_configured_queue() {
    let router = build_router();
    let setup = "the configuration every event below is delivered under";
    let function = "async-failure-records-fn";
    let (url, arn) = given_queue(&router, setup, "async-failure-records").await;
    let role = given_role(
        &router,
        setup,
        "async-failure-records-exec",
        &["sqs:SendMessage"],
        std::slice::from_ref(&arn),
    )
    .await;
    given_deployed_function(
        &router,
        setup,
        "async-failure-records-stack",
        function,
        &role,
    )
    .await;

    // No retries, so each event has exactly one delivery attempt and the record's
    // `approximateInvokeCount` is a number this suite states rather than reads back.
    let configured = lambda(
        &router,
        "PUT",
        &config_uri(function),
        Some(&json!({
            "DestinationConfig": { "OnFailure": { "Destination": arn } },
            "MaximumRetryAttempts": 0,
        })),
        &[],
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "case {setup:?}: expected PutFunctionEventInvokeConfig to be accepted, got {} with body \
         {}",
        configured.status, configured.body
    );
    // The configuration the asynchronous path reads is the STORED one, so it is read back
    // through the operation that reports it rather than taken from the put's own answer.
    let held = lambda(&router, "GET", &config_uri(function), None, &[]).await;
    assert_eq!(
        held.status, 200,
        "case {setup:?}: expected Lambda to hold the configuration, got {} with body {}",
        held.status, held.body
    );
    assert_eq!(
        held.body["DestinationConfig"]["OnFailure"]["Destination"].as_str(),
        Some(arn.as_str()),
        "case {setup:?}: expected the on-failure destination to be the queue, got {}",
        held.body
    );
    assert_eq!(
        held.body["MaximumRetryAttempts"].as_i64(),
        Some(0),
        "case {setup:?}: expected the configured retry count, got {}",
        held.body
    );

    let rows = event_rows();
    for row in &rows {
        let answered = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            Some(&row.event),
            &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
        )
        .await;
        assert_eq!(
            answered.status, 202,
            "case {:?}: expected an asynchronous Invoke to be accepted, got {} with body {}",
            row.case, answered.status, answered.body
        );
    }

    // One record per event, whichever order the attempts settled in.
    let messages = awaited(&router, "every event of the table", &url, rows.len()).await;
    assert_eq!(
        messages.len(),
        rows.len(),
        "expected exactly one record per event, got {messages:?}"
    );
    let records: Vec<Value> = messages
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("a record on the destination queue is not a document: {error} in {body}")
            })
        })
        .collect();

    let mut request_ids: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let case = row.case;
        let record = records
            .iter()
            .find(|record| record.pointer("/requestPayload") == Some(&row.event))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a record whose requestPayload is this event's own \
                     ({}), got {records:?}",
                    row.event
                )
            });
        // (the member the record carries, the value this row says it has to carry)
        let stated: &[(&str, Value)] = &[
            ("/version", json!("1.0")),
            (
                // The record names the function QUALIFIED by the qualifier the event was
                // addressed to, because the configuration it was written for is held per
                // qualifier.
                "/requestContext/functionArn",
                json!(format!("{}:{UNPUBLISHED}", function_arn(function))),
            ),
            ("/requestContext/condition", json!(RETRIES_EXHAUSTED)),
            ("/requestContext/approximateInvokeCount", json!(1)),
            ("/responseContext/statusCode", json!(200)),
            ("/responseContext/executedVersion", json!(UNPUBLISHED)),
            ("/responseContext/functionError", json!(UNHANDLED)),
        ];
        for (pointer, expected) in stated {
            let reported = record.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {record}"
            );
        }
        // The handler's OWN error document, which names the event it was handed: that is what
        // tells a client WHICH event this record is about beside its payload.
        let message = record
            .pointer("/responsePayload/errorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message,
            format!("{REFUSED}{}", row.named_by),
            "case {case:?}: expected the handler's own message for this event, got {record}"
        );
        let request_id = record
            .pointer("/requestContext/requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the record to name the request it is about, got {record}"
        );
        assert!(
            request_ids.insert(request_id.clone()),
            "case {case:?}: expected each event's record to name its own request, got \
             {request_id:?} twice"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a destination the role may not write is refused, and nothing is stored
// ---------------------------------------------------------------------------

/// One row of the role table: what the role is granted, which kind of destination the
/// configuration names, and how the call has to answer.
struct RoleRow {
    case: &'static str,
    /// The queue, role and function this row owns.
    slug: &'static str,
    /// Whether the destination is an Amazon SNS topic rather than this row's own queue.
    topic: bool,
    /// The actions the role's inline policy grants, on the destination itself.
    granted: &'static [&'static str],
    /// The call the refusal must name beside the destination the request asked for, or `None`
    /// for a row the configuration is stored for.
    withheld: Option<&'static str>,
}

/// The rows differ along what the role is granted — nothing at all, another service's write,
/// the write the destination itself takes — and along the KIND of destination that write is
/// judged for: a standard queue (`sqs:SendMessage`) and a standard topic (`sns:Publish`), two
/// of the kinds `DestinationConfig` names.
const ROLE_ROWS: &[RoleRow] = &[
    RoleRow {
        case: "a queue destination and a role granted nothing",
        slug: "dest-none",
        topic: false,
        granted: &[],
        withheld: Some("SendMessage"),
    },
    RoleRow {
        case: "a queue destination and a role granted only another service's write",
        slug: "dest-other",
        topic: false,
        granted: &["sns:Publish"],
        withheld: Some("SendMessage"),
    },
    RoleRow {
        case: "a queue destination and a role granted the queue's own write",
        slug: "dest-granted",
        topic: false,
        granted: &["sqs:SendMessage"],
        withheld: None,
    },
    RoleRow {
        case: "a topic destination and a role granted only the queue's write",
        slug: "dest-topic",
        topic: true,
        granted: &["sqs:SendMessage"],
        withheld: Some("Publish"),
    },
];

/// The sentence Lambda refuses a configuration whose execution role may not make the write that
/// delivers a record to the named destination with — fixed text, with only the call and the
/// DESTINATION it would have been made on, by the ARN the request named, varying with the
/// request.
fn refusal_sentence(withheld: &str, destination: &str) -> String {
    format!(
        "The function execution role does not have permissions to call {withheld} on \
         {destination}"
    )
}

#[tokio::test]
async fn a_destination_the_execution_role_cannot_write_is_refused_and_nothing_is_stored() {
    let router = build_router();
    for row in ROLE_ROWS {
        let case = row.case;
        let function = format!("{}-fn", row.slug);
        // The row's own queue exists whichever destination it configures, so the grant it
        // states is a grant on a resource this emulator really holds.
        let (_, queue) = given_queue(&router, case, &format!("{}-records", row.slug)).await;
        let destination = if row.topic {
            topic_arn(&format!("{}-records", row.slug))
        } else {
            queue.clone()
        };
        let role = given_role(
            &router,
            case,
            &format!("{}-exec", row.slug),
            row.granted,
            std::slice::from_ref(&queue),
        )
        .await;
        let created = lambda(
            &router,
            "POST",
            FUNCTIONS_URI,
            Some(&json!({
                "FunctionName": function,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role,
                "Code": { "ZipFile": ::aws_smithy_types::base64::encode(UNRUN_PACKAGE) },
            })),
            &[],
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected the function to be created, got {} with body {}",
            created.status, created.body
        );

        let configured = lambda(
            &router,
            "PUT",
            &config_uri(&function),
            Some(&json!({
                "DestinationConfig": { "OnFailure": { "Destination": destination } },
            })),
            &[],
        )
        .await;
        let Some(withheld) = row.withheld else {
            assert_eq!(
                configured.status, 200,
                "case {case:?}: expected the configuration to be stored, got {} with body {}",
                configured.status, configured.body
            );
            let held = lambda(&router, "GET", &config_uri(&function), None, &[]).await;
            assert_eq!(
                held.status, 200,
                "case {case:?}: expected Lambda to report the stored configuration, got {} with \
                 body {}",
                held.status, held.body
            );
            assert_eq!(
                held.body["DestinationConfig"]["OnFailure"]["Destination"].as_str(),
                Some(destination.as_str()),
                "case {case:?}: expected Lambda to hold the configured destination, got {}",
                held.body
            );
            continue;
        };
        assert_eq!(
            configured.status, 400,
            "case {case:?}: expected the configuration to be refused, got {} with body {}",
            configured.status, configured.body
        );
        assert!(
            configured.error_type.contains(INVALID_PARAMETER_VALUE),
            "case {case:?}: expected {INVALID_PARAMETER_VALUE} in {ERROR_TYPE_HEADER}, got {:?} \
             with body {}",
            configured.error_type,
            configured.body
        );
        let sentence = refusal_sentence(withheld, &destination);
        assert_eq!(
            configured.body["message"].as_str(),
            Some(sentence.as_str()),
            "case {case:?}: expected the refusal {sentence:?}, got {}",
            configured.body
        );
        // Nothing was persisted: the configuration the call refused is not one a later read
        // reports.
        let held = lambda(&router, "GET", &config_uri(&function), None, &[]).await;
        assert_eq!(
            held.status, 404,
            "case {case:?}: expected no configuration to be held for a destination the role \
             cannot write, got {} with body {}",
            held.status, held.body
        );
    }
}
