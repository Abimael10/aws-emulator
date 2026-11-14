// SPDX-License-Identifier: CC0-1.0

//! Delivering a matched event to an API DESTINATION target, end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`, with SQS mounted on the same engine so
//! an undeliverable event is observed where a client observes it: on the
//! dead-letter queue the target names.
//!
//! The RULES asserted here:
//!
//! * a matched event whose target ARN names an API destination IS attempted: the
//!   destination's `HttpMethod` request is made to its `InvocationEndpoint`,
//!   bounded by the client execution timeout, and an attempt that never reached
//!   an endpoint is tried AGAIN until one of the target's two retry bounds is
//!   met — "retry attempts continue until either the maximum number of attempts
//!   is made or until the duration of the `MaximumEventAgeInSeconds` is met" —
//!   so a target with N extra retries makes measurably more attempts than an
//!   otherwise identical target with none;
//! * an event no attempt delivered goes to the target's dead-letter queue,
//!   carrying the rule, the target, the error and — for a delivery that was
//!   retried — how many retries it made and WHICH of the two bounds stopped it;
//! * a target whose ARN names no API destination this account holds is dead
//!   lettered WITHOUT any retry accounting, because nothing was retried;
//! * the request is made through the in-process emulator transport and nothing
//!   else, so an endpoint a caller stored reaches only what this emulator serves
//!   itself.
//!
//! The table spans the class rather than one recorded case: the rows differ along
//! the retry allowance the target carries — none at all, zero, one, several, and
//! more than the event's age bound admits — and each row has its own dead-letter
//! queue so the attempts of one row are told apart from another's. Which bound
//! ends a delivery, over the `RetryPolicy` members that decide it, is
//! `tests/all/api_destination_retry_bounds.rs`'s.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state;
use service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState;
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-1";

/// The source and detail type the rule below matches on.
const SOURCE: &str = "com.demo.webhooks";
const DETAIL_TYPE: &str = "Hook Fired";

/// The endpoint every destination below points at: a host this emulator serves
/// nothing at, which is what an endpoint that never answers looks like from
/// inside the in-process transport — the attempt reaches no endpoint.
pub(crate) const ENDPOINT: &str = "https://hooks.invalid.example/orders";

/// The emulator: EventBridge and SQS on one engine.
pub(crate) struct Emulator {
    pub(crate) router: MultiServiceRouter,
    pub(crate) engine: Arc<engine::InMemoryEngine>,
}

pub(crate) fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// One `awsJson` RPC call against the service whose signing name is
/// `signing_name` — the credential scope is what routes it.
async fn rpc(
    emulator: &Emulator,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let credential = format!(
        "AKIDTESTTESTTEST/20260101/{REGION}/{signing_name}/aws4_request, SignedHeaders=host, \
         Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", format!("application/x-amz-json-{json_version}"))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential={credential}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

pub(crate) async fn events(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "events", "AWSEvents", "1.1", operation, body).await
}

pub(crate) async fn sqs(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// A required string member of a response body.
pub(crate) fn member(case: &str, body: &Value, name: &str) -> String {
    body[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: no {name} in {body}"))
        .to_string()
}

/// A queue called `name`, carrying the policy a rule's target (and a dead-letter
/// queue) needs: EventBridge "relies on resource-based policies" for a queue
/// (`PutTargets`), so one that has granted the service nothing receives nothing —
/// the rule `tests/all/queue_target_policy_authorization.rs` is the suite for.
/// Answers its url and the ARN a target names it by.
pub(crate) async fn given_queue(emulator: &Emulator, name: &str) -> (String, String) {
    let (status, body) = sqs(emulator, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(status, 200, "CreateQueue answered {status} {body}");
    let url = member(name, &body, "QueueUrl");
    let (status, body) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(status, 200, "GetQueueAttributes answered {status} {body}");
    let arn = body["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no QueueArn in {body}"))
        .to_string();
    let (status, body) = sqs(
        emulator,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": crate::target_dispatch::granting_events(&arn) },
        }),
    )
    .await;
    assert_eq!(status, 200, "SetQueueAttributes answered {status} {body}");
    (url, arn)
}

/// Every message waiting on the queue at `url`, with its attributes.
pub(crate) async fn drain(emulator: &Emulator, url: &str) -> Vec<Value> {
    let (status, body) = sqs(
        emulator,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    assert_eq!(status, 200, "ReceiveMessage answered {status} {body}");
    body["Messages"].as_array().cloned().unwrap_or_default()
}

/// The value of one dead-letter attribute of `message`, or `None` when the
/// message does not carry it.
pub(crate) fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(str::to_string)
}

/// An API destination over an API-key connection that has settled into
/// `AUTHORIZED`, so the destination is `ACTIVE`; answers its ARN.
pub(crate) async fn given_api_destination(emulator: &Emulator, name: &str) -> String {
    given_api_destination_at(emulator, name, ENDPOINT).await
}

