// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH clock decides that a parked `ModifyDBInstance` change has
//! landed: the maintenance window the DB instance itself keeps.
//!
//! The rule under test: `ApplyImmediately` disabled means "changes to the DB instance are
//! applied during the next maintenance window", so what an instance reports as pending is a
//! function of the window THAT INSTANCE keeps and of the instant it is read at — the change
//! stays in `PendingModifiedValues` for as long as the clock is outside an occurrence of that
//! window, and is in force on the instance itself once the clock is inside one. Nothing else
//! about the host decides it: an instance whose window is closed keeps the change parked
//! however often it is read, and one whose window is open has it applied on the first report
//! that reads the instance.
//!
//! The table below spans what the rule generalises over — the forms a window takes around the
//! instant the service reads it: one whose next occurrence is days away, one whose last
//! occurrence has already ended, one that has started and not yet ended, and one that wraps
//! the week's end so that only an hour ahead of now lies outside it. Each window is built
//! from the INSTANT the case runs at, so the row is the same case whenever the suite runs,
//! and each row's premise — whether that window is open — is asserted through this service's
//! own reader of a window before the behaviour is driven, so a fixture that would make a case
//! pass or fail by the hour of the day fails on its premise instead.
//!
//! Every call goes over the wire through `db_instances::emulator`, and every wait is the poll
//! loop this test target shares: nothing sleeps and no assertion is on how fast the host
//! runs.
//!
//! `pending_modified_values.rs` is the suite next door: it pins WHICH settings the service
//! defers to the window and drives one instance whose window is MOVED under a staged change.
//! This one holds the instance's window still and varies the shape of the window itself, so
//! the two stay on their own halves of the rule.

use std::time::SystemTime;

use service_com_amazonaws_rds_amazonrdsv19::state::window_is_open;

use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees};
use crate::pending_modified_values::{
    an_instance, pending, poll_instance, window_days_ahead, window_point, window_that_started,
};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The class a parked modification asks for. What the instance is RUNNING is read back off
/// the created instance rather than restated here, so the fixture's own class and this suite
/// cannot drift apart.
const PARKED_CLASS: &str = "db.t3.small";

/// One row of the window table: a window around the instant the case runs at, and whether an
/// occurrence of it is OPEN then.
struct WindowCase {
    case: &'static str,
    identifier: &'static str,
    /// The window the instance is created with, built from the instant the row runs at.
    window: fn() -> String,
    /// Whether the clock is inside an occurrence of that window, which is what decides
    /// whether a change parked against it is still pending.
    open: bool,
}

/// A window whose last occurrence has ENDED: from three hours before now to one hour before
/// it, in the `ddd:hh24:mi-ddd:hh24:mi` form `PreferredMaintenanceWindow` documents.
///
/// Built from the INSTANT, like the windows the sibling suite builds, so the span is the same
/// distance behind the clock whatever day or hour the case runs at.
fn window_that_ended() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}-{}",
        window_point(now - chrono::Duration::hours(3)),
        window_point(now - chrono::Duration::hours(1))
    )
}

/// A window that leaves only the hour AHEAD of now outside it: it begins two hours from now
/// and ends one hour from now, a span whose end is earlier in the week than its start, which
/// wraps the week's end and is therefore open on either side of that one hour.
///
/// The other form of an OPEN window, beside the plain span that has started and not yet
/// ended, and the one that proves the wrap is read as a schedule rather than as an empty span.
fn window_with_only_the_hour_ahead_outside() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}-{}",
        window_point(now + chrono::Duration::hours(2)),
        window_point(now + chrono::Duration::hours(1))
    )
}

/// Each form a window takes around the instant a case reads the instance at.
const WINDOWS: &[WindowCase] = &[
    WindowCase {
        case: "a window whose next occurrence is days away",
        identifier: "emu-window-days-away",
        window: window_days_ahead,
        open: false,
    },
    WindowCase {
        case: "a window whose last occurrence ended an hour ago",
        identifier: "emu-window-ended",
        window: window_that_ended,
        open: false,
    },
    WindowCase {
        case: "a window that has started and not yet ended",
        identifier: "emu-window-started",
        window: window_that_started,
        open: true,
    },
    WindowCase {
        case: "a window wrapping the week's end, with only the hour ahead outside it",
        identifier: "emu-window-wrapping",
        window: window_with_only_the_hour_ahead_outside,
        open: true,
    },
];

