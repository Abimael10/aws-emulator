// SPDX-License-Identifier: CC0-1.0

//! The TWO channels an occurrence that did not reach its target is observed through —
//! the message its schedule's dead-letter queue holds and the series this service
//! publishes about itself — asserted together, end to end over the wire through the
//! real `MultiServiceRouter` with Amazon SQS, IAM and CloudWatch mounted beside
//! EventBridge Scheduler on ONE engine.
//!
//! RULE: a report names the failure the way the SERVICE THAT ANSWERED the call names
//! it — its `ERROR_CODE` is the error code that service puts on the wire for the
//! failure class, which for a query-compatible service is its legacy Query code and
//! never the modelled exception shape's name — and what the queue is handed is the
//! failed EVENT, the request the attempt made of the target's own API, so a queue
//! target's report carries the `SendMessage` request that attempt addressed
//! (`QueueUrl`) and the undelivered payload it stated (`MessageBody`).
//!
//! RULE: a report names the retry bound that ENDED the attempts under the member name
//! `RetryPolicy` states it by, and names none where neither bound was reached — the
//! attempt allowance being spent only by attempts actually made, so a refusal no
//! further attempt could have come to anything else spends none of it while the
//! invocation's age runs on regardless.
//!
//! RULE: what the service publishes about the same occurrence follows the attempts
//! ACTUALLY MADE rather than the policy's cap: one observation per attempt made, one
//! per target call that failed, one per invocation no further attempt will be made of
//! and one per dead-letter write, each dimensioned on the SCHEDULE GROUP the schedule
//! belongs to. So the two channels agree about the one thing both report — a report
//! counts the RETRIES made, which are the attempts beyond the first, so the attempts
//! published for its group are one more than the retries it counts.
//!
//! The rows span the class rather than one recorded case: they differ along the KIND of
//! failure (the target does not exist, the permissions withhold the call) and so along
//! the answering service's own code for it, along the age window stated beside the
//! failure — over both published bounds of `MaximumEventAgeInSeconds`
//! (`smithy_query(query="shape", shape="RetryPolicy")`: `range 60..86400`) — along the
//! attempt cap stated with it, and along whether the occurrence failed at all; each row
//! runs in its OWN schedule group, which is what shows the counts are keyed by that
//! dimension rather than pooled.
//!
//! Every wait here looks again until the condition it needs holds, on a generous
//! deadline; nothing is asserted about elapsed time.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_scheduler_awschronosservice::state::schedule_metrics::{
    INVOCATIONS_FAILED_TO_BE_SENT_TO_DEAD_LETTER_COUNT, INVOCATIONS_SENT_TO_DEAD_LETTER_COUNT,
    INVOCATION_ATTEMPT_COUNT, INVOCATION_DROPPED_COUNT, TARGET_ERROR_COUNT,
};

use crate::dead_letter_delivery::{
    body_document, messages_once, received, refusal_code, QUEUE_ACCESS_DENIED,
};
use crate::invocation_metrics::{emulator_with_cloudwatch, sum_of};
use crate::schedule_invocation::{
    at_expression_body, given_execution_role_for, given_queue, scheduler, seconds_from_now,
    POLL_INTERVAL, REGION,
};
use crate::wire_harness::account_id;

/// The attribute naming the bound that ended the attempts, and the attributes read
/// beside it.
const EXHAUSTED_RETRY_CONDITION: &str = "EXHAUSTED_RETRY_CONDITION";
/// See [`EXHAUSTED_RETRY_CONDITION`].
const ERROR_CODE: &str = "ERROR_CODE";
/// See [`EXHAUSTED_RETRY_CONDITION`].
const RETRY_ATTEMPTS: &str = "RETRY_ATTEMPTS";

/// `MaximumEventAgeInSeconds`, as `RetryPolicy` spells the member
/// (`smithy_query(query="shape", shape="RetryPolicy")`) — which is how a report that
/// ended on that bound names it.
const MAXIMUM_EVENT_AGE_IN_SECONDS: &str = "MaximumEventAgeInSeconds";

