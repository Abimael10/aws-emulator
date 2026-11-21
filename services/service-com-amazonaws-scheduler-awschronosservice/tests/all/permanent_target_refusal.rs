// SPDX-License-Identifier: CC0-1.0

//! A target call the schedule's own execution role may not make is a PERMANENT
//! failure: end to end over the wire through the real `MultiServiceRouter`, with
//! Amazon SQS and IAM mounted beside EventBridge Scheduler on ONE engine, so the
//! classification is asserted where a client observes it — in the dead-letter
//! message's own attributes.
//!
//! RULE: an authorization refusal of an occurrence's call cannot come to anything
//! else however often it is repeated, so no retry of it is made and the report of it
//! counts the attempts ACTUALLY made — none beyond the first — whatever
//! `Target.RetryPolicy` admits. `MaximumRetryAttempts` is "the maximum number of
//! retry attempts to make before the request fails" (modelled `range 0..185`): an
//! UPPER BOUND on the retries a failure worth retrying may spend, never a count to
//! report, so the figure a report carries is independent of the figure the schedule
//! states and of `MaximumEventAgeInSeconds` (modelled `range 60..86400`), of which such
//! an invocation spends no attempt either. What the age window still decides for such an
//! invocation — whether it ran out of TIME before a further attempt of it could have
//! been made, which is the bound its report then names — is
//! `tests/all/retry_exhaustion_condition.rs`, so a row here only states what its own
//! age window makes of that.
//!
//! RULE: a write that states a new cap changes the CONFIGURATION and nothing else:
//! `GetSchedule` reports the cap the last write stated, while the next occurrence's
//! report of the same permanent refusal carries the same error code and the same
//! attempt count as the one before it.
//!
//! The rows span the class rather than one recorded case: they differ along the cap
//! the schedule states — no policy at all, and each published bound of
//! `MaximumRetryAttempts` with one figure between them — and along the event age
//! stated beside it, over both of that member's published bounds. What a report of a
//! failure carries BESIDE the attempt count, and which failures are reported at all,
//! is `tests/all/dead_letter_delivery.rs`; how many attempts a failure worth retrying
//! is admitted is asserted without a wait in `src/state/dead_letter.rs`'s own tests.

use std::collections::BTreeSet;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::invocation;
use smithy_server_runtime::MultiServiceRouter;

// The read of a queue's messages WITH their attributes, the reading of a failed
// event's body as the document it is, how long a report is waited for and the code the
// refusing service answers a withheld call with are this crate's own
// (`tests/all/dead_letter_delivery.rs`), so this suite spells none of them again.
use crate::dead_letter_delivery::{body_document, received, PATIENCE, QUEUE_ACCESS_DENIED};
use crate::schedule_invocation::{
    at_expression_body, drain, emulator, given_execution_role_for, given_queue, peek, schedule_read,
    scheduler, seconds_from_now, POLL_INTERVAL,
};

/// How long a case keeps watching a queue whose reports have all arrived, so a second
/// report of one invocation would have had its chance to arrive. Nothing is asserted
/// about how long anything took: the window is only how long the absence is watched
/// for.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// How far ahead of the write each case's own `at(...)` occurrence stands.
const OCCURRENCE_IN: u64 = 4;

/// The schedule group a schedule lands in when the write names none.
const DEFAULT_GROUP: &str = "default";

/// The error code every operation of this service declares for an input the model does
/// not admit (`smithy_query(query="operation", operation="CreateSchedule")`:
/// `ValidationException`, `@httpError 400`).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The member a cap past the published maximum violates, as the refusal names it: the
/// dotted path to the member, each name as the wire spells it.
const MAXIMUM_RETRY_ATTEMPTS_MEMBER: &str = "target.retryPolicy.maximumRetryAttempts";

/// The action the withheld call is authorized by, named the way Amazon SQS names an
/// action in a refusal of its own (case-folded throughout).
const WITHHELD_ACTION: &str = "sqs:sendmessage";

/// The largest `MaximumRetryAttempts` the member admits
/// (`smithy_query(query="shape", shape="RetryPolicy")`: `range 0..185`).
const MOST_RETRY_ATTEMPTS: i64 = 185;

