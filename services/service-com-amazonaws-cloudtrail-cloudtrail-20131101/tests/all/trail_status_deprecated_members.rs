// SPDX-License-Identifier: CC0-1.0

//! What `GetTrailStatus` reports in the DEPRECATED String half of its response, driven end
//! to end over the wire through the shared harness (`tests/all/emulator.rs`).
//!
//! THE RULE UNDER TEST: `GetTrailStatusResponse` states the same instants twice — once as
//! a `Date` member (`LatestDeliveryTime`, `StartLoggingTime`, …) and once as a String
//! member the model marks "This field is no longer in use"
//! (`LatestDeliveryAttemptTime`, `TimeLoggingStarted`, …);
//! `smithy_query(query="operation",operation="GetTrailStatus")` lists both halves. The two
//! halves are reported DIFFERENTLY, and that split is the contract: every String member is
//! always PRESENT, carrying the empty string until the instant it reports has happened,
//! while its `Date` twin is OMITTED until then. So a client that reads the deprecated half
//! unconditionally finds the member whatever state the trail is in, and an empty value says
//! "not yet", never "not reported".
//!
//! The rows span the states a trail reaches through this API — never logged, logging, logged
//! and stopped, stopped and started again — and the two forms `Name` takes (a bare name and
//! the trail's ARN), which is what the rule generalises over: no state and no addressing
//! form drops a member.

use serde_json::{json, Value};

use super::emulator::{Emulator, HOME_REGION};

/// The bucket every trail in this suite delivers to; the harness records it for the call.
const BUCKET: &str = "trail-status-logs";

/// Each deprecated String member of the report, with the `Date` member that states the
/// same instant — read off
/// `smithy_query(query="operation",operation="GetTrailStatus")`, whose output carries both
/// halves, and the model doc for each String member ("This field is no longer in use").
///
/// The delivery attempt and the attempt that SUCCEEDED both report the delivery
/// `LatestDeliveryTime` states, and the notification pair likewise.
const STRING_MEMBER_AND_ITS_DATE_TWIN: [(&str, &str); 6] = [
    ("LatestDeliveryAttemptTime", "LatestDeliveryTime"),
    ("LatestDeliveryAttemptSucceeded", "LatestDeliveryTime"),
    ("LatestNotificationAttemptTime", "LatestNotificationTime"),
    ("LatestNotificationAttemptSucceeded", "LatestNotificationTime"),
    ("TimeLoggingStarted", "StartLoggingTime"),
    ("TimeLoggingStopped", "StopLoggingTime"),
];

/// The ARN a trail called `name` carries in [`HOME_REGION`] — the second form `Name` takes
/// ("the name or the CloudTrail ARN of the trail").
fn trail_arn(name: &str) -> String {
    format!(
        "arn:aws:cloudtrail:{HOME_REGION}:{}:trail/{name}",
        engine::emulator_account_id()
    )
}

/// One row: a trail, the logging calls made on it in order, and how it is then addressed.
struct Row {
    case: &'static str,
    name: &'static str,
    /// `StartLogging` / `StopLogging`, in the order the row makes them.
    logging_calls: &'static [&'static str],
    /// Address the trail by its ARN rather than by its bare name.
    by_arn: bool,
}

