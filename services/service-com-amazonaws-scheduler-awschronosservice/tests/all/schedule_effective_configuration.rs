// SPDX-License-Identifier: CC0-1.0

//! A schedule is stored, and read back, as its EFFECTIVE configuration.
//!
//! EventBridge Scheduler supplies a value for every optional member a write request
//! omits, and that value is part of the schedule from the moment it is written — so
//! `GetSchedule` answers the configuration the schedule will be invoked under rather than
//! only the subset the request happened to state. The members this suite pins, with the
//! source of each default:
//!
//! * `ActionAfterCompletion` — `NONE`, one of the two values the enum admits
//!   (`smithy_query(query="shape",
//!   shape="com.amazonaws.scheduler#ActionAfterCompletion")`: `NONE | DELETE`).
//! * `ScheduleExpressionTimezone` — `UTC`, the zone an expression with no zone of its own
//!   is read in (`smithy_query(query="shape",
//!   shape="com.amazonaws.scheduler#ScheduleExpressionTimezone")`: `@length 1..50`).
//! * `Target.RetryPolicy` — each member at the MAXIMUM of its modelled range
//!   (`smithy_query(query="shape", shape="com.amazonaws.scheduler#RetryPolicy")`:
//!   `MaximumEventAgeInSeconds` `range 60..86400`, `MaximumRetryAttempts` `range 0..185`),
//!   and each member defaulted on its own, so a policy stating one figure keeps it beside
//!   the other's default.
//!
//! The rule holds for BOTH operations that write a schedule: `UpdateSchedule` is a full
//! replace, so it restates the effective configuration exactly as a create does. Every
//! request goes through the shared [`crate::wire_harness`], the real restJson1 front door.

use crate::wire_harness::{call, scheduler_router};

/// The `ScheduleExpressionTimezone` a write that states none is answered with.
const DEFAULT_TIMEZONE: &str = "UTC";

/// The `ActionAfterCompletion` a write that states none is answered with.
const DEFAULT_ACTION: &str = "NONE";

/// The `RetryPolicy` members a write that states neither is answered with: the maximum of
/// each modelled range.
const DEFAULT_POLICY: (i64, i64) = (86_400, 185);

/// A target ARN of each templated kind, so the rows differ in what the schedule points at
/// as well as in what they state.
const QUEUE: &str = "arn:aws:sqs:eu-west-2:000000000000:q";
const FUNCTION: &str = "arn:aws:lambda:eu-west-2:000000000000:function:fn";
const ROLE: &str = "arn:aws:iam::000000000000:role/r";

/// One row: a whole write body (bar the schedule's own name), and the effective values a
/// read of the written schedule must answer.
struct Row {
    case: &'static str,
    write: serde_json::Value,
    timezone: &'static str,
    action: &'static str,
    /// `(MaximumEventAgeInSeconds, MaximumRetryAttempts)` the schedule reports.
    policy: (i64, i64),
}

