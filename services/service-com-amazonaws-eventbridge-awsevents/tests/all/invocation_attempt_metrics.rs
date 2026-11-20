// SPDX-License-Identifier: CC0-1.0

//! Every ATTEMPT a matched rule's target invocation makes is counted on that
//! rule's own `AWS/Events` `InvocationAttempts`, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) with
//! CloudWatch and the target's own service mounted beside EventBridge on one
//! engine.
//!
//! The RULE asserted here: EventBridge publishes one `InvocationAttempts`
//! datapoint of value 1 per attempt it MADE at a matched rule's target —
//! "InvocationAttempts: the number of times EventBridge attempted invoking a
//! target", the attempt series' total of which `SuccessfulInvocationAttempts` is
//! the accepted part — so the attempt counts whatever it came to (the target took
//! the event, or refused it), a delivery that was RETRIED counts one per attempt
//! rather than one per delivery, and a handover nothing was attempted for counts
//! none at all. The series are the ones every per-handover counter is published
//! under: the rule's whole identity (`EventBusName` with `RuleName`) and the rule
//! name on its own.
//!
//! That is what tells a client "the event WAS handed over" apart from "nothing
//! was ever dispatched": a delivery accepted on its first attempt, with no retry
//! and nothing parked, leaves `FailedInvocations` and `InvocationsSentToDlq`
//! holding no series at all, and the attempt it made is the only datapoint there
//! is to read it off.
//!
//! The tables span the class rather than one case: the first differs along the
//! BUS the rule lives on (two custom buses, one of them carrying a dot, and the
//! default one), along HOW MANY matching events the round publishes (so a count
//! is a count rather than a flag) and along what the target's own policy does
//! with the delivery (admits it, or admits nothing); the second differs along the
//! retry allowance the target carries, so a delivery that made four attempts is
//! told from one that made one.
//!
//! The metric and dimension names are this crate's own constants; the harness
//! (the router with EventBridge, SQS and CloudWatch on one engine) is the one
//! `target_dispatch` owns.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state;
use service_com_amazonaws_eventbridge_awsevents::state::retry_bounds::RETRY_ATTEMPTS;
use service_com_amazonaws_eventbridge_awsevents::state::{
    DEFAULT_EVENT_BUS_NAME, FAILED_INVOCATIONS_METRIC, INVOCATIONS_METRIC,
    INVOCATIONS_SENT_TO_DLQ_METRIC, INVOCATION_ATTEMPTS_METRIC, MATCHED_EVENTS_METRIC,
    SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC, TRIGGERED_RULES_METRIC,
};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_queue_with_policy, metric_sum, metric_sum_over,
    ordered, published_series, rule_name_dimensions, rule_series, Emulator, DETAIL_TYPE, SOURCE,
};

/// The rule every event below matches, and the one on the same bus that nothing
/// published here matches — so a bus holding two rules counts attempts for the
/// one that was handed an event and nothing for the other.
const MATCHING_RULE: &str = "attempting";
const SIBLING_RULE: &str = "idle";

/// The source the sibling rule pins, which nothing published below carries.
const OTHER_SOURCE: &str = "com.demo.unmatched";

/// The id of the one target each rule carries.
const TARGET_ID: &str = "t1";

/// Create `bus` unless it is the one every account already holds.
async fn given_bus(emulator: &Emulator, case: &str, bus: &str) {
    if bus == DEFAULT_EVENT_BUS_NAME {
        return;
    }
    let (status, created) = events(emulator, "CreateEventBus", &json!({ "Name": bus })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status} {created}"
    );
}

