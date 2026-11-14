// SPDX-License-Identifier: CC0-1.0

//! What an asynchronous invocation's record looks like when its destination is an Amazon
//! EventBridge EVENT BUS, end to end over the wire with Lambda, EventBridge, Amazon SQS, IAM
//! and the token service on ONE engine.
//!
//! RULE: a destination slot is chosen by the OUTCOME the event reached —
//! `DestinationConfig$OnSuccess` is "the destination configuration for successful invocations"
//! and `DestinationConfig$OnFailure` "the destination configuration for failed invocations" —
//! and an event-bus destination carries the invocation record inside an EventBridge envelope,
//! so that envelope STATES the outcome: it is published by `lambda` with a `detail-type` of
//! `Lambda Function Invocation Result - Success` for an event whose handler returned and
//! `Lambda Function Invocation Result - Failure` for one Lambda gave up on, naming the
//! function the record is about in `resources` and carrying the record itself as `detail`.
//! Those are the keys EventBridge matches a pattern on, so ONE bus wired as both slots'
//! destination still routes each outcome to the targets of the rule keyed on it, and a rule
//! keyed on the other outcome receives nothing.
//!
//! That is why the suite is driven through two RULES on one bus, each with a queue of its own:
//! a record that reached the right queue is a record whose envelope carried the right
//! `detail-type`, read exactly where a client reads it. A record delivered with the class
//! alone would reach both rules — or neither — which is what each row's negative half pins.
//!
//! The write that publishes the record is made ACTING AS the function's execution role, so the
//! role here is a real IAM record granted `events:PutEvents` on the bus the configuration
//! names: the publication EventBridge serves is the one those policies admit.
//!
//! The rows differ along what the rule generalises over: the outcome the event reached (a
//! handler that returned, a handler that raised) and the FORM of what the handler answered (an
//! object, a list, text that is no document), since the record carries the response payload
//! beside the condition.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// The Region every call below is made in, and the Region every ARN here carries.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `PutFunctionEventInvokeConfig` is bound.
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The request header `Invoke$InvocationType` is bound to, and the value that QUEUES the event
/// instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The runtime the function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function, its execution role and the bus every record is published onto.
const FUNCTION_NAME: &str = "async-destination-bus";
const ROLE_NAME: &str = "async-destination-bus-exec";
const BUS_NAME: &str = "async-invocation-records";

/// The values the envelope an invocation record travels in carries on the WIRE, written out
/// here rather than imported from the emulator so a change to either side of them fails a row.
///
/// Source: AWS's published asynchronous-invocation destination envelope — the `source` Lambda
/// publishes a record under, and the `detail-type` of each of the two outcomes.
const RECORD_SOURCE: &str = "lambda";
/// See [`RECORD_SOURCE`].
const SUCCESS_DETAIL_TYPE: &str = "Lambda Function Invocation Result - Success";
/// See [`RECORD_SOURCE`].
const FAILURE_DETAIL_TYPE: &str = "Lambda Function Invocation Result - Failure";

/// The `requestContext.condition` the record of each outcome reports.
const CONDITION_SUCCESS: &str = "Success";
/// See [`CONDITION_SUCCESS`].
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The schema version of an invocation record.
const RECORD_SCHEMA_VERSION: &str = "1.0";

/// The unpublished version, as the model spells it: the qualifier an event addressed to none
/// belongs to, and so the one the record's function ARN names.
const LATEST: &str = "$LATEST";

/// The `MaximumRetryAttempts` the configuration states: none, the floor of the member's own
/// `@range(min: 0, max: 2)`. So an event that fails is one Lambda gives up on after its one
/// attempt, which is what makes the failure record due as soon as that attempt is over.
const NO_RETRIES: i64 = 0;

/// The action publishing onto a bus performs, which the execution role's policy grants.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The service principal EventBridge delivers to a queue as, which the queue's own policy has
/// to grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
/// relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// How long a row waits for work the CLOCK owes — a function reaching `Active`, the delivery
/// attempt of an accepted event, the record that follows it and the rule's own handover.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes, so a loaded host is waited for and only
/// something that never arrives fails a row.
const BUDGET: Duration = Duration::from_secs(120);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every row runs: it RAISES when its event asks it to and otherwise returns
/// whatever the event names as its answer, so both the outcome and the FORM of the response
/// payload are the event's own doing.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    if event.get("fail"):
        raise RuntimeError("the handler refused " + str(event.get("mark")))
    return event.get("answer")