/// The classes a write varies over: which of the three defaulted members it states, how
/// much of the retry policy it states, and — for the members that are enums or bounded
/// ranges — each published value rather than only the one a recording happened to carry.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "nothing optional stated at all",
            write: serde_json::json!({
                "ScheduleExpression": "rate(10 minutes)",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": { "Arn": QUEUE, "RoleArn": ROLE }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: DEFAULT_POLICY,
        },
        Row {
            case: "a zone of its own, nothing else",
            write: serde_json::json!({
                "ScheduleExpression": "cron(0 8 1 * ? *)",
                "ScheduleExpressionTimezone": "Europe/London",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": { "Arn": FUNCTION, "RoleArn": ROLE }
            }),
            timezone: "Europe/London",
            action: DEFAULT_ACTION,
            policy: DEFAULT_POLICY,
        },
        Row {
            case: "the zone the service would have supplied, stated explicitly",
            write: serde_json::json!({
                "ScheduleExpression": "rate(1 day)",
                "ScheduleExpressionTimezone": DEFAULT_TIMEZONE,
                "FlexibleTimeWindow": { "Mode": "FLEXIBLE", "MaximumWindowInMinutes": 1440 },
                "Target": { "Arn": QUEUE, "RoleArn": ROLE }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: DEFAULT_POLICY,
        },
        Row {
            case: "the other action after completion",
            write: serde_json::json!({
                "ScheduleExpression": "at(2027-01-01T00:00:00)",
                "ActionAfterCompletion": "DELETE",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": { "Arn": QUEUE, "RoleArn": ROLE }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: "DELETE",
            policy: DEFAULT_POLICY,
        },
        Row {
            case: "an empty retry policy",
            write: serde_json::json!({
                "ScheduleExpression": "rate(5 minutes)",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": { "Arn": QUEUE, "RoleArn": ROLE, "RetryPolicy": {} }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: DEFAULT_POLICY,
        },
        Row {
            case: "only the event age stated, at the lower bound of its range",
            write: serde_json::json!({
                "ScheduleExpression": "rate(5 minutes)",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": {
                    "Arn": QUEUE,
                    "RoleArn": ROLE,
                    "RetryPolicy": { "MaximumEventAgeInSeconds": 60 }
                }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: (60, DEFAULT_POLICY.1),
        },
        Row {
            case: "only the attempt count stated, at the lower bound of its range",
            write: serde_json::json!({
                "ScheduleExpression": "rate(5 minutes)",
                "ActionAfterCompletion": "NONE",
                "FlexibleTimeWindow": { "Mode": "FLEXIBLE", "MaximumWindowInMinutes": 1 },
                "Target": {
                    "Arn": FUNCTION,
                    "RoleArn": ROLE,
                    "RetryPolicy": { "MaximumRetryAttempts": 0 }
                }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: (DEFAULT_POLICY.0, 0),
        },
        Row {
            case: "every defaulted member stated, each at a value of its own",
            write: serde_json::json!({
                "ScheduleExpression": "rate(2 hours)",
                "ScheduleExpressionTimezone": "America/New_York",
                "ActionAfterCompletion": "DELETE",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": {
                    "Arn": QUEUE,
                    "RoleArn": ROLE,
                    "RetryPolicy": { "MaximumEventAgeInSeconds": 120, "MaximumRetryAttempts": 3 }
                }
            }),
            timezone: "America/New_York",
            action: "DELETE",
            policy: (120, 3),
        },
        Row {
            case: "a universal target, which names an API operation rather than a resource",
            write: serde_json::json!({
                "ScheduleExpression": "rate(30 minutes)",
                "FlexibleTimeWindow": { "Mode": "OFF" },
                "Target": {
                    "Arn": "arn:aws:scheduler:::aws-sdk:sqs:sendMessage",
                    "RoleArn": ROLE,
                    "Input": "{\"QueueUrl\":\"http://127.0.0.1/q\",\"MessageBody\":\"m\"}"
                }
            }),
            timezone: DEFAULT_TIMEZONE,
            action: DEFAULT_ACTION,
            policy: DEFAULT_POLICY,
        },
    ]
}

/// What a `GetSchedule` answer says about the three defaulted members.
#[derive(Debug, PartialEq, Eq)]
struct Effective {
    timezone: Option<String>,
    action: Option<String>,
    maximum_event_age_in_seconds: Option<i64>,
    maximum_retry_attempts: Option<i64>,
}

impl Effective {
    /// The effective members a read reports.
    fn of(read: &serde_json::Value) -> Self {
        let text = |value: &serde_json::Value| {
            value.as_str().map(ToString::to_string)
        };
        let policy = &read["Target"]["RetryPolicy"];
        Self {
            timezone: text(&read["ScheduleExpressionTimezone"]),
            action: text(&read["ActionAfterCompletion"]),
            maximum_event_age_in_seconds: policy["MaximumEventAgeInSeconds"].as_i64(),
            maximum_retry_attempts: policy["MaximumRetryAttempts"].as_i64(),
        }
    }

    /// The members a row expects.
    fn expected(timezone: &str, action: &str, policy: (i64, i64)) -> Self {
        Self {
            timezone: Some(timezone.to_string()),
            action: Some(action.to_string()),
            maximum_event_age_in_seconds: Some(policy.0),
            maximum_retry_attempts: Some(policy.1),
        }
    }
}

#[tokio::test]
async fn a_created_schedule_reads_back_as_its_effective_configuration() {
    let router = scheduler_router();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            write,
            timezone,
            action,
            policy,
        } = row;
        let name = format!("effective-{index}");

        let (status, body) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(write.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSchedule answered {status} {body}"
        );

        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSchedule answered {status} {read}"
        );
        assert_eq!(
            Effective::of(&read),
            Effective::expected(timezone, action, policy),
            "case {case:?}: expected ({timezone}, {action}, {policy:?}), got {read}"
        );

        // A read does not change what the next reader sees: the defaults were filled in
        // by the WRITE, so a second read answers the same configuration.
        let (status, again) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            (status, Effective::of(&again)),
            (200, Effective::of(&read)),
            "case {case:?}: a second read must answer the same configuration, got {again}"
        );
    }
}

#[tokio::test]
async fn a_full_replace_update_restates_the_effective_configuration() {
    let router = scheduler_router();
    let name = "effective-update";

    // The schedule starts out stating every defaulted member, so a later write that
    // states none of them is a real replacement rather than a no-op.
    let (status, body) = call(
        &router,
        "POST",
        &format!("/schedules/{name}"),
        Some(serde_json::json!({
            "ScheduleExpression": "rate(1 hour)",
            "ScheduleExpressionTimezone": "Asia/Tokyo",
            "ActionAfterCompletion": "DELETE",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "Target": {
                "Arn": QUEUE,
                "RoleArn": ROLE,
                "RetryPolicy": { "MaximumEventAgeInSeconds": 300, "MaximumRetryAttempts": 7 }
            }
        })),
    )
    .await;
    assert_eq!(status, 200, "the seeding create answered {status} {body}");

    for row in rows() {
        let Row {
            case,
            write,
            timezone,
            action,
            policy,
        } = row;
        let (status, body) = call(
            &router,
            "PUT",
            &format!("/schedules/{name}"),
            Some(write.clone()),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateSchedule answered {status} {body}"
        );

        let (status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSchedule answered {status} {read}"
        );
        assert_eq!(
            Effective::of(&read),
            Effective::expected(timezone, action, policy),
            "case {case:?}: expected ({timezone}, {action}, {policy:?}) after a full-replace \
             update, got {read}"
        );
    }
}
