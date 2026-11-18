// SPDX-License-Identifier: CC0-1.0

//! The instant a schedule's invocation window states is the instant every later
//! read of that schedule reports.
//!
//! `StartDate` ("The date, in UTC, after which the schedule can begin invoking its
//! target") and `EndDate` ("The date, in UTC, before which the schedule can invoke
//! its target") are modelled `timestamp` members
//! (`smithy_query(query="operation", operation="CreateSchedule")`), and this
//! protocol carries a `timestamp` member of a JSON body as DECIMAL epoch seconds.
//! The service holds such an instant at millisecond resolution, so what a read has
//! to answer is the millisecond the request stated — for every remainder, including
//! the overwhelming majority that no binary floating-point number represents
//! exactly.
//!
//! Each suite below drives a TABLE of remainders through both operations that write
//! a window (`CreateSchedule` and `UpdateSchedule`) and reads each one back through
//! `GetSchedule`.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, scheduler_router};

/// A target every row shares: these suites are about the window's instants, and
/// this router mounts no IAM to resolve a role against, so the role is only shaped
/// like one.
fn target() -> serde_json::Value {
    serde_json::json!({
        "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
        "RoleArn": "arn:aws:iam::000000000000:role/r"
    })
}

/// A day, in seconds: how far past its start each row's window closes, so every
/// row states both boundaries and the pair is ordered.
const A_DAY: f64 = 86_400.0;

/// The same day in whole milliseconds, which is what the reported `EndDate` has to
/// stand past the reported `StartDate` by.
const A_DAY_IN_MILLISECONDS: i64 = 86_400_000;

/// A write body stating `window` as its `StartDate`/`EndDate` pair.
fn schedule_body(start: f64, end: f64) -> serde_json::Value {
    serde_json::json!({
        "ScheduleExpression": "rate(5 minutes)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": target(),
        "StartDate": start,
        "EndDate": end,
    })
}

/// One member of a read schedule as the whole epoch-millisecond count it reports:
/// the resolution the service holds the instant at, so a comparison is exact
/// instead of depending on how a decimal remainder lands in a binary float.
fn reported_milliseconds(read: &serde_json::Value, member: &str) -> i64 {
    let seconds = read
        .get(member)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("no {member} in {read}"));
    (seconds * 1_000.0).round() as i64
}

/// `GET /schedules/{Name}`, which has to answer 200 with the schedule just
/// written.
async fn read_schedule(router: &MultiServiceRouter, name: &str) -> serde_json::Value {
    let (status, read) = call(router, "GET", &format!("/schedules/{name}"), None).await;
    assert_eq!(
        status, 200,
        "GetSchedule {name} must answer the schedule, got {status} {read}"
    );
    read
}

/// One row: the decimal epoch seconds a request states, and the whole
/// epoch-millisecond count the service has to report back.
struct InstantCase {
    case: &'static str,
    submitted: f64,
    reported_milliseconds: i64,
}

/// The remainders a window's boundaries are stated with.
///
/// The rows differ along the remainder (none, one, two and three decimal digits,
/// one just under a whole second, one finer than the resolution) and along the
/// magnitude of the second it sits on, which is what decides how far a binary
/// float lands from the decimal value: `.12` on a second of 2099 is a different
/// approximation from `.12` on a second of 1970.
fn instant_cases() -> Vec<InstantCase> {
    let rows: [(&str, f64, i64); 13] = [
        // The remainder of the recorded instant, which no binary float holds: it
        // arrives a few hundred nanoseconds short of 120 ms.
        (
            "two decimal digits on a second far ahead",
            4_085_209_845.12,
            4_085_209_845_120,
        ),
        (
            "the same remainder on a nearer second",
            1_800_000_000.12,
            1_800_000_000_120,
        ),
        (
            "the same remainder on a second of the first day of the epoch",
            86_400.12,
            86_400_120,
        ),
        (
            "one decimal digit",
            4_085_209_845.1,
            4_085_209_845_100,
        ),
        (
            "three decimal digits",
            4_085_209_845.123,
            4_085_209_845_123,
        ),
        (
            "a single millisecond",
            4_085_209_845.001,
            4_085_209_845_001,
        ),
        (
            "a remainder of nine hundred and ninety-nine milliseconds",
            4_085_209_845.999,
            4_085_209_845_999,
        ),
        (
            "a remainder a binary float holds exactly",
            4_085_209_845.5,
            4_085_209_845_500,
        ),
        (
            "a quarter of a second, which a binary float also holds exactly",
            4_085_209_845.25,
            4_085_209_845_250,
        ),
        ("no remainder at all", 4_085_209_845.0, 4_085_209_845_000),
        // Finer than the resolution the service holds: the millisecond the
        // instant is nearest to.
        (
            "microseconds past a millisecond",
            4_085_209_845.123_456,
            4_085_209_845_123,
        ),
        (
            "a remainder under half a millisecond",
            4_085_209_845.000_4,
            4_085_209_845_000,
        ),
        // Past the last millisecond of the second, which is the next second.
        (
            "a remainder within a tenth of a millisecond of the next second",
            4_085_209_845.999_96,
            4_085_209_846_000,
        ),
    ];
    rows.into_iter()
        .map(|(case, submitted, reported_milliseconds)| InstantCase {
            case,
            submitted,
            reported_milliseconds,
        })
        .collect()
}

