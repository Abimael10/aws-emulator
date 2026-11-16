// SPDX-License-Identifier: CC0-1.0

//! WHICH of a target's two retry bounds ended a delivery whose endpoint ANSWERED
//! a retryable status, and what the dead letter reports about it — end to end
//! over the wire, with EventBridge, AWS Lambda and Amazon SQS on one engine and
//! the destination's `InvocationEndpoint` pointing at a function URL that answers
//! the status its path names.
//!
//! The RULE pinned here is the one the model states on
//! `RetryPolicy$MaximumRetryAttempts` ("retry attempts continue until either the
//! maximum number of attempts is made or until the duration of the
//! `MaximumEventAgeInSeconds` is met"), read together with what a dead letter
//! reports for an endpoint that answered: a delivery whose endpoint answers a
//! retryable status is tried again under BOTH bounds at once and ends on
//! whichever is met first, and the dead letter then carries
//!
//! * `ERROR_CODE` `UNKNOWN` — the condition of a failure that is not the endpoint
//!   refusing the request,
//! * `ERROR_MESSAGE` stating the status the endpoint answered and the payload it
//!   answered with, so a consumer recovers the HTTP status from it,
//! * `EXHAUSTED_RETRY_CONDITION` naming the bound that actually ended the
//!   delivery — `MaximumEventAgeInSeconds` or `MaximumRetryAttempts` — and
//! * `RETRY_ATTEMPTS` the retries that were actually made,
//!
//! beside `RULE_ARN` and `TARGET_ARN`. Telling the two bounds apart is the whole
//! point: an endpoint outage that used a target's attempts up and one that
//! outlived the event are the same dead letter otherwise.
//!
//! `tests/all/api_destination_status_classification.rs` spans the STATUS the
//! endpoint answers (a delivery, refusals across the 4xx range, a server error)
//! with one retry policy; `tests/all/api_destination_retry_bounds.rs` spans the
//! POLICY against an endpoint nothing answers at, where there is no status to
//! report. This suite is the product of the two: an endpoint that answers, over
//! policies and event ages that make each bound in turn the binding one, which is
//! the only place the status-bearing message and the bound that ended the
//! delivery are reported together.
//!
//! The rows differ along both members of the policy (an allowance of none, of
//! one, and one no age bound admits; the smallest age bound the model admits and
//! the largest), along the event's own age (fresh, at its bound, and a day past
//! it) and along the retryable status the endpoint answers, so no row is the
//! recorded case re-stated. The two bounds are driven by two tests, so neither
//! waits on the other's attempts.
//!
//! How many RETRIES the pacing fits inside an age bound is the other table's
//! subject (`tests/all/api_destination_retry_bounds.rs` spans it from none to
//! sixteen), so no row here is chosen to make a long retry sequence: every row
//! reaches its bound within two attempts at the endpoint, which keeps the suite
//! inside the request's own endpoint-wait budget however slowly the host runs the
//! function behind that endpoint.
//!
//! The harness (the router, the echo function URL, the connection, the queues) is
//! `tests/all/api_destination_status_classification.rs`'s, which is the suite for
//! the status classification itself, and the event's age is expressed with
//! `tests/all/api_destination_retry_bounds.rs`'s `published_at`, which is the
//! suite for the bounds themselves.

use serde_json::{json, Value};

use crate::api_destination_retry_bounds::published_at;
use crate::api_destination_status_classification::{
    attribute, drain, emulator, events, given_basic_connection, given_dead_letter_queue,
    given_echo_endpoint, member, until_dead_lettered, Emulator, DETAIL_TYPE, HTTP_METHOD,
};

/// The member names `EXHAUSTED_RETRY_CONDITION` names a bound by, which are the
/// two members of `RetryPolicy` (`smithy_query` query="shape" shape="RetryPolicy":
/// `MaximumRetryAttempts` @range 0..185, `MaximumEventAgeInSeconds` @range
/// 60..86400).
const AGE_CONDITION: &str = "MaximumEventAgeInSeconds";
const ATTEMPTS_CONDITION: &str = "MaximumRetryAttempts";

