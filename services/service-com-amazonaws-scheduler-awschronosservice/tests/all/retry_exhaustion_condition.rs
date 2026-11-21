// SPDX-License-Identifier: CC0-1.0

//! WHICH bound ended an invocation's attempts, as the dead-letter message reports it:
//! end to end over the wire through the real `MultiServiceRouter`, with Amazon SQS and
//! IAM mounted beside EventBridge Scheduler on ONE engine, so the condition is asserted
//! where a client reads it — in the attributes of the message the schedule's dead-letter
//! queue holds.
//!
//! RULE: `RetryPolicy` bounds an invocation two ways — `MaximumRetryAttempts` is "the
//! maximum number of retry attempts to make before the request fails" and
//! `MaximumEventAgeInSeconds` "the maximum amount of time, in seconds, to continue to
//! make retry attempts", retries continuing "until either the maximum number of attempts
//! is made or until the duration of the `MaximumEventAgeInSeconds` is reached" — and a
//! dead-letter message names the bound that ENDED the attempts, under the member name the
//! policy states it by. The two bounds are spent differently: the attempt ALLOWANCE is
//! spent only by attempts actually made, so an invocation whose failure was one no further
//! attempt could have come to anything else spends none of it and ends on no attempt bound
//! — while the invocation's AGE is the clock's and runs whatever the target answered, so
//! such an invocation whose next attempt would fall outside its age window ran out of TIME,
//! which is what ended it, and one still inside that window ends on no bound and its report
//! names no condition at all.
//!
//! The rows span the class rather than one recorded case: every row's invocation fails
//! the same way (a target call its execution role withholds, which is never repeated), and
//! they differ along the age window stated beside it — over both published bounds of
//! `MaximumEventAgeInSeconds` (`smithy_query(query="shape", shape="RetryPolicy")`:
//! `range 60..86400`), at the span this service leaves between two attempts and one second
//! past it, and over a schedule stating no policy at all — and along the attempt cap
//! stated with it, from the fewest retries the member admits to the most.
//!
//! What a report carries BESIDE the condition is `tests/all/dead_letter_delivery.rs`;
//! which bound ends a series of attempts, over the whole range of both members, is
//! asserted without a wait in `src/state/dead_letter.rs`'s own tests.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::RETRY_INTERVAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::dead_letter_delivery::{body_document, received, PATIENCE, QUEUE_ACCESS_DENIED};
use crate::schedule_invocation::{
    at_expression_body, emulator, given_execution_role_for, given_queue, scheduler,
    seconds_from_now, POLL_INTERVAL,
};

/// The attribute that names the bound which ended the attempts.
const EXHAUSTED_RETRY_CONDITION: &str = "EXHAUSTED_RETRY_CONDITION";

/// The two member names a condition is reported under, as `RetryPolicy` spells them
/// (`smithy_query(query="shape", shape="RetryPolicy")`).
const MAXIMUM_EVENT_AGE_IN_SECONDS: &str = "MaximumEventAgeInSeconds";

/// The shortest `MaximumEventAgeInSeconds` the member admits (`range 60..86400`).
const SHORTEST_EVENT_AGE: u64 = 60;

/// The longest that member admits.
const LONGEST_EVENT_AGE: u64 = 86_400;

/// The largest `MaximumRetryAttempts` the member admits (`range 0..185`).
const MOST_RETRY_ATTEMPTS: i64 = 185;

/// How far ahead of the write each row's own `at(...)` occurrence stands.
const OCCURRENCE_IN: u64 = 4;

/// The age window a row states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventAge {
    /// No `RetryPolicy` at all, so the schedule states neither member.
    NoPolicy,
    /// The shortest window the member admits, which is no longer than the span this
    /// service leaves between two attempts.
    Shortest,
    /// One second past that span, so a further attempt would still fall inside the
    /// window.
    PastTheRetrySpan,
    /// The longest window the member admits.
    Longest,
}

