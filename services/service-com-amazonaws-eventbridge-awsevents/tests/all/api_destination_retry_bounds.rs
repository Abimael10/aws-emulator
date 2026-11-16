// SPDX-License-Identifier: CC0-1.0

//! WHICH retry bound ends a delivery that keeps failing, end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`, with SQS mounted on the same engine so
//! the dead letter is read where a client reads it.
//!
//! The RULE asserted here is the one the model states on
//! `RetryPolicy$MaximumRetryAttempts`: "retry attempts continue until either the
//! maximum number of attempts is made or until the duration of the
//! `MaximumEventAgeInSeconds` is met". So a retryable delivery failure is under
//! TWO bounds at once, the delivery ends on whichever is met first, and the dead
//! letter reports that bound (`EXHAUSTED_RETRY_CONDITION`) beside the retries it
//! made (`RETRY_ATTEMPTS`) — which is the only thing that tells a consumer of the
//! queue an endpoint outage that used the attempts up from one that outlived the
//! event. A target that configures neither member is under the defaults AWS
//! documents for it.
//!
//! The age is the EVENT's age: it runs from the time the event carries, so an
//! entry published with a `Time` in the past is that much closer to its bound
//! before the first attempt, and one already past it is dead-lettered on the age
//! condition with no retry at all.
//!
//! The table spans the class rather than one case: the rows differ along both
//! members of the policy (an allowance of none, of a few, and one no age bound
//! admits; the smallest age bound the model admits, one in the middle, the
//! largest, and none at all) and along the event's own age (fresh, part-way to
//! its bound, and already past it). Every row shares one endpoint that nothing
//! answers at, so what differs between rows is only the policy and the age.
//!
//! The harness (the router, the queues, the destination) is
//! `tests/all/api_destination_delivery.rs`'s, which is the suite for the
//! attempts themselves.

use serde_json::{json, Value};

use crate::api_destination_delivery::{
    attribute, drain, emulator, events, given_api_destination, given_queue, member, Emulator,
    ENDPOINT,
};

/// The region every request below is signed for, as the shared harness signs
/// them.
const REGION: &str = "us-west-1";

/// The detail type every entry below carries; the `Source` is what tells one
/// row's event from another's.
const DETAIL_TYPE: &str = "Hook Fired";

/// One row of the table: the case, the `MaximumRetryAttempts` and
/// `MaximumEventAgeInSeconds` the target carries (`None` for a target with no
/// `RetryPolicy` at all), how many seconds old the event is when it is
/// published, the retries the dead letter must report, and the bound it must
/// name as what ended the delivery.
struct Row {
    case: &'static str,
    maximum_retry_attempts: Option<i32>,
    maximum_event_age_in_seconds: Option<i32>,
    aged_seconds: u64,
    retries: &'static str,
    condition: &'static str,
}

/// The retries the pacing fits inside each age bound below, worked out from the
/// schedule the emulator states ([`service_com_amazonaws_eventbridge_awsevents::state::retry_bounds`]):
/// retry `n` sits `2^n - 1` seconds after the first attempt, so the last retry
/// inside a bound is the largest `n` with `age + 2^n - 1` within it.
fn rows() -> [Row; 8] {
    [
        Row {
            case: "a policy allowing no retry at all",
            maximum_retry_attempts: Some(0),
            maximum_event_age_in_seconds: Some(60),
            aged_seconds: 0,
            retries: "0",
            condition: "MaximumRetryAttempts",
        },
        Row {
            case: "an allowance the age bound leaves room for",
            maximum_retry_attempts: Some(2),
            maximum_event_age_in_seconds: Some(900),
            aged_seconds: 0,
            retries: "2",
            condition: "MaximumRetryAttempts",
        },
        Row {
            // 1 + 2 + 4 + 8 + 16 = 31 seconds of pacing fits; the sixth retry
            // would sit at 63, past the smallest bound the model admits.
            case: "the smallest age bound the model admits",
            maximum_retry_attempts: Some(185),
            maximum_event_age_in_seconds: Some(60),
            aged_seconds: 0,
            retries: "5",
            condition: "MaximumEventAgeInSeconds",
        },
        Row {
            // The sixth retry sits at 63 and fits; the seventh, at 127, does
            // not.
            case: "an age bound in the middle of the range",
            maximum_retry_attempts: Some(185),
            maximum_event_age_in_seconds: Some(120),
            aged_seconds: 0,
            retries: "6",
            condition: "MaximumEventAgeInSeconds",
        },
        Row {
            case: "the largest age bound the model admits",
            maximum_retry_attempts: Some(185),
            maximum_event_age_in_seconds: Some(86_400),
            aged_seconds: 0,
            retries: "16",
            condition: "MaximumEventAgeInSeconds",
        },
        Row {
            // No policy at all is delivered under the defaults AWS documents:
            // 185 retries over 24 hours, so the age is the binding bound.
            case: "no retry policy at all",
            maximum_retry_attempts: None,
            maximum_event_age_in_seconds: None,
            aged_seconds: 0,
            retries: "16",
            condition: "MaximumEventAgeInSeconds",
        },
        Row {
            // Published 86,000 seconds ago against a 86,400 second bound: the
            // eighth retry sits at 255 and fits, the ninth at 511 does not.
            case: "an event part-way to its age bound when it is published",
            maximum_retry_attempts: Some(185),
            maximum_event_age_in_seconds: Some(86_400),
            aged_seconds: 86_000,
            retries: "8",
            condition: "MaximumEventAgeInSeconds",
        },
        Row {
            case: "an event already older than its age bound",
            maximum_retry_attempts: Some(185),
            maximum_event_age_in_seconds: Some(60),
            aged_seconds: 90_000,
            retries: "0",
            condition: "MaximumEventAgeInSeconds",
        },
    ]
}