/// The shortest `MaximumEventAgeInSeconds` the member admits (`range 60..86400`).
const SHORTEST_EVENT_AGE: u64 = 60;

/// The longest that member admits.
const LONGEST_EVENT_AGE: u64 = 86_400;

/// Every counter this service publishes, so a row asserts the ones it expects to move
/// AND the ones it expects to stay quiet.
const COUNTERS: &[&str] = &[
    INVOCATION_ATTEMPT_COUNT,
    TARGET_ERROR_COUNT,
    INVOCATION_DROPPED_COUNT,
    INVOCATIONS_SENT_TO_DEAD_LETTER_COUNT,
    INVOCATIONS_FAILED_TO_BE_SENT_TO_DEAD_LETTER_COUNT,
];

/// How long a wait here looks again before it gives up: generous rather than tight,
/// because a host running every suite at once is slow at everything, and every wait
/// stops as soon as what it needs holds.
const PATIENCE: Duration = Duration::from_secs(90);

/// How far ahead of the write each row states its one occurrence: far enough that every
/// row's queues, role, group and schedule are written while it is still ahead.
const OCCURRENCE_IN: u64 = 6;

/// What the row's schedule points its target at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetQueue {
    /// A queue of this account, which takes the invocation.
    Present,
    /// A queue of this account that was never created, which its own API refuses a
    /// message to whatever the caller may do.
    Absent,
}

/// What the row's occurrence comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The target took the invocation, so there is nothing to report.
    Delivered,
    /// The invocation was reported to the schedule's dead-letter queue.
    Reported {
        /// The wire error code the report must carry, or `None` where the row asks the
        /// target's own API for it rather than restating that service's vocabulary.
        code: Option<&'static str>,
        /// The bound the report must name, or `None` for a report that must name none.
        condition: Option<&'static str>,
    },
}

/// One row: the schedule written, what its execution role may write to, the retry
/// policy stated beside it, what its occurrence comes to and what its group holds
/// afterwards.
struct Row {
    case: &'static str,
    /// The schedule group the row runs in, which is the dimension its counts are
    /// published under.
    group: &'static str,
    target: TargetQueue,
    /// Whether the role's permissions allow the invocation's own call.
    may_invoke: bool,
    /// `MaximumEventAgeInSeconds`, stated by every row.
    event_age: u64,
    /// `MaximumRetryAttempts`, stated by every row.
    retry_cap: i64,
    outcome: Outcome,
    /// The counters that must MOVE for the row's group, each with the sum it reaches;
    /// every other counter of that group must hold nothing.
    counts: &'static [(&'static str, f64)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a target queue the execution role may not write to, well inside its age window",
        group: "withheld-call",
        target: TargetQueue::Present,
        may_invoke: false,
        event_age: LONGEST_EVENT_AGE,
        // A refusal of the request itself is the invocation's last attempt whatever the
        // policy would still have admitted, so this cap raises nothing.
        retry_cap: 2,
        outcome: Outcome::Reported {
            code: Some(QUEUE_ACCESS_DENIED),
            condition: None,
        },
        counts: &[
            (INVOCATION_ATTEMPT_COUNT, 1.0),
            (TARGET_ERROR_COUNT, 1.0),
            (INVOCATION_DROPPED_COUNT, 1.0),
            (INVOCATIONS_SENT_TO_DEAD_LETTER_COUNT, 1.0),
        ],
    },
    Row {
        case: "a target queue that does not exist, in the shortest age window the member admits",
        group: "absent-target",
        target: TargetQueue::Absent,
        // The role may make the call: what refuses it is the queue's own absence, which
        // is a different failure class and so a different code.
        may_invoke: true,
        event_age: SHORTEST_EVENT_AGE,
        retry_cap: 0,
        outcome: Outcome::Reported {
            code: None,
            condition: Some(MAXIMUM_EVENT_AGE_IN_SECONDS),
        },
        counts: &[
            (INVOCATION_ATTEMPT_COUNT, 1.0),
            (TARGET_ERROR_COUNT, 1.0),
            (INVOCATION_DROPPED_COUNT, 1.0),
            (INVOCATIONS_SENT_TO_DEAD_LETTER_COUNT, 1.0),
        ],
    },
    Row {
        case: "an invocation the target accepts",
        group: "delivered",
        target: TargetQueue::Present,
        may_invoke: true,
        event_age: SHORTEST_EVENT_AGE,
        retry_cap: 2,
        outcome: Outcome::Delivered,
        counts: &[(INVOCATION_ATTEMPT_COUNT, 1.0)],
    },
];