/// The longest `MaximumEventAgeInSeconds` that member admits (`range 60..86400`).
const LONGEST_EVENT_AGE: i64 = 86_400;

/// The shortest `MaximumEventAgeInSeconds` that member admits.
const SHORTEST_EVENT_AGE: i64 = 60;

/// The member an invocation that ran out of TIME names as the bound that ended it, as
/// `RetryPolicy` spells it.
const MAXIMUM_EVENT_AGE_IN_SECONDS: &str = "MaximumEventAgeInSeconds";

/// One row: the retry policy the schedule states, over the published range of both of
/// `RetryPolicy`'s members.
struct Row {
    case: &'static str,
    /// `Target.RetryPolicy.MaximumRetryAttempts`, or `None` for a schedule stating no
    /// retry policy at all.
    cap: Option<i64>,
    /// `Target.RetryPolicy.MaximumEventAgeInSeconds`, stated beside the cap.
    event_age: Option<i64>,
    /// The bound the report names as having ended the attempts: none for a row whose
    /// age window would still have admitted the next attempt.
    condition: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "no retry policy at all",
        cap: None,
        event_age: None,
        condition: None,
    },
    Row {
        case: "a policy admitting no retry",
        cap: Some(0),
        event_age: Some(LONGEST_EVENT_AGE),
        condition: None,
    },
    Row {
        case: "a cap between the bounds, at the shortest event age",
        cap: Some(3),
        event_age: Some(SHORTEST_EVENT_AGE),
        // The shortest window the member admits is no longer than the span this service
        // leaves between two attempts, so this invocation was out of time before the
        // retry its cap still admitted could have been made.
        condition: Some(MAXIMUM_EVENT_AGE_IN_SECONDS),
    },
    Row {
        case: "the most retries the member admits",
        cap: Some(MOST_RETRY_ATTEMPTS),
        event_age: Some(LONGEST_EVENT_AGE),
        condition: None,
    },
];