/// A rule on `bus` matching `source`, with one api-destination target carrying
/// `row`'s retry policy and its own dead-letter queue; answers the rule's ARN
/// and the queue's url.
async fn given_rule(
    emulator: &Emulator,
    bus: &str,
    index: usize,
    row: &Row,
    destination_arn: &str,
    source: &str,
) -> (String, String) {
    let name = format!("retry-rule-{index}");
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": json!({ "source": [source] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: PutRule answered {status} {body}",
        row.case
    );
    let rule_arn = member(row.case, &body, "RuleArn");
    let (queue, queue_arn) = given_queue(emulator, &format!("retry-dlq-{index}")).await;
    let mut target = json!({
        "Id": "t1",
        "Arn": destination_arn,
        "DeadLetterConfig": { "Arn": queue_arn },
    });
    let mut policy = ::serde_json::Map::new();
    if let Some(attempts) = row.maximum_retry_attempts {
        policy.insert("MaximumRetryAttempts".to_string(), json!(attempts));
    }
    if let Some(age) = row.maximum_event_age_in_seconds {
        policy.insert("MaximumEventAgeInSeconds".to_string(), json!(age));
    }
    if !policy.is_empty() {
        target["RetryPolicy"] = Value::Object(policy);
    }
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({ "Rule": name, "EventBusName": bus, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: PutTargets answered {status} {body}",
        row.case
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {:?}: PutTargets reported a failed entry: {body}",
        row.case
    );
    (rule_arn, queue)
}

/// `aged_seconds` before now, as an entry's `Time` carries it (epoch seconds,
/// which `awsJson` takes a timestamp as).
pub fn published_at(aged_seconds: u64) -> f64 {
    let now = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs();
    (now.saturating_sub(aged_seconds)) as f64
}

