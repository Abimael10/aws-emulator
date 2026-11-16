// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's Lambda target is invoked ASYNCHRONOUSLY, so the FUNCTION's
//! asynchronous-invocation configuration decides what follows the run — end to end over the
//! wire, with EventBridge, Lambda and Amazon SQS on ONE engine and a real `python3.x` handler
//! that either returns or raises.
//!
//! The RULE pinned here: because a function target is handed its event with the asynchronous
//! invocation type, the completion of that invocation delivers an invocation record to the
//! slot of the function's `DestinationConfig` for the outcome the event reached —
//! `OnSuccess` is "the destination configuration for successful invocations" and `OnFailure`
//! "the destination configuration for failed invocations"
//! (`smithy_query(query="shape", shape="DestinationConfig",
//! model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`)
//! — carrying what EventBridge DELIVERED as `requestPayload`, what the handler answered as
//! `responsePayload`, the condition the event ended on and how many attempts it had. The
//! handler's own failure is Lambda's to retry, never the rule's to dead-letter: the rule's
//! `DeadLetterConfig` queue stays empty whichever way the handler ends, because from
//! EventBridge's side the invocation was accepted.
//!
//! The rows span that class rather than one recorded case. They differ along the outcome the
//! handler reaches (returns, raises), along which slots the configuration states (both, only
//! the one the outcome selects), and along the FORM of what was delivered (the bare
//! EventBridge envelope, the text the target's `InputTransformer` renders) — the last of which
//! is what shows `requestPayload` is the delivered payload itself rather than an envelope
//! rebuilt for the record. Every row states `MaximumRetryAttempts` 0
//! (`FunctionEventInvokeConfig$MaximumRetryAttempts`, `@range(min: 0, max: 2)`), so each event
//! has the one delivery attempt that configuration allows and the attempt count a record
//! reports is a known number; how many attempts a larger allowance gives is the Lambda crate's
//! own rule, read there against the waits AWS publishes between two of them.
//!
//! Only a suite mounting Lambda and Amazon SQS beside EventBridge can pin this: a destination
//! record is written by LAMBDA, after the invocation EventBridge made has completed, so the
//! queue this suite reads is the queue a client reads.
//!
//! `lambda_target_handler_failure_is_not_a_delivery_failure` is the suite for the rule's own
//! side of the same handover — which failures ARE dead-lettered at the rule; what this one
//! adds is the function-side record the asynchronous invocation type makes reachable.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` every event this suite publishes carries, and what each
/// rule matches on.
const SOURCE: &str = "com.demo.async-destination";
const DETAIL_TYPE: &str = "Async Destination Probe";

/// The principal EventBridge invokes a resource-policy-governed target as, and the action an
/// invocation of a function performs — what the grant each row writes has to name.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The id of the one target each rule of a row carries.
const TARGET_ID: &str = "fn";

/// The schema `version` a Lambda invocation record carries, as the LITERAL the documented
/// record holds rather than through the service's own constant.
const RECORD_SCHEMA_VERSION: &str = "1.0";

/// The `requestContext.condition` of the record of an event whose handler RETURNED.
const CONDITION_SUCCESS: &str = "Success";

/// The `requestContext.condition` of the record of an event Lambda gave up on after every
/// delivery attempt its configuration allowed.
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `FunctionError` a run whose handler raised reports, and so the value the record's
/// `responseContext` carries beside the status.
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// The status the `responseContext` of a record of an invocation that RAN reports.
const INVOKED_STATUS: i64 = 200;

/// The qualifier an unqualified asynchronous invocation's record names: the unpublished
/// version, which is the qualifier whose configuration delivered the record.
const UNPUBLISHED_VERSION: &str = "$LATEST";

/// The exception the handler raises, and the sentence it raises it with — the marker of the
/// event being processed is the variable part, so each row looks for its own.
const ERROR_TYPE: &str = "RuntimeError";

fn failure_sentence(marker: &str) -> String {
    format!("the handler refused {marker}")
}

/// The state Lambda reports for a function that may be invoked: "during this time, you can't
/// invoke or modify the function" (`CreateFunction`) is what a created function leaves behind
/// when it reaches this one.
const ACTIVE_STATE: &str = "Active";

