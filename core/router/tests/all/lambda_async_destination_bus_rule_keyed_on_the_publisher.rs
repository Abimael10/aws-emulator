// SPDX-License-Identifier: CC0-1.0

//! What an asynchronous invocation leaves on an Amazon EventBridge EVENT BUS named as its
//! destination, and how a configuration naming a bus the execution role may not publish onto
//! is refused — CloudFormation, AWS Lambda, Amazon EventBridge, Amazon SQS and IAM on ONE
//! engine.
//!
//! The rules under test:
//!
//! 1. **An asynchronous invoke whose qualifier's `DestinationConfig` names an event bus
//!    publishes the invocation record onto that bus, once per completed event, as an envelope
//!    whose `source` is the PUBLISHING SERVICE and whose `detail-type` states the OUTCOME the
//!    event reached** — `DestinationConfig$OnSuccess` is "the destination configuration for
//!    successful invocations" and `DestinationConfig$OnFailure` "the destination configuration
//!    for failed invocations" (`smithy_query(query="shape",shape="DestinationConfig")`) — with
//!    the record itself as the envelope's `detail` and the function it is about in `resources`.
//!    So a rule keyed on the PUBLISHER ALONE receives one envelope per completed event whatever
//!    outcome it reached, and each envelope's detail is THAT event's own record: the payload it
//!    carried, the request it was accepted under, the condition it ended in and the answer the
//!    handler gave for it.
//! 2. **A destination the function's execution role may not write is refused by the call that
//!    CONFIGURES it**, with the modelled `InvalidParameterValueException` (HTTP 400,
//!    `smithy_query(query="operation",operation="PutFunctionEventInvokeConfig")`) and nothing
//!    persisted, in a sentence naming the call Lambda would have made and the DESTINATION it
//!    would have made it on — by the ARN the request carried, since that is the only value that
//!    tells a configuration's two destination slots apart.
//!
//! Five crates have to agree for those to hold (CloudFormation archives the handler's source
//! into the function's deployment package, IAM answers for the execution role, Lambda serves
//! the configuration, judges the role and runs the asynchronous invocation, EventBridge routes
//! the envelope and Amazon SQS holds what the rule's target received), so the suite lives here
//! rather than in any one crate. Its sibling
//! [`crate::lambda_async_failure_record_names_its_own_event`] drives a QUEUE destination, where
//! the record travels on its own; what this one adds is the destination kind that wraps the
//! record in an envelope of its own, read through a rule keyed on nothing but the publisher —
//! so an envelope published under the wrong `source`, or none at all, is an empty queue here.
//!
//! The rows of the first table differ along what rule 1 generalises over: the OUTCOME the event
//! reached (a handler that returned, a handler that raised) and the FORM of what the handler
//! answered (an object, an array, text), since the record carries the response payload beside
//! the condition. The rows of the second differ along the FORM of the event-bus ARN the
//! configuration names (the account's default bus, a custom bus, a bus of another Region, a bus
//! of another account) and along what the role is granted, because the sentence carries the ARN
//! through whatever it is.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::form_encode;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region every resource below belongs
/// to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `StackStatus` the one stack of this suite must reach before its function is invocable.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where `PutFunctionEventInvokeConfig` and `GetFunctionEventInvokeConfig` are bound:
/// `/2019-09-25/functions/{FunctionName}/event-invoke-config`.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The header `Invoke$InvocationType` is bound to, and the value that QUEUES the event instead
/// of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The qualifier an invocation that names none is addressed to, which is the qualifier the
/// configuration below belongs to and the one the record's function ARN names.
const UNPUBLISHED: &str = "$LATEST";

