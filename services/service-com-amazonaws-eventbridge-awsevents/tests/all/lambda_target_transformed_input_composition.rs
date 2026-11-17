// SPDX-License-Identifier: CC0-1.0

//! What a rule's Lambda target is RUN with when the target carries an `InputTransformer` whose
//! template COMPOSES a value out of the matched event — end to end over the wire, with the
//! services the target's own handler calls mounted beside EventBridge on one engine.
//!
//! The RULE pinned here: **a target carrying an `InputTransformer` is handed, instead of the
//! event envelope, its `InputTemplate` with every `<key>` placeholder replaced by what
//! `InputPathsMap[key]` selects from the matched event — the template's own text surviving
//! exactly as written around the substitutions, so a placeholder standing inside one of the
//! template's strings composes with the fixed characters beside it and one standing as a JSON
//! value keeps the selected value's JSON type — and that rendering is the payload the target is
//! actually INVOKED with, so it is what the function's own code reads and what everything
//! downstream of the function then carries. A target of the same rule carrying no transformer
//! is handed the whole envelope, and an entry the rule's pattern does not match is handed to
//! nothing.**
//!
//! Every observation is one a client makes: the function's handler relays the payload it was
//! handed to an Amazon SQS queue, and the suite reads that queue through `ReceiveMessage`. The
//! direct `RequestResponse` `Invoke` a client makes before wiring the rule is asserted too —
//! it must ANSWER, carrying no function error — because a warm-up that never returns hides
//! every later observation behind a deadline.
//!
//! The targets differ along what the rule generalises over: where the placeholder stands (as a
//! JSON value, inside one of the template's own strings), how much fixed text surrounds it
//! (text before it, text after it, text between two placeholders), the KIND of place its path
//! names (a member of `detail`, a member nested two levels deeper, an element of an array
//! there, a top-level envelope member, the envelope's own `id`), and whether the target carries
//! a transformer at all.
//!
//! `tests/all/input_transformation.rs` is the suite for the rendering itself over a queue
//! target; `tests/all/lambda_target_handler_records_the_invoking_rule.rs` is the suite for
//! WHICH policies admit a function target's invocation. What this one adds is a COMPOSED
//! rendering — fixed text and extracted values in one string — reaching a function that really
//! runs, beside an untransformed target of the same rule, from ONE publication.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    ComAmazonawsEventbridgeAwseventsState, InvocationOutcome,
};
use smithy_server_runtime::MultiServiceRouter;

// The one percent-encoder the suites of this crate share, for the awsQuery body an IAM call
// carries.
use crate::target_dispatch::form_encode;

/// The Region every request below is signed for, and so the Region the queue, the function and
/// the rule belong to.
const REGION: &str = "us-west-2";

/// The `source` the rule's pattern names, and one it does not.
const SOURCE: &str = "com.demo.cdn.simulated-cloudtrail";
/// See [`SOURCE`].
const OTHER_SOURCE: &str = "com.demo.cdn.elsewhere";

/// The `detail-type` every entry carries and the pattern pins — the shape a CloudTrail-sourced
/// entry has.
const DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The members of the published `detail` the templates select from, and their values. All of
/// them are values a client publishes, never values this emulator computes.
const EVENT_NAME: &str = "CreateInvalidation2020_05_31";
/// See [`EVENT_NAME`].
const DISTRIBUTION_ID: &str = "EDFDVBD6EXAMPLE";
/// See [`EVENT_NAME`].
const CORRELATION_ID: &str = "7c3f0b9a-detail-event-id";
/// See [`EVENT_NAME`].
const PATH_QUANTITY: i64 = 2;
/// See [`EVENT_NAME`].
const FIRST_PATH: &str = "/images/*";
/// See [`EVENT_NAME`].
const SECOND_PATH: &str = "/index.html";

/// The principal EventBridge invokes a resource-policy-governed target as, and the action such
/// an invocation performs: "for Lambda functions, Amazon SNS topics and Amazon SQS queues,
/// EventBridge relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
/// See [`EVENTS_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The principal the function's execution role is assumed by.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler needs on any host.
const FUNCTION_TIMEOUT: u64 = 60;