#[tokio::test]
async fn a_parked_modification_waits_for_the_window_the_instance_keeps() {
    let router = emulator();
    for row in WINDOWS {
        let window = (row.window)();
        // The row's premise, judged by the service's own reader of a window: whether the
        // clock is inside an occurrence of this one. A fixture that is not what the row
        // claims fails HERE rather than on the behaviour the row drives.
        let actually_open = window_is_open(&window, SystemTime::now());
        assert_eq!(
            actually_open, row.open,
            "case {:?}: the window {window:?} must be open = {}, got {actually_open}",
            row.case, row.open
        );
        an_instance(
            &router,
            row.identifier,
            &[("PreferredMaintenanceWindow", window.clone())],
        )
        .await;
        // Read back what the instance keeps, so the window under test is the stored one.
        let created = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => row.identifier],
            )
            .await,
        );
        let created = subtrees(&created, "DBInstance")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {:?}: the created instance is reported", row.case));
        let stored = optional_text(&created, "PreferredMaintenanceWindow");
        assert_eq!(
            stored.as_deref(),
            Some(window.as_str()),
            "case {:?}: expected the instance to keep {window:?}, got {stored:?}",
            row.case
        );
        // The class the instance is RUNNING, which a parked change must leave alone: a
        // `DBInstanceClass` change is a setting the modelled `PendingModifiedValues` carries a
        // member for, which is what makes it one the service parks for the window.
        let running_class = optional_text(&created, "DBInstanceClass").unwrap_or_else(|| {
            panic!(
                "case {:?}: the created instance reports the class it runs, got {created}",
                row.case
            )
        });
        assert_ne!(
            running_class, PARKED_CLASS,
            "case {:?}: the parked class must differ from the one the instance runs, got \
             {running_class:?}",
            row.case
        );

        // A modification the caller did NOT ask to apply immediately.
        expect_ok(
            row.case,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => row.identifier,
                    "ApplyImmediately" => false,
                    "DBInstanceClass" => PARKED_CLASS,
                ],
            )
            .await,
        );

        if row.open {
            // The window is open, so the change is owed: the instance is READ until it
            // reports the staged class rather than waited on for a span of wall-clock.
            let applied = poll_instance(
                &router,
                row.case,
                "the change parked for an open window being applied",
                row.identifier,
                |instance| {
                    optional_text(instance, "DBInstanceClass").as_deref() == Some(PARKED_CLASS)
                },
            )
            .await;
            let live = optional_text(&applied, "DBInstanceClass");
            assert_eq!(
                live.as_deref(),
                Some(PARKED_CLASS),
                "case {:?}: expected the class applied inside the window, got {live:?}",
                row.case
            );
            assert!(
                pending(&applied).is_none(),
                "case {:?}: nothing stays pending once the window applied it, got {applied}",
                row.case
            );
        } else {
            // The window is closed, so the change waits for it: the instance keeps the class
            // it is running under and reports the staged one as pending, however often it is
            // read. A parked modification leaves the status alone, so the read below holds at
            // once and nothing is waited out.
            let parked = poll_instance(
                &router,
                row.case,
                &format!("{} reporting itself available", row.identifier),
                row.identifier,
                |instance| optional_text(instance, "DBInstanceStatus").as_deref() == Some("available"),
            )
            .await;
            let live = optional_text(&parked, "DBInstanceClass");
            assert_eq!(
                live.as_deref(),
                Some(running_class.as_str()),
                "case {:?}: expected the live class to stay {running_class:?} outside the \
                 window, got {live:?}",
                row.case
            );
            let staged = pending(&parked).unwrap_or_else(|| {
                panic!(
                    "case {:?}: a change parked for a window that is not open must be reported \
                     as pending, got {parked}",
                    row.case
                )
            });
            let waiting = optional_text(&staged, "DBInstanceClass");
            assert_eq!(
                waiting.as_deref(),
                Some(PARKED_CLASS),
                "case {:?}: expected {PARKED_CLASS:?} reported as pending, got {waiting:?}",
                row.case
            );
        }
    }
}
