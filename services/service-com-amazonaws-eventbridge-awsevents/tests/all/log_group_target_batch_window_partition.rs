// SPDX-License-Identifier: CC0-1.0

//! ONE published BATCH whose entries fall on both sides of the ingestion window of the
//! CloudWatch Logs log group a rule target names, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target`) through the real `MultiServiceRouter`,
//! with CloudWatch Logs and Amazon SQS mounted on the same engine.
//!
//! The RULE asserted here: every entry of a `PutEvents` batch is routed and delivered
//! on its OWN, so a log-group target's delivery is decided per EVENT — by where that
//! event's own instant sits relative to the group's ingestion window — and the entries
//! of one batch to one target are therefore partitioned by it: each entry the window
//! admits is written into the group as one log event stamped at its own instant, each
//! entry the window refuses is written nowhere and is handed to that target's
//! `DeadLetterConfig` queue as the envelope of that entry, and no entry is both. One
//! entry of a batch neither carries another entry's outcome nor suppresses it.
//!
//! `PutEvents` itself accepts every such entry — "each `Time` ... is the time stamp of
//! the event" (`PutEventsRequestEntry$Time`), which the batch's answer does not judge,
//! so `FailedEntryCount` is 0 and each entry is answered with its own `EventId` — and
//! it is the DELIVERY that classifies the entry afterwards.
//!
//! The window is CloudWatch Logs' own: "events more than 2 hours in the future are
//! rejected while processing remaining valid events. Events older than 14 days or
//! preceding the log group's retention period are rejected while processing remaining
//! valid events" (`PutLogEvents`). The rows of the one batch below differ along where
//! each entry's own `Time` sits relative to it — absent (so the entry is stamped as it
//! is published), just behind the clock, near the far edge of the window's past, past
//! the 14-day horizon, far past it, ahead of the window and far ahead of it — which is
//! what this rule generalises over; the recorded case is three of these rows.
//!
//! `tests/all/log_group_target_window_dead_letter.rs` is the suite for WHAT each class
//! of refusal is reported as, one entry at a time, and this one reuses its wiring and
//! its readers; what this suite adds is that the classes hold TOGETHER, over one batch
//! published onto one target, one group and one dead-letter queue.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{rule_arn, DEFAULT_EVENT_BUS_NAME};

use crate::log_group_target_window_dead_letter::{
    attribute, attribute_names, epoch_now, given_log_group, put_log_events_failure, written,
    Expected, DAY, DETAIL, EVENTS_IN_BATCH_REQUEST_REJECTED, EXHAUSTED_RETRY_CONDITION, HOUR,
    INVALID_PARAMETER, MAXIMUM_RETRY_ATTEMPTS, RETRY_ATTEMPTS, UNRETRIED_ATTRIBUTES,
};
use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_rule, Emulator, DETAIL_TYPE, REGION, SOURCE,
};

/// The rule the batch below publishes through.
const RULE: &str = "log-group-batch-rule";

/// The id of its single log-group target.
const TARGET_ID: &str = "log-group";

/// The group every entry of the batch is delivered to, and the dead-letter queue every
/// refused one is handed to.
const GROUP: &str = "/aws/events/batch-window";
const DEAD_LETTER_QUEUE: &str = "log-group-batch-dlq";

/// The `RetryPolicy` the target carries: no retry at all (`MaximumRetryAttempts`,
/// `range 0..185`) under the smallest age bound the model admits
/// (`MaximumEventAgeInSeconds`, `range 60..86400`), from
/// `smithy_query(query="operation", operation="PutTargets")` — so a refused entry is
/// dead-lettered on the attempt it was refused on and the retry accounting a retried
/// delivery reports is the other suite's subject rather than this one's.
const MAXIMUM_RETRY_ATTEMPTS_ALLOWED: i64 = 0;
const MAXIMUM_EVENT_AGE_SECONDS: i64 = 60;

/// One entry of the batch: where its own `Time` sits relative to now, and what its
/// delivery must come to — the outcomes a log-group delivery has, shared with
/// `tests/all/log_group_target_window_dead_letter.rs` so the two suites cannot drift on
/// what each means.
struct Entry {
    case: &'static str,
    /// How far the entry's `Time` sits from now, in seconds; `None` for an entry
    /// carrying no `Time` at all, which is stamped as it is published.
    offset_seconds: Option<i64>,
    expected: Expected,
}

