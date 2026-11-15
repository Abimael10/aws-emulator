// SPDX-License-Identifier: CC0-1.0

//! The restorable window CATCHES UP with the clock, so a point in time a caller observed
//! becomes restorable.
//!
//! The rule pinned here: while point-in-time recovery is on, the newest point
//! `DescribeContinuousBackups` reports trails the present and ADVANCES with the clock, so
//! an instant a caller took off the running emulator falls inside the window after one
//! transient span — and `RestoreTableToPointInTime` then accepts that instant, drives the
//! table it creates to `ACTIVE`, and fills it with exactly the writes made at or before
//! it. A window whose newest point never reached such an instant would leave a backup
//! runbook polling for ever instead of being served or refused.
//!
//! The rows differ along what the rule generalises over: how many writes land before the
//! point and how many after it, whether the point is taken before or after recovery has
//! writes to remember, and which of the two selectors the restore names the point with —
//! the instant itself and the newest restorable one.
//!
//! Every call is driven over the wire, and every wait is for the CONDITION it waits on,
//! polled until a generous deadline rather than timed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    describe_when_active, emulator, ok, restorable_window, ACTIVE_DEADLINE, POLL_INTERVAL,
};

/// How long a poll for the window to reach an instant may take before this suite calls it
/// a hang. Generous slack over the emulator's own transient span: a loaded machine is not
/// a defect, and the point of the bound is that the wait ENDS rather than how fast it
/// does.
const WINDOW_DEADLINE: Duration = Duration::from_secs(60);

/// How far ahead of the writes that precede it the restore point is taken, so the writes
/// on either side of it are unambiguously on their side of it whatever resolution the
/// host's clock has.
const POINT_MARGIN: Duration = Duration::from_millis(50);

/// One active table keyed on `pk`.
async fn active_table(router: &MultiServiceRouter, name: &str) {
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
    describe_when_active(router, "the point-in-time source table", name).await;
}

/// Turn point-in-time recovery on and answer the status it reports.
async fn enable_recovery(router: &MultiServiceRouter, name: &str) -> Value {
    ok(
        router,
        "UpdateContinuousBackups",
        json!({
            "TableName": name,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
        }),
    )
    .await["ContinuousBackupsDescription"]["PointInTimeRecoveryDescription"]
        ["PointInTimeRecoveryStatus"]
        .clone()
}

/// Write one item under `pk`.
async fn put(router: &MultiServiceRouter, table: &str, pk: &str, value: &str) {
    ok(
        router,
        "PutItem",
        json!({"TableName": table, "Item": {"pk": {"S": pk}, "val": {"S": value}}}),
    )
    .await;
}

/// The item `pk` names in `table` as a consistent read sees it, `Value::Null` for a key
/// that holds none.
async fn item(router: &MultiServiceRouter, table: &str, pk: &str) -> Value {
    ok(
        router,
        "GetItem",
        json!({"TableName": table, "Key": {"pk": {"S": pk}}, "ConsistentRead": true}),
    )
    .await
    .get("Item")
    .cloned()
    .unwrap_or(Value::Null)
}

/// Wait until this process's clock has passed `instant`, so a write made afterwards lands
/// strictly after it. The wait ends as soon as that holds; the deadline is only there so a
/// clock that never advances fails instead of hanging.
async fn clock_past(instant: SystemTime) {
    let started = std::time::Instant::now();
    while SystemTime::now() <= instant && started.elapsed() < ACTIVE_DEADLINE {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        SystemTime::now() > instant,
        "case waiting for the clock to pass a restore point: the clock did not advance past it"
    );
}