"#;

/// The message the handler raises for the event `mark` marks, which is what a failure record's
/// response payload must carry.
fn refusal_of(mark: &str) -> String {
    format!("the handler refused {mark}")
}

/// Lambda, EventBridge, Amazon SQS, IAM and the token service on ONE engine, as
/// `core/router/src/lib.rs` mounts them: the record is published through the seam the
/// invocation carries, so the bus it reaches is this emulator's own and the session it is
/// published as is one the services below resolve.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda control-plane or data-plane call, the way a client makes it.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(
            builder
                .body(bytes::Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(response.body()).to_string())
        })
    };
    (status, parsed)
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`, which must
/// be served.
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
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(response.body()).to_string())
        })
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call that must be served.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call that must be served.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One awsQuery call to IAM that must be served.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut form = form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", "2010-05-08");
    for (name, value) in parameters {
        form.append_pair(name, value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(form.finish()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} {answered}"
    );
}

/// The ARN of the execution role, as IAM holds it.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE_NAME}",
        engine::emulator_account_id()
    )
}

/// The execution role: trusted by Lambda, so a function may run as it, and granted the
/// publication onto `bus_arn` that delivering a record to that bus takes. Answers its ARN.
async fn given_role(router: &MultiServiceRouter, case: &str, bus_arn: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": PUT_EVENTS_ACTION,
            "Resource": bus_arn,
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "publishes-records"),
            ("PolicyDocument", &grant),
        ],
    )
    .await;
    role_arn()
}

/// A queue of this account whose own `Policy` admits EventBridge's deliveries; answers its url
/// and its ARN, both as the queue itself reports them.
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

/// An ENABLED rule on `bus` keyed on `detail_type` — on the `source` Lambda publishes a record
/// under and on that one outcome — with `queue_arn` as its only target.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    name: &str,
    detail_type: &str,
    queue_arn: &str,
) {
    let pattern =
        json!({ "source": [RECORD_SOURCE], "detail-type": [detail_type] }).to_string();
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
            "Targets": [{ "Id": "t1", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targeted}"
    );
}

/// Wait until `name` reports `State` `Active`, which is the move the clock owes after a create
/// and what every invocation of it waits for.
async fn await_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_status, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
            &[],
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name:?} to reach Active within {BUDGET:?}, last read \
             {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The envelope of the first message waiting on the queue at `url`, taken off it; the wait is
/// on the message, not on a span.
async fn awaited_envelope(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(envelope) = received_envelope(router, case, url).await {
            return envelope;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one message on {url} within {BUDGET:?}, got none"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The envelope of one message waiting on the queue at `url` right now, or `None` when it holds
/// none.
async fn received_envelope(router: &MultiServiceRouter, case: &str, url: &str) -> Option<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let body = messages.first()?["Body"].as_str().unwrap_or_default().to_string();
    Some(serde_json::from_str(&body).unwrap_or(Value::String(body)))
}

/// One row: the event the handler is given, the outcome it reaches, and what the record of it
/// must report.
struct Row {
    case: &'static str,
    /// Whether the event asks the handler to RAISE.
    fails: bool,
    /// What the handler answers when it does not raise — an object, a list, or text.
    answer: fn() -> Value,
    /// The `detail-type` the envelope carrying the record must have.
    detail_type: &'static str,
    /// The `requestContext.condition` the record must report.
    condition: &'static str,
}

/// The rows differ along the outcome the event reached — which is what chooses both the
/// destination slot and the envelope's `detail-type` — and, for the outcomes that returned,
/// along the FORM of what the handler answered.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handler that returns an object",
            fails: false,
            answer: || json!({ "handled": true, "items": 2 }),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        Row {
            case: "a handler that returns a list",
            fails: false,
            answer: || json!([1, 2, 3]),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        Row {
            case: "a handler that returns text",
            fails: false,
            answer: || json!("handled"),
            detail_type: SUCCESS_DETAIL_TYPE,
            condition: CONDITION_SUCCESS,
        },
        Row {
            case: "a handler that raises",
            fails: true,
            answer: || Value::Null,
            detail_type: FAILURE_DETAIL_TYPE,
            condition: CONDITION_RETRIES_EXHAUSTED,
        },
    ]
}

/// RULE: the envelope an invocation record is published to an event bus in states the OUTCOME
/// in its `detail-type`, so the rule keyed on that outcome receives the record and the rule
/// keyed on the other one receives nothing.
#[tokio::test]
async fn an_invocation_record_on_a_bus_states_its_outcome_in_the_envelope() {
    let case = "the suite's own wiring";
    let router = router();
    // ONE bus, wired as the destination of BOTH slots, with a rule per outcome on it: which
    // queue a record lands on is then decided by the envelope alone.
    let created = events(&router, case, "CreateEventBus", &json!({ "Name": BUS_NAME })).await;
    let bus_arn = created["EventBusArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no EventBusArn in {created}"))
        .to_string();
    let (success_url, success_queue) = given_queue(&router, case, "records-success").await;
    let (failure_url, failure_queue) = given_queue(&router, case, "records-failure").await;
    given_rule(
        &router,
        case,
        BUS_NAME,
        "records-success-rule",
        SUCCESS_DETAIL_TYPE,
        &success_queue,
    )
    .await;
    given_rule(
        &router,
        case,
        BUS_NAME,
        "records-failure-rule",
        FAILURE_DETAIL_TYPE,
        &failure_queue,
    )
    .await;

    let role = given_role(&router, case, &bus_arn).await;
    let (status, created) = lambda(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION_NAME,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(HANDLER_SOURCE)),
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} with {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string();
    await_active(&router, case, FUNCTION_NAME).await;

    let (status, configured) = lambda(
        &router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{FUNCTION_NAME}/event-invoke-config"),
        &json!({
            "MaximumRetryAttempts": NO_RETRIES,
            "DestinationConfig": {
                "OnSuccess": { "Destination": bus_arn },
                "OnFailure": { "Destination": bus_arn },
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutFunctionEventInvokeConfig, got {status} with \
         {configured}"
    );

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let mark = format!("{index}");
        let event = json!({ "mark": mark, "fail": row.fails, "answer": (row.answer)() });
        let (status, accepted) = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION_NAME}/invocations"),
            &event,
            &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected an asynchronous Invoke to be accepted, got {status} with \
             {accepted}"
        );

        let (wanted, other) = if row.fails {
            (&failure_url, &success_url)
        } else {
            (&success_url, &failure_url)
        };
        let envelope = awaited_envelope(&router, case, wanted).await;
        assert_eq!(
            envelope["source"].as_str(),
            Some(RECORD_SOURCE),
            "case {case:?}: expected the record to be published by {RECORD_SOURCE:?}, got \
             {envelope}"
        );
        assert_eq!(
            envelope["detail-type"].as_str(),
            Some(row.detail_type),
            "case {case:?}: expected the envelope to state the outcome as {:?}, got {envelope}",
            row.detail_type
        );
        // The function the record is about, qualified by the version that ran, is what a
        // pattern keyed on one function matches.
        assert_eq!(
            envelope["resources"],
            json!([format!("{function_arn}:{LATEST}")]),
            "case {case:?}: expected the envelope to name the function the record is about, got \
             {envelope}"
        );
        let detail = &envelope["detail"];
        // (the member of the record, what it must carry)
        let stated: &[(&str, Value)] = &[
            ("/version", json!(RECORD_SCHEMA_VERSION)),
            ("/requestContext/condition", json!(row.condition)),
            ("/requestContext/approximateInvokeCount", json!(1)),
            (
                "/requestContext/functionArn",
                json!(format!("{function_arn}:{LATEST}")),
            ),
            ("/requestPayload", event.clone()),
        ];
        for (pointer, expected) in stated {
            let reported = detail.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected the record's {pointer} to be {expected}, got {detail}"
            );
        }
        if row.fails {
            let message = detail
                .pointer("/responsePayload/errorMessage")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                message,
                refusal_of(&mark),
                "case {case:?}: expected the record to carry the handler's own failure, got \
                 {detail}"
            );
        } else {
            assert_eq!(
                detail["responsePayload"],
                (row.answer)(),
                "case {case:?}: expected the record to carry what the handler answered, got \
                 {detail}"
            );
        }

        // The rule keyed on the OTHER outcome saw the same publication and matched nothing: the
        // record that has already reached its own queue is the barrier that makes this an
        // absence rather than a delivery still in flight.
        let leaked = received_envelope(&router, case, other).await;
        assert_eq!(
            leaked, None,
            "case {case:?}: expected the rule keyed on the other outcome to receive nothing, \
             got {leaked:?}"
        );
    }
}
