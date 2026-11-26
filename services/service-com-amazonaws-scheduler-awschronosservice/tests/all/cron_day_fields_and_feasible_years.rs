// SPDX-License-Identifier: CC0-1.0

//! The two rules a `cron(...)` `ScheduleExpression` is held to beyond its fields,
//! driven over the real `restJson1` front door on BOTH operations that write a
//! schedule.
//!
//! * the cron form carries a day-of-month field and a day-of-week field, and exactly
//!   one of the two states "no specific value" (`?`): an expression that states a day
//!   in both names instants the form does not define, and one that states `?` in both
//!   names no day at all, so neither is a schedule — refused 400 `ValidationException`
//!   in the malformed-expression class, `Invalid Schedule Expression <expression>.`;
//! * an expression the service CAN read still has to name an occurrence inside the
//!   span from its window's opening to the end of the lookahead this service names
//!   instants in: one whose years all fall outside that span invokes its target never,
//!   which is a different condition and a different sentence — `The schedule
//!   expression <expression>, with StartDate <start> and EndDate <end>, is invalid
//!   because it will never schedule an invocation.`, with each boundary echoed as the
//!   service states it and `null` for one the request left out.
//!
//! Each suite is a TABLE over the class — the day fields each carry every form the
//! grammar and the service's own dialect admit, and the year field is written as a
//! year, a range, a list, a step and `*`, inside and outside the lookahead — rather
//! than one recorded expression. A refused write leaves nothing behind (create) and
//! leaves the stored schedule as it was (update).

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, scheduler_router};

/// The target every row shares: this suite is about the schedule's expression, and
/// this router mounts no IAM for a role to be resolved against.
fn target() -> serde_json::Value {
    serde_json::json!({
        "Arn": "arn:aws:sqs:eu-west-2:000000000000:q",
        "RoleArn": "arn:aws:iam::000000000000:role/r"
    })
}

/// The expression a seeded schedule carries, which a refused update must leave on it.
const SEEDED_EXPRESSION: &str = "rate(5 minutes)";

/// A write body stating `expression` and, where a row gives them, the two window
/// boundaries as the epoch seconds this protocol carries a timestamp in.
fn body(expression: &str, start: Option<i64>, end: Option<i64>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "ScheduleExpression": expression,
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": target()
    });
    let object = body.as_object_mut().expect("the body under construction");
    for (member, instant) in [("StartDate", start), ("EndDate", end)] {
        if let Some(instant) = instant {
            object.insert(member.to_string(), serde_json::json!(instant));
        }
    }
    body
}