/// One row: the retryable status the endpoint answers, the `RetryPolicy` the
/// target carries, how old the event is when it is published, and what the dead
/// letter has to report — the retries made and the bound that ended the
/// delivery.
struct Row {
    case: &'static str,
    status: u16,
    maximum_retry_attempts: i32,
    maximum_event_age_in_seconds: i32,
    aged_seconds: u64,
    retries: &'static str,
    condition: &'static str,
}

/// The rows whose AGE bound is the binding one: the allowance is the largest the
/// model admits, so nothing but the event's own age can end these deliveries.
///
/// Both rows have REACHED their bound where the delivery begins — one exactly and
/// one by a day — so no retry is due at all, whatever the pacing between retries
/// is: a retry is owed only while the bound is still ahead.
fn age_bounded_rows() -> [Row; 2] {
    [
        Row {
            case: "the largest age bound the model admits, an event that has reached it",
            status: 502,
            maximum_retry_attempts: 185,
            maximum_event_age_in_seconds: 86_400,
            aged_seconds: 86_400,
            retries: "0",
            condition: AGE_CONDITION,
        },
        Row {
            case: "the smallest age bound the model admits, an event long past it",
            status: 500,
            maximum_retry_attempts: 185,
            maximum_event_age_in_seconds: 60,
            aged_seconds: 90_000,
            retries: "0",
            condition: AGE_CONDITION,
        },
    ]
}

/// The rows whose retry ALLOWANCE is the binding one: an age bound wide enough
/// that the attempts run out first.
fn allowance_bounded_rows() -> [Row; 2] {
    [
        Row {
            // A day of age bound ahead of a fresh event, so the allowance is what
            // ends this one — and it ends it having RETRIED, which is the arm the
            // age rows above cannot report.
            case: "an allowance of one retry under the widest age bound",
            status: 504,
            maximum_retry_attempts: 1,
            maximum_event_age_in_seconds: 86_400,
            aged_seconds: 0,
            retries: "1",
            condition: ATTEMPTS_CONDITION,
        },
        Row {
            case: "a policy allowing no retry at all",
            status: 503,
            maximum_retry_attempts: 0,
            maximum_event_age_in_seconds: 60,
            aged_seconds: 0,
            retries: "0",
            condition: ATTEMPTS_CONDITION,
        },
    ]
}