/// RULE: every deprecated String member of a trail's status is always present — the empty
/// string until the instant it states has happened — while its `Date` twin is omitted until
/// then.
#[tokio::test]
async fn every_deprecated_string_member_is_reported_in_every_trail_state() {
    let emulator = Emulator::new();
    let rows = [
        Row {
            case: "a trail that has never logged",
            name: "status-fresh",
            logging_calls: &[],
            by_arn: false,
        },
        Row {
            case: "a trail that has never logged, addressed by its ARN",
            name: "status-fresh-by-arn",
            logging_calls: &[],
            by_arn: true,
        },
        Row {
            case: "a trail that is logging now",
            name: "status-logging",
            logging_calls: &["StartLogging"],
            by_arn: false,
        },
        Row {
            case: "a trail that logged and stopped",
            name: "status-stopped",
            logging_calls: &["StartLogging", "StopLogging"],
            by_arn: true,
        },
        Row {
            case: "a trail that stopped and started again",
            name: "status-restarted",
            logging_calls: &["StartLogging", "StopLogging", "StartLogging"],
            by_arn: false,
        },
    ];

    for Row {
        case,
        name,
        logging_calls,
        by_arn,
    } in rows
    {
        emulator
            .call(
                "CreateTrail",
                &json!({"Name": name, "S3BucketName": BUCKET}),
            )
            .await
            .expect_ok(case);
        for operation in logging_calls {
            emulator
                .call(operation, &json!({"Name": name}))
                .await
                .expect_ok(&format!("{case}: {operation}"));
        }

        let identifier = if by_arn {
            trail_arn(name)
        } else {
            name.to_string()
        };
        let reply = emulator
            .call("GetTrailStatus", &json!({"Name": identifier}))
            .await;
        let status = reply.expect_ok(case).clone();

        // The logging switch the row ended on is what `IsLogging` reports.
        let logging = logging_calls.last() == Some(&"StartLogging");
        assert_eq!(
            status.get("IsLogging"),
            Some(&json!(logging)),
            "case {case:?}: expected IsLogging {logging}, got {status:?}"
        );

        for (string_member, date_member) in STRING_MEMBER_AND_ITS_DATE_TWIN {
            // The String half is ALWAYS a present string, whatever the trail has done.
            let reported = status.get(string_member).and_then(Value::as_str);
            let Some(reported) = reported else {
                panic!(
                    "case {case:?}: {string_member} must be present as a string, got {status:?}"
                );
            };
            // The `Date` half is present exactly when the instant has happened, and the
            // String half is non-empty exactly then.
            let stated = status.get(date_member).filter(|value| !value.is_null());
            assert_eq!(
                !reported.is_empty(),
                stated.is_some(),
                "case {case:?}: {string_member} must carry a value exactly when \
                 {date_member} is reported, got {string_member}={reported:?} and \
                 {date_member}={stated:?}"
            );
            if let Some(stated) = stated {
                assert!(
                    stated.as_f64().is_some(),
                    "case {case:?}: {date_member} must be reported as a timestamp, got \
                     {stated:?}"
                );
                // The deprecated half carries the same instant in the ISO 8601 form those
                // members take (`2026-10-21T21:38:12Z`), truncated to the second.
                let seconds = stated.as_f64().unwrap_or_default() as i64;
                let expected = aws_smithy_types::DateTime::from_secs(seconds)
                    .fmt(aws_smithy_types::date_time::Format::DateTime)
                    .unwrap_or_default();
                assert_eq!(
                    reported, expected,
                    "case {case:?}: {string_member} must state {date_member}'s instant, got \
                     {reported:?}"
                );
            }
        }

        // Reading the status never moves it: the second read reports the same thing.
        let again = emulator
            .call("GetTrailStatus", &json!({"Name": identifier}))
            .await;
        assert_eq!(
            again.expect_ok(case),
            &status,
            "case {case:?}: a read must report the same status, got {again:?}"
        );
    }
}

/// RULE (the never-logged end of the same rule, stated as the whole body): the status of a
/// trail that has never logged carries `IsLogging` and the six String members as empty
/// strings, and NO `Date` member at all.
#[tokio::test]
async fn a_trail_that_has_never_logged_reports_only_the_string_half() {
    let emulator = Emulator::new();
    let case = "a freshly created trail";
    emulator
        .call(
            "CreateTrail",
            &json!({"Name": "status-never-logged", "S3BucketName": BUCKET}),
        )
        .await
        .expect_ok(case);

    let reply = emulator
        .call("GetTrailStatus", &json!({"Name": "status-never-logged"}))
        .await;
    let status = reply.expect_ok(case);

    let mut expected = json!({"IsLogging": false});
    for (string_member, _) in STRING_MEMBER_AND_ITS_DATE_TWIN {
        expected[string_member] = json!("");
    }
    assert_eq!(
        status, &expected,
        "case {case:?}: expected exactly {expected:?}, got {status:?}"
    );
}