/// One row: the policy the schedule states, and the condition its report must name.
struct Row {
    case: &'static str,
    age: EventAge,
    /// `MaximumRetryAttempts`, stated beside the window where the row states a policy.
    cap: Option<i64>,
    /// The `EXHAUSTED_RETRY_CONDITION` the report must carry, or `None` for a report
    /// that must carry none.
    condition: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "no retry policy at all",
        age: EventAge::NoPolicy,
        cap: None,
        condition: None,
    },
    Row {
        case: "the longest event age, with one retry admitted",
        age: EventAge::Longest,
        cap: Some(1),
        condition: None,
    },
    Row {
        case: "an event age a second past the span between attempts",
        age: EventAge::PastTheRetrySpan,
        cap: Some(3),
        condition: None,
    },
    Row {
        case: "the shortest event age, with one retry admitted",
        age: EventAge::Shortest,
        cap: Some(1),
        condition: Some(MAXIMUM_EVENT_AGE_IN_SECONDS),
    },
    Row {
        case: "the shortest event age, with the most retries admitted",
        age: EventAge::Shortest,
        cap: Some(MOST_RETRY_ATTEMPTS),
        condition: Some(MAXIMUM_EVENT_AGE_IN_SECONDS),
    },
    Row {
        case: "the shortest event age, with no retry admitted",
        age: EventAge::Shortest,
        cap: Some(0),
        condition: Some(MAXIMUM_EVENT_AGE_IN_SECONDS),
    },
];

/// Every row: the report names the bound that ended the attempts, and names none where
/// neither bound was reached — with the attempts actually made reported beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_names_the_bound_that_ended_the_attempts() {
    let suite = "a queue target the execution role may not write to";
    let emulator = emulator();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, suite, "dlq").await;
    let (_, target_arn) = given_queue(&emulator, suite, "target").await;
    // The role may carry the REPORT and nothing else, so every row's own invocation is
    // refused the same way while its report still reaches the queue.
    let role = given_execution_role_for(&emulator, suite, "role", &[&dead_letter_arn]).await;

    // (the row, the payload its occurrence could not deliver)
    let mut written: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let marker = json!({ "row": index }).to_string();
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
                "ScheduleExpression": format!(
                    "at({})",
                    at_expression_body(seconds_from_now(OCCURRENCE_IN))
                ),
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
        written.push((row, marker));
    }

    for (row, marker) in &written {
        let case = row.case;
        let report = report_of(&emulator, case, &dead_letter_url, marker).await;
        let attribute = |name: &str| {
            report["MessageAttributes"][name]["StringValue"]
                .as_str()
                .map(ToString::to_string)
        };
        assert_eq!(
            attribute(EXHAUSTED_RETRY_CONDITION).as_deref(),
            row.condition,
            "case {case:?}: expected {EXHAUSTED_RETRY_CONDITION} {:?}, got {report}",
            row.condition
        );
        // The failure was a refusal of the request itself, so no attempt of it was made
        // again whatever the cap admitted: what the report counts is the attempts MADE.
        assert_eq!(
            attribute("RETRY_ATTEMPTS").as_deref(),
            Some("0"),
            "case {case:?}: expected no retry to be counted, got {report}"
        );
        assert_eq!(
            attribute("ERROR_CODE").as_deref(),
            Some(QUEUE_ACCESS_DENIED),
            "case {case:?}: expected the refusing service's own code, got {report}"
        );
    }
}

/// The `RetryPolicy` a row states, or `None` for a row stating none.
fn retry_policy(row: &Row) -> Option<Value> {
    let age = match row.age {
        EventAge::NoPolicy => return None,
        EventAge::Shortest => SHORTEST_EVENT_AGE,
        // The window a further attempt would still fall inside, which is why this row's
        // figure is read off the span this service leaves between two attempts rather
        // than written as a number.
        EventAge::PastTheRetrySpan => RETRY_INTERVAL.as_secs() + 1,
        EventAge::Longest => LONGEST_EVENT_AGE,
    };
    let mut policy = json!({ "MaximumEventAgeInSeconds": age });
    if let Some(cap) = row.cap {
        policy["MaximumRetryAttempts"] = json!(cap);
    }
    Some(policy)
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
/// EVENT — the `SendMessage` request the invocation attempted of its queue target — so
/// the payload is the `MessageBody` that request states.
fn undelivered_payload(report: &Value) -> String {
    let event = body_document(report);
    event["MessageBody"]
        .as_str()
        .map_or_else(|| event.to_string(), ToString::to_string)
}