/// [`given_api_destination`] with the `InvocationEndpoint` the caller names, for
/// a suite that needs its own endpoint.
pub(crate) async fn given_api_destination_at(
    emulator: &Emulator,
    name: &str,
    endpoint: &str,
) -> String {
    let (status, body) = events(
        emulator,
        "CreateConnection",
        &json!({
            "Name": format!("{name}-connection"),
            "AuthorizationType": "API_KEY",
            "AuthParameters": {
                "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "sekrit" },
            },
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateConnection answered {status} {body}");
    let connection_arn = member(name, &body, "ConnectionArn");

    let (status, body) = events(
        emulator,
        "CreateApiDestination",
        &json!({
            "Name": name,
            "ConnectionArn": connection_arn,
            "InvocationEndpoint": endpoint,
            "HttpMethod": "POST",
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateApiDestination answered {status} {body}");
    let arn = member(name, &body, "ApiDestinationArn");
    // A destination is ACTIVE only once the connection it uses is authorized,
    // which the connection settles into on its own: the state is LOOKED AT again
    // until it holds rather than waited out, so the suite neither depends on how
    // fast the host runs nor on the span the settling takes.
    let deadline = ::std::time::Instant::now() + ACTIVE_DEADLINE;
    loop {
        let (status, body) = events(emulator, "DescribeApiDestination", &json!({ "Name": name })).await;
        assert_eq!(status, 200, "DescribeApiDestination answered {status} {body}");
        if body["ApiDestinationState"] == json!(state::API_DESTINATION_STATE_ACTIVE) {
            return arn;
        }
        assert!(
            ::std::time::Instant::now() < deadline,
            "case {name:?}: the destination has to be ACTIVE before a delivery is attempted, \
             got {body}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How long the loop above looks for an `ACTIVE` destination before giving up:
/// generous room over the span a connection's authorization settles in, so only
/// a destination that never becomes active fails the suite.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long that loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: the case, the `MaximumRetryAttempts` the target carries (`None` for
/// no `RetryPolicy` at all), how many RETRIES the dead-letter message must
/// report, and the condition it must name as what stopped the delivery.
type RetryRow = (&'static str, Option<i32>, &'static str, Option<&'static str>);

/// An api-destination delivery is attempted and retried within the target's
/// retry policy, and the event it never delivered is dead-lettered with what the
/// retries came to — so two targets differing only in their retry allowance are
/// told apart.
#[tokio::test]
async fn an_api_destination_delivery_is_retried_within_the_targets_policy_then_dead_lettered() {
    // (case, MaximumRetryAttempts, the RETRY_ATTEMPTS the dead letter reports,
    //  the EXHAUSTED_RETRY_CONDITION it names)
    let rows: [RetryRow; 5] = [
        (
            // No `RetryPolicy` at all is delivered under the defaults AWS
            // documents — 185 retries over 24 hours — so the age bound is the
            // one the retries reach first.
            "no retry policy at all",
            None,
            "16",
            Some("MaximumEventAgeInSeconds"),
        ),
        (
            "a policy allowing no retry",
            Some(0),
            "0",
            Some("MaximumRetryAttempts"),
        ),
        (
            "a policy allowing one retry",
            Some(1),
            "1",
            Some("MaximumRetryAttempts"),
        ),
        (
            "a policy allowing three retries",
            Some(3),
            "3",
            Some("MaximumRetryAttempts"),
        ),
        (
            // An allowance larger than the event's age bound admits: the age is
            // what the retries reach first, so it is the condition reported and
            // the allowance is never reached.
            "a policy allowing more retries than the event's age admits",
            Some(50),
            "16",
            Some("MaximumEventAgeInSeconds"),
        ),
    ];

    let emulator = emulator();
    let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": "hooks" })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let destination_arn = given_api_destination(&emulator, "orders-hook").await;

    // One rule, one target per row — each with its OWN dead-letter queue, so the
    // attempts of one row are never read off another's.
    let mut queues: Vec<(&str, String, &str, Option<&str>)> = Vec::new();
    let mut targets: Vec<Value> = Vec::new();
    for (index, (case, allowance, expected, condition)) in rows.iter().enumerate() {
        let (queue, queue_arn) = given_queue(&emulator, &format!("dlq-{index}")).await;
        let mut target = json!({
            "Id": format!("t{index}"),
            "Arn": destination_arn,
            "DeadLetterConfig": { "Arn": queue_arn },
        });
        if let Some(allowance) = allowance {
            target["RetryPolicy"] = json!({ "MaximumRetryAttempts": allowance });
        }
        targets.push(target);
        queues.push((case, queue, expected, *condition));
    }
    let (status, body) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "hook-rule",
            "EventBusName": "hooks",
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
    let rule_arn = member("rule", &body, "RuleArn");
    let (status, body) = events(
        &emulator,
        "PutTargets",
        &json!({ "Rule": "hook-rule", "EventBusName": "hooks", "Targets": targets }),
    )
    .await;
    assert_eq!(status, 200, "PutTargets answered {status} {body}");
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "PutTargets reported a failed entry: {body}"
    );

    let (status, body) = events(
        &emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": "hooks",
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "hook": "one" }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutEvents answered {status} {body}");
    let event_id = body["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("no EventId in {body}"))
        .to_string();

    for (case, queue, expected, condition) in &queues {
        let messages = drain(&emulator, queue).await;
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: the event no attempt delivered is dead lettered, got {messages:?}"
        );
        let message = &messages[0];
        let delivered: Value = serde_json::from_str(message["Body"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the dead letter is not the event envelope ({error})")
            });
        assert_eq!(
            delivered["id"],
            json!(event_id),
            "case {case:?}: the dead letter carries the event that was not delivered, got \
             {delivered}"
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
        assert_eq!(
            attribute(message, "RETRY_ATTEMPTS").as_deref(),
            Some(*expected),
            "case {case:?}: expected {expected} retries reported, got {message}"
        );
        assert_eq!(
            attribute(message, "EXHAUSTED_RETRY_CONDITION").as_deref(),
            *condition,
            "case {case:?}: expected the stopping condition {condition:?}, got {message}"
        );
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(state::UNKNOWN_CONDITION),
            "case {case:?}: a failure the classification does not place carries the unplaced \
             condition, got {message}"
        );
        let error = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
        assert!(
            error.contains(ENDPOINT),
            "case {case:?}: the dead letter says where the attempts went, got {error:?}"
        );
        let attempts: i32 = expected.parse::<i32>().unwrap_or_default() + 1;
        assert!(
            error.contains(&format!("{attempts} attempt(s)")),
            "case {case:?}: the dead letter says how many attempts were made, expected \
             {attempts}, got {error:?}"
        );
    }

    // Every handover is recorded as a FAILED invocation of this rule — one per
    // target, never one per attempt.
    let recorded: Vec<state::InvocationOutcome> = emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .invocations
        .iter()
        .map(|invocation| invocation.outcome.clone())
        .collect();
    assert_eq!(
        recorded.len(),
        rows.len(),
        "one invocation is recorded per target, got {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .all(|outcome| matches!(outcome, state::InvocationOutcome::Failed { .. })),
        "every api-destination delivery to an endpoint nothing answered failed, got {recorded:?}"
    );
}

/// A target whose ARN names no API destination this account holds is dead
/// lettered with no retry accounting at all: nothing was attempted, so there is
/// nothing to report retries for.
#[tokio::test]
async fn a_target_naming_no_api_destination_is_dead_lettered_without_retries() {
    let emulator = emulator();
    let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": "hooks" })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let (queue, queue_arn) = given_queue(&emulator, "ghost-dlq").await;
    // An ARN of the modelled api-destination form for a destination that was
    // never created.
    let ghost = state::api_destination_arn(REGION, "ghost", "00000000-0000-0000-0000-000000000000");

    let (status, body) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "ghost-rule",
            "EventBusName": "hooks",
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
    let (status, body) = events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": "ghost-rule",
            "EventBusName": "hooks",
            "Targets": [{
                "Id": "t1",
                "Arn": ghost,
                "DeadLetterConfig": { "Arn": queue_arn },
                "RetryPolicy": { "MaximumRetryAttempts": 2 },
            }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutTargets answered {status} {body}");

    let (status, body) = events(
        &emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": "hooks",
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "hook": "ghost" }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutEvents answered {status} {body}");

    let messages = drain(&emulator, &queue).await;
    assert_eq!(
        messages.len(),
        1,
        "case a target naming no destination: the event is dead lettered, got {messages:?}"
    );
    let message = &messages[0];
    assert_eq!(
        attribute(message, "ERROR_CODE").as_deref(),
        Some(state::NO_RESOURCE),
        "case a target naming no destination: the condition is the one a resource that is not \
         there carries, got {message}"
    );
    assert_eq!(
        attribute(message, "RETRY_ATTEMPTS"),
        None,
        "case a target naming no destination: nothing was retried, so no retry count is \
         reported, got {message}"
    );
    assert_eq!(
        attribute(message, "EXHAUSTED_RETRY_CONDITION"),
        None,
        "case a target naming no destination: no retry allowance was used up, got {message}"
    );
    let error = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
    assert!(
        error.contains(&ghost),
        "case a target naming no destination: the dead letter names the target that could not \
         be invoked, got {error:?}"
    );
}