/// How long this suite waits for work the clock owes — an asynchronous invocation, the
/// handler's run, the record that follows it.
///
/// A ceiling on PATIENCE and never a span anything here asserts on: every wait below looks
/// again until what it waits for is there or this deadline passes, so a loaded host is waited
/// out and only something that never arrives fails a row.
const WORK_WAIT: Duration = Duration::from_secs(90);

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// EventBridge, Lambda and Amazon SQS over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the invocation EventBridge makes is served by the Lambda of this engine, and
/// the destination that Lambda writes to is a queue of this engine's Amazon SQS.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
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
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A Lambda `restJson1` call, as its status and parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: Value) -> (u16, Value) {
    let payload = if body.is_null() {
        bytes::Bytes::new()
    } else {
        bytes::Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The handler every row deploys. What it DOES is taken from what it was handed: it raises
/// when the payload asks it to and otherwise returns a document of its own, so the record's
/// `responsePayload` is the handler's own answer either way. The payload is the EventBridge
/// envelope for a target that delivers one and the rendered template for a target that
/// transforms the input, so the handler reads its `detail` where there is one and the payload
/// itself where there is not.
const PROBE_SOURCE: &str = r#"
def lambda_handler(event, context):
    asked = event.get("detail", event) if isinstance(event, dict) else {}
    marker = asked.get("marker")
    if asked.get("fail"):
        raise RuntimeError("the handler refused %s" % marker)
    return {"echoed": marker}
"#;

/// A .zip deployment package holding the handler above as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(PROBE_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// The account every emulated resource of this suite belongs to, as the engine holds it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// Create the function the rule targets and answer the ARN Lambda assigned it.
async fn create_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": format!("arn:aws:iam::{}:role/service-role/lambda-exec", account()),
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 20,
            "Code": { "ZipFile": package() },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda CreateFunction, got {status} ({created})"
    );
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
        .to_string()
}

/// Grant this service the invocation of the function through `AddPermission`, which is what a
/// function target's handover is decided by.
async fn add_permission(router: &MultiServiceRouter, case: &str, function: &str) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        json!({
            "StatementId": "events-invoke",
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission, got {status} ({answered})"
    );
}

/// `PutFunctionEventInvokeConfig` on the function's unpublished version, stating the retries
/// `retries` asks for and the destination slots `on_success` and `on_failure` name.
async fn put_event_invoke_config(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    on_success: Option<&str>,
    on_failure: Option<&str>,
    retries: i64,
) {
    let mut destinations = json!({});
    if let Some(destination) = on_success {
        destinations["OnSuccess"] = json!({ "Destination": destination });
    }
    if let Some(destination) = on_failure {
        destinations["OnFailure"] = json!({ "Destination": destination });
    }
    let (status, answered) = lambda(
        router,
        "PUT",
        &format!("/2019-09-25/functions/{function}/event-invoke-config"),
        json!({
            "MaximumRetryAttempts": retries,
            "DestinationConfig": destinations,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Lambda PutFunctionEventInvokeConfig, got \
         {status} ({answered})"
    );
}

/// Wait until Lambda reports the function `Active`, which is the condition a delivery to it
/// needs: "during this time, you can't invoke or modify the function" (`CreateFunction`), so
/// the row looks for the STATE rather than waiting out a span, and a slow host is waited for.
async fn until_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let started = Instant::now();
    loop {
        let (status, described) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}"),
            Value::Null,
        )
        .await;
        let state = described["Configuration"]["State"].as_str().unwrap_or_default();
        if status == 200 && state == ACTIVE_STATE {
            return;
        }
        assert!(
            started.elapsed() < WORK_WAIT,
            "case {case:?}: expected Lambda to report {function} {ACTIVE_STATE}, got HTTP \
             {status} with the state {state:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A queue of this account, as the queue itself reports it: its url and its ARN.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
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
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    (url, arn)
}

/// The queue a rule's target dead-letters to, carrying the policy this service's own send
/// needs; answers its url and its ARN.
async fn rule_dead_letter_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let (url, arn) = queue(router, case, name).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    (url, arn)
}

/// Every message waiting on the queue at `url`, taken off it.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answered = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    answered["Messages"].as_array().cloned().unwrap_or_default()
}

/// Wait until something is on the queue at `url` and answer what was taken off it, looking
/// again until `deadline` has passed.
async fn until_delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    deadline: Duration,
) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let held = drain(router, case, url).await;
        if !held.is_empty() {
            return held;
        }
        if started.elapsed() >= deadline {
            return Vec::new();
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The one record on the queue at `url`, as the document its body holds.
async fn one_record(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    deadline: Duration,
) -> Value {
    let delivered = until_delivered(router, case, url, deadline).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected exactly one invocation record at the destination, got \
         {delivered:?}"
    );
    let body = delivered[0]["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: expected an invocation record document, got {body:?} ({error})")
    })
}