#[tokio::test]
async fn a_created_window_reports_the_instants_the_request_stated() {
    let router = scheduler_router();

    for (index, row) in instant_cases().iter().enumerate() {
        let name = format!("created-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(row.submitted, row.submitted + A_DAY)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateSchedule must succeed, got {status} {answer}",
            row.case
        );

        let read = read_schedule(&router, &name).await;
        let start = reported_milliseconds(&read, "StartDate");
        assert_eq!(
            start, row.reported_milliseconds,
            "case {:?}: expected StartDate reported at epoch millisecond {}, got {start} \
             ({read})",
            row.case, row.reported_milliseconds
        );
        let end = reported_milliseconds(&read, "EndDate");
        assert_eq!(
            end,
            row.reported_milliseconds + A_DAY_IN_MILLISECONDS,
            "case {:?}: expected EndDate a day past the start, at epoch millisecond {}, got \
             {end} ({read})",
            row.case,
            row.reported_milliseconds + A_DAY_IN_MILLISECONDS
        );
    }
}

#[tokio::test]
async fn an_updated_window_reports_the_instants_the_request_stated() {
    let router = scheduler_router();
    let rows = instant_cases();
    let name = "updated.window_1";
    // One schedule the rows then rewrite, so each row is an UPDATE of a window a
    // different row left behind.
    let (status, answer) = call(
        &router,
        "POST",
        &format!("/schedules/{name}"),
        Some(schedule_body(1_000_000_000.0, 1_000_000_000.0 + A_DAY)),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule must succeed, got {status} {answer}"
    );

    for row in &rows {
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(schedule_body(row.submitted, row.submitted + A_DAY)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: UpdateSchedule must succeed, got {status} {answer}",
            row.case
        );

        let read = read_schedule(&router, name).await;
        let start = reported_milliseconds(&read, "StartDate");
        assert_eq!(
            start, row.reported_milliseconds,
            "case {:?}: expected StartDate reported at epoch millisecond {}, got {start} \
             ({read})",
            row.case, row.reported_milliseconds
        );
        let end = reported_milliseconds(&read, "EndDate");
        assert_eq!(
            end,
            row.reported_milliseconds + A_DAY_IN_MILLISECONDS,
            "case {:?}: expected EndDate a day past the start, at epoch millisecond {}, got \
             {end} ({read})",
            row.case,
            row.reported_milliseconds + A_DAY_IN_MILLISECONDS
        );
    }
}

#[tokio::test]
async fn rewriting_a_schedule_with_what_it_reports_moves_no_instant() {
    // What the rule is FOR: a client that reads a schedule and writes back what it
    // read sees no difference, however many times it reconciles — so comparing
    // desired against actual settles instead of diverging by a millisecond a round.
    let router = scheduler_router();

    for (index, row) in instant_cases().iter().enumerate() {
        let name = format!("reconciled-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(schedule_body(row.submitted, row.submitted + A_DAY)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateSchedule must succeed, got {status} {answer}",
            row.case
        );

        let mut read = read_schedule(&router, &name).await;
        for round in 0..3 {
            let start = read
                .get("StartDate")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_else(|| panic!("no StartDate in {read}"));
            let end = read
                .get("EndDate")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_else(|| panic!("no EndDate in {read}"));
            let (status, answer) = call(
                &router,
                "PUT",
                &format!("/schedules/{name}"),
                Some(schedule_body(start, end)),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?} round {round}: UpdateSchedule must succeed, got {status} {answer}",
                row.case
            );

            read = read_schedule(&router, &name).await;
            assert_eq!(
                (
                    reported_milliseconds(&read, "StartDate"),
                    reported_milliseconds(&read, "EndDate")
                ),
                (
                    row.reported_milliseconds,
                    row.reported_milliseconds + A_DAY_IN_MILLISECONDS
                ),
                "case {:?} round {round}: writing back what the read reported must move \
                 nothing, got {read}",
                row.case
            );
        }
    }
}