/// The entries of the ONE batch below, in the order they are published.
fn batch() -> Vec<Entry> {
    vec![
        Entry {
            case: "an entry carrying no Time of its own",
            offset_seconds: None,
            expected: Expected::Written,
        },
        Entry {
            case: "an entry stamped just behind the clock",
            offset_seconds: Some(-30),
            expected: Expected::Written,
        },
        Entry {
            case: "an entry near the far edge of the window's past",
            offset_seconds: Some(-13 * DAY),
            expected: Expected::Written,
        },
        Entry {
            case: "an entry past the ingestion horizon",
            offset_seconds: Some(-15 * DAY),
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Entry {
            case: "an entry far past that horizon",
            offset_seconds: Some(-40 * DAY),
            expected: Expected::DeadLettered(INVALID_PARAMETER),
        },
        Entry {
            case: "an entry ahead of the window",
            offset_seconds: Some(3 * HOUR),
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
        Entry {
            case: "an entry far ahead of the window",
            offset_seconds: Some(25 * HOUR),
            expected: Expected::DeadLettered(EVENTS_IN_BATCH_REQUEST_REJECTED),
        },
    ]
}

/// The envelope `message` carries in its body, as the document it is.
fn envelope_of(message: &Value) -> Value {
    serde_json::from_str(message["Body"].as_str().unwrap_or_default()).unwrap_or(Value::Null)
}

/// The log event of `held` whose message is the envelope of `event_id`, or `None` when
/// the group holds none for it.
fn log_event_of<'a>(held: &'a [Value], event_id: &str) -> Option<&'a Value> {
    held.iter().find(|event| {
        let carried: Value =
            serde_json::from_str(event["message"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null);
        carried["id"] == json!(event_id)
    })
}

/// The dead letter of `messages` carrying the envelope of `event_id`, or `None` when
/// none of them does.
fn dead_letter_of<'a>(messages: &'a [Value], event_id: &str) -> Option<&'a Value> {
    messages
        .iter()
        .find(|message| envelope_of(message)["id"] == json!(event_id))
}

/// Publish the whole batch and answer the `EventId` of each entry, in order.
async fn publish_batch(emulator: &Emulator, entries: &[Entry], now: i64) -> Vec<String> {
    let published: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut published = json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": DETAIL,
            });
            if let Some(offset) = entry.offset_seconds {
                published["Time"] = json!(now + offset);
            }
            published
        })
        .collect();
    let (status, body) = events(emulator, "PutEvents", &json!({ "Entries": published })).await;
    assert_eq!(status, 200, "the batch: PutEvents answered {status} {body}");
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "the batch: expected PutEvents to accept every entry, got {body}"
    );
    let answered = body["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        answered.len(),
        entries.len(),
        "the batch: expected {} answered entries, got {answered:?}",
        entries.len()
    );
    let ids: Vec<String> = entries
        .iter()
        .zip(&answered)
        .map(|(entry, answer)| {
            let case = entry.case;
            answer["EventId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: PutEvents minted no EventId: {answer}"))
                .to_string()
        })
        .collect();
    for (entry, id) in entries.iter().zip(&ids) {
        let case = entry.case;
        let minted = ids.iter().filter(|other| *other == id).count();
        assert_eq!(
            minted, 1,
            "case {case:?}: expected {id:?} to be minted for one entry only, got {minted} of \
             {ids:?}"
        );
    }
    ids
}