/// Each row's delivery ends on the bound the row's policy and the event's age
/// make the binding one, and the dead letter names that bound beside the
/// retries it made.
#[tokio::test]
async fn the_bound_that_ends_a_retried_delivery_is_the_one_the_dead_letter_names() {
    let emulator = emulator();
    let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": "retries" })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let destination_arn = given_api_destination(&emulator, "retry-hook").await;

    for (index, row) in rows().iter().enumerate() {
        // One rule, one source and one dead-letter queue per row, so no row
        // reads another's attempts.
        let source = format!("com.demo.retries.{index}");
        let (rule_arn, queue) =
            given_rule(&emulator, "retries", index, row, &destination_arn, &source).await;

        let (status, body) = events(
            &emulator,
            "PutEvents",
            &json!({
                "Entries": [{
                    "EventBusName": "retries",
                    "Source": source,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "row": index }).to_string(),
                    "Time": published_at(row.aged_seconds),
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: PutEvents answered {status} {body}",
            row.case
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {:?}: PutEvents reported a failed entry: {body}",
            row.case
        );
        let event_id = member(row.case, &body["Entries"][0], "EventId");

        let messages = drain(&emulator, &queue).await;
        assert_eq!(
            messages.len(),
            1,
            "case {:?}: expected exactly one dead letter, got {messages:?}",
            row.case
        );
        let message = &messages[0];
        let delivered: Value = ::serde_json::from_str(
            message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {:?}: no Body in {message}", row.case)),
        )
        .unwrap_or_else(|error| {
            panic!(
                "case {:?}: the dead letter's body is not the event envelope ({error}): {message}",
                row.case
            )
        });
        assert_eq!(
            delivered["id"],
            json!(event_id),
            "case {:?}: the dead letter carries the event that was not delivered, got {delivered}",
            row.case
        );
        assert_eq!(
            attribute(message, "RULE_ARN").as_deref(),
            Some(rule_arn.as_str()),
            "case {:?}: the dead letter names the rule, got {message}",
            row.case
        );
        assert_eq!(
            attribute(message, "TARGET_ARN").as_deref(),
            Some(destination_arn.as_str()),
            "case {:?}: the dead letter names the target, got {message}",
            row.case
        );
        assert_eq!(
            attribute(message, "RETRY_ATTEMPTS").as_deref(),
            Some(row.retries),
            "case {:?}: expected {} retries reported, got {message}",
            row.case,
            row.retries
        );
        assert_eq!(
            attribute(message, "EXHAUSTED_RETRY_CONDITION").as_deref(),
            Some(row.condition),
            "case {:?}: expected the bound {:?} to be named, got {message}",
            row.case,
            row.condition
        );
        // The attempts the delivery made are one more than its retries, and the
        // endpoint nothing answered at is where they went.
        let attempts: i32 = row.retries.parse::<i32>().expect("the row counts retries") + 1;
        let error = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
        assert!(
            error.contains(&format!("{attempts} attempt(s)")) && error.contains(ENDPOINT),
            "case {:?}: expected {attempts} attempts at {ENDPOINT} reported, got {error:?}",
            row.case
        );
    }
}

/// An endpoint that ANSWERS and does not acknowledge the event: the dead letter
/// carries AWS's own wording for it, so the status the endpoint answered is
/// readable back out of `ERROR_MESSAGE`, and a 4xx is the endpoint refusing the
/// event — not retried at all, so no retry accounting goes with it.
///
/// The endpoint is this emulator's own events endpoint reached with no operation
/// named, which the protocol refuses with a 4xx: a real answer from something
/// that served the request, as opposed to the transport's refusal of a URL
/// nothing serves (the table above).
#[tokio::test]
async fn an_endpoint_that_answers_a_refusal_is_dead_lettered_with_its_status_and_not_retried() {
    let emulator = emulator();
    let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": "answered" })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let endpoint = format!("https://events.{REGION}.amazonaws.com/");
    let destination_arn =
        crate::api_destination_delivery::given_api_destination_at(&emulator, "answered", &endpoint)
            .await;
    let (queue, queue_arn) = given_queue(&emulator, "answered-dlq").await;
    let source = "com.demo.answered";
    let (status, body) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": "answered-rule",
            "EventBusName": "answered",
            "EventPattern": json!({ "source": [source] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
    let rule_arn = member("answered", &body, "RuleArn");
    let (status, body) = events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": "answered-rule",
            "EventBusName": "answered",
            "Targets": [{
                "Id": "t1",
                "Arn": destination_arn,
                "DeadLetterConfig": { "Arn": queue_arn },
                // An allowance that would admit many retries: an answer the
                // endpoint REFUSED with is not retried whatever it allows.
                "RetryPolicy": { "MaximumRetryAttempts": 185, "MaximumEventAgeInSeconds": 86_400 },
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
                "EventBusName": "answered",
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "hook": "answered" }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutEvents answered {status} {body}");

    let messages = drain(&emulator, &queue).await;
    assert_eq!(
        messages.len(),
        1,
        "expected exactly one dead letter, got {messages:?}"
    );
    let message = &messages[0];
    assert_eq!(
        attribute(message, "RULE_ARN").as_deref(),
        Some(rule_arn.as_str()),
        "the dead letter names the rule, got {message}"
    );
    let error = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
    // The status the endpoint answered and the body it answered with are the
    // only variable parts of AWS's wording.
    let expected = "ApiDestination returned HTTP status 400 with payload: ";
    assert!(
        error.starts_with(expected),
        "expected the status the endpoint answered in AWS's wording, got {error:?}"
    );
    let payload = &error[expected.len()..];
    assert!(
        ::serde_json::from_str::<Value>(payload).is_ok(),
        "expected the body the endpoint answered as the payload, got {payload:?}"
    );
    assert_eq!(
        attribute(message, "ERROR_CODE").as_deref(),
        Some("INVALID_PARAMETER"),
        "an answer the endpoint refused the event with carries its own condition, got {message}"
    );
    // Nothing was retried, so the letter carries no retry accounting at all.
    assert_eq!(
        attribute(message, "RETRY_ATTEMPTS"),
        None,
        "a refusal on the first attempt retried nothing, got {message}"
    );
    assert_eq!(
        attribute(message, "EXHAUSTED_RETRY_CONDITION"),
        None,
        "no retry bound ended a delivery that was never retried, got {message}"
    );
}
