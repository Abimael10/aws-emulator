// SPDX-License-Identifier: CC0-1.0

//! What the refusal of a `RestoreTableToPointInTime` outside the restorable window SAYS.
//!
//! The rule pinned here: a refusal that turns on a value the request named and a bound the
//! addressed resource's own state derives STATES those values — the instant asked for, the
//! two bounds of the window it missed and the table they belong to — interpolated into the
//! error's fixed sentence, so a caller can correct the request from the refusal alone
//! instead of making a second call to read the window.
//!
//! The rows differ along what the rule generalises over: which side of the window the
//! instant falls on, how far outside it sits, whether it carries a fraction of a second,
//! whether it is before the epoch at all, how long a recovery period the window was given,
//! and whether the request addresses its source table by name or by ARN. The recorded case
//! — one instant years before a window — is one row of that table.
//!
//! The two bounds are read from `DescribeContinuousBackups` on either side of the refused
//! call, and the quoted bounds are held between those two readings: the window MOVES with
//! the clock (its newest point advances), so what is pinned is that the sentence names the
//! window in force at the call rather than a fixed instant.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use, and every wait is a poll for the condition it waits on.

use std::time::Duration;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{
    call, describe_when_active, emulator, error_code, message, ok, restorable_window,
    POLL_INTERVAL, REGION,
};

/// How long a poll for the restorable window to widen may take before this suite calls it
/// a hang. Generous slack over the emulator's own transient span: a loaded machine is not a
/// defect, and the point of the bound is that the wait ENDS rather than how fast it does.
const WINDOW_DEADLINE: Duration = Duration::from_secs(60);

/// How many seconds one day holds, for the rows that place an instant whole days outside
/// the window.
const DAY: f64 = 24.0 * 60.0 * 60.0;

/// One active table whose point-in-time recovery is on, given `period` days of recovery
/// where the row names one.
async fn table_with_recovery(router: &MultiServiceRouter, name: &str, period: Option<i32>) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, name, name).await;
    let mut specification = json!({"PointInTimeRecoveryEnabled": true});
    if let Some(days) = period {
        specification["RecoveryPeriodInDays"] = json!(days);
    }
    ok(
        router,
        "UpdateContinuousBackups",
        json!({"TableName": name, "PointInTimeRecoverySpecification": specification}),
    )
    .await;
}