/// The values the envelope carrying an invocation record has on the WIRE, written out here
/// rather than imported from the emulator so a change on either side of them fails a row.
///
/// Source: AWS's published asynchronous-invocation destination envelope — the `source` Lambda
/// publishes a record under, and the `detail-type` of each of the two outcomes.
const RECORD_SOURCE: &str = "lambda";
/// See [`RECORD_SOURCE`].
const SUCCESS_DETAIL_TYPE: &str = "Lambda Function Invocation Result - Success";
/// See [`RECORD_SOURCE`].
const FAILURE_DETAIL_TYPE: &str = "Lambda Function Invocation Result - Failure";

/// The schema version an invocation record reports.
const RECORD_VERSION: &str = "1.0";

/// The `requestContext.condition` the record of each outcome reports: an event whose handler
/// returned, and one Lambda gave up on after every attempt its configuration allowed.
const CONDITION_SUCCESS: &str = "Success";
/// See [`CONDITION_SUCCESS`].
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `MaximumRetryAttempts` the configuration states: none, the floor of the member's own
/// `@range(min: 0, max: 2)`. So an event whose handler raises is one Lambda gives up on after
/// its single attempt, which makes the failure record due as soon as that attempt is over.
const NO_RETRIES: i64 = 0;

/// The IAM action publishing onto a bus performs, which the execution role of the delivery test
/// is granted on the bus the configuration names.
const PUT_EVENTS: &str = "events:PutEvents";

/// The call the refusal names for a bus destination, which is [`PUT_EVENTS`] without its
/// namespace.
const PUT_EVENTS_CALL: &str = "PutEvents";

/// The service principal EventBridge delivers to an Amazon SQS target as, which the queue's own
/// policy has to grant ("for Lambda functions, Amazon SNS topics and Amazon SQS queues,
/// EventBridge relies on resource-based policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The wire error code `PutFunctionEventInvokeConfig` refuses an unreachable destination with
/// (`@httpError(400)`), and the header `restJson1` names an error shape in.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";
/// See [`INVALID_PARAMETER_VALUE`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// How long a wait for work the CLOCK owes goes on — the stack settling, an event's delivery
/// attempt, the publication that follows it and the rule's own handover.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until the thing it waits for is there or this deadline passes, so a slow or loaded host is
/// waited for and only something that never arrives at all fails a row.
const WAIT_DEADLINE: Duration = Duration::from_secs(180);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the deployed function runs: it RAISES when its event asks it to, naming the
/// event's own marker in the message it raises with, and otherwise returns whatever the event
/// names as its answer — so both the outcome and the FORM of the response payload are the
/// event's own doing.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    if event.get("fail"):
        raise RuntimeError("the handler refused " + str(event.get("mark")))
    return event.get("answer")
"#;

/// The message that handler raises for the event `mark` marks, which is what the record of a
/// failed event must carry as its response payload.
fn refusal_of(mark: &str) -> String {
    format!("the handler refused {mark}")
}

/// The five services this suite exercises, mounted on ONE engine as `core/router/src/lib.rs`
/// mounts them. Request authentication is off, as in the sibling suites here; the calls this
/// emulator makes to itself sign themselves either way.
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
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
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

/// One awsJson RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// One Amazon EventBridge call that must be served.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call that must be served.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
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

/// The URI addressing the asynchronous-invocation configuration of one function's unpublished
/// version.
fn config_uri(function: &str) -> String {
    format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config")
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

/// Create the event bus `name` through EventBridge's own `CreateEventBus`, and answer the ARN
/// that service reports for it — so the destination a configuration names is a bus this
/// emulator really holds.
async fn given_bus(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = events(router, case, "CreateEventBus", &json!({ "Name": name })).await;
    created["EventBusArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EventBusArn in {created}"))
        .to_string()
}