/// Drive ONE row: its own destination at the endpoint answering its status, its
/// own rule, target and dead-letter queue, one published event, and then every
/// assertion the rule owes that row's dead letter.
async fn the_dead_letter_reports_the_binding_bound(
    emulator: &Emulator,
    endpoint: &str,
    connection_arn: &str,
    index: usize,
    row: &Row,
) {
    let case = row.case;
    let status = row.status;
    // One destination, rule, source and queue per row, so no row reads another's
    // attempts or another's dead letter.
    let source = format!("com.demo.retry-bound.{index}");
    let destination_name = format!("bound-hook-{index}");
    let rule_name = format!("bound-rule-{index}");

    let created = events(
        emulator,
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
    let (dead_letter_url, dead_letter_arn) =
        given_dead_letter_queue(emulator, case, &format!("bound-dlq-{index}")).await;

    let rule = events(
        emulator,
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
        emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": rule_name,
            "Targets": [{
                "Id": "hook",
                "Arn": destination_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": row.maximum_retry_attempts,
                    "MaximumEventAgeInSeconds": row.maximum_event_age_in_seconds,
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

    let published = events(
        emulator,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "row": index }).to_string(),
                "Time": published_at(row.aged_seconds),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {published}"
    );
    let event_id = member(case, &published["Entries"][0], "EventId");

    let dead_letters = until_dead_lettered(emulator, case, &dead_letter_url).await;
    assert_eq!(
        dead_letters.len(),
        1,
        "case {case:?}: an endpoint answering HTTP {status} dead-letters the event once, got \
         {dead_letters:?}"
    );
    let message = &dead_letters[0];
    let delivered: Value = serde_json::from_str(message["Body"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the dead letter is not the event envelope ({error})")
        });
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

    // A retryable answer is not the endpoint refusing the request, so the
    // condition is the one every other delivery failure carries.
    assert_eq!(
        attribute(message, "ERROR_CODE").as_deref(),
        Some("UNKNOWN"),
        "case {case:?}: an endpoint answering HTTP {status} is retryable, not a refusal, got \
         {message}"
    );
    // WHICH bound ended the delivery, and the retries it made before it.
    assert_eq!(
        attribute(message, "EXHAUSTED_RETRY_CONDITION").as_deref(),
        Some(row.condition),
        "case {case:?}: expected the bound {:?} to be named as what ended the delivery, got \
         {message}",
        row.condition
    );
    assert_eq!(
        attribute(message, "RETRY_ATTEMPTS").as_deref(),
        Some(row.retries),
        "case {case:?}: expected {} retries reported, got {message}",
        row.retries
    );

    // The status the endpoint answered is readable back out of the message,
    // which is what a consumer classifies the outage by, and what follows it is
    // the endpoint's OWN answer rather than anything composed about the
    // attempts.
    let reported = attribute(message, "ERROR_MESSAGE").unwrap_or_default();
    let opening = format!("ApiDestination returned HTTP status {status} with payload: ");
    let payload = reported.strip_prefix(&opening).unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to open {opening:?}, got {reported:?}")
    });
    let echoed: Value = serde_json::from_str(payload).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the endpoint's answer as the payload, got {error}")
    });
    let sent: Value = serde_json::from_str(echoed["body"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: expected the envelope as the echoed body, got {error}")
        });
    assert_eq!(
        sent["id"],
        json!(event_id),
        "case {case:?}: the endpoint answered the event that was dead-lettered, got {sent}"
    );

    // And nothing else was dead-lettered once the delivery ended: the bound that
    // ended it ended it.
    let further = drain(emulator, case, &dead_letter_url).await;
    assert!(
        further.is_empty(),
        "case {case:?}: the delivery made no further attempt after the bound was met, got \
         {further:?}"
    );
}

/// RULE: a delivery whose endpoint answers a retryable status is retried under
/// the target's two bounds at once; where the EVENT'S AGE is the binding one, the
/// dead letter names it, reports the retries the age left room for, and carries
/// the status and payload the endpoint answered under `ERROR_CODE` `UNKNOWN`.
#[tokio::test]
async fn the_age_bound_that_ends_an_answered_retryable_delivery_is_the_one_reported() {
    let emulator = emulator();
    let endpoint = given_echo_endpoint(&emulator).await;
    let connection_arn = given_basic_connection(&emulator).await;
    for (index, row) in age_bounded_rows().iter().enumerate() {
        the_dead_letter_reports_the_binding_bound(
            &emulator,
            &endpoint,
            &connection_arn,
            index,
            row,
        )
        .await;
    }
}

/// RULE: the same delivery under an age bound wide enough to leave the ALLOWANCE
/// the binding one ends on the allowance instead, and the dead letter names that
/// bound with the retries it made — including none at all for a policy that
/// allows no retry.
#[tokio::test]
async fn the_attempt_allowance_that_ends_an_answered_retryable_delivery_is_the_one_reported() {
    let emulator = emulator();
    let endpoint = given_echo_endpoint(&emulator).await;
    let connection_arn = given_basic_connection(&emulator).await;
    for (index, row) in allowance_bounded_rows().iter().enumerate() {
        the_dead_letter_reports_the_binding_bound(
            &emulator,
            &endpoint,
            &connection_arn,
            index,
            row,
        )
        .await;
    }
}