/// One row: the expression written, the window it is written with — each boundary as
/// the seconds submitted beside the text the service echoes it as — and what the
/// service answers.
struct Row {
    case: &'static str,
    expression: &'static str,
    start: Option<(i64, &'static str)>,
    end: Option<(i64, &'static str)>,
    answer: Answer,
}

/// What a write of one row is answered with.
enum Answer {
    /// The expression states a schedule the service can read and fire.
    Accepted,
    /// The service cannot read the expression at all.
    Malformed,
    /// The service reads it, and it names no occurrence the window could hold.
    NeverFires,
}

/// What the boundary echoes in a never-fires sentence: the text a row states, or the
/// literal the service writes for a boundary the request left out.
fn echoed(boundary: Option<(i64, &str)>) -> String {
    boundary.map_or("null".to_string(), |(_, echo)| echo.to_string())
}

impl Row {
    /// The body this row writes.
    fn body(&self) -> serde_json::Value {
        body(
            self.expression,
            self.start.map(|(seconds, _)| seconds),
            self.end.map(|(seconds, _)| seconds),
        )
    }

    /// The message the service answers this row with, or `None` for an accepted row.
    fn message(&self) -> Option<String> {
        match self.answer {
            Answer::Accepted => None,
            Answer::Malformed => Some(format!(
                "Invalid Schedule Expression {}.",
                self.expression
            )),
            Answer::NeverFires => Some(format!(
                "The schedule expression {}, with StartDate {} and EndDate {}, is invalid \
                 because it will never schedule an invocation.",
                self.expression,
                echoed(self.start),
                echoed(self.end)
            )),
        }
    }
}

/// Create the schedule a row's update is applied to, and answer its name.
async fn seeded(router: &MultiServiceRouter, name: &str) -> String {
    let (status, answer) = call(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(body(SEEDED_EXPRESSION, None, None)),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule {name} must succeed, got {status} {answer}"
    );
    name.to_string()
}

/// Drive `rows` through `CreateSchedule` and `UpdateSchedule`.
async fn assert_expression_rows(rows: &[Row]) {
    let router = scheduler_router();
    let held = seeded(&router, "held.schedule").await;

    for (index, row) in rows.iter().enumerate() {
        let name = format!("created-{index}");
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(row.body()),
        )
        .await;
        let (read_status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        match row.message() {
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: CreateSchedule must accept it, got {status} {answer}",
                    row.case
                );
                assert_eq!(
                    read_status, 200,
                    "case {:?}: the schedule must be persisted, got {read_status} {read}",
                    row.case
                );
                assert_eq!(
                    read.get("ScheduleExpression")
                        .and_then(serde_json::Value::as_str),
                    Some(row.expression),
                    "case {:?}: the expression must round-trip, got {read}",
                    row.case
                );
            }
            Some(expected) => {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (400, "ValidationException"),
                    "case {:?}: CreateSchedule must refuse it 400 ValidationException, got \
                     {status} {answer}",
                    row.case
                );
                assert_eq!(
                    error_message(&answer),
                    expected,
                    "case {:?}: expected the message {expected:?}, got {:?}",
                    row.case,
                    error_message(&answer)
                );
                assert_eq!(
                    (read_status, error_code(&read).as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {:?}: a refused create persists nothing, got {read_status} {read}",
                    row.case
                );
            }
        }

        // The same rule on the other write path, against a schedule that exists.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{held}"),
            Some(row.body()),
        )
        .await;
        let (_, held_read) = call(&router, "GET", &format!("/schedules/{held}"), None).await;
        let stored = held_read
            .get("ScheduleExpression")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        match row.message() {
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: UpdateSchedule must accept it, got {status} {answer}",
                    row.case
                );
                assert_eq!(
                    stored, row.expression,
                    "case {:?}: an accepted update must be applied, got {held_read}",
                    row.case
                );
                // Put the held schedule back as the next row expects to find it.
                let (status, answer) = call(
                    &router,
                    "PUT",
                    &format!("/schedules/{held}"),
                    Some(body(SEEDED_EXPRESSION, None, None)),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: restoring the held schedule, got {status} {answer}",
                    row.case
                );
            }
            Some(expected) => {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (400, "ValidationException"),
                    "case {:?}: UpdateSchedule must refuse it 400 ValidationException, got \
                     {status} {answer}",
                    row.case
                );
                assert_eq!(
                    error_message(&answer),
                    expected,
                    "case {:?}: expected the message {expected:?} from the update too, got {:?}",
                    row.case,
                    error_message(&answer)
                );
                assert_eq!(
                    stored, SEEDED_EXPRESSION,
                    "case {:?}: a refused update changes nothing, got {held_read}",
                    row.case
                );
            }
        }
    }
}