/// Poll `DescribeContinuousBackups` until the newest restorable point has reached
/// `point`, and answer the window it then reports.
///
/// This is the rule under test: the window's newest point advances with the clock, so the
/// condition becomes true on its own. The deadline makes a window that never advances fail
/// as the hang it is rather than looping for ever.
async fn window_reaching(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    point: f64,
) -> (f64, f64) {
    let started = std::time::Instant::now();
    loop {
        let window = restorable_window(router, name).await;
        if window.1 >= point {
            return window;
        }
        assert!(
            started.elapsed() < WINDOW_DEADLINE,
            "case {case:?}: the newest restorable point never reached {point}, last window was \
             {window:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many writes one row makes on each side of the restore point.
struct Row {
    case: &'static str,
    before: usize,
    after: usize,
    /// Whether the restore names the point as the newest restorable one instead of the
    /// instant itself.
    by_latest: bool,
}

#[tokio::test]
async fn the_window_reaches_an_observed_instant_and_a_restore_to_it_is_served() {
    let rows: [Row; 4] = [
        Row {
            case: "one write before the point and one after it",
            before: 1,
            after: 1,
            by_latest: false,
        },
        Row {
            case: "several writes before the point and several after it",
            before: 3,
            after: 2,
            by_latest: false,
        },
        Row {
            case: "every write before the point",
            before: 2,
            after: 0,
            by_latest: false,
        },
        Row {
            case: "the newest restorable point as the selector",
            before: 2,
            after: 1,
            by_latest: true,
        },
    ];

    for (position, row) in rows.iter().enumerate() {
        let router = emulator();
        let source = format!("PitrWindow{position}");
        let target = format!("PitrWindowRestored{position}");
        active_table(&router, &source).await;

        let status = enable_recovery(&router, &source).await;
        assert_eq!(
            status,
            json!("ENABLED"),
            "case {:?}: recovery reports itself on, got {status}",
            row.case
        );

        for index in 0..row.before {
            put(&router, &source, &format!("before-{index}"), "before-restore-time").await;
        }
        // The instant a client takes off its own clock, a margin past the writes that
        // precede it — the server's `Date` header is the same instant.
        let point_instant = SystemTime::now() + POINT_MARGIN;
        let point = point_instant
            .duration_since(UNIX_EPOCH)
            .expect("the point is after the epoch")
            .as_secs_f64();
        clock_past(point_instant).await;
        for index in 0..row.after {
            put(&router, &source, &format!("after-{index}"), "after-restore-time").await;
        }

        // The rule: the window's newest point advances until it has reached that instant.
        let (earliest, latest) = window_reaching(&router, row.case, &source, point).await;
        assert!(
            earliest <= point && point <= latest,
            "case {:?}: the observed instant {point} is inside the window ({earliest}, \
             {latest})",
            row.case
        );

        let mut request = json!({"SourceTableName": &source, "TargetTableName": &target});
        if row.by_latest {
            request["UseLatestRestorableTime"] = json!(true);
        } else {
            request["RestoreDateTime"] = json!(point);
        }
        let restored = ok(&router, "RestoreTableToPointInTime", request).await
            ["TableDescription"]
            .clone();
        assert_eq!(
            restored["TableName"],
            json!(&target),
            "case {:?}: the restore reports the table it created, got {restored}",
            row.case
        );

        // It reaches ACTIVE on its own, which is what a client's waiter polls for.
        let active = describe_when_active(&router, row.case, &target).await;
        assert_eq!(
            active["TableStatus"],
            json!("ACTIVE"),
            "case {:?}: the restored table settles ACTIVE, got {active}",
            row.case
        );

        // And it holds exactly the writes made at or before the point: a restore to the
        // newest restorable point is at or after the instant above, so the writes before
        // it are there either way, while the writes after it are only there for a point
        // the window had already moved past them.
        for index in 0..row.before {
            let key = format!("before-{index}");
            let read = item(&router, &target, &key).await;
            assert_eq!(
                read,
                json!({"pk": {"S": key}, "val": {"S": "before-restore-time"}}),
                "case {:?}: {key} was written before the point, so the restored table holds \
                 it, got {read}",
                row.case
            );
        }
        if !row.by_latest {
            for index in 0..row.after {
                let key = format!("after-{index}");
                let read = item(&router, &target, &key).await;
                assert_eq!(
                    read,
                    Value::Null,
                    "case {:?}: {key} was written after the point, so the restored table \
                     holds nothing for it, got {read}",
                    row.case
                );
            }
        }

        // The SOURCE table is untouched: every write it took is still there.
        for index in 0..row.after {
            let key = format!("after-{index}");
            let read = item(&router, &source, &key).await;
            assert_eq!(
                read,
                json!({"pk": {"S": key}, "val": {"S": "after-restore-time"}}),
                "case {:?}: the source keeps what was written after the point, got {read}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn an_instant_the_window_has_not_reached_is_refused_rather_than_served() {
    let router = emulator();
    let source = "PitrAhead";
    active_table(&router, source).await;
    enable_recovery(&router, source).await;
    let (_, latest) = restorable_window(&router, source).await;

    // Rows differ in how far past the newest restorable point the instant sits: just past
    // the end of this suite's own patience for it, an hour on, and a year on. None of them
    // can fall inside the window while this test runs, so each one states the modelled
    // refusal of a point in time nothing has reached yet.
    let instants: [(&str, f64); 3] = [
        (
            "past the newest point by more than this suite can take",
            latest + WINDOW_DEADLINE.as_secs_f64() * 4.0,
        ),
        ("an hour past the newest point", latest + 3_600.0),
        ("a year past the newest point", latest + 31_536_000.0),
    ];

    for (case, instant) in instants {
        let (status, answered) = crate::emulator::call(
            &router,
            "RestoreTableToPointInTime",
            json!({"SourceTableName": source, "TargetTableName": format!("Ahead{instant}"),
                   "RestoreDateTime": instant}),
        )
        .await;
        assert_eq!(
            (status, crate::emulator::error_code(&answered).as_str()),
            (400, "InvalidRestoreTimeException"),
            "case {case:?}: expected the modelled refusal of a point the window has not \
             reached, got {status} with {answered}"
        );
    }
}