/// What one row's schedule was written with, carried from the write to the reads.
struct Written {
    row: &'static Row,
    /// The url the `SendMessage` request of this row's invocation addresses.
    target_url: String,
    dead_letter_url: String,
    /// `Target.Input`: the payload the target was to be handed.
    input: String,
    /// The retries the row's own report counted, once it has been read.
    retries_reported: Option<f64>,
}

/// Every row: its report names the answering service's own code, the request the
/// attempt made and the bound that ended it, and its group holds exactly the counts
/// that occurrence came to — the attempts published agreeing with the retries the
/// report counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_and_the_published_counts_agree() {
    let emulator = emulator_with_cloudwatch();

    let mut written: Vec<Written> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("agree-dlq-{index}")).await;
        let (target_url, target_arn) = match row.target {
            TargetQueue::Present => given_queue(&emulator, case, &format!("agree-{index}")).await,
            // A queue named by the ARN a queue of that name would carry and never
            // created, so the url is the one the invocation itself resolves that ARN to.
            TargetQueue::Absent => {
                let name = format!("agree-absent-{index}");
                (
                    format!(
                        "https://sqs.{REGION}.amazonaws.com/{}/{name}",
                        account_id()
                    ),
                    format!("arn:aws:sqs:{REGION}:{}:{name}", account_id()),
                )
            }
        };
        // Every row's role may carry the REPORT, so what differs between rows is only
        // whether it may make the invocation's own call.
        let mut granted: Vec<&str> = vec![dead_letter_arn.as_str()];
        if row.may_invoke {
            granted.push(target_arn.as_str());
        }
        let role =
            given_execution_role_for(&emulator, case, &format!("agree-role-{index}"), &granted)
                .await;

        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedule-groups/{}", row.group),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateScheduleGroup answered {status} {created}"
        );

        let input = json!({ "marker": format!("agree-{index}") }).to_string();
        let (status, created) = scheduler(
            &emulator,
            "POST",
            &format!("/schedules/agree-{index}"),
            Some(json!({
                "GroupName": row.group,
                "ScheduleExpression": format!(
                    "at({})",
                    at_expression_body(seconds_from_now(OCCURRENCE_IN))
                ),
                "ScheduleExpressionTimezone": "UTC",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "State": "ENABLED",
                "Target": {
                    "Arn": target_arn,
                    "RoleArn": role,
                    "Input": input,
                    "RetryPolicy": {
                        "MaximumRetryAttempts": row.retry_cap,
                        "MaximumEventAgeInSeconds": row.event_age,
                    },
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {created}"
        );
        written.push(Written {
            row,
            target_url,
            dead_letter_url,
            input,
            retries_reported: None,
        });
    }

    for entry in &mut written {
        let case = entry.row.case;
        match entry.row.outcome {
            Outcome::Delivered => {
                let delivered = messages_once(&emulator, &entry.target_url, 1, PATIENCE).await;
                let body = delivered
                    .first()
                    .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
                    .unwrap_or_else(|| panic!("case {case:?}: the target was never invoked"));
                assert_eq!(
                    body, entry.input,
                    "case {case:?}: expected the schedule's own Input {:?}, got {body:?}",
                    entry.input
                );
            }
            Outcome::Reported { code, condition } => {
                // The code the report must carry is the one the target's own API answers
                // that call with, read the way a client reads it, so a row that does not
                // state one asks that service rather than restating its vocabulary here.
                let expected_code = match code {
                    Some(code) => code.to_string(),
                    None => refusal_code(&emulator, case, &entry.target_url).await,
                };
                let reported = messages_once(&emulator, &entry.dead_letter_url, 1, PATIENCE).await;
                let report = reported
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: nothing was reported"));
                // What the queue is handed is the failed EVENT: the `SendMessage`
                // request the attempt addressed to the target queue, carrying the
                // undelivered payload as its `MessageBody`.
                let expected_event =
                    json!({ "QueueUrl": entry.target_url, "MessageBody": entry.input });
                assert_eq!(
                    body_document(report),
                    expected_event,
                    "case {case:?}: expected the failed event {expected_event}, got {report}"
                );
                let attribute = |name: &str| {
                    report["MessageAttributes"][name]["StringValue"]
                        .as_str()
                        .map(ToString::to_string)
                };
                assert_eq!(
                    attribute(ERROR_CODE).as_deref(),
                    Some(expected_code.as_str()),
                    "case {case:?}: expected {ERROR_CODE} {expected_code:?}, got {report}"
                );
                // Amazon SQS is query-compatible, so the code it puts on the wire is its
                // legacy Query code: never the modelled exception shape's name, which
                // ends in that suffix.
                assert!(
                    !expected_code.ends_with("Exception"),
                    "case {case:?}: expected the wire code of the refusing service, got \
                     {expected_code:?}"
                );
                assert_eq!(
                    attribute(EXHAUSTED_RETRY_CONDITION).as_deref(),
                    condition,
                    "case {case:?}: expected {EXHAUSTED_RETRY_CONDITION} {condition:?}, got \
                     {report}"
                );
                let retries = attribute(RETRY_ATTEMPTS)
                    .unwrap_or_else(|| panic!("case {case:?}: no {RETRY_ATTEMPTS} in {report}"));
                let retries: f64 = retries.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: expected {RETRY_ATTEMPTS} as a figure, got {retries:?}")
                });
                // Neither failure is one a further attempt could have come to anything
                // else, so no attempt of it was made again whatever the cap admitted.
                assert!(
                    (retries - 0.0).abs() < f64::EPSILON,
                    "case {case:?}: expected no retry to be counted, got {report}"
                );
                entry.retries_reported = Some(retries);
            }
        }
    }

    // Every row's counters are waited for together, so one slow occurrence does not
    // spend another row's patience.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let mut pending: Vec<String> = Vec::new();
        for row in ROWS {
            for (metric, expected) in row.counts {
                let held = sum_of(&emulator, row.group, metric).await;
                if held < *expected {
                    pending.push(format!("{}/{metric}={held}", row.group));
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected every counter of every row to be published; still short: {pending:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // What each row's occurrence came to is EXACTLY these counts: a counter it did not
    // move holds nothing at all — including the one for a report that could not be
    // written, every row's report having been carried.
    for row in ROWS {
        let case = row.case;
        for metric in COUNTERS {
            let expected = row
                .counts
                .iter()
                .find(|(moved, _)| moved == metric)
                .map_or(0.0, |(_, count)| *count);
            let held = sum_of(&emulator, row.group, metric).await;
            assert!(
                (held - expected).abs() < f64::EPSILON,
                "case {case:?}: expected {metric} of group {:?} to hold {expected}, got {held}",
                row.group
            );
        }
    }

    // The two channels agree about the one thing both report: the attempts actually
    // made. A report counts the RETRIES made — the attempts beyond the first — so the
    // attempts published for its group are one more than that.
    for entry in &written {
        let case = entry.row.case;
        let Some(retries) = entry.retries_reported else {
            continue;
        };
        let attempts = sum_of(&emulator, entry.row.group, INVOCATION_ATTEMPT_COUNT).await;
        assert!(
            (attempts - (retries + 1.0)).abs() < f64::EPSILON,
            "case {case:?}: expected the {INVOCATION_ATTEMPT_COUNT} of group {:?} to be one more \
             than the {retries} retries its report counts, got {attempts}",
            entry.row.group
        );
        // And the report it came from is the only one that queue holds: one occurrence
        // is reported once.
        let held = received(&emulator, &entry.dead_letter_url).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected the one report the counts claim was written, got {held:?}"
        );
    }
}
