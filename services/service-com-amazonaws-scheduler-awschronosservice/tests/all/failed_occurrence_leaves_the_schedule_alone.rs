// SPDX-License-Identifier: CC0-1.0

//! What a failed occurrence does TO THE SCHEDULE it belongs to — nothing — and that it
//! is reported all the same: end to end over the wire through the real
//! `MultiServiceRouter`, with Lambda, Amazon SQS and IAM mounted beside EventBridge
//! Scheduler on ONE engine, so both halves are asserted where a client observes them —
//! the report on the queue `Target.DeadLetterConfig.Arn` names, and the schedule
//! `GetSchedule` reports afterwards.
//!
//! RULE: an occurrence whose target call is refused — by the target's own service,
//! because the resource the `Target.Arn` names is not there, or by the permissions of
//! `Target.RoleArn`, which the invocation is made under — is a failed event, so it is
//! delivered to the queue `Target.DeadLetterConfig` names ("EventBridge Scheduler
//! delivers failed events that could not be successfully delivered to a target to the
//! queue") once the `Target.RetryPolicy` is exhausted, and the schedule ITSELF is
//! untouched by it: a schedule is the configuration its owner last wrote, so every
//! member a client reads back — `State`, "specifies whether the schedule is enabled or
//! disabled", among them — is exactly what that write left, and a recurring
//! `ScheduleExpression` keeps naming the occurrences after the failed one, each
//! reported in its turn.
//!
//! The rows of the first suite span the class rather than one recorded case: they
//! differ along the KIND of target whose call is refused (a function and a queue),
//! along WHY it is refused (the target was deleted after the schedule was written, the
//! account never held it, and the execution role withholds the invoking action), along
//! the group the schedule lives in, and along the retry policy it states — over both
//! published bounds of `MaximumRetryAttempts` (`smithy_query` query="shape"
//! shape="RetryPolicy": `range 0..185`) and a schedule stating no policy at all.
//!
//! The second suite takes the same rule over a RECURRING series, which is the shape a
//! schedule whose target has gone breaks in: the series keeps running, every occurrence
//! of it is reported exactly once, and the schedule keeps reporting the state it was
//! written with.
//!
//! What a report CARRIES, and which failures are reported at all, is
//! `tests/all/dead_letter_delivery.rs` and `tests/all/function_target_dead_letter.rs`;
//! how many attempts a policy admits is asserted without a wait in
//! `src/state/dead_letter.rs`'s own tests.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_scheduler_awschronosservice::state::invocation;
use smithy_server_runtime::MultiServiceRouter;

// The queue reader that leaves a report where it is, the reading of a report's body as
// the request document it is, and the discovery of a refusal's code from the target's
// own API are this crate's own (`tests/all/dead_letter_delivery.rs`); the Lambda-beside-
// Scheduler emulator, the function and the role helpers are
// `tests/all/per_schedule_target_input.rs`'s, and the deletion of a function is
// `tests/all/function_target_dead_letter.rs`'s — so this suite spells none of them
// again.
use crate::dead_letter_delivery::{body_document, messages_once, received, refusal_code};
use crate::function_target_dead_letter::delete_function;
use crate::per_schedule_target_input::{
    emulator_with_lambda, given_function, given_group, given_invoking_role, INVOKE_FUNCTION,
};
use crate::schedule_invocation::{
    at_expression_body, given_queue, given_schedule, schedule_read, seconds_from_now,
    POLL_INTERVAL, REGION,
};
use crate::wire_harness::account_id;

/// How long the first suite waits for the report of one row's occurrence. Every row's
/// schedule states its one occurrence [`OCCURRENCE_IN`] seconds ahead; the rest is room
/// for every row's setup, the clock's tick, the refused invocation and the dead-letter
/// delivery.
const PATIENCE: Duration = Duration::from_secs(90);

/// How long the second suite waits for the occurrences of a one-minute series to be
/// reported. It stops as soon as they have been: the figure is only the deadline that
/// ends the poll, so it is generous enough for a loaded host to serve
/// [`OCCURRENCES_AWAITED`] periods and the deliveries after them.
const SERIES_PATIENCE: Duration = Duration::from_secs(240);

/// How long a suite keeps watching a queue after the reports it expected have arrived,
/// so a second report of one occurrence has had its chance to appear. Nothing is
/// asserted about how long anything took: the window is only how long the absence is
/// watched for.
const QUIET_WINDOW: Duration = Duration::from_secs(8);