/// A rule called `name` on `bus` pinning `source`, carrying `target` as its one
/// target.
async fn given_rule(
    emulator: &Emulator,
    case: &str,
    bus: &str,
    name: &str,
    source: &str,
    target: Value,
) {
    let pattern = json!({ "source": [source], "detail-type": [DETAIL_TYPE] }).to_string();
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: PutRule answered {status} {body}");
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({ "Rule": name, "EventBusName": bus, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {body}"
    );
}

/// Publish one matching event onto `bus`, carrying `nonce` in its detail.
async fn publish_onto(emulator: &Emulator, case: &str, bus: &str, source: &str, nonce: &str) {
    let (status, body) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": bus,
                "Source": source,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "nonce": nonce }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {body}"
    );
}

/// The value of one dead-letter message attribute, or `None` when the message
/// carries no such attribute.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(str::to_string)
}

/// The `Sum` of `metric` for `rule` on `bus`, asserted to be the same under both
/// dimension sets a per-handover counter is published under: a client reading the
/// rule's whole identity and one reading the rule name alone see one count.
async fn counted(emulator: &Emulator, case: &str, metric: &str, bus: &str, rule: &str) -> f64 {
    let by_identity = metric_sum(emulator, metric, bus, rule).await;
    let by_name = metric_sum_over(emulator, metric, &rule_name_dimensions(rule)).await;
    assert_eq!(
        by_identity, by_name,
        "case {case:?}: expected {metric} for rule {rule} on bus {bus} to read the same under \
         the rule's whole identity and under the rule name alone, got {by_identity} and {by_name}"
    );
    by_identity
}

/// One row of the first table: the bus, how many matching events the round
/// publishes, and whether the target queue's own policy admits the delivery.
struct Row {
    case: &'static str,
    bus: &'static str,
    events: usize,
    admitted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a custom bus, one event the target admits",
            bus: "orders-bus",
            events: 1,
            admitted: true,
        },
        Row {
            case: "the default bus, three events the target admits",
            bus: DEFAULT_EVENT_BUS_NAME,
            events: 3,
            admitted: true,
        },
        Row {
            case: "a custom bus whose name carries a dot, two events the target's policy refuses",
            bus: "payments.bus-2",
            events: 2,
            admitted: false,
        },
    ]
}