/// The environment variable the handler reads the witness queue's name from.
const WITNESS_VARIABLE: &str = "WITNESS_QUEUE";

/// The member the warm-up payload carries, which tells the handler that what it was handed is
/// no delivery and nothing is to be relayed.
const WARMUP_MEMBER: &str = "warmup";

/// The member every rendered template carries, naming the target the rendering belongs to, so a
/// relayed payload is read back against the template it came from.
const CASE_MEMBER: &str = "case";

/// How long each observation of work the clock owes — the function becoming `Active`, an
/// asynchronous invocation running, the handler's own relay arriving — waits before giving up,
/// and how long between two looks. The deadline is generous: what is under test is that the
/// work happens at all, never how fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long an observation that must stay EMPTY keeps looking before concluding nothing came.
/// Nothing rests on this window alone: the run's own positive leg, asserted afterwards, is what
/// shows the dispatcher was live while it was open.
const QUIET_BUDGET: Duration = Duration::from_secs(3);

/// The queue, function, role and rule names this suite's resources carry.
const WITNESS_QUEUE: &str = "transformed-input-witness";
/// See [`WITNESS_QUEUE`].
const FUNCTION: &str = "transformed-input-relay";
/// See [`WITNESS_QUEUE`].
const RULE: &str = "transformed-input-rule";
/// See [`WITNESS_QUEUE`].
const EXECUTION_ROLE: &str = "transformed-input-relay-exec";

/// The handler the function target deploys: it relays the payload the invocation handed it to
/// the witness queue, which is where a client observes both that the invocation really ran and
/// WHAT it was handed. The warm-up payload a client sends before wiring the rule is no delivery
/// at all, so it relays nothing and says so.
const RELAY_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    if event.get("warmup"):
        return {"relayed": False}
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["WITNESS_QUEUE"])["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event, sort_keys=True))
    return {"relayed": True}
"#;

/// EventBridge, Lambda, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into this
/// same router, so the queue it writes is the queue this suite reads.
fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id from. The
    // handler's OWN call is signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
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

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, http::HeaderMap, Value) {
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
    let headers = response.headers().clone();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// One awsQuery call to IAM, as the account's own credential, which must be served.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, String)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {answered}"
    );
}

/// A .zip deployment package holding [`RELAY_SOURCE`] as `lambda_function.py`, written in memory
/// the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(RELAY_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// The account every emulated resource of this suite belongs to, as the engine holds it — the
/// account a composed ARN therefore names.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The pattern the rule carries: the `source`, the `detail-type` and the `eventName` inside
/// `detail`.
fn pattern() -> String {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { "eventName": [EVENT_NAME] },
    })
    .to_string()
}

/// The `detail` the published entry carries: a CloudTrail-shaped document holding a scalar, a
/// correlation id, a sub-object two levels down, a number and an array.
fn detail() -> Value {
    json!({
        "eventName": EVENT_NAME,
        "eventID": CORRELATION_ID,
        "requestParameters": {
            "distributionId": DISTRIBUTION_ID,
            "invalidationBatch": {
                "paths": { "quantity": PATH_QUANTITY, "items": [FIRST_PATH, SECOND_PATH] },
            },
        },
    })
}

/// One target of the rule: what its `InputTransformer` selects and renders, and the document the
/// running function must therefore be handed.
struct Row {
    case: &'static str,
    /// The target's `Id`, and the value its template writes into [`CASE_MEMBER`] so a relayed
    /// payload is read back against the target it came from.
    marker: &'static str,
    /// `InputTransformer$InputPathsMap`.
    paths: Value,
    /// `InputTransformer$InputTemplate`.
    template: String,
    /// The rendering, given the account the composed ARN names and the `EventId` the
    /// publication answered.
    expected: fn(&str, &str) -> Value,
}