/// How far ahead of the write the first suite's rows state their one occurrence: the
/// occurrence has to still be ahead once every row's queue, function, role and schedule
/// has been written.
const OCCURRENCE_IN: u64 = 8;

/// The recurring expression the second suite states, and the shortest cadence a
/// `rate(...)` expression can name at all.
const ONE_MINUTE_RATE: &str = "rate(1 minute)";

/// The period [`ONE_MINUTE_RATE`] names, which consecutive occurrences of that series
/// stand apart.
const PERIOD: Duration = Duration::from_secs(60);

/// How many occurrences of that series the second suite waits to see reported: enough
/// that the schedule is observed firing again AFTER an occurrence failed.
const OCCURRENCES_AWAITED: usize = 2;

/// The group a schedule lands in when the write names none.
const DEFAULT_GROUP: &str = "default";

/// The group the one row that states a group of its own lives in.
const OWN_GROUP: &str = "ops";

/// The `State` every schedule below is written with, and the one every read of it must
/// still report.
const ENABLED: &str = "ENABLED";

/// The IAM action a queue's invocation — and a dead-letter delivery, which is the same
/// call — is authorized by.
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The refusal Lambda answers an invoke of a function it does not hold with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The refusal AWS's authorization layer answers a call the caller's permissions
/// withhold with, in the modern form Lambda's protocol spells it in.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The same refusal as a QUERY-COMPATIBLE service answers it, Amazon SQS among them:
/// the legacy Query code, which is the one an SDK surfaces for such a service.
const QUERY_ACCESS_DENIED: &str = "AccessDenied";

/// The largest `MaximumRetryAttempts` the member admits (`range 0..185`).
const MOST_RETRY_ATTEMPTS: i64 = 185;

/// WHICH kind of target a row's schedule names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A Lambda function, invoked by `lambda:InvokeFunction`.
    Function,
    /// An Amazon SQS queue, invoked by `sqs:SendMessage`.
    Queue,
}

/// WHY a row's invocation of that target is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failing {
    /// The target existed when the schedule was written and was DELETED before the
    /// occurrence, which is how a working schedule most often breaks.
    Deleted,
    /// The `Target.Arn` names a resource this account never held.
    NeverHeld,
    /// The target is there, and the execution role withholds the invoking action.
    Withheld,
}

/// One row: the target the schedule names, why its invocation is refused, where the
/// schedule lives, what retry policy it states, and the code the refusing service
/// answers with.
struct Row {
    case: &'static str,
    kind: Kind,
    failing: Failing,
    /// `GroupName`, or `None` for the account's implicit `default` group.
    group: Option<&'static str>,
    /// `Target.RetryPolicy.MaximumRetryAttempts`, or `None` for a schedule stating no
    /// retry policy at all.
    retries: Option<i64>,
    /// The error code the report carries, or `None` where the row discovers it from the
    /// target's own API rather than stating one.
    refused_with: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a function target deleted after the schedule was written, no retry admitted",
        kind: Kind::Function,
        failing: Failing::Deleted,
        group: None,
        retries: Some(0),
        refused_with: Some(RESOURCE_NOT_FOUND),
    },
    Row {
        case: "a function target this account never held, the most retries admitted",
        kind: Kind::Function,
        failing: Failing::NeverHeld,
        group: None,
        retries: Some(MOST_RETRY_ATTEMPTS),
        refused_with: Some(RESOURCE_NOT_FOUND),
    },
    Row {
        case: "a function target the execution role may not invoke, in a group of its own",
        kind: Kind::Function,
        failing: Failing::Withheld,
        group: Some(OWN_GROUP),
        retries: None,
        refused_with: Some(ACCESS_DENIED),
    },
    Row {
        case: "a queue target the execution role may not write to",
        kind: Kind::Queue,
        failing: Failing::Withheld,
        group: None,
        retries: Some(1),
        refused_with: Some(QUERY_ACCESS_DENIED),
    },
    Row {
        case: "a queue target this account never held",
        kind: Kind::Queue,
        failing: Failing::NeverHeld,
        group: None,
        retries: Some(2),
        // The code is the one Amazon SQS itself answers such a call with, read off that
        // service rather than stated here.
        refused_with: None,
    },
    Row {
        case: "a queue target deleted after the schedule was written",
        kind: Kind::Queue,
        failing: Failing::Deleted,
        group: None,
        retries: None,
        refused_with: None,
    },
];