/// Create a queue whose own `Policy` admits EventBridge's deliveries, and answer its url and
/// the ARN the queue itself reports.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule on `bus` keyed on the PUBLISHER alone — nothing but the `source` an
/// invocation record is published under — with `queue_arn` as its only target.
///
/// Keyed on nothing else on purpose: the rule then matches every record the bus receives,
/// whatever outcome it is about, so the queue behind it is where "one envelope per completed
/// event" is read.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    name: &str,
    queue_arn: &str,
) {
    let pattern = json!({ "source": [RECORD_SOURCE] }).to_string();
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": bus,
            "Targets": [{ "Id": "sink", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targeted}"
    );
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
                    "Code": { "ZipFile": HANDLER_SOURCE },
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
/// makes visible again while this poll is waiting is not counted twice: what is asserted is one
/// envelope per event, which only distinct envelopes can answer.
async fn awaited(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    loop {
        for (id, message) in drained(router, case, url).await {
            held.insert(id, message);
        }
        if held.len() >= wanted {
            // One more drain once the count is reached, so an EXTRA envelope published beside
            // the ones expected is read here and counted rather than left behind on the queue:
            // the assertion the caller makes is one envelope per event, and a surplus has to
            // fail it.
            let surplus = drained(router, case, url).await;
            for (id, message) in surplus {
                held.insert(id, message);
            }
            return held.into_values().collect();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {wanted} envelope(s) on {url} within {WAIT_DEADLINE:?}, got \
             {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every message waiting on the queue `url` addresses right now, taken off it, under its own
/// `MessageId`.
async fn drained(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeMap<String, Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let mut taken: BTreeMap<String, Value> = BTreeMap::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let id = message["MessageId"].as_str().unwrap_or_default().to_string();
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
        taken.insert(id, message);
    }
    taken
}

// ---------------------------------------------------------------------------
// Rule 1: one envelope per completed event, stating its outcome
// ---------------------------------------------------------------------------

/// One row of the delivery table: the event one `Invoke` carries, the outcome it reaches, and
/// what the envelope carrying its record must say.
struct EventRow {
    case: &'static str,
    /// The marker this row's event carries, which is what finds its envelope again.
    mark: &'static str,
    /// Whether the event asks the handler to RAISE.
    fails: bool,
    /// What the handler answers when it does not raise — an object, an array, or text.
    answer: Value,
    /// The `detail-type` the envelope carrying this row's record must have.
    detail_type: &'static str,
    /// The `requestContext.condition` that record must report.
    condition: &'static str,
}

/// The rows differ along the OUTCOME the event reached — which is what chooses both the
/// destination slot and the envelope's `detail-type` — and, for the outcomes that returned,
/// along the FORM of what the handler answered.
fn event_rows() -> Vec<EventRow> {
    vec![
        EventRow {
            case: "a handler that returns an object",
            mark: "object",
            fails: false,
            answer: json!({ "handled": true, "items": 2 }),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        EventRow {
            case: "a handler that returns an array",
            mark: "array",
            fails: false,
            answer: json!([1, 2, 3]),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        EventRow {
            case: "a handler that returns text",
            mark: "text",
            fails: false,
            answer: json!("handled"),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        EventRow {
            case: "a handler that raises",
            mark: "raised",
            fails: true,
            answer: Value::Null,
            detail_type: FAILURE_DETAIL_TYPE,
            condition: CONDITION_RETRIES_EXHAUSTED,
        },
    ]
}

/// The event one row invokes the function with.
fn event_of(row: &EventRow) -> Value {
    json!({ "mark": row.mark, "fail": row.fails, "answer": row.answer })
}

#[tokio::test]
async fn each_completed_event_publishes_one_record_envelope_onto_the_destination_bus() {
    let router = build_router();
    let setup = "the bus, rule and configuration every event below is delivered under";
    let function = "async-bus-records-fn";
    let bus_name = "async-bus-records";

    // ONE bus, named as the destination of BOTH slots, with ONE rule on it keyed on the
    // publisher alone: every record the invocations publish then reaches the same queue, so a
    // missing publication is a short queue and a mis-sourced one is an empty queue.
    let bus = given_bus(&router, setup, bus_name).await;
    let (url, queue_arn) = given_queue(&router, setup, "async-bus-records-sink").await;
    given_rule(
        &router,
        setup,
        bus_name,
        "async-bus-records-by-publisher",
        &queue_arn,
    )
    .await;

    // The record is published ACTING AS the function's execution role, so that role is granted
    // exactly the publication onto the bus the configuration names.
    let role = given_role(
        &router,
        setup,
        "async-bus-records-exec",
        &[PUT_EVENTS],
        std::slice::from_ref(&bus),
    )
    .await;
    given_deployed_function(&router, setup, "async-bus-records-stack", function, &role).await;

    let configured = lambda(
        &router,
        "PUT",
        &config_uri(function),
        Some(&json!({
            "MaximumRetryAttempts": NO_RETRIES,
            "DestinationConfig": {
                "OnSuccess": { "Destination": bus },
                "OnFailure": { "Destination": bus },
            },
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
    for slot in ["OnSuccess", "OnFailure"] {
        assert_eq!(
            held.body["DestinationConfig"][slot]["Destination"].as_str(),
            Some(bus.as_str()),
            "case {setup:?}: expected the {slot} destination to be the bus, got {}",
            held.body
        );
    }
    assert_eq!(
        held.body["MaximumRetryAttempts"].as_i64(),
        Some(NO_RETRIES),
        "case {setup:?}: expected the configured retry count, got {}",
        held.body
    );

    let rows = event_rows();
    for row in &rows {
        let answered = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            Some(&event_of(row)),
            &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
        )
        .await;
        assert_eq!(
            answered.status, 202,
            "case {:?}: expected an asynchronous Invoke to be accepted, got {} with body {}",
            row.case, answered.status, answered.body
        );
    }

    // One envelope per completed event, whichever order the attempts settled in.
    let messages = awaited(&router, "every event of the table", &url, rows.len()).await;
    assert_eq!(
        messages.len(),
        rows.len(),
        "expected exactly one envelope per event, got {messages:?}"
    );
    let envelopes: Vec<Value> = messages
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("an envelope on the rule's target queue is not a document: {error} in {body}")
            })
        })
        .collect();

    let qualified = format!("{}:{UNPUBLISHED}", function_arn(function));
    let mut request_ids: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let case = row.case;
        let event = event_of(row);
        let envelope = envelopes
            .iter()
            .find(|envelope| envelope.pointer("/detail/requestPayload") == Some(&event))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an envelope whose detail.requestPayload is this \
                     event's own ({event}), got {envelopes:?}"
                )
            });
        // (the key of the envelope, the value this row says it has to carry)
        let stated: &[(&str, Value)] = &[
            ("/source", json!(RECORD_SOURCE)),
            ("/detail-type", json!(row.detail_type)),
            ("/resources", json!([qualified.clone()])),
            ("/detail/version", json!(RECORD_VERSION)),
            ("/detail/requestContext/condition", json!(row.condition)),
            ("/detail/requestContext/approximateInvokeCount", json!(1)),
            ("/detail/requestContext/functionArn", json!(qualified)),
        ];
        for (pointer, expected) in stated {
            let reported = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {envelope}"
            );
        }
        if row.fails {
            // The handler's OWN error document, which names the event it was handed.
            let message = envelope
                .pointer("/detail/responsePayload/errorMessage")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                message,
                refusal_of(row.mark),
                "case {case:?}: expected the record to carry the handler's own failure, got \
                 {envelope}"
            );
        } else {
            let answered = envelope
                .pointer("/detail/responsePayload")
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(
                answered, row.answer,
                "case {case:?}: expected the record to carry what the handler answered ({}), got \
                 {envelope}",
                row.answer
            );
        }
        let request_id = envelope
            .pointer("/detail/requestContext/requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the record to name the request it is about, got {envelope}"
        );
        assert!(
            request_ids.insert(request_id.clone()),
            "case {case:?}: expected each event's record to name its own request, got \
             {request_id:?} twice"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a bus the role may not publish onto is refused, naming that bus
// ---------------------------------------------------------------------------

/// One row of the role table: the event-bus ARN the configuration names, what the role is
/// granted and on what, and whether the call has to be refused.
struct BusRow {
    case: &'static str,
    /// The queue, role and function slug this row owns.
    slug: &'static str,
    /// The destination the configuration names.
    destination: String,
    /// The actions the role's inline policy grants.
    granted: &'static [&'static str],
    /// What those actions are granted ON: this row's destination, or another bus entirely.
    granted_on_destination: bool,
    /// Whether the configuration must be refused rather than stored.
    refused: bool,
}

/// The ARN an event bus of `name` carries in this suite's Region and account.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// A bus of this account that is never the destination under test, which is what a grant
/// naming the WRONG resource names.
fn other_bus_arn() -> String {
    bus_arn("other-records")
}

/// The rows differ along the FORM of the event-bus ARN the configuration names — the account's
/// own default bus, a custom bus, a bus of another Region, a bus of another account — and along
/// what the role is granted: nothing, the publication on ANOTHER bus, or the publication on the
/// bus itself.
fn bus_rows() -> Vec<BusRow> {
    vec![
        BusRow {
            case: "the account's default bus, with the publication granted nowhere",
            slug: "bus-default",
            destination: bus_arn("default"),
            granted: &[],
            granted_on_destination: false,
            refused: true,
        },
        BusRow {
            case: "a custom bus, with the publication granted on another bus",
            slug: "bus-elsewhere",
            destination: bus_arn("async-records"),
            granted: &[PUT_EVENTS],
            granted_on_destination: false,
            refused: true,
        },
        BusRow {
            case: "a bus of another Region",
            slug: "bus-other-region",
            destination: format!("arn:aws:events:eu-west-1:{ACCOUNT}:event-bus/async-records"),
            granted: &[PUT_EVENTS],
            granted_on_destination: false,
            refused: true,
        },
        BusRow {
            case: "a bus of another account",
            slug: "bus-other-account",
            destination: "arn:aws:events:us-east-1:210987654321:event-bus/async-records"
                .to_string(),
            granted: &[PUT_EVENTS],
            granted_on_destination: false,
            refused: true,
        },
        BusRow {
            // The positive control: exactly the publication, on the bus itself.
            case: "a custom bus, with the publication granted on it",
            slug: "bus-granted",
            destination: bus_arn("async-records"),
            granted: &[PUT_EVENTS],
            granted_on_destination: true,
            refused: false,
        },
    ]
}

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
async fn a_bus_destination_the_execution_role_cannot_publish_onto_is_refused_by_its_arn() {
    let router = build_router();
    for row in bus_rows() {
        let case = row.case;
        let function = format!("{}-fn", row.slug);
        let resources = if row.granted_on_destination {
            vec![row.destination.clone()]
        } else {
            vec![other_bus_arn()]
        };
        let role = given_role(
            &router,
            case,
            &format!("{}-exec", row.slug),
            row.granted,
            &resources,
        )
        .await;
        // The refusal is decided before any invocation, so these functions never run: what the
        // package has to be is one `CreateFunction` accepts.
        let created = lambda(
            &router,
            "POST",
            FUNCTIONS_URI,
            Some(&json!({
                "FunctionName": function,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role,
                "Code": {
                    "ZipFile": ::aws_smithy_types::base64::encode(
                        "the refusal rows never run this package",
                    ),
                },
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
                "DestinationConfig": { "OnSuccess": { "Destination": row.destination } },
            })),
            &[],
        )
        .await;
        if !row.refused {
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
                held.body["DestinationConfig"]["OnSuccess"]["Destination"].as_str(),
                Some(row.destination.as_str()),
                "case {case:?}: expected Lambda to hold the configured destination, got {}",
                held.body
            );
            continue;
        }
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
        let sentence = refusal_sentence(PUT_EVENTS_CALL, &row.destination);
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