/// Create the rule, wire the function as its only target with the rule's own dead-letter
/// queue, and answer the rule's ARN. `transformed` wires an `InputTransformer`, so the target
/// is handed the rendered template rather than the envelope.
async fn wire_rule(
    router: &MultiServiceRouter,
    case: &str,
    rule_name: &str,
    function_arn: &str,
    dead_letter_arn: &str,
    transformed: bool,
) {
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": rule_name, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
    let mut target = json!({
        "Id": TARGET_ID,
        "Arn": function_arn,
        "RetryPolicy": { "MaximumRetryAttempts": 0 },
        "DeadLetterConfig": { "Arn": dead_letter_arn },
    });
    if transformed {
        target["InputTransformer"] = json!({
            "InputPathsMap": { "marker": "$.detail.marker" },
            "InputTemplate": "{\"marker\": <marker>}",
        });
    }
    let wired = events(
        router,
        case,
        "PutTargets",
        &json!({ "Rule": rule_name, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );
}

/// Publish one matching entry carrying `marker`, asking the handler to raise when `fails`;
/// answers the id EventBridge assigned the event.
async fn publish(router: &MultiServiceRouter, case: &str, marker: &str, fails: bool) -> String {
    let published = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": marker, "fail": fails }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {published}"
    );
    published["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EventId, got {published}"))
        .to_string()
}

/// One row: what the handler comes to, which slots the configuration states, and what form the
/// target delivers.
struct Row {
    case: &'static str,
    /// Whether the handler the event asks for RAISES, which is what selects the slot.
    fails: bool,
    /// Whether the configuration states BOTH slots, so the row also pins that the slot the
    /// outcome did not select receives nothing.
    both_slots: bool,
    /// Whether the target hands the function the template its `InputTransformer` renders
    /// instead of the bare envelope.
    transformed: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handler that returns, both destination slots stated",
            fails: false,
            both_slots: true,
            transformed: false,
        },
        Row {
            case: "a handler that raises, both destination slots stated",
            fails: true,
            both_slots: true,
            transformed: false,
        },
        Row {
            case: "a handler that returns, only the on-success slot stated",
            fails: false,
            both_slots: false,
            transformed: false,
        },
        Row {
            case: "a handler that raises, only the on-failure slot stated",
            fails: true,
            both_slots: false,
            transformed: false,
        },
        Row {
            case: "a target that transforms the input, and a handler that returns",
            fails: false,
            both_slots: true,
            transformed: true,
        },
    ]
}