/// Every transformer target of the rule. The fixed text each template writes — the literal
/// `arn:aws:cloudfront::` and `:distribution/`, the words between two placeholders, the member
/// names — is part of the template and survives the rendering; only the `<key>` placeholders are
/// replaced.
fn rows(account: &str) -> Vec<Row> {
    vec![
        Row {
            case: "fixed text before and after one placeholder inside the template's own string \
                   composes an ARN from a member of the detail",
            marker: "composed-arn",
            paths: json!({
                "distId": "$.detail.requestParameters.distributionId",
                "corrId": "$.detail.eventID",
            }),
            template: json!({
                CASE_MEMBER: "composed-arn",
                "distributionArn": "arn:aws:cloudfront::<distId-account>:distribution/<distId>",
                "correlationId": "<corrId>",
            })
            .to_string()
            .replace("<distId-account>", account),
            expected: |account, _| {
                json!({
                    CASE_MEMBER: "composed-arn",
                    "distributionArn":
                        format!("arn:aws:cloudfront::{account}:distribution/{DISTRIBUTION_ID}"),
                    "correlationId": CORRELATION_ID,
                })
            },
        },
        Row {
            case: "two placeholders in one of the template's strings, separated by fixed text",
            marker: "two-in-one-string",
            paths: json!({
                "name": "$.detail.eventName",
                "distId": "$.detail.requestParameters.distributionId",
            }),
            template: json!({
                CASE_MEMBER: "two-in-one-string",
                "summary": "<name> touched <distId>",
            })
            .to_string(),
            expected: |_, _| {
                json!({
                    CASE_MEMBER: "two-in-one-string",
                    "summary": format!("{EVENT_NAME} touched {DISTRIBUTION_ID}"),
                })
            },
        },
        Row {
            case: "a bare placeholder keeps the selected value's JSON type beside a string \
                   composed from an element of an array two levels deeper",
            marker: "typed-and-nested",
            paths: json!({
                "quantity": "$.detail.requestParameters.invalidationBatch.paths.quantity",
                "first": "$.detail.requestParameters.invalidationBatch.paths.items[0]",
            }),
            template: json!({
                CASE_MEMBER: "typed-and-nested",
                "quantity": "<quantity>",
                "firstPath": "https://cdn.example<first>",
            })
            .to_string()
            // The quantity placeholder stands as a JSON VALUE rather than inside a string, so
            // what it carries keeps the selected value's own JSON type.
            .replace("\"<quantity>\"", "<quantity>"),
            expected: |_, _| {
                json!({
                    CASE_MEMBER: "typed-and-nested",
                    "quantity": PATH_QUANTITY,
                    "firstPath": format!("https://cdn.example{FIRST_PATH}"),
                })
            },
        },
        Row {
            case: "placeholders naming top-level envelope members, one of them the event's own \
                   id",
            marker: "envelope-members",
            paths: json!({ "kind": "$.detail-type", "eid": "$.id" }),
            template: json!({
                CASE_MEMBER: "envelope-members",
                "kind": "<kind>",
                "eventId": "<eid>",
            })
            .to_string(),
            expected: |_, event_id| {
                json!({
                    CASE_MEMBER: "envelope-members",
                    "kind": DETAIL_TYPE,
                    "eventId": event_id,
                })
            },
        },
    ]
}

/// The `Id` the one target carrying NO transformer is configured under: the contrast that shows
/// the transformer is what reshaped the others, since this one is handed the whole envelope.
const UNTRANSFORMED_TARGET: &str = "untransformed";

/// Publish one entry with `source` and the detail above, assert it is ACCEPTED, and answer the
/// `EventId` it was given.
async fn publish(router: &MultiServiceRouter, case: &str, source: &str) -> String {
    let published = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": detail().to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: an accepted publication reports no failed entry, got {published}"
    );
    published["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an EventId, got {published}"))
        .to_string()
}

/// Every message waiting on the queue at `url`, taken off it: each is DELETED as it is read, so
/// what a later look answers is what arrived after this one rather than a copy whose visibility
/// timeout ran out.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    messages
}

