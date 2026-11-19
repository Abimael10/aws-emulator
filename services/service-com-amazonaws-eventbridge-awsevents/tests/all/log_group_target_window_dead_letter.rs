// SPDX-License-Identifier: CC0-1.0

//! What a rule target's DEAD-LETTER queue receives when the CloudWatch Logs log group
//! a target names would not hold the event, end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target`) through the real `MultiServiceRouter`, with
//! CloudWatch Logs and Amazon SQS mounted on the same engine.
//!
//! The RULE asserted here: a delivery whose `PutLogEvents` wrote no log event is
//! dead-lettered in EventBridge's OWN vocabulary — `ERROR_CODE` is one of
//! EventBridge's delivery conditions and `ERROR_MESSAGE` is EventBridge's sentence
//! for the call it could not complete, "Could not complete putLogEvents call for
//! <logGroupName>.", never the shape name or the prose of the answer the write came
//! back with — and WHICH condition follows from whether the group can ever take that
//! event: an instant the group's ingestion window has already left behind is a
//! permanent refusal, retried not at all and reported as the parameter the service
//! would not take, while an instant AHEAD of that window is retried under the
//! target's `RetryPolicy` and reported as the batch rejection it is, with the retries
//! it made and the bound that ended them beside it.
//!
//! The window is CloudWatch Logs' own: "events more than 2 hours in the future are
//! rejected while processing remaining valid events. Events older than 14 days or
//! preceding the log group's retention period are rejected while processing remaining
//! valid events" (`PutLogEvents`). So the first table's rows differ along where the
//! event's own `Time` falls relative to it — inside it, near the far edge of its
//! past, past the 14-day horizon, far past it, before the group's own retention
//! period, ahead of the window and far ahead of it — which is what the rule
//! generalises over; one recorded case is three of these rows.
//!
//! The second table varies the `RetryPolicy` instead (`MaximumRetryAttempts`
//! `range 0..185`, `MaximumEventAgeInSeconds` `range 60..86400`, from
//! `smithy_query(query="operation", operation="PutTargets")`), so what it pins is
//! that the retry accounting on the dead letter is the delivery's OWN: the retries
//! really made, and WHICH of the target's two bounds ended them.
//!
//! `tests/all/log_group_target_delivery.rs` is the suite for what a log-group target
//! RECEIVES and which documents admit the write; this one is the suite for what a
//! write that group refused is reported as.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    rule_arn, DEFAULT_EVENT_BUS_NAME, SERVICE_PRINCIPAL,
};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_rule, rpc, Emulator, DETAIL_TYPE, REGION, SOURCE,
};

/// The rule every row below publishes through.
const RULE: &str = "log-group-window-rule";

/// The id its single target carries.
const TARGET_ID: &str = "log-group";

/// The two calls a log-group delivery is made of, each needing its own grant.
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// The condition EventBridge names on a dead letter for a call whose parameters the
/// target would not take, spelled out here rather than imported from the service so a
/// wrong value in the service would fail this suite.
const INVALID_PARAMETER: &str = "INVALID_PARAMETER";

/// The condition it names for a batch call the target answered without holding the
/// event it carried. Spelled out for the same reason.
const EVENTS_IN_BATCH_REQUEST_REJECTED: &str = "EVENTS_IN_BATCH_REQUEST_REJECTED";

/// The dead-letter attributes that account for a RETRIED delivery, and the two bounds
/// either of which ends one (the members of `RetryPolicy`). Spelled out for the same
/// reason.
const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";
const EXHAUSTED_RETRY_CONDITION: &str = "EXHAUSTED_RETRY_CONDITION";
const MAXIMUM_RETRY_ATTEMPTS: &str = "MaximumRetryAttempts";
const MAXIMUM_EVENT_AGE: &str = "MaximumEventAgeInSeconds";

/// The attributes a dead letter of a delivery nothing retried carries, and no others.
const UNRETRIED_ATTRIBUTES: [&str; 4] = ["ERROR_CODE", "ERROR_MESSAGE", "RULE_ARN", "TARGET_ARN"];

/// Seconds in one hour and in one day, the units the rows state a `Time` offset in.
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// The `detail` every event below carries.
const DETAIL: &str = r#"{"orderId":"A-1"}"#;

/// One CloudWatch Logs call (`Logs_20140328`, awsJson1_1).
async fn logs(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "logs", "Logs_20140328", "1.1", operation, body).await
}