#[tokio::test]
async fn every_attempted_handover_counts_one_attempt_whatever_it_came_to() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();
        given_bus(&emulator, case, row.bus).await;
        // The target queue: one whose policy grants this service the send, and
        // one that has granted nothing — the second refuses the delivery the
        // attempt was made for.
        let (target_url, target_arn) = if row.admitted {
            given_queue(&emulator, "deliveries").await
        } else {
            given_queue_with_policy(&emulator, "deliveries", None).await
        };
        let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, "dead-letters").await;
        let target = json!({
            "Id": TARGET_ID,
            "Arn": target_arn,
            // No retry at all, so each handover is exactly one attempt and the
            // count is the handovers'.
            "RetryPolicy": { "MaximumRetryAttempts": 0, "MaximumEventAgeInSeconds": 60 },
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        given_rule(&emulator, case, row.bus, MATCHING_RULE, SOURCE, target).await;
        given_rule(
            &emulator,
            case,
            row.bus,
            SIBLING_RULE,
            OTHER_SOURCE,
            json!({ "Id": TARGET_ID, "Arn": dead_letter_arn }),
        )
        .await;

        for round in 0..row.events {
            publish_onto(&emulator, case, row.bus, SOURCE, &format!("n-{round}")).await;
        }

        // What the attempts came to, observed where a client observes it: the
        // messages the target holds, and the dead letters a refusal parked.
        let handed_over = drain(&emulator, &target_url).await;
        let dead_letters = drain(&emulator, &dead_letter_url).await;
        let (expected_delivered, expected_parked) = if row.admitted {
            (row.events, 0)
        } else {
            (0, row.events)
        };
        assert_eq!(
            handed_over.len(),
            expected_delivered,
            "case {case:?}: expected the target to hold {expected_delivered} event(s), \
             got {handed_over:?}"
        );
        assert_eq!(
            dead_letters.len(),
            expected_parked,
            "case {case:?}: expected {expected_parked} dead letter(s), got {dead_letters:?}"
        );

        // The counters: one attempt per handover whichever way it went, and the
        // outcome counters splitting those same attempts.
        let attempts = f64::from(u32::try_from(row.events).unwrap_or(u32::MAX));
        let (accepted, refused) = if row.admitted {
            (attempts, 0.0)
        } else {
            (0.0, attempts)
        };
        let expectations = [
            (INVOCATION_ATTEMPTS_METRIC, attempts),
            (INVOCATIONS_METRIC, accepted),
            (SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC, accepted),
            (FAILED_INVOCATIONS_METRIC, refused),
            (INVOCATIONS_SENT_TO_DLQ_METRIC, refused),
        ];
        for (metric, want) in expectations {
            let got = counted(&emulator, case, metric, row.bus, MATCHING_RULE).await;
            assert_eq!(
                got, want,
                "case {case:?}: expected {metric} for rule {MATCHING_RULE} on bus {} to sum to \
                 {want}, got {got}",
                row.bus
            );
            // The rule nothing was handed to was matched by nothing, so it has
            // no attempt and no outcome of one.
            let sibling = metric_sum(&emulator, metric, row.bus, SIBLING_RULE).await;
            assert_eq!(
                sibling, 0.0,
                "case {case:?}: expected {metric} for the rule nothing matched to sum to 0, \
                 got {sibling}"
            );
        }
        // The attempts are exactly the accepted ones plus the refused ones: the
        // total of a series whose parts the other two counters are.
        assert_eq!(
            counted(
                &emulator,
                case,
                INVOCATION_ATTEMPTS_METRIC,
                row.bus,
                MATCHING_RULE
            )
            .await,
            accepted + refused,
            "case {case:?}: expected the attempts to be the accepted ones plus the refused ones"
        );

        // And the series CloudWatch holds in the namespace: the matched rule's
        // routing counters, its attempt counter, and the outcome counters its
        // handovers decided — nothing for the rule nothing matched.
        let mut metrics = vec![
            MATCHED_EVENTS_METRIC,
            TRIGGERED_RULES_METRIC,
            INVOCATION_ATTEMPTS_METRIC,
        ];
        if row.admitted {
            metrics.push(INVOCATIONS_METRIC);
            metrics.push(SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC);
        } else {
            metrics.push(FAILED_INVOCATIONS_METRIC);
            metrics.push(INVOCATIONS_SENT_TO_DLQ_METRIC);
        }
        let series = published_series(&emulator).await;
        assert_eq!(
            ordered(&series),
            ordered(&rule_series(row.bus, MATCHING_RULE, &metrics)),
            "case {case:?}: expected one series per dimension set of the matched rule's own \
             counters, got {series:?}"
        );
    }
}

/// The bus the retried deliveries below are published onto.
const RETRY_BUS: &str = "retries";

/// An endpoint this emulator serves nothing at, so every attempt made at it
/// reaches no endpoint and the delivery is retried within the target's policy.
const UNREACHABLE_ENDPOINT: &str = "https://hooks.invalid.example/attempts";

/// The age bound every retried target below carries: far enough off that the
/// target's ATTEMPT ALLOWANCE is the bound the retries reach, and not the age.
const AGE_BOUND_SECONDS: i32 = 900;