/// Each entry of one published batch is delivered to the log-group target on its own:
/// the entries the group's ingestion window admits are written into it under their own
/// instants, the entries it refuses are dead-lettered under the condition each earns,
/// and no entry appears on both sides of that split.
#[tokio::test]
async fn one_batch_is_partitioned_between_the_log_group_and_its_dead_letter_queue() {
    let entries = batch();
    let emulator = emulator();
    let group_arn = given_log_group(&emulator, "the batch", GROUP, None).await;
    // The target names the group ON ITS OWN, which is the form a client writes after
    // reading the ARN off the group and stripping everything under it; both forms name
    // the one group, and it is this ARN the dead letter reports as the target.
    let target_arn = group_arn
        .strip_suffix(":*")
        .unwrap_or(&group_arn)
        .to_string();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, DEAD_LETTER_QUEUE).await;
    given_rule(
        &emulator,
        RULE,
        json!([{
            "Id": TARGET_ID,
            "Arn": target_arn,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
            "RetryPolicy": {
                "MaximumRetryAttempts": MAXIMUM_RETRY_ATTEMPTS_ALLOWED,
                "MaximumEventAgeInSeconds": MAXIMUM_EVENT_AGE_SECONDS,
            },
        }]),
    )
    .await;

    let now = epoch_now();
    let ids = publish_batch(&emulator, &entries, now).await;

    let held = written(&emulator, "the batch", GROUP).await;
    let dead_letters = drain(&emulator, &dead_letter_url).await;
    let expected_written = entries
        .iter()
        .filter(|entry| entry.expected == Expected::Written)
        .count();
    let expected_dead = entries.len() - expected_written;
    assert_eq!(
        held.len(),
        expected_written,
        "the batch: expected the group to hold {expected_written} of the {} entries, got {held:?}",
        entries.len()
    );
    assert_eq!(
        dead_letters.len(),
        expected_dead,
        "the batch: expected {expected_dead} dead letter(s), got {dead_letters:?}"
    );

    let expected_message = put_log_events_failure(GROUP);
    let expected_rule_arn = rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE);
    for (entry, event_id) in entries.iter().zip(&ids) {
        let case = entry.case;
        match entry.expected {
            Expected::Written => {
                let event = log_event_of(&held, event_id).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the group to hold the envelope of \
                         {event_id:?}, got {held:?}"
                    )
                });
                // The log event's instant is the event's OWN: the entry's `Time` in
                // epoch milliseconds, and for an entry carrying none the instant it was
                // published at, which the window admits.
                match entry.offset_seconds {
                    Some(offset) => assert_eq!(
                        event["timestamp"],
                        json!((now + offset) * 1_000),
                        "case {case:?}: expected the log event's timestamp {}, got {:?}",
                        (now + offset) * 1_000,
                        event["timestamp"]
                    ),
                    None => assert!(
                        event["timestamp"]
                            .as_i64()
                            .is_some_and(|millis| millis >= now * 1_000),
                        "case {case:?}: expected the log event to be stamped at its \
                         publication, got {:?}",
                        event["timestamp"]
                    ),
                }
                assert!(
                    dead_letter_of(&dead_letters, event_id).is_none(),
                    "case {case:?}: an entry the group holds is dead-lettered nowhere, got \
                     {dead_letters:?}"
                );
            }
            Expected::DeadLettered(expected_code) => {
                assert!(
                    log_event_of(&held, event_id).is_none(),
                    "case {case:?}: a refused entry writes nothing, got {held:?}"
                );
                let message = dead_letter_of(&dead_letters, event_id).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected a dead letter carrying the envelope of \
                         {event_id:?}, got {dead_letters:?}"
                    )
                });
                assert_eq!(
                    attribute(message, "ERROR_CODE").as_deref(),
                    Some(expected_code),
                    "case {case:?}: expected ERROR_CODE {expected_code:?}, got {:?}",
                    attribute(message, "ERROR_CODE")
                );
                assert_eq!(
                    attribute(message, "ERROR_MESSAGE").as_deref(),
                    Some(expected_message.as_str()),
                    "case {case:?}: expected ERROR_MESSAGE {expected_message:?}, got {:?}",
                    attribute(message, "ERROR_MESSAGE")
                );
                assert_eq!(
                    attribute(message, "RULE_ARN").as_deref(),
                    Some(expected_rule_arn.as_str()),
                    "case {case:?}: expected RULE_ARN {expected_rule_arn:?}, got {:?}",
                    attribute(message, "RULE_ARN")
                );
                assert_eq!(
                    attribute(message, "TARGET_ARN").as_deref(),
                    Some(target_arn.as_str()),
                    "case {case:?}: expected TARGET_ARN {target_arn:?}, got {:?}",
                    attribute(message, "TARGET_ARN")
                );
                // The retried class accounts for the retries the delivery made beside
                // those four, under the bound that ended it; the class no attempt could
                // have cleared retried nothing and carries no accounting at all.
                let carried = attribute_names(message);
                let mut expected_names: Vec<String> = UNRETRIED_ATTRIBUTES
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                if expected_code == EVENTS_IN_BATCH_REQUEST_REJECTED {
                    expected_names.push(RETRY_ATTEMPTS.to_string());
                    expected_names.push(EXHAUSTED_RETRY_CONDITION.to_string());
                    let allowed = MAXIMUM_RETRY_ATTEMPTS_ALLOWED.to_string();
                    assert_eq!(
                        attribute(message, RETRY_ATTEMPTS).as_deref(),
                        Some(allowed.as_str()),
                        "case {case:?}: expected {RETRY_ATTEMPTS} {allowed:?}, got {:?}",
                        attribute(message, RETRY_ATTEMPTS)
                    );
                    assert_eq!(
                        attribute(message, EXHAUSTED_RETRY_CONDITION).as_deref(),
                        Some(MAXIMUM_RETRY_ATTEMPTS),
                        "case {case:?}: expected {EXHAUSTED_RETRY_CONDITION} \
                         {MAXIMUM_RETRY_ATTEMPTS:?}, got {:?}",
                        attribute(message, EXHAUSTED_RETRY_CONDITION)
                    );
                }
                expected_names.sort();
                assert_eq!(
                    carried, expected_names,
                    "case {case:?}: expected the dead letter to carry exactly \
                     {expected_names:?}, got {carried:?}"
                );
            }
        }
    }
}