/// A log group called `name` carrying `retention_days` when the row states one, and a
/// resource policy granting this service both calls the write is made of. Answers the
/// ARN `DescribeLogGroups` reports for it, which is the ARN the target carries.
async fn given_log_group(
    emulator: &Emulator,
    case: &str,
    name: &str,
    retention_days: Option<i64>,
) -> String {
    let (status, body) = logs(emulator, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLogGroup answered {status} {body}"
    );
    if let Some(days) = retention_days {
        let (status, body) = logs(
            emulator,
            "PutRetentionPolicy",
            &json!({ "logGroupName": name, "retentionInDays": days }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRetentionPolicy answered {status} {body}"
        );
    }
    let (status, body) = logs(
        emulator,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLogGroups answered {status} {body}"
    );
    let arn = body["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeLogGroups reported no ARN: {body}"))
        .to_string();
    // The grant is written over everything under the group, which is the resource the
    // write — taken on a STREAM of it — falls under.
    let bare = arn.strip_suffix(":*").unwrap_or(&arn);
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": [CREATE_LOG_STREAM, PUT_LOG_EVENTS],
            "Resource": format!("{bare}:*"),
        }],
    })
    .to_string();
    let (status, body) = logs(
        emulator,
        "PutResourcePolicy",
        &json!({
            "policyName": format!("{name}-events"),
            "policyDocument": document,
            "resourceArn": arn,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutResourcePolicy answered {status} {body}"
    );
    arn
}

/// Every log event the group `group` holds, as `FilterLogEvents` reports them.
async fn written(emulator: &Emulator, case: &str, group: &str) -> Vec<Value> {
    let (status, body) = logs(emulator, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: FilterLogEvents answered {status} {body}"
    );
    body["events"].as_array().cloned().unwrap_or_default()
}

/// Publish ONE matching entry timestamped at `at` (epoch seconds), and answer its
/// `EventId`.
async fn publish_at(emulator: &Emulator, case: &str, at: i64) -> String {
    let (status, body) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": DETAIL,
                "Time": at,
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
    body["Entries"][0]["EventId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: PutEvents minted no EventId: {body}"))
        .to_string()
}

/// Now, in the epoch seconds an `awsJson1_1` timestamp carries, read off the same
/// clock the services stamp their own records with — so no row depends on a date.
fn epoch_now() -> i64 {
    let now = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(now).unwrap_or_default()
}