/// RULE: a Lambda target's invocation is asynchronous, so its completion delivers the
/// invocation record to the slot of the function's own `DestinationConfig` for the outcome the
/// event reached — carrying what was delivered, what the handler answered and the condition
/// the event ended on — and the rule dead-letters nothing either way.
#[tokio::test]
async fn an_eventbridge_lambda_targets_completion_records_at_its_async_destination() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = router();
        let marker = format!("marker-{index}");
        let (success_url, success_arn) =
            queue(&router, case, &format!("async-success-{index}")).await;
        let (failure_url, failure_arn) =
            queue(&router, case, &format!("async-failure-{index}")).await;
        let (dead_letter_url, dead_letter_arn) =
            rule_dead_letter_queue(&router, case, &format!("rule-dlq-{index}")).await;

        let function_name = format!("async-destination-{index}");
        let function_arn = create_function(&router, case, &function_name).await;
        add_permission(&router, case, &function_name).await;
        // The slot the row's outcome selects is always stated; the other one only where the
        // row states both, so a row that states one pins that the record goes to the slot the
        // outcome names and nowhere else.
        let on_success = (row.both_slots || !row.fails).then_some(success_arn.as_str());
        let on_failure = (row.both_slots || row.fails).then_some(failure_arn.as_str());
        // No retry, so the event has exactly one delivery attempt and the record's attempt
        // count is a known number; the retried case is the test below.
        put_event_invoke_config(&router, case, &function_name, on_success, on_failure, 0).await;

        wire_rule(
            &router,
            case,
            &format!("async-destination-rule-{index}"),
            &function_arn,
            &dead_letter_arn,
            row.transformed,
        )
        .await;

        // The created function leaves `Pending` by the clock, and a delivery made before it is
        // `Active` is one the lifecycle refuses, so the row waits for that state.
        until_active(&router, case, &function_name).await;
        let event_id = publish(&router, case, &marker, row.fails).await;

        let (expected_url, other_url) = if row.fails {
            (&failure_url, &success_url)
        } else {
            (&success_url, &failure_url)
        };
        let record = one_record(&router, case, expected_url, WORK_WAIT).await;

        assert_eq!(
            record["version"],
            json!(RECORD_SCHEMA_VERSION),
            "case {case:?}: expected the record schema version {RECORD_SCHEMA_VERSION:?}, got \
             {record}"
        );
        let expected_condition = if row.fails {
            CONDITION_RETRIES_EXHAUSTED
        } else {
            CONDITION_SUCCESS
        };
        let context = &record["requestContext"];
        assert_eq!(
            context["condition"],
            json!(expected_condition),
            "case {case:?}: expected the condition {expected_condition:?}, got {record}"
        );
        assert_eq!(
            context["approximateInvokeCount"],
            json!(1),
            "case {case:?}: expected the one attempt no retry allows, got {record}"
        );
        // The record names the qualifier whose configuration delivered it, which for an
        // unqualified invocation is the unpublished version.
        assert_eq!(
            context["functionArn"],
            json!(format!("{function_arn}:{UNPUBLISHED_VERSION}")),
            "case {case:?}: expected the record to name {function_arn}:{UNPUBLISHED_VERSION}, \
             got {record}"
        );
        assert!(
            context["requestId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the invocation's own request id, got {record}"
        );

        // `requestPayload` is WHAT WAS DELIVERED: the envelope for a plain target, and the
        // rendered template for one that transforms the input.
        let payload = &record["requestPayload"];
        if row.transformed {
            assert_eq!(
                *payload,
                json!({ "marker": marker }),
                "case {case:?}: expected the rendered template as the request payload, got \
                 {record}"
            );
        } else {
            assert_eq!(
                payload["id"],
                json!(event_id),
                "case {case:?}: expected the published event's id {event_id:?} in the request \
                 payload, got {record}"
            );
            assert_eq!(
                payload["source"],
                json!(SOURCE),
                "case {case:?}: expected the envelope's source {SOURCE:?}, got {record}"
            );
            assert_eq!(
                payload["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected the envelope's detail-type {DETAIL_TYPE:?}, got \
                 {record}"
            );
            assert_eq!(
                payload["detail"],
                json!({ "marker": marker, "fail": row.fails }),
                "case {case:?}: expected the published detail in the request payload, got \
                 {record}"
            );
        }

        // The response the record reports is the handler's OWN answer, and the run that
        // produced it is reported beside it.
        let response = &record["responseContext"];
        assert_eq!(
            response["statusCode"],
            json!(INVOKED_STATUS),
            "case {case:?}: expected the status of a run that executed, got {record}"
        );
        if row.fails {
            assert_eq!(
                response["functionError"],
                json!(FUNCTION_ERROR_UNHANDLED),
                "case {case:?}: expected the function error of a raising handler, got {record}"
            );
            assert_eq!(
                record["responsePayload"]["errorType"],
                json!(ERROR_TYPE),
                "case {case:?}: expected the handler's own error type, got {record}"
            );
            assert_eq!(
                record["responsePayload"]["errorMessage"],
                json!(failure_sentence(&marker)),
                "case {case:?}: expected the handler's own message, got {record}"
            );
        } else {
            assert_eq!(
                record["responsePayload"],
                json!({ "echoed": marker }),
                "case {case:?}: expected the handler's own return value, got {record}"
            );
            assert!(
                response.get("functionError").is_none(),
                "case {case:?}: a handler that returned reports no function error, got {record}"
            );
        }

        // The slot the outcome did not select received nothing, and — the point of the whole
        // rule — the handler's own failure is not the rule's: its dead-letter queue is empty.
        // The record above has already waited the completion out, so anything owed to either
        // queue would be on it by now.
        let other = drain(&router, case, other_url).await;
        assert!(
            other.is_empty(),
            "case {case:?}: expected the other destination slot to receive nothing, got {other:?}"
        );
        let dead_letters = drain(&router, case, &dead_letter_url).await;
        assert!(
            dead_letters.is_empty(),
            "case {case:?}: expected the rule to dead-letter nothing, got {dead_letters:?}"
        );
    }
}