#[tokio::test]
async fn exactly_one_of_the_two_day_fields_states_no_specific_value() {
    let rows = [
        Row {
            case: "a day of the month",
            expression: "cron(0 12 1 * ? *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "every day of the month",
            expression: "cron(0 12 * * ? *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "a range of weekdays",
            expression: "cron(0 12 ? * MON-FRI *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "the last day of the month",
            expression: "cron(0 12 L * ? *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "the last day of the week",
            expression: "cron(0 12 ? * L *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "the nearest weekday to a day of the month",
            expression: "cron(0 12 3W * ? *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "the nth weekday of the month",
            expression: "cron(0 12 ? * 3#2 *)",
            start: None,
            end: None,
            answer: Answer::Accepted,
        },
        // Both day fields stating a day, in the shared grammar's forms and in the
        // service's own dialect alike.
        Row {
            case: "the last day of the month beside the nth weekday",
            expression: "cron(0 12 L * 3#2 *)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
        Row {
            case: "the nearest weekday beside the last day of the week",
            expression: "cron(0 12 3W * L *)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
        Row {
            case: "a day of the month beside a named weekday",
            expression: "cron(0 12 1 * MON *)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
        Row {
            case: "every day of the month beside every day of the week",
            expression: "cron(0 12 * * * *)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
        // Neither day field stating a day states no day at all.
        Row {
            case: "no specific value in either day field",
            expression: "cron(0 12 ? * ? *)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
    ];

    assert_expression_rows(&rows).await;
}

#[tokio::test]
async fn an_expression_naming_no_reachable_occurrence_is_refused_as_never_firing() {
    // The instant a row's window opens at: 2027-01-01T00:00:00 UTC, which the service
    // echoes at minute resolution, and 2027-01-01T00:00:30.120 UTC, which it echoes
    // with the seconds and the millisecond remainder it holds.
    const OPENS: (i64, &str) = (1_798_761_600, "2027-01-01T00:00");
    const CLOSES: (i64, &str) = (1_830_297_600, "2028-01-01T00:00");
    let rows = [
        Row {
            case: "every year",
            expression: "cron(0 12 * * ? *)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "a range reaching the end of the lookahead",
            expression: "cron(0 12 * * ? 2026-2199)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "the last year of the lookahead",
            expression: "cron(0 12 * * ? 2100)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Accepted,
        },
        // Years the service reads and names no reachable occurrence at.
        Row {
            case: "a year past the lookahead",
            expression: "cron(0 12 * * ? 2176)",
            start: Some(OPENS),
            end: None,
            answer: Answer::NeverFires,
        },
        Row {
            case: "the last year the service publishes",
            expression: "cron(0 12 * * ? 2199)",
            start: None,
            end: None,
            answer: Answer::NeverFires,
        },
        Row {
            case: "a range wholly past the lookahead",
            expression: "cron(0 12 * * ? 2150-2199)",
            start: Some(OPENS),
            end: Some(CLOSES),
            answer: Answer::NeverFires,
        },
        Row {
            case: "a list of years past the lookahead",
            expression: "cron(0 12 * * ? 2101,2176)",
            start: None,
            end: None,
            answer: Answer::NeverFires,
        },
        Row {
            case: "a step opening past the lookahead",
            expression: "cron(0 12 * * ? 2120/5)",
            start: None,
            end: None,
            answer: Answer::NeverFires,
        },
        Row {
            case: "a year the window has already left behind",
            expression: "cron(0 12 * * ? 1971)",
            start: None,
            end: None,
            answer: Answer::NeverFires,
        },
        Row {
            case: "a year behind the window's own opening",
            expression: "cron(0 12 * * ? 2026)",
            start: Some(OPENS),
            end: None,
            answer: Answer::NeverFires,
        },
        // A year outside the range the service publishes is a field it cannot read at
        // all, which is the malformed class rather than this one.
        Row {
            case: "a year past the published range",
            expression: "cron(0 12 * * ? 2200)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Malformed,
        },
        Row {
            case: "a year before the published range",
            expression: "cron(0 12 * * ? 1969)",
            start: None,
            end: None,
            answer: Answer::Malformed,
        },
        // The other two forms name no year, so neither is this rule's.
        Row {
            case: "a rate",
            expression: "rate(5 minutes)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Accepted,
        },
        Row {
            case: "a one-time instant",
            expression: "at(2030-01-01T00:00:00)",
            start: Some(OPENS),
            end: None,
            answer: Answer::Accepted,
        },
    ];

    assert_expression_rows(&rows).await;
}