/// Everything the queue at `url` holds once it holds `want` messages, so a delivery still in
/// flight is looked for again rather than read as missing; gives up after [`BUDGET`] so a
/// message that never arrives fails an assertion instead of hanging.
async fn messages_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<Value> = Vec::new();
    loop {
        held.extend(drain(router, case, url).await);
        if held.len() >= want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Whatever the queue at `url` holds within [`QUIET_BUDGET`], looked for again and again so a
/// relay that did happen is seen rather than missed.
async fn whatever_arrives(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let until = Instant::now() + QUIET_BUDGET;
    let mut held: Vec<Value> = Vec::new();
    while Instant::now() < until {
        held.extend(drain(router, case, url).await);
        if !held.is_empty() {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    held
}

/// The document one relayed message carries: the payload the invocation handed the handler.
fn payload(case: &str, message: &Value) -> Value {
    let body = message["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: a relayed message carries a JSON payload, got {body} ({error})")
    })
}

/// What EventBridge recorded for each handover of a routed event, once it has recorded `want` of
/// them.
async fn handovers_until(
    engine: &Arc<engine::InMemoryEngine>,
    want: usize,
) -> Vec<InvocationOutcome> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let recorded: Vec<InvocationOutcome> = {
            let state = engine.state::<ComAmazonawsEventbridgeAwseventsState>();
            let store = state.lock();
            store
                .invocations
                .iter()
                .map(|invocation| invocation.outcome.clone())
                .collect()
        };
        if recorded.len() >= want || Instant::now() >= deadline {
            return recorded;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: each target of a matched rule is invoked with what its own `InputTransformer` renders —
/// the template's fixed text with every placeholder replaced by what its path selects — and a
/// target carrying no transformer is invoked with the whole envelope, while an entry the
/// pattern does not match is invoked for nothing.
#[tokio::test]
async fn a_function_target_runs_on_the_rendering_its_input_transformer_composed() {
    let case = "a rule whose targets reshape one CloudTrail-shaped entry";
    let (router, engine) = router();
    let account = account();

    // The queue the handler relays to, which is where a client observes what each invocation was
    // handed. The handler writes it as its own execution role, so the queue carries no policy
    // for this service.
    let created = sqs(
        &router,
        case,
        "CreateQueue",
        &json!({ "QueueName": WITNESS_QUEUE }),
    )
    .await;
    let witness_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();

    // The execution role the handler's own relay is judged against.
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        &router,
        case,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    let inline = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        &router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    let role_arn = format!("arn:aws:iam::{account}:role/{EXECUTION_ROLE}");

    // The function, and the direct warm-up invocation a client makes before wiring the rule: it
    // has to ANSWER, carrying no function error, and it relays nothing — so the witness queue is
    // untouched while the function and its role are already shown to work.
    let function_arn = given_function(&router, case, &role_arn).await;
    let (status, headers, answered) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        &json!({ WARMUP_MEMBER: true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from a RequestResponse Invoke of the warm-up payload, \
         got {status} with {answered}"
    );
    assert!(
        !headers.contains_key("x-amz-function-error"),
        "case {case:?}: expected the warm-up invocation to carry no function error, got \
         {headers:?} with {answered}"
    );
    assert_eq!(
        answered,
        json!({ "relayed": false }),
        "case {case:?}: expected the warm-up invocation to relay nothing, got {answered}"
    );

    // The rule, its pattern, and the grant the function's own resource-based policy has to carry
    // for this service to invoke it, scoped by `aws:SourceArn` to this rule.
    let written = events(
        &router,
        case,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": pattern(), "State": "ENABLED" }),
    )
    .await;
    let rule_arn = written["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {written}"))
        .to_string();
    let (status, _, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        &json!({
            "StatementId": "AllowEventBridgeInvoke",
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {status} with {granted}"
    );

    // Every transformer target, plus the one carrying no transformer at all, on ONE rule — so
    // every rendering below comes from the SAME matched entry.
    let rows = rows(&account);
    let mut targets: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "Id": row.marker,
                "Arn": function_arn,
                "InputTransformer": {
                    "InputPathsMap": row.paths,
                    "InputTemplate": row.template,
                },
            })
        })
        .collect();
    targets.push(json!({ "Id": UNTRANSFORMED_TARGET, "Arn": function_arn }));
    let targeted = events(
        &router,
        case,
        "PutTargets",
        &json!({ "Rule": RULE, "Targets": targets }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target must be accepted, got {targeted}"
    );

    // Leg one: an entry the pattern does not match is accepted and handed to nothing, so no
    // rendering of it reaches the function.
    publish(&router, case, OTHER_SOURCE).await;
    let early = whatever_arrives(&router, case, &witness_url).await;
    assert!(
        early.is_empty(),
        "case {case:?}: expected an entry the pattern does not match to be handed to nothing, \
         got {early:?}"
    );

    // Leg two — which is what makes leg one mean something: the matching entry reaches every
    // target once, each with its own rendering.
    let event_id = publish(&router, case, SOURCE).await;
    let want = rows.len() + 1;
    let relayed = messages_until(&router, case, &witness_url, want).await;
    assert_eq!(
        relayed.len(),
        want,
        "case {case:?}: expected the matching entry to reach each of the {want} targets once, \
         got {relayed:?}"
    );
    let payloads: Vec<Value> = relayed
        .iter()
        .map(|message| payload(case, message))
        .collect();

    for row in &rows {
        let expected = (row.expected)(&account, &event_id);
        let found = payloads
            .iter()
            .find(|payload| payload[CASE_MEMBER] == json!(row.marker));
        let Some(actual) = found else {
            panic!(
                "case {:?}: expected the target to be invoked with its rendering, got {payloads:?}",
                row.case
            );
        };
        assert_eq!(
            actual, &expected,
            "case {:?}: expected the template {:?} to be rendered as {expected} and handed to \
             the running function, got {actual}",
            row.case, row.template
        );
    }

    // The contrast: the target of the same rule carrying no transformer was handed the whole
    // envelope, so what reshaped the others is the transformer and nothing else.
    let envelope = payloads
        .iter()
        .find(|payload| payload["detail-type"] == json!(DETAIL_TYPE))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the target carrying no transformer to be handed the \
                 envelope, got {payloads:?}"
            )
        });
    assert_eq!(
        envelope["source"],
        json!(SOURCE),
        "case {case:?}: expected the envelope to carry the published source, got {envelope}"
    );
    assert_eq!(
        envelope["id"],
        json!(event_id),
        "case {case:?}: expected the envelope to carry the answered EventId, got {envelope}"
    );
    assert_eq!(
        envelope["detail"],
        detail(),
        "case {case:?}: expected the envelope to carry the published detail unchanged, got \
         {envelope}"
    );

    // What EventBridge itself recorded agrees: one handover per target, every one of them made.
    let recorded = handovers_until(&engine, want).await;
    assert_eq!(
        recorded.len(),
        want,
        "case {case:?}: expected one recorded handover per target, got {recorded:?}"
    );
    for outcome in &recorded {
        assert_eq!(
            outcome,
            &InvocationOutcome::Invoked,
            "case {case:?}: expected every handover to be invoked, got {recorded:?}"
        );
    }

    // And nothing further: the entry the pattern does not match is handed to nothing later
    // either, now that every delivery the matching entry owed has been made.
    let late = whatever_arrives(&router, case, &witness_url).await;
    assert!(
        late.is_empty(),
        "case {case:?}: expected nothing further to be handed to the function, got {late:?}"
    );
}

/// Create the function the rule targets, wait until Lambda reports it `Active` — "during this
/// time, you can't invoke or modify the function" (`CreateFunction`) — and answer its ARN.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let (status, _, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": FUNCTION,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { WITNESS_VARIABLE: WITNESS_QUEUE } },
            "Code": { "ZipFile": package() },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} with {created}"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &Value::Null,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {FUNCTION:?} Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