/// What one row was prepared with, and what it is then judged by.
struct Prepared<'row> {
    row: &'row Row,
    /// The schedule's name and the group it lives in.
    name: String,
    group: &'static str,
    /// The queue its failed occurrence is reported to.
    dead_letter_url: String,
    /// The target the occurrence could not reach.
    target_arn: String,
    /// The `Target.Input` the occurrence carried, which is the payload the report's own
    /// request document states.
    marker: String,
    /// The error code the report must carry.
    refused_with: String,
    /// The schedule as a client read it BEFORE the occurrence: the document every read
    /// after the failure must still answer.
    written: Value,
}

/// Every row: the failed occurrence is reported once to the schedule's dead-letter
/// queue, and the schedule a client reads afterwards is byte for byte the one the write
/// left — `State` included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_occurrence_is_reported_and_leaves_its_schedule_exactly_as_written() {
    let (emulator, _engine) = emulator_with_lambda();
    given_group(&emulator, "the group one row lives in", OWN_GROUP).await;
    // Every row's schedule names the SAME instant, so the suite waits for one
    // occurrence rather than one per row.
    let occurrence = seconds_from_now(OCCURRENCE_IN);
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&emulator, case, &format!("unchanged-dlq-{index}")).await;
        let (target_arn, target_url) = given_target(&emulator, row, index).await;
        // The role may always write to the row's own dead-letter queue, so the report
        // is delivered whatever refused the invocation itself.
        let mut granted = vec![(SEND_MESSAGE, dead_letter_arn.as_str())];
        let invoking = match row.kind {
            Kind::Function => INVOKE_FUNCTION,
            Kind::Queue => SEND_MESSAGE,
        };
        if row.failing != Failing::Withheld {
            granted.push((invoking, target_arn.as_str()));
        }
        let role =
            given_invoking_role(&emulator, case, &format!("unchanged-role-{index}"), &granted).await;
        let marker = json!({ "row": index }).to_string();
        let mut target = json!({
            "Arn": target_arn,
            "RoleArn": role,
            "Input": marker,
            "DeadLetterConfig": { "Arn": dead_letter_arn },
        });
        if let Some(retries) = row.retries {
            target["RetryPolicy"] = json!({ "MaximumRetryAttempts": retries });
        }
        let mut body = json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": ENABLED,
            "Target": target,
        });
        if let Some(group) = row.group {
            body["GroupName"] = json!(group);
        }
        let name = format!("unchanged-{index}");
        given_schedule(&emulator, case, &name, body).await;
        let group = row.group.unwrap_or(DEFAULT_GROUP);
        let written = schedule_as_read(&emulator, case, group, &name).await;
        // A row that states no code discovers it where a client would: from the answer
        // the target's own API gives the very call the invocation attempts.
        let refused_with = match row.refused_with {
            Some(code) => code.to_string(),
            None => refusal_code(&emulator, case, &target_url).await,
        };
        prepared.push(Prepared {
            row,
            name,
            group,
            dead_letter_url,
            target_arn,
            marker,
            refused_with,
            written,
        });
    }

    for prep in &prepared {
        let case = prep.row.case;
        let held = messages_once(&emulator, &prep.dead_letter_url, 1, PATIENCE).await;
        let report = held
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: nothing was dead-lettered"));
        assert_eq!(
            undelivered_payload(report),
            prep.marker,
            "case {case:?}: expected the report of the occurrence carrying {:?}, got {report}",
            prep.marker
        );
        assert_eq!(
            attribute(report, "ERROR_CODE"),
            Some(prep.refused_with.clone()),
            "case {case:?}: expected {:?} to be reported, got {report}",
            prep.refused_with
        );
        assert_eq!(
            attribute(report, "TARGET_ARN"),
            Some(prep.target_arn.clone()),
            "case {case:?}: expected the target {:?} to be reported, got {report}",
            prep.target_arn
        );
        // The schedule a client reads after the failure is the one the write left:
        // nothing of it moved because an occurrence could not be delivered.
        let read = schedule_as_read(&emulator, case, prep.group, &prep.name).await;
        assert_eq!(
            read, prep.written,
            "case {case:?}: expected the failed occurrence to leave the schedule as written \
             ({}), got {read}",
            prep.written
        );
    }

    // Each queue holds that ONE report and no other, and each schedule keeps reading
    // the same: a refusal of the request is one failed event, and it changes nothing.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        for prep in &prepared {
            let case = prep.row.case;
            let held = received(&emulator, &prep.dead_letter_url).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected exactly one report on the dead-letter queue, got {held:?}"
            );
            let read = schedule_as_read(&emulator, case, prep.group, &prep.name).await;
            assert_eq!(
                read, prep.written,
                "case {case:?}: expected the schedule to keep reading as written, got {read}"
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Target.Input` the recurring series below hands its target, which the report of
/// each failed occurrence carries as the payload its request document states.
const SERIES_INPUT: &str = r#"{"marker":"series"}"#;

/// A recurring schedule whose function target has GONE keeps its series: every
/// occurrence is reported to the dead-letter queue exactly once, consecutive reported
/// occurrences stand one period apart, and the schedule keeps reading exactly as it was
/// written — still `ENABLED`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recurring_schedule_whose_target_has_gone_reports_each_occurrence_once() {
    let case = "a recurring schedule whose function target was deleted";
    let (emulator, _engine) = emulator_with_lambda();
    let (dead_letter_url, dead_letter_arn) = given_queue(&emulator, case, "series-dlq").await;
    let function = "series-handler";
    let function_arn = given_function(&emulator, case, function).await;
    // The role allows the invocation AND the report, so what fails the occurrence is
    // the target's absence and nothing else.
    let role = given_invoking_role(
        &emulator,
        case,
        "series-role",
        &[
            (INVOKE_FUNCTION, function_arn.as_str()),
            (SEND_MESSAGE, dead_letter_arn.as_str()),
        ],
    )
    .await;
    let name = "series";
    given_schedule(
        &emulator,
        case,
        name,
        json!({
            "ScheduleExpression": ONE_MINUTE_RATE,
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": ENABLED,
            "Target": {
                "Arn": function_arn,
                "RoleArn": role,
                "Input": SERIES_INPUT,
                // A refusal of the request itself is never retried, so the report of
                // each occurrence is of its one attempt.
                "RetryPolicy": { "MaximumRetryAttempts": 0 },
                "DeadLetterConfig": { "Arn": dead_letter_arn },
            },
        }),
    )
    .await;
    // The target goes while the schedule stays exactly as it was written.
    delete_function(&emulator, case, function).await;
    let written = schedule_as_read(&emulator, case, DEFAULT_GROUP, name).await;

    let deadline = Instant::now() + SERIES_PATIENCE;
    let held = loop {
        let held = received(&emulator, &dead_letter_url).await;
        let occurrences: BTreeSet<String> = held.iter().map(scheduled_time_reported).collect();
        if occurrences.len() >= OCCURRENCES_AWAITED || Instant::now() >= deadline {
            break held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let mut reports_per_occurrence: BTreeMap<String, usize> = BTreeMap::new();
    for report in &held {
        let scheduled = scheduled_time_reported(report);
        assert!(
            !scheduled.is_empty(),
            "case {case:?}: expected every report to name the occurrence it is of, got {report}"
        );
        *reports_per_occurrence.entry(scheduled).or_default() += 1;
        assert_eq!(
            attribute(report, "ERROR_CODE"),
            Some(RESOURCE_NOT_FOUND.to_string()),
            "case {case:?}: expected {RESOURCE_NOT_FOUND} to be reported, got {report}"
        );
        assert_eq!(
            attribute(report, "TARGET_ARN"),
            Some(function_arn.clone()),
            "case {case:?}: expected the target {function_arn:?} to be reported, got {report}"
        );
        assert_eq!(
            undelivered_payload(report),
            SERIES_INPUT,
            "case {case:?}: expected the undelivered payload {SERIES_INPUT:?}, got {report}"
        );
    }
    assert!(
        reports_per_occurrence.len() >= OCCURRENCES_AWAITED,
        "case {case:?}: expected at least {OCCURRENCES_AWAITED} occurrences of \
         {ONE_MINUTE_RATE:?} reported, got {reports_per_occurrence:?}"
    );
    for (scheduled, reports) in &reports_per_occurrence {
        assert_eq!(
            *reports, 1,
            "case {case:?}: expected the occurrence at {scheduled:?} reported exactly once, got \
             {reports} reports of it"
        );
    }
    // The occurrences the expression keeps naming: the reported instants are the
    // series' own, one period apart, so the schedule fired again after one failed.
    let instants: Vec<SystemTime> = reports_per_occurrence
        .keys()
        .filter_map(|text| invocation::scheduled_time_of(text))
        .collect();
    assert_eq!(
        instants.len(),
        reports_per_occurrence.len(),
        "case {case:?}: expected every reported occurrence in the published form, got \
         {reports_per_occurrence:?}"
    );
    for pair in instants.windows(2) {
        let gap = pair[1].duration_since(pair[0]).unwrap_or_default();
        assert_eq!(
            gap, PERIOD,
            "case {case:?}: expected consecutive reported occurrences {PERIOD:?} apart, got \
             {gap:?} between {:?} and {:?}",
            pair[0], pair[1]
        );
    }

    // The schedule is untouched by any of it, and stays so while the queue is watched:
    // EventBridge Scheduler does not disable, delete or otherwise move a schedule whose
    // target has gone.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let read = schedule_as_read(&emulator, case, DEFAULT_GROUP, name).await;
        assert_eq!(
            read, written,
            "case {case:?}: expected the schedule to keep reading as written ({written}), got \
             {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The target one row's schedule names: its ARN, and the url Amazon SQS addresses it by
/// for a QUEUE target, which is the address a row that discovers its own refusal code
/// probes.
///
/// A target the account NEVER HELD is named by the ARN such a resource would carry and
/// nothing is created for it; a DELETED one is created and then removed, which is the
/// same answer from the target's own API by another route.
async fn given_target(router: &MultiServiceRouter, row: &Row, index: usize) -> (String, String) {
    let case = row.case;
    match row.kind {
        Kind::Function => {
            let name = format!("unchanged-fn-{index}");
            let arn = match row.failing {
                Failing::NeverHeld => format!(
                    "arn:aws:lambda:{REGION}:{}:function:{name}",
                    account_id()
                ),
                Failing::Deleted => {
                    let arn = given_function(router, case, &name).await;
                    delete_function(router, case, &name).await;
                    arn
                }
                Failing::Withheld => given_function(router, case, &name).await,
            };
            // A function is not addressed by a url: the row states its own code.
            (arn, String::new())
        }
        Kind::Queue => {
            let name = format!("unchanged-queue-{index}");
            match row.failing {
                Failing::NeverHeld => (
                    format!("arn:aws:sqs:{REGION}:{}:{name}", account_id()),
                    format!("https://sqs.{REGION}.amazonaws.com/{}/{name}", account_id()),
                ),
                Failing::Deleted => {
                    let (url, arn) = given_queue(router, case, &name).await;
                    delete_queue(router, case, &url).await;
                    (arn, url)
                }
                Failing::Withheld => {
                    let (url, arn) = given_queue(router, case, &name).await;
                    (arn, url)
                }
            }
        }
    }
}

/// Delete the queue at `url`, so an invocation of it is refused the way one of a queue
/// nothing holds is.
async fn delete_queue(router: &MultiServiceRouter, case: &str, url: &str) {
    let (status, deleted) = crate::schedule_invocation::sqs(
        router,
        "DeleteQueue",
        &json!({ "QueueUrl": url }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteQueue answered {status} {deleted}"
    );
}

/// The schedule of `group` as a client reads it, which must resolve.
async fn schedule_as_read(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> Value {
    let (status, read) = schedule_read(router, group, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSchedule answered {status} {read}"
    );
    assert_eq!(
        read["State"].as_str(),
        Some(ENABLED),
        "case {case:?}: expected the schedule to report State={ENABLED}, got {read}"
    );
    read
}

/// One message attribute of a report, as a client reads it.
fn attribute(report: &Value, name: &str) -> Option<String> {
    report["MessageAttributes"][name]["StringValue"]
        .as_str()
        .map(ToString::to_string)
}

/// The occurrence one report is OF, as its own attribute names it.
fn scheduled_time_reported(report: &Value) -> String {
    attribute(report, "SCHEDULED_TIME").unwrap_or_default()
}

/// The payload one report's occurrence could not deliver: a report carries the failed
/// EVENT — the request the invocation attempted of its target's own API — so the
/// payload is read from the member of that request which takes it (`Payload` for
/// Lambda's `Invoke`, `MessageBody` for Amazon SQS's `SendMessage`).
fn undelivered_payload(report: &Value) -> String {
    let event = body_document(report);
    ["Payload", "MessageBody"]
        .iter()
        .find_map(|member| event[*member].as_str().map(ToString::to_string))
        .unwrap_or_else(|| event.to_string())
}