/// The value of the `name` message attribute of `message`, or `None` when it carries
/// none.
fn attribute(message: &Value, name: &str) -> Option<String> {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// The sentence EventBridge words for a `putLogEvents` it could not complete, whose
/// one variable part is the name of the group the target names.
fn put_log_events_failure(group: &str) -> String {
    format!("Could not complete putLogEvents call for {group}.")
}

/// Wire one row: the group (with its retention), a dead-letter queue, and the rule
/// whose log-group target names both and carries the row's own `RetryPolicy`. Answers
/// the group's ARN and the dead-letter queue's url.
async fn given_wiring(
    emulator: &Emulator,
    case: &str,
    group: &str,
    retention_days: Option<i64>,
    maximum_retry_attempts: i64,
    maximum_event_age: i64,
) -> (String, String) {
    let group_arn = given_log_group(emulator, case, group, retention_days).await;
    let (dead_letter_url, dead_letter_arn) = given_queue(emulator, "log-group-window-dlq").await;
    given_rule(
        emulator,
        RULE,
        json!([{
            "Id": TARGET_ID,
            "Arn": group_arn,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
            "RetryPolicy": {
                "MaximumRetryAttempts": maximum_retry_attempts,
                "MaximumEventAgeInSeconds": maximum_event_age,
            },
        }]),
    )
    .await;
    (group_arn, dead_letter_url)
}

/// What the delivery of one row must come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The group held the event, so nothing is dead-lettered.
    Written,
    /// One dead letter under this condition.
    DeadLettered(&'static str),
}

/// One row of the window table: where the event's own `Time` sits relative to now,
/// the retention the group carries, and what the delivery must come to.
struct Row {
    case: &'static str,
    /// The group this row writes into, named for the row so its name is readable in
    /// the sentence the dead letter carries.
    group: &'static str,
    /// How far the event's `Time` sits from now, in seconds.
    offset_seconds: i64,
    /// `retentionInDays` on the group, which narrows the window it ingests; `None`
    /// for a group that never expires its events.
    retention_days: Option<i64>,
    expected: Expected,
}

/// A log-group delivery the group would not hold is dead-lettered under the condition
/// the event's instant earns and in EventBridge's own sentence for the call it could
/// not complete; one the group holds is dead-lettered nowhere.
#[tokio::test]
async fn a_refused_log_group_write_is_dead_lettered_in_eventbridges_own_vocabulary() {
    let rows = [
        Row {
            case: "an event inside the window is written and dead-letters nothing",
            group: "/aws/events/window-inside",
            offset_seconds: -30,
            retention_days: None,
            expected: Expected::Written,
        },
        Row {
            case: "an event near the far edge of the window's past is still written",
            group: "/aws/events/window-near-edge",
            offset_seconds: -13 * DAY,
            retention_days: None,
            expected: Expected::Written,
        },
        Row {
            case: "an event past the ingestion horizon is refused for good",
            group: "/aws/events/window-fifteen-days",
            offset_seconds: -15 * DAY,
            retention_days: None,
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event far past that horizon is refused the same way",
            group: "/aws/events/window-forty-days",
            offset_seconds: -40 * DAY,
            retention_days: None,
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event preceding the group's own retention period is refused for good",
            group: "/aws/events/window-retention",
            offset_seconds: -3 * DAY,
            retention_days: Some(1),
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Row {
            case: "an event ahead of the window is refused as a rejected batch",
            group: "/aws/events/window-three-hours-ahead",
            offset_seconds: 3 * HOUR,
            retention_days: None,
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
        Row {
            case: "an event far ahead of the window is refused the same way",
            group: "/aws/events/window-a-day-ahead",
            offset_seconds: 25 * HOUR,
            retention_days: None,
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
    ];
    for row in rows {
        let case = row.case;
        // A FRESH emulator per row, so one row's group and queue never decide
        // another's delivery. No retry is allowed, so a retried delivery's
        // accounting is the second test's subject rather than this one's.
        let emulator = emulator();
        let (group_arn, dead_letter_url) =
            given_wiring(&emulator, case, row.group, row.retention_days, 0, 60).await;
        let at = epoch_now() + row.offset_seconds;
        let event_id = publish_at(&emulator, case, at).await;

        let dead_letters = drain(&emulator, &dead_letter_url).await;
        let wanted = usize::from(row.expected != Expected::Written);
        assert_eq!(
            dead_letters.len(),
            wanted,
            "case {case:?}: expected {wanted} dead letter(s), got {dead_letters:?}"
        );
        let held = written(&emulator, case, row.group).await;

        let Expected::DeadLettered(expected_code) = row.expected else {
            // The group holds the event, under the event's own instant — which is
            // what the log event's timestamp carries, in milliseconds.
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected the group to hold the delivered event, got {held:?}"
            );
            assert_eq!(
                held[0]["timestamp"],
                json!(at * 1_000),
                "case {case:?}: expected the log event's timestamp to be the event's own \
                 instant {}, got {:?}",
                at * 1_000,
                held[0]
            );
            let carried: Value =
                serde_json::from_str(held[0]["message"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null);
            assert_eq!(
                carried["id"],
                json!(event_id),
                "case {case:?}: expected the written message to be the envelope of \
                 {event_id:?}, got {:?}",
                held[0]["message"]
            );
            continue;
        };

        assert!(
            held.is_empty(),
            "case {case:?}: a refused write stores nothing, got {held:?}"
        );
        let message = &dead_letters[0];
        let body: Value = serde_json::from_str(message["Body"].as_str().unwrap_or_default())
            .unwrap_or(Value::Null);
        assert_eq!(
            body["id"],
            json!(event_id),
            "case {case:?}: expected the dead letter to carry the envelope of {event_id:?}, \
             got {:?}",
            message["Body"]
        );
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(expected_code),
            "case {case:?}: expected the dead letter's ERROR_CODE {expected_code:?}, got {:?}",
            attribute(message, "ERROR_CODE")
        );
        let expected_message = put_log_events_failure(row.group);
        assert_eq!(
            attribute(message, "ERROR_MESSAGE").as_deref(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the dead letter's ERROR_MESSAGE {expected_message:?}, got \
             {:?}",
            attribute(message, "ERROR_MESSAGE")
        );
        let expected_rule_arn = rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE);
        assert_eq!(
            attribute(message, "RULE_ARN").as_deref(),
            Some(expected_rule_arn.as_str()),
            "case {case:?}: expected the dead letter's RULE_ARN {expected_rule_arn:?}, got {:?}",
            attribute(message, "RULE_ARN")
        );
        assert_eq!(
            attribute(message, "TARGET_ARN").as_deref(),
            Some(group_arn.as_str()),
            "case {case:?}: expected the dead letter's TARGET_ARN {group_arn:?}, got {:?}",
            attribute(message, "TARGET_ARN")
        );
        // A refusal for good retried nothing, so its dead letter accounts for no
        // retries at all and carries those four attributes and no others; the
        // retryable one is retried under the policy and names both.
        if expected_code == INVALID_PARAMETER {
            let carried: Vec<String> = message["MessageAttributes"]
                .as_object()
                .map(|attributes| attributes.keys().cloned().collect())
                .unwrap_or_default();
            assert_eq!(
                carried.len(),
                UNRETRIED_ATTRIBUTES.len(),
                "case {case:?}: expected exactly {:?}, got {carried:?}",
                UNRETRIED_ATTRIBUTES
            );
            for name in UNRETRIED_ATTRIBUTES {
                assert!(
                    carried.iter().any(|held| held == name),
                    "case {case:?}: expected the dead letter to carry {name}, got {carried:?}"
                );
            }
        } else {
            for name in [RETRY_ATTEMPTS, EXHAUSTED_RETRY_CONDITION] {
                assert!(
                    attribute(message, name).is_some(),
                    "case {case:?}: expected the retried delivery's dead letter to carry \
                     {name}, got {message}"
                );
            }
        }
    }
}

/// One row of the retry table: the bounds the target's `RetryPolicy` carries, and the
/// retry accounting the dead letter of a write the group has not taken YET reports.
struct RetryRow {
    case: &'static str,
    group: &'static str,
    /// `MaximumRetryAttempts` (`range 0..185`).
    maximum_retry_attempts: i64,
    /// `MaximumEventAgeInSeconds` (`range 60..86400`).
    maximum_event_age: i64,
    /// `RETRY_ATTEMPTS`: how many retries the delivery really made.
    attempts: &'static str,
    /// `EXHAUSTED_RETRY_CONDITION`: which of the two bounds ended it.
    exhausted: &'static str,
}

/// The retry accounting a dead letter reports is the delivery's own: the retries it
/// made, and WHICH of the target's two bounds ended them — the allowance where it is
/// the smaller, the event's age bound where the retry pacing reaches it first.
#[tokio::test]
async fn an_unheld_log_group_write_reports_the_retries_its_policy_bounded() {
    let rows = [
        RetryRow {
            case: "a target allowing no retry at all is stopped by its allowance",
            group: "/aws/events/retry-none",
            maximum_retry_attempts: 0,
            maximum_event_age: 60,
            attempts: "0",
            exhausted: MAXIMUM_RETRY_ATTEMPTS,
        },
        RetryRow {
            case: "a target allowing two retries makes both and is stopped by its allowance",
            group: "/aws/events/retry-two",
            maximum_retry_attempts: 2,
            maximum_event_age: 60,
            attempts: "2",
            exhausted: MAXIMUM_RETRY_ATTEMPTS,
        },
        RetryRow {
            // 1 + 2 + 4 + 8 + 16 seconds of exponential pacing fall inside the
            // smallest age bound the model admits, and the sixth retry would be made
            // at 63 seconds — past it — so the age bound ends this delivery while the
            // allowance still admits another attempt.
            case: "a target whose allowance outlasts the event's age bound is stopped by the \
                   age bound",
            group: "/aws/events/retry-age-bound",
            maximum_retry_attempts: 185,
            maximum_event_age: 60,
            attempts: "5",
            exhausted: MAXIMUM_EVENT_AGE,
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = emulator();
        let (_, dead_letter_url) = given_wiring(
            &emulator,
            case,
            row.group,
            None,
            row.maximum_retry_attempts,
            row.maximum_event_age,
        )
        .await;
        // Three hours ahead of the window the group ingests, so every attempt this
        // delivery makes is refused and the retry bounds are what end it.
        publish_at(&emulator, case, epoch_now() + 3 * HOUR).await;

        let dead_letters = drain(&emulator, &dead_letter_url).await;
        assert_eq!(
            dead_letters.len(),
            1,
            "case {case:?}: expected exactly one dead letter, got {dead_letters:?}"
        );
        let message = &dead_letters[0];
        assert_eq!(
            attribute(message, "ERROR_CODE").as_deref(),
            Some(EVENTS_IN_BATCH_REQUEST_REJECTED),
            "case {case:?}: expected the dead letter's ERROR_CODE \
             {EVENTS_IN_BATCH_REQUEST_REJECTED:?}, got {:?}",
            attribute(message, "ERROR_CODE")
        );
        assert_eq!(
            attribute(message, RETRY_ATTEMPTS).as_deref(),
            Some(row.attempts),
            "case {case:?}: expected {RETRY_ATTEMPTS} {:?}, got {:?}",
            row.attempts,
            attribute(message, RETRY_ATTEMPTS)
        );
        assert_eq!(
            attribute(message, EXHAUSTED_RETRY_CONDITION).as_deref(),
            Some(row.exhausted),
            "case {case:?}: expected {EXHAUSTED_RETRY_CONDITION} {:?}, got {:?}",
            row.exhausted,
            attribute(message, EXHAUSTED_RETRY_CONDITION)
        );
    }
}
