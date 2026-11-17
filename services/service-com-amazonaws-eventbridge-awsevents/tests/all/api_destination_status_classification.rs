// SPDX-License-Identifier: CC0-1.0

//! What an API DESTINATION's endpoint ANSWERS decides the delivery, end to end over the
//! wire: EventBridge, AWS Lambda and Amazon SQS on ONE engine, with the destination's
//! `InvocationEndpoint` pointing at a Lambda FUNCTION URL whose `python3.x` handler answers
//! the status its path names and echoes the request it received.
//!
//! The RULE pinned here: a matched event whose target is an API destination is sent to the
//! destination's `InvocationEndpoint` with the destination's `HttpMethod`, carrying the
//! connection's authorization material (for a `BASIC` connection,
//! `Authorization: Basic <base64(user:password)>`), no query string unless one is
//! configured, and the bare EventBridge envelope as the body. The ANSWER then decides what
//! happens:
//!
//! * an answer in the 2xx range is the delivery — nothing is retried and nothing is
//!   dead-lettered;
//! * an answer that is the endpoint refusing the request itself (a 4xx) is NOT retried: the
//!   event is dead-lettered immediately, under `ERROR_CODE` `INVALID_PARAMETER`, with no
//!   retry accounting at all;
//! * an answer the endpoint may take the event under later (a 5xx) is tried again until the
//!   target's `RetryPolicy$MaximumRetryAttempts` is used up and only then dead-lettered,
//!   under `ERROR_CODE` `UNKNOWN`, carrying `EXHAUSTED_RETRY_CONDITION`
//!   `MaximumRetryAttempts` and the `RETRY_ATTEMPTS` it made;
//! * and either way the dead letter's `ERROR_MESSAGE` states the status the endpoint
//!   answered and the payload it answered with, so a consumer recovers the HTTP status from
//!   it.
//!
//! The rows span that class rather than one recorded pair: they differ along the status the
//! endpoint answers (a delivery, three refusals across the 4xx range, a server error) and
//! therefore along whether the delivery is retried, whether it is dead-lettered and under
//! which condition. Every expected message is composed in the test from the row's own status
//! and the endpoint's own answer, never imported from the implementation.
//!
//! Only a suite mounting Lambda beside EventBridge can pin this: an api-destination
//! invocation is made through the IN-PROCESS emulator transport, so an endpoint that really
//! ANSWERS a chosen status is one this emulator serves itself — a function URL over a
//! handler that runs.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state;
use service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
pub const REGION: &str = "us-west-2";

/// The `detail-type` every event this suite publishes carries.
pub const DETAIL_TYPE: &str = "Hook Fired";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The credentials the `BASIC` connection every destination here invokes through holds, and
/// so the pair the `Authorization` header carries the base64 of (RFC 7617).
const BASIC_USER: &str = "hook-user";
const BASIC_PASSWORD: &str = "hook-password";

/// The `Authorization` a destination invoking through that connection has to carry, composed
/// the way RFC 7617 defines it rather than asserted as one encoded literal.
pub fn basic_authorization() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{BASIC_USER}:{BASIC_PASSWORD}"))
    )
}

/// The method every destination here is invoked with — not the default, so a request made
/// with some other method fails the row.
pub const HTTP_METHOD: &str = "PUT";

/// How many retries the target of every row allows, which is what the retried rows have to
/// report as `RETRY_ATTEMPTS`.
const MAXIMUM_RETRY_ATTEMPTS: i32 = 1;

/// The condition a delivery that used its retry allowance up names.
const EXHAUSTED_RETRY_CONDITION: &str = "MaximumRetryAttempts";

/// How long this suite waits for work it has asked for before giving up on it. Far longer
/// than the emulator's own spans, so a loaded machine does not fail a row the clock would
/// have served.
const WORK_WAIT: Duration = Duration::from_secs(60);

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// EventBridge, Lambda and Amazon SQS over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the api-destination invocation is dispatched back into this same router, so
/// the function URL the destination names is served by the function this suite created.
pub struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