/// Poll `DescribeContinuousBackups` until the window has WIDENED — its oldest point is
/// strictly older than its newest — so the oldest bound has reached the recovery period in
/// force and stays put while a row runs.
///
/// The wait is for that condition, bounded only so a window that never widens fails as the
/// hang it is rather than looping for ever.
async fn widened_window(router: &MultiServiceRouter, case: &str, name: &str) {
    let started = std::time::Instant::now();
    loop {
        let window = restorable_window(router, name).await;
        if window.0 < window.1 {
            return;
        }
        assert!(
            started.elapsed() < WINDOW_DEADLINE,
            "case {case:?}: the restorable window of {name:?} never widened, last window was \
             {window:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The epoch second an instant quoted in the refusal names, or `None` for text that is no
/// date-time at all — the INVERSE of the rendering under test, so the assertion ties the
/// quoted text back to the number the window and the request carry.
fn quoted_second(rendered: &str) -> Option<i64> {
    DateTime::from_str(rendered, Format::DateTime)
        .ok()
        .map(|instant| instant.secs())
}

/// The epoch second a wire timestamp falls in, which is the resolution the refusal states
/// an instant at.
fn whole_second(instant: f64) -> i64 {
    instant.floor() as i64
}

/// The four values the refusal quotes, in the order it states them: the requested instant,
/// the window's oldest and newest points, and the table.
///
/// Read by splitting on the quote the sentence delimits each value with, so the assertion
/// that follows can rebuild the whole sentence and compare it to the one answered — which
/// is what pins the fixed text around the values.
fn quoted(stated: &str) -> Vec<String> {
    stated
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(ToString::to_string)
        .collect()
}

/// One instant outside one table's window, and how the row places it there.
struct Row {
    case: &'static str,
    /// The table the restore names as its source, created with the recovery period its
    /// name stands for.
    table: &'static str,
    /// How far outside the window the instant sits, in seconds.
    beyond: f64,
    /// Whether it sits PAST the newest restorable point rather than before the oldest.
    after_latest: bool,
    /// An instant named in absolute terms instead of relative to a bound.
    absolute: Option<f64>,
    /// Whether the request addresses its source table by ARN rather than by name.
    by_arn: bool,
}

#[tokio::test]
async fn an_instant_outside_the_window_is_refused_naming_the_window_and_the_table() {
    let rows: [Row; 7] = [
        Row {
            case: "one second before the oldest restorable point",
            table: "WindowNamed",
            beyond: 1.0,
            after_latest: false,
            absolute: None,
            by_arn: false,
        },
        Row {
            case: "forty days before the oldest restorable point",
            table: "WindowNamed",
            beyond: 40.0 * DAY,
            after_latest: false,
            absolute: None,
            by_arn: false,
        },
        Row {
            case: "an hour past the newest restorable point",
            table: "WindowNamed",
            beyond: 3_600.0,
            after_latest: true,
            absolute: None,
            by_arn: false,
        },
        Row {
            case: "past the newest restorable point by a fraction of a second over an hour",
            table: "WindowNamed",
            beyond: 3_600.5,
            after_latest: true,
            absolute: None,
            by_arn: false,
        },
        Row {
            case: "before the epoch, which no window can cover",
            table: "WindowNamed",
            beyond: 0.0,
            after_latest: false,
            absolute: Some(-1_000.0),
            by_arn: false,
        },
        Row {
            case: "before a window given the shortest recovery period",
            table: "WindowShortest",
            beyond: 2.0 * DAY,
            after_latest: false,
            absolute: None,
            by_arn: false,
        },
        Row {
            case: "a source table addressed by ARN",
            table: "WindowByArn",
            beyond: 3_600.0,
            after_latest: true,
            absolute: None,
            by_arn: true,
        },
    ];

    let router = emulator();
    for (name, period) in [
        ("WindowNamed", None),
        ("WindowShortest", Some(1)),
        ("WindowByArn", Some(7)),
    ] {
        table_with_recovery(&router, name, period).await;
    }
    for row in &rows {
        widened_window(&router, row.case, row.table).await;
    }

    for (index, row) in rows.iter().enumerate() {
        // The window on either side of the refused call: its oldest point only ever moves
        // back and its newest only ever forward, so these two readings BRACKET whatever
        // window the call itself was served against.
        let before = restorable_window(&router, row.table).await;
        let requested = row.absolute.unwrap_or(if row.after_latest {
            before.1 + row.beyond
        } else {
            before.0 - row.beyond
        });
        let mut request = json!({
            "TargetTableName": format!("WindowRestored{index}"),
            "RestoreDateTime": requested,
        });
        if row.by_arn {
            request["SourceTableArn"] = json!(state::table_arn(REGION, row.table));
        } else {
            request["SourceTableName"] = json!(row.table);
        }
        let (status, answered) = call(&router, "RestoreTableToPointInTime", request.clone()).await;
        let after = restorable_window(&router, row.table).await;

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "InvalidRestoreTimeException"),
            "case {:?} ({request}): expected the modelled refusal of a point outside the \
             window, got {status} with {answered}",
            row.case
        );
        let stated = message(&answered);
        let values = quoted(&stated);
        assert_eq!(
            values.len(),
            4,
            "case {:?}: the refusal quotes the instant, both bounds and the table, got \
             {stated:?}",
            row.case
        );
        let (instant, earliest, latest, table) =
            (&values[0], &values[1], &values[2], &values[3]);
        // The fixed text, rebuilt around the values the answer itself quoted: the sentence
        // may differ from this only in those four values.
        assert_eq!(
            stated,
            format!(
                "RestoreDateTime '{instant}' must be within EarliestRestorableDateTime \
                 '{earliest}' and LatestRestorableDateTime '{latest}' for table '{table}'"
            ),
            "case {:?}: expected the refusal's fixed sentence around the values it quotes, \
             got {stated:?}",
            row.case
        );
        assert_eq!(
            table, row.table,
            "case {:?}: the refusal names the source table, got {stated:?}",
            row.case
        );
        for (member, rendered) in [
            ("RestoreDateTime", instant),
            ("EarliestRestorableDateTime", earliest),
            ("LatestRestorableDateTime", latest),
        ] {
            assert!(
                quoted_second(rendered).is_some()
                    && rendered.ends_with('Z')
                    && !rendered.contains('.'),
                "case {:?}: {member} is stated as a whole-second date-time, got {rendered:?}",
                row.case
            );
        }
        assert_eq!(
            quoted_second(instant),
            Some(whole_second(requested)),
            "case {:?}: the refusal names the instant the request asked for ({requested}), \
             got {instant:?}",
            row.case
        );
        // Each bound lies between the two readings around the call, in the direction that
        // bound moves: so the sentence names the window the call was served against.
        let quoted_earliest = quoted_second(earliest).unwrap_or_default();
        assert!(
            (whole_second(after.0)..=whole_second(before.0)).contains(&quoted_earliest),
            "case {:?}: the oldest point named ({quoted_earliest}) is not the one in force \
             between {} and {}, message was {stated:?}",
            row.case,
            after.0,
            before.0
        );
        let quoted_latest = quoted_second(latest).unwrap_or_default();
        assert!(
            (whole_second(before.1)..=whole_second(after.1)).contains(&quoted_latest),
            "case {:?}: the newest point named ({quoted_latest}) is not the one in force \
             between {} and {}, message was {stated:?}",
            row.case,
            before.1,
            after.1
        );
        // And the instant really is on the side of the window the row placed it on, so the
        // refusal is about the bound it names.
        if row.after_latest {
            assert!(
                whole_second(requested) > quoted_latest,
                "case {:?}: expected an instant past the newest point named, got \
                 {requested} against {stated:?}",
                row.case
            );
        } else {
            assert!(
                whole_second(requested) < quoted_earliest,
                "case {:?}: expected an instant before the oldest point named, got \
                 {requested} against {stated:?}",
                row.case
            );
        }
    }

    // The control: an instant INSIDE the same window is SERVED, so each refusal above is
    // about where the instant fell rather than about the request's shape.
    let window = restorable_window(&router, "WindowNamed").await;
    let served = ok(
        &router,
        "RestoreTableToPointInTime",
        json!({
            "SourceTableName": "WindowNamed",
            "TargetTableName": "WindowRestoredInside",
            "RestoreDateTime": window.0,
        }),
    )
    .await;
    assert_eq!(
        served["TableDescription"]["TableName"],
        json!("WindowRestoredInside"),
        "case an instant inside the window: expected the restored table, got {served}"
    );
}