/// Every row: a withheld invocation is reported ONCE, under the refusing service's own
/// authorization code, counting no retry whatever the schedule's cap admits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_invocation_reports_the_attempts_it_made_whatever_the_cap_admits() {
    let case = "a queue target the execution role may not write to";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, case, "dlq").await;
    let (target_url, target_arn) = given_queue(&emulator, case, "target").await;
    // The role is granted the dead-letter delivery and nothing else, so every
    // occurrence's own call is refused while the report of it can still be carried.
    let role = given_execution_role_for(&emulator, case, "role", &[&dead_letter_arn]).await;

    // (the row, the payload its occurrence could not deliver, the occurrence's instant)
    let mut written: Vec<(&Row, String, SystemTime)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let marker = json!({ "row": index }).to_string();
        let occurrence = seconds_from_now(OCCURRENCE_IN);
        let mut target = json!({
            "Arn": target_arn,
            "RoleArn": role,
            "Input": marker,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        if let Some(policy) = retry_policy(row) {
            target["RetryPolicy"] = policy;
        }
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/row-{index}"),
            Some(json!({
                "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": target,
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateSchedule answered {status} {created}",
            row.case
        );
        written.push((row, marker, occurrence));
    }

    for (row, marker, occurrence) in &written {
        let case = row.case;
        let report = report_of(&emulator, case, &dead_letter_url, marker).await;
        let attribute = |name: &str| {
            report["MessageAttributes"][name]["StringValue"]
                .as_str()
                .map(ToString::to_string)
        };
        for (name, expected) in [
            ("ERROR_CODE", QUEUE_ACCESS_DENIED.to_string()),
            // The attempts ACTUALLY made beyond the first: a refusal of the request
            // itself is never repeated, so none were, however many the cap admits.
            ("RETRY_ATTEMPTS", "0".to_string()),
            ("TARGET_ARN", target_arn.clone()),
            (
                "SCHEDULED_TIME",
                invocation::scheduled_time_text(*occurrence),
            ),
        ] {
            assert_eq!(
                attribute(name),
                Some(expected.clone()),
                "case {case:?}: expected {name} to carry {expected:?}, got {report}"
            );
        }
        assert_eq!(
            report["MessageAttributes"]["RETRY_ATTEMPTS"]["DataType"].as_str(),
            Some("Number"),
            "case {case:?}: expected the attempt count reported as a figure, got {report}"
        );
        // The attempt allowance was not spent, so what the report names — if anything —
        // is the age window having closed before a further attempt could be made.
        assert_eq!(
            attribute("EXHAUSTED_RETRY_CONDITION").as_deref(),
            row.condition,
            "case {case:?}: expected the exhausted-retry condition {:?}, got {report}",
            row.condition
        );
        let detail = attribute("ERROR_MESSAGE").unwrap_or_default();
        for expected in [WITHHELD_ACTION, target_arn.as_str()] {
            assert!(
                detail.contains(expected),
                "case {case:?}: expected the refusal to name {expected:?}, got {detail:?}"
            );
        }
    }

    // Nothing reached the target, and each invocation was reported exactly once: a
    // report is of an invocation that did not arrive, and a permanent refusal is one
    // invocation however long the queue is watched.
    let markers: BTreeSet<&str> = written
        .iter()
        .map(|(_, marker, _)| marker.as_str())
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let held = received(&emulator, &dead_letter_url).await;
        let payloads: Vec<String> = held.iter().map(undelivered_payload).collect();
        for payload in &payloads {
            assert!(
                markers.contains(payload.as_str()),
                "case {case:?}: expected a report of one of {markers:?}, got {payload:?}"
            );
            let copies = payloads.iter().filter(|other| *other == payload).count();
            assert_eq!(
                copies, 1,
                "case {case:?}: expected exactly one report of {payload:?}, got {copies} in \
                 {payloads:?}"
            );
        }
        seen.extend(payloads);
        let delivered = peek(&emulator, &target_url).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: expected the target to hold nothing, got {delivered:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let reported: BTreeSet<&str> = seen.iter().map(String::as_str).collect();
    assert_eq!(
        reported, markers,
        "case {case:?}: expected one report per schedule, got {reported:?}"
    );
}

/// A cap past the largest figure the member admits is no configuration at all: the
/// write is refused, so the rows above span every cap a schedule can hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cap_past_the_published_maximum_is_refused() {
    let case = "a cap past the published maximum";
    let emulator = emulator();
    let (_, target_arn) = given_queue(&emulator, case, "target").await;
    let role = given_execution_role_for(&emulator, case, "role", &[&target_arn]).await;
    let (status, refused) = scheduler(
        &emulator,
        "POST",
        "/schedules/past-the-bound",
        Some(json!({
            "ScheduleExpression": format!("at({})", at_expression_body(seconds_from_now(3_600))),
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "Target": {
                "Arn": target_arn,
                "RoleArn": role,
                "Input": "{}",
                "RetryPolicy": { "MaximumRetryAttempts": MOST_RETRY_ATTEMPTS + 1 },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a cap of {} to be refused, got {status} {refused}",
        MOST_RETRY_ATTEMPTS + 1
    );
    // The modelled refusal, naming the member the value failed and the figures it
    // admits: a 400 under any other code would be a different refusal.
    assert_eq!(
        refused["__type"]
            .as_str()
            .or_else(|| refused["code"].as_str()),
        Some(VALIDATION_EXCEPTION),
        "case {case:?}: expected the modelled {VALIDATION_EXCEPTION}, got {refused}"
    );
    let detail = refused["message"]
        .as_str()
        .or_else(|| refused["Message"].as_str())
        .unwrap_or_default();
    for expected in [
        MAXIMUM_RETRY_ATTEMPTS_MEMBER,
        &MOST_RETRY_ATTEMPTS.to_string(),
    ] {
        assert!(
            detail.contains(expected),
            "case {case:?}: expected the refusal to name {expected:?}, got {detail:?}"
        );
    }
}

/// Raising the cap with `UpdateSchedule` changes what the schedule REPORTS and not
/// what the next occurrence's report counts: the same permanent refusal, the same
/// code, the same attempt count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raising_the_cap_does_not_change_the_reported_attempt_count() {
    let case = "a cap raised while the refusal stays permanent";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, case, "dlq").await;
    let (_, target_arn) = given_queue(&emulator, case, "target").await;
    let role = given_execution_role_for(&emulator, case, "role", &[&dead_letter_arn]).await;
    let name = "re-capped";

    // (what the write is, the generation's own payload, the cap that write states)
    let generations: [(&str, &str, i64); 3] = [
        ("the first write", "gen-one", 0),
        ("a write raising the cap", "gen-two", 3),
        (
            "a write raising it to the published maximum",
            "gen-three",
            MOST_RETRY_ATTEMPTS,
        ),
    ];
    for (index, (generation, payload, cap)) in generations.iter().enumerate() {
        let case = *generation;
        let marker = json!({ "gen": payload }).to_string();
        let occurrence = seconds_from_now(OCCURRENCE_IN);
        let body = json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": target_arn,
                "RoleArn": role,
                "Input": marker,
                "RetryPolicy": {
                    "MaximumRetryAttempts": cap,
                    "MaximumEventAgeInSeconds": LONGEST_EVENT_AGE,
                },
                "DeadLetterConfig": { "Arn": dead_letter_arn },
            },
        });
        // The first write creates the schedule; every one after it updates it.
        let method = if index == 0 { "POST" } else { "PUT" };
        let (status, answered) =
            scheduler(&emulator, method, &format!("/schedules/{name}"), Some(body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the write answered {status} {answered}"
        );
        // The CONFIGURATION does change: the schedule reports the cap just stated.
        let (status, read) = schedule_read(&emulator, DEFAULT_GROUP, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSchedule answered {status} {read}"
        );
        let stored = read["Target"]["RetryPolicy"]["MaximumRetryAttempts"].as_i64();
        assert_eq!(
            stored,
            Some(*cap),
            "case {case:?}: expected the schedule to report a cap of {cap}, got {read}"
        );

        let report = report_of(&emulator, case, &dead_letter_url, &marker).await;
        let attribute = |name: &str| {
            report["MessageAttributes"][name]["StringValue"]
                .as_str()
                .map(ToString::to_string)
        };
        assert_eq!(
            attribute("RETRY_ATTEMPTS"),
            Some("0".to_string()),
            "case {case:?}: expected the attempt count to stay at 0 under a cap of {cap}, got \
             {report}"
        );
        // The code is the refusing service's own, and the SAME one every generation
        // carries: the failure does not change when the cap does.
        assert_eq!(
            attribute("ERROR_CODE"),
            Some(QUEUE_ACCESS_DENIED.to_string()),
            "case {case:?}: expected the refusing service's authorization code, got {report}"
        );
        // The next generation's report is read on its own.
        drain(&emulator, &dead_letter_url).await;
    }
}

/// The `RetryPolicy` a row states, or `None` for a row stating none.
fn retry_policy(row: &Row) -> Option<Value> {
    let mut policy = json!({});
    if let Some(cap) = row.cap {
        policy["MaximumRetryAttempts"] = json!(cap);
    }
    if let Some(age) = row.event_age {
        policy["MaximumEventAgeInSeconds"] = json!(age);
    }
    policy
        .as_object()
        .is_some_and(|members| !members.is_empty())
        .then_some(policy)
}

/// The report whose undelivered payload is `payload`, once it arrives — looking again
/// until [`PATIENCE`] runs out.
async fn report_of(router: &MultiServiceRouter, case: &str, url: &str, payload: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let held = received(router, url).await;
        if let Some(found) = held
            .iter()
            .find(|report| undelivered_payload(report) == payload)
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no dead-letter report carrying {payload:?} arrived; the queue holds \
             {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The payload one report's invocation could not deliver: a report carries the failed
/// EVENT — the `SendMessage` request the invocation attempted of its queue target —
/// so the payload is the `MessageBody` that request states.
fn undelivered_payload(report: &Value) -> String {
    let event = body_document(report);
    event["MessageBody"]
        .as_str()
        .map_or_else(|| event.to_string(), ToString::to_string)
}