pub fn emulator() -> Emulator {
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
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
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
    emulator: &Emulator,
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
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
pub async fn events(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(emulator, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A Lambda `restJson1` call, as its status and parsed body.
async fn lambda(emulator: &Emulator, method: &str, uri: &str, body: Value) -> (u16, Value) {
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
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A required string member of a body.
pub fn member(case: &str, body: &Value, name: &str) -> String {
    body[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name}, got {body}"))
        .to_string()
}

/// The endpoint every destination of this suite points at: the handler answers the status
/// the LAST path segment names and echoes what the request carried — its method, its
/// `Authorization`, the path and query string it arrived on and its body — so one function
/// serves every row and the row's own status is in the address.
///
/// Nothing here is derived from the request's content: the status comes from the path the
/// destination was created with, and the echo is what the invocation handed the handler.
const ECHO_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    path = event.get("rawPath") or "/"
    headers = event.get("headers") or {}
    context_http = (event.get("requestContext") or {}).get("http") or {}
    echoed = {
        "method": context_http.get("method") or "",
        "authorization": headers.get("authorization") or "",
        "path": path,
        "query": event.get("rawQueryString") or "",
        "body": event.get("body") or "",
    }
    return {
        "statusCode": int(path.rsplit("/", 1)[-1]),
        "headers": {"content-type": "application/json"},
        "body": json.dumps(echoed),
    }
"#;

/// A .zip deployment package holding the handler above as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries
/// it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(ECHO_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// Create the echo function, wait out the span it is `Pending` for, publish a function URL
/// over it and grant the anonymous caller the invocation; answers the URL with no trailing
/// separator, so a row appends its own path.
pub async fn given_echo_endpoint(emulator: &Emulator) -> String {
    let case = "the echo endpoint";
    let name = "api-destination-echo";
    let (status, created) = lambda(
        emulator,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": format!(
                "arn:aws:iam::{}:role/service-role/lambda-exec",
                engine::emulator_account_id()
            ),
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
    // "During this time, you can't invoke or modify the function" (`CreateFunction`): the
    // function leaves `Pending` by the clock, so the suite LOOKS until it is `Active`
    // rather than waiting a span of its own.
    let deadline = Instant::now() + WORK_WAIT;
    let mut reported = String::new();
    while Instant::now() < deadline {
        let (_, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            Value::Null,
        )
        .await;
        reported = configuration["State"].as_str().unwrap_or_default().to_string();
        if reported == "Active" {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        reported, "Active",
        "case {case:?}: the echo function has to reach Active before it can answer, last \
         reported {reported:?}"
    );
    let (status, url) = lambda(
        emulator,
        "POST",
        &format!("/2021-10-31/functions/{name}/url"),
        json!({ "AuthType": "NONE" }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda CreateFunctionUrlConfig, got {status} \
         ({url})"
    );
    // `AuthType=NONE` bypasses IAM authentication; the function's own resource policy is
    // what admits the anonymous caller an api-destination invocation is.
    let (status, granted) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission, got {status} ({granted})"
    );
    member(case, &url, "FunctionUrl")
        .trim_end_matches('/')
        .to_string()
}

/// A `BASIC` connection, waited until it reports `AUTHORIZED` — which is what makes a
/// destination invoking through it `ACTIVE`; answers its ARN.
pub async fn given_basic_connection(emulator: &Emulator) -> String {
    let case = "the basic connection";
    let name = "api-destination-basic";
    let created = events(
        emulator,
        case,
        "CreateConnection",
        &json!({
            "Name": name,
            "AuthorizationType": "BASIC",
            "AuthParameters": {
                "BasicAuthParameters": { "Username": BASIC_USER, "Password": BASIC_PASSWORD },
            },
        }),
    )
    .await;
    let arn = member(case, &created, "ConnectionArn");
    let deadline = Instant::now() + WORK_WAIT;
    let mut reported = String::new();
    while Instant::now() < deadline {
        let described = events(
            emulator,
            case,
            "DescribeConnection",
            &json!({ "Name": name }),
        )
        .await;
        reported = described["ConnectionState"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == state::CONNECTION_STATE_AUTHORIZED {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        reported,
        state::CONNECTION_STATE_AUTHORIZED,
        "case {case:?}: a destination is invoked only while its connection is authorized, \
         last reported {reported:?}"
    );
    arn
}

/// The queue one row's target dead-letters to, carrying the policy this service's own send
/// needs; answers its URL and its ARN.
pub async fn given_dead_letter_queue(
    emulator: &Emulator,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = member(case, &created, "QueueUrl");
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    (url, arn)
}

/// Every message waiting on the queue at `url`, with its attributes, taken off it.
pub async fn drain(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let answered = sqs(
        emulator,
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

/// Wait until something is on the queue at `url`, and answer what was taken off it.
pub async fn until_dead_lettered(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + WORK_WAIT;
    while Instant::now() < deadline {
        let held = drain(emulator, case, url).await;
        if !held.is_empty() {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    Vec::new()
}

/// The value of one dead-letter attribute of `message`, or `None` when the message does not
/// carry it.
pub fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(str::to_string)
}

/// How many invocations this store recorded, and how many of them were delivered.
fn recorded_outcomes(emulator: &Emulator) -> Vec<state::InvocationOutcome> {
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .invocations
        .iter()
        .map(|invocation| invocation.outcome.clone())
        .collect()
}

/// What a row expects of the delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The endpoint took the event: nothing is retried and nothing is dead-lettered.
    Delivered,
    /// The endpoint refused the request itself: the event is dead-lettered on the first
    /// attempt, with no retry accounting at all.
    RefusedAtOnce,
    /// The endpoint may take the event later: the delivery is retried until the target's
    /// allowance is used up and then dead-lettered, reporting the retries it made.
    RetriedThenDeadLettered,
}

/// One row: the status the endpoint answers and what the delivery then comes to.
struct Row {
    case: &'static str,
    status: u16,
    expect: Expect,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the endpoint takes the event",
            status: 200,
            expect: Expect::Delivered,
        },
        Row {
            case: "the endpoint answers a bad request",
            status: 400,
            expect: Expect::RefusedAtOnce,
        },
        Row {
            case: "the endpoint answers a conflict",
            status: 409,
            expect: Expect::RefusedAtOnce,
        },
        Row {
            // The widest 4xx this suite drives: every status in that range is the
            // endpoint refusing the request, so none of them is retried.
            case: "the endpoint answers too many requests",
            status: 429,
            expect: Expect::RefusedAtOnce,
        },
        Row {
            case: "the endpoint answers a server error",
            status: 500,
            expect: Expect::RetriedThenDeadLettered,
        },
    ]
}

/// RULE: what the endpoint answers decides the delivery — a 2xx is the delivery, a 4xx ends
/// it at once under `INVALID_PARAMETER` with no retry accounting, and a retryable answer is
/// tried again to the target's allowance and then dead-lettered under `UNKNOWN` with that
/// accounting — and every request carries the destination's method, the connection's
/// `Authorization: Basic`, no query string and the bare envelope.
#[tokio::test]
async fn an_endpoints_answer_decides_the_api_destination_delivery_and_its_dead_letter() {
    let emulator = emulator();
    let endpoint = given_echo_endpoint(&emulator).await;
    let connection_arn = given_basic_connection(&emulator).await;
    // What the connection's own authorization header has to be ([`basic_authorization`]).
    let expected_authorization = basic_authorization();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let status = row.status;
        let source = format!("com.demo.api-destination.{index}");
        let destination_name = format!("hook-{status}");
        let rule_name = format!("hook-rule-{index}");

        let created = events(
            &emulator,
            case,
            "CreateApiDestination",
            &json!({
                "Name": destination_name,
                "ConnectionArn": connection_arn,
                "InvocationEndpoint": format!("{endpoint}/{status}"),
                "HttpMethod": HTTP_METHOD,
            }),
        )
        .await;
        let destination_arn = member(case, &created, "ApiDestinationArn");
        assert_eq!(
            created["ApiDestinationState"],
            json!(state::API_DESTINATION_STATE_ACTIVE),
            "case {case:?}: the destination has to be ACTIVE before a delivery is attempted, \
             got {created}"
        );
        let (dead_letter_url, dead_letter_arn) =
            given_dead_letter_queue(&emulator, case, &format!("hook-dlq-{index}")).await;

        let rule = events(
            &emulator,
            case,
            "PutRule",
            &json!({
                "Name": rule_name,
                "EventPattern": json!({ "source": [source] }).to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let rule_arn = member(case, &rule, "RuleArn");
        let wired = events(
            &emulator,
            case,
            "PutTargets",
            &json!({
                "Rule": rule_name,
                "Targets": [{
                    "Id": "hook",
                    "Arn": destination_arn,
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                    "RetryPolicy": {
                        "MaximumRetryAttempts": MAXIMUM_RETRY_ATTEMPTS,
                        "MaximumEventAgeInSeconds": 900,
                    },
                }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );

        let before = recorded_outcomes(&emulator).len();
        let published = events(
            &emulator,
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": source,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "hook": index }).to_string(),
                }],
            }),
        )
        .await;
        let event_id = published["Entries"][0]["EventId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EventId, got {published}"))
            .to_string();

        if row.expect == Expect::Delivered {
            let outcomes = recorded_outcomes(&emulator);
            assert!(
                matches!(
                    outcomes.get(before),
                    Some(state::InvocationOutcome::Invoked)
                ),
                "case {case:?}: an answer of HTTP {status} is the delivery, got {:?}",
                outcomes.get(before)
            );
            let held = drain(&emulator, case, &dead_letter_url).await;
            assert!(
                held.is_empty(),
                "case {case:?}: a delivered event is dead-lettered nowhere, got {held:?}"
            );
            continue;
        }

        let dead_letters = until_dead_lettered(&emulator, case, &dead_letter_url).await;
        assert_eq!(
            dead_letters.len(),
            1,
            "case {case:?}: an answer of HTTP {status} dead-letters the event once, got \
             {dead_letters:?}"
        );
        let message = &dead_letters[0];
        // The dead letter carries the EVENT, and names the rule and the target it could not
        // be delivered to.
        let delivered: Value =
            serde_json::from_str(message["Body"].as_str().unwrap_or_default()).unwrap_or_else(
                |error| panic!("case {case:?}: the dead letter is not the envelope ({error})"),
            );
        assert_eq!(
            delivered["id"],
            json!(event_id),
            "case {case:?}: the dead letter carries the published event, got {delivered}"
        );
        assert_eq!(
            attribute(message, "RULE_ARN").as_deref(),
            Some(rule_arn.as_str()),
            "case {case:?}: the dead letter names the rule, got {message}"
        );
        assert_eq!(
            attribute(message, "TARGET_ARN").as_deref(),
            Some(destination_arn.as_str()),
            "case {case:?}: the dead letter names the target, got {message}"
        );

        // The condition and the retry accounting follow from the CLASS the answer is in.
        let (expected_code, expected_retries) = match row.expect {
            Expect::RefusedAtOnce => ("INVALID_PARAMETER", None),
            _ => (
                "UNKNOWN",
                Some((MAXIMUM_RETRY_ATTEMPTS.to_string(), EXHAUSTED_RETRY_CONDITION)),
            ),
        };
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(expected_code),
            "case {case:?}: expected the condition {expected_code}, got {message}"
        );
        match &expected_retries {
            None => {
                assert_eq!(
                    attribute(message, "RETRY_ATTEMPTS"),
                    None,
                    "case {case:?}: an answer of HTTP {status} is not retried, so no retry \
                     count is reported, got {message}"
                );
                assert_eq!(
                    attribute(message, "EXHAUSTED_RETRY_CONDITION"),
                    None,
                    "case {case:?}: an answer of HTTP {status} uses no retry allowance up, \
                     got {message}"
                );
            }
            Some((attempts, condition)) => {
                assert_eq!(
                    attribute(message, "RETRY_ATTEMPTS").as_deref(),
                    Some(attempts.as_str()),
                    "case {case:?}: expected {attempts} retries reported, got {message}"
                );
                assert_eq!(
                    attribute(message, "EXHAUSTED_RETRY_CONDITION").as_deref(),
                    Some(*condition),
                    "case {case:?}: expected the condition {condition} to be named, got \
                     {message}"
                );
            }
        }

        // The message states the status the endpoint answered and the payload it answered
        // with, which is what lets a consumer recover the status from it.
        let reported = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
        let opening = format!("ApiDestination returned HTTP status {status} with payload: ");
        let payload = reported.strip_prefix(&opening).unwrap_or_else(|| {
            panic!("case {case:?}: expected the message to open {opening:?}, got {reported:?}")
        });
        // And that payload is the endpoint's own echo of the request EventBridge made: the
        // destination's method, the connection's authorization, no query string, and the
        // bare envelope as the body.
        let echoed: Value = serde_json::from_str(payload).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the endpoint's answer as the payload, got {error}")
        });
        assert_eq!(
            echoed["method"],
            json!(HTTP_METHOD),
            "case {case:?}: the request is made with the destination's HttpMethod, got \
             {echoed}"
        );
        assert_eq!(
            echoed["authorization"],
            json!(expected_authorization),
            "case {case:?}: the request carries the connection's basic authorization, got \
             {echoed}"
        );
        assert_eq!(
            echoed["query"],
            json!(""),
            "case {case:?}: nothing configured a query string, so the request carries none, \
             got {echoed}"
        );
        let sent: Value = serde_json::from_str(echoed["body"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: expected the envelope as the body, got {error}")
            });
        assert_eq!(
            sent["id"],
            json!(event_id),
            "case {case:?}: the body is the bare envelope of the published event, got {sent}"
        );
        assert_eq!(
            sent["detail-type"],
            json!(DETAIL_TYPE),
            "case {case:?}: the body is the bare envelope of the published event, got {sent}"
        );
        assert_eq!(
            sent["source"],
            json!(source),
            "case {case:?}: the body is the bare envelope of the published event, got {sent}"
        );
    }
}

/// How many events the burst below publishes at a destination limited to one invocation a
/// second, and what that limit is: enough events that the destination cannot absorb them in
/// one instant, so every one of them but the first is deferred.
const BURST: usize = 3;
const BURST_RATE: i32 = 1;

/// Every invocation this store recorded of the rule `rule`, as (event id, outcome).
fn recorded_for(emulator: &Emulator, rule: &str) -> Vec<(String, state::InvocationOutcome)> {
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .invocations
        .iter()
        .filter(|invocation| invocation.rule == rule)
        .map(|invocation| (invocation.event_id.clone(), invocation.outcome.clone()))
        .collect()
}

/// RULE: `InvocationRateLimitPerSecond` THROTTLES a destination rather than dropping what it
/// cannot send yet, so a burst larger than the rate is still delivered — every event exactly
/// once, and none of them dead-lettered.
///
/// What the pacing itself computes (which instant each invocation of a burst is due at, one
/// period apart and in order) is pinned by the unit tests of
/// `state::api_destination_pacing`; what this one pins is that a burst the destination cannot
/// absorb still reaches it completely, which is what the deferral is for.
#[tokio::test]
async fn a_burst_past_an_api_destinations_rate_is_deferred_and_still_delivered() {
    let case = "a burst past the destination's rate";
    let emulator = emulator();
    let endpoint = given_echo_endpoint(&emulator).await;
    let connection_arn = given_basic_connection(&emulator).await;
    let source = "com.demo.api-destination.burst";
    let rule_name = "hook-burst-rule";

    let created = events(
        &emulator,
        case,
        "CreateApiDestination",
        &json!({
            "Name": "hook-burst",
            "ConnectionArn": connection_arn,
            "InvocationEndpoint": format!("{endpoint}/200"),
            "HttpMethod": HTTP_METHOD,
            "InvocationRateLimitPerSecond": BURST_RATE,
        }),
    )
    .await;
    let destination_arn = member(case, &created, "ApiDestinationArn");
    let (dead_letter_url, dead_letter_arn) =
        given_dead_letter_queue(&emulator, case, "hook-burst-dlq").await;
    events(
        &emulator,
        case,
        "PutRule",
        &json!({
            "Name": rule_name,
            "EventPattern": json!({ "source": [source] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let wired = events(
        &emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": rule_name,
            "Targets": [{
                "Id": "hook",
                "Arn": destination_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": MAXIMUM_RETRY_ATTEMPTS,
                    "MaximumEventAgeInSeconds": 900,
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );

    let entries: Vec<Value> = (0..BURST)
        .map(|nth| {
            json!({
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "nth": nth }).to_string(),
            })
        })
        .collect();
    let published = events(
        &emulator,
        case,
        "PutEvents",
        &json!({ "Entries": entries }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected every entry to be accepted, got {published}"
    );
    let mut ids: Vec<String> = published["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry["EventId"].as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    assert_eq!(
        ids.len(),
        BURST,
        "case {case:?}: expected an EventId per entry, got {published}"
    );

    // `PutEvents` "returns an EventId per accepted entry" and nothing about the deliveries,
    // and the pacing is what makes the burst take longer than one request: the invocations
    // this destination could not make at once are still OWED when the publisher is answered.
    // Asserting FEWER than the whole burst here cannot be made flaky by a slow machine —
    // throttling can only make a delivery later — while an unpaced path, which invokes every
    // entry inline before answering, fails it.
    let at_once = recorded_for(&emulator, rule_name).len();
    assert!(
        at_once < BURST,
        "case {case:?}: a destination limited to {BURST_RATE} invocation(s) a second cannot \
         absorb {BURST} events at once, so the publisher is answered with some of them still \
         owed, got {at_once} of {BURST} already invoked"
    );

    // Every event of the burst reaches the destination, however long the pacing holds it:
    // the suite LOOKS until each of them has been invoked rather than waiting a span of its
    // own.
    let deadline = Instant::now() + WORK_WAIT;
    let mut recorded = recorded_for(&emulator, rule_name);
    while Instant::now() < deadline && recorded.len() < BURST {
        tokio::time::sleep(LOOK_INTERVAL).await;
        recorded = recorded_for(&emulator, rule_name);
    }
    assert_eq!(
        recorded.len(),
        BURST,
        "case {case:?}: a deferred burst is still delivered whole, got {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .all(|(_, outcome)| matches!(outcome, state::InvocationOutcome::Invoked)),
        "case {case:?}: every event of the burst is delivered, got {recorded:?}"
    );
    let mut delivered: Vec<String> = recorded.into_iter().map(|(id, _)| id).collect();
    delivered.sort();
    assert_eq!(
        delivered, ids,
        "case {case:?}: each published event is delivered exactly once, expected {ids:?}, got \
         {delivered:?}"
    );
    let held = drain(&emulator, case, &dead_letter_url).await;
    assert!(
        held.is_empty(),
        "case {case:?}: a deferred event is not a lost one, so none is dead-lettered, got \
         {held:?}"
    );
}