/// How long the loop below looks for an `ACTIVE` destination before giving up:
/// generous room over the span a connection's authorization settles in, so only
/// a destination that never becomes active fails the suite.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long that loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// An API destination at [`UNREACHABLE_ENDPOINT`], over an API-key connection
/// that settles into `AUTHORIZED` so the destination is `ACTIVE`; answers its
/// ARN.
async fn given_api_destination(emulator: &Emulator, name: &str) -> String {
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
    let connection_arn = body["ConnectionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no ConnectionArn in {body}"))
        .to_string();
    let (status, body) = events(
        emulator,
        "CreateApiDestination",
        &json!({
            "Name": name,
            "ConnectionArn": connection_arn,
            "InvocationEndpoint": UNREACHABLE_ENDPOINT,
            "HttpMethod": "POST",
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateApiDestination answered {status} {body}");
    let arn = body["ApiDestinationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no ApiDestinationArn in {body}"))
        .to_string();
    // The destination is ACTIVE only once its connection is authorized, which
    // the connection settles into on its own: the state is LOOKED AT again until
    // it holds rather than waited out.
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let (status, body) =
            events(emulator, "DescribeApiDestination", &json!({ "Name": name })).await;
        assert_eq!(status, 200, "DescribeApiDestination answered {status} {body}");
        if body["ApiDestinationState"] == json!(state::API_DESTINATION_STATE_ACTIVE) {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "the destination has to be ACTIVE before a delivery is attempted, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the retry table: the allowance the target carries, the retries the
/// delivery therefore makes, and the attempts that comes to — one more than the
/// retries, because the first attempt is not a retry.
struct RetryRow {
    case: &'static str,
    maximum_retry_attempts: i32,
    retries: i32,
}

fn retry_rows() -> [RetryRow; 3] {
    [
        RetryRow {
            case: "a policy allowing no retry: one attempt",
            maximum_retry_attempts: 0,
            retries: 0,
        },
        RetryRow {
            case: "a policy allowing one retry: two attempts",
            maximum_retry_attempts: 1,
            retries: 1,
        },
        RetryRow {
            case: "a policy allowing three retries: four attempts",
            maximum_retry_attempts: 3,
            retries: 3,
        },
    ]
}

#[tokio::test]
async fn a_retried_delivery_counts_one_attempt_per_attempt_it_made() {
    let emulator = emulator();
    given_bus(&emulator, "retried deliveries", RETRY_BUS).await;
    let destination_arn = given_api_destination(&emulator, "attempt-hook").await;

    for (index, row) in retry_rows().iter().enumerate() {
        let case = row.case;
        // One rule, one source and one dead-letter queue per row, so no row
        // reads another's attempts.
        let rule = format!("retried-{index}");
        let source = format!("{SOURCE}.retried.{index}");
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, &format!("retried-dlq-{index}")).await;
        given_rule(
            &emulator,
            case,
            RETRY_BUS,
            &rule,
            &source,
            json!({
                "Id": TARGET_ID,
                "Arn": destination_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
                "RetryPolicy": {
                    "MaximumRetryAttempts": row.maximum_retry_attempts,
                    "MaximumEventAgeInSeconds": AGE_BOUND_SECONDS,
                },
            }),
        )
        .await;

        publish_onto(&emulator, case, RETRY_BUS, &source, "retried").await;

        // What the attempts came to: nothing was delivered, so the event is on
        // the target's dead-letter queue, reporting the retries it made.
        let dead_letters = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            dead_letters.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {dead_letters:?}"
        );
        assert_eq!(
            attribute(&dead_letters[0], RETRY_ATTEMPTS),
            Some(row.retries.to_string()),
            "case {case:?}: expected the dead letter to report {} retries, got {:?}",
            row.retries,
            dead_letters[0]
        );

        // The counters: one attempt per attempt the delivery made — the first
        // one and each retry of it — against the ONE delivery that failed and
        // the ONE event it parked.
        let attempts = f64::from(row.retries + 1);
        let expectations = [
            (INVOCATION_ATTEMPTS_METRIC, attempts),
            (FAILED_INVOCATIONS_METRIC, 1.0),
            (INVOCATIONS_SENT_TO_DLQ_METRIC, 1.0),
            (INVOCATIONS_METRIC, 0.0),
            (SUCCESSFUL_INVOCATION_ATTEMPTS_METRIC, 0.0),
        ];
        for (metric, want) in expectations {
            let got = counted(&emulator, case, metric, RETRY_BUS, &rule).await;
            assert_eq!(
                got, want,
                "case {case:?}: expected {metric} for rule {rule} to sum to {want}, got {got}"
            );
        }
    }
}
