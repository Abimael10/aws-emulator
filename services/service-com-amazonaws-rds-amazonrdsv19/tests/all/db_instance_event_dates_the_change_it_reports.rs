// SPDX-License-Identifier: CC0-1.0

//! WHEN an Amazon RDS lifecycle event is dated and published: at the instant the change it
//! reports actually happened.
//!
//! The sibling suites assert the envelope itself ([`crate::db_instance_bus_events`]), which
//! rules it reaches ([`crate::db_instance_event_pattern_precision`]) and that a refused call
//! publishes none ([`crate::db_instance_refused_change_publishes_no_event`]). What this one
//! asserts is the INSTANT: a change this service performs AFTER answering the call — the
//! restart a reboot's momentary outage ends with, the stop that leaves the instance
//! `stopped`, the start that brings it back into service — happens when the clock makes the
//! transition due, so the event naming that change carries that instant, which is at or
//! after the moment the call's own response was served, and reaches the bus THEN.
//!
//! Both halves matter to a client. A consumer of the bus is not a caller of this service:
//! nothing it does would make the transition settle, so an event that waited for the next
//! control-plane call would never arrive at all. And a client that notes when its call was
//! answered and then waits for the event of the change it ordered — the shape of every
//! "reboot the instance, react to RDS-EVENT-0006" automation — must find an event dated at
//! or after that moment, or it discards the event it was waiting for as one from before.
//!
//! The table spans what the rule generalises over: the THREE changes this service answers
//! while they are still to come, each with its own published event identifier, its own
//! message and its own transient status, each matched by a rule of its own and delivered to
//! a queue of its own. Every arrival is read off a queue that really handed the message
//! over, with EventBridge and Amazon SQS mounted beside this service on ONE engine exactly
//! as the assembled router mounts them, and every wait is a poll on the ARRIVAL under a
//! generous deadline.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_bus_events::{
    collect, emulator, wait_for_status, DB_INSTANCE_EVENT, EVENT_SOURCE,
};
use crate::db_instance_event_pattern_precision::{create_available, rule_to_its_own_queue};
use crate::db_instances::{call, expect_ok, subtrees, text};

/// The DB instance every change below is made to.
const INSTANCE: &str = "emu-when-db";

/// One change this service answers BEFORE it has happened: the call that orders it, the
/// event identifier and message of the change itself, and the status the instance settles on
/// once it has happened.
struct Change {
    case: &'static str,
    /// The operation a client calls, each taking the instance's identifier alone.
    action: &'static str,
    /// `detail.EventID` of the change, and the rule pattern's own pin.
    ///
    /// Source: the per-source-type tables of "Amazon RDS event categories and event
    /// messages" (Amazon RDS User Guide) — the DB instance rows whose messages are the
    /// `message` beside each id here.
    event_id: &'static str,
    /// `detail.Message`, from the same row.
    message: &'static str,
    /// The name of this row's rule and of the queue it targets.
    slug: &'static str,
    /// The status the instance reports once the change has happened.
    settled_status: &'static str,
}

/// The three changes, in an order each of which the next one is accepted from: a reboot and
/// a stop are accepted from `available`, a start from `stopped`.
const CHANGES: &[Change] = &[
    Change {
        case: "the restart a reboot's momentary outage ends with",
        action: "RebootDBInstance",
        event_id: "RDS-EVENT-0006",
        message: "DB instance restarted",
        slug: "emu-when-restarted",
        settled_status: "available",
    },
    Change {
        case: "the stop that leaves the instance stopped",
        action: "StopDBInstance",
        event_id: "RDS-EVENT-0087",
        message: "DB instance stopped",
        slug: "emu-when-stopped",
        settled_status: "stopped",
    },
    Change {
        case: "the start that brings it back into service",
        action: "StartDBInstance",
        event_id: "RDS-EVENT-0088",
        message: "DB instance started",
        slug: "emu-when-started",
        settled_status: "available",
    },
];

/// The pattern that matches this row's change for the instance at `arn` exactly.
fn pattern_of(row: &Change, arn: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [DB_INSTANCE_EVENT],
        "detail": { "EventID": [row.event_id], "SourceArn": [arn] },
    })
}

/// `at` as seconds since the epoch, the scale an event's `Date` is compared on.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

/// The instant `text` names, read with the same reader an SDK reads a date-time member with.
fn instant_of(case: &str, text: &str) -> f64 {
    aws_smithy_types::DateTime::from_str(text, aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {text:?} must be an RFC 3339 instant, got {error}")
        })
        .as_secs_f64()
}

/// The `Date` the control plane reports for the one event of `event_id`'s message on
/// `INSTANCE`, or a panic naming what the log held instead.
async fn reported_date(router: &MultiServiceRouter, row: &Change) -> String {
    let body = expect_ok(
        row.case,
        call(
            router,
            "DescribeEvents",
            &[
                ("SourceType", "db-instance"),
                ("SourceIdentifier", INSTANCE),
            ],
        )
        .await,
    );
    let rows: Vec<String> = subtrees(&body, "Event")
        .into_iter()
        .filter(|event| text(event, "Message") == row.message)
        .map(|event| text(&event, "Date"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "case {:?}: the log reports exactly the one change that happened, got {body}",
        row.case
    );
    rows.into_iter().next().unwrap_or_default()
}

/// RULE: each of these changes is dated — and published — at the instant it happened, which
/// is at or after the moment the call that ordered it was answered, and it reaches the bus
/// with no further call of this service.
#[tokio::test]
async fn each_change_is_published_when_it_happens_and_not_when_it_was_ordered() {
    let router = emulator();
    let arn = create_available(&router, INSTANCE).await;
    let mut queues = Vec::new();
    for row in CHANGES {
        queues.push(rule_to_its_own_queue(&router, row.slug, &pattern_of(row, &arn)).await);
    }

    for (row, queue_url) in CHANGES.iter().zip(&queues) {
        expect_ok(
            row.case,
            call(&router, row.action, &[("DBInstanceIdentifier", INSTANCE)]).await,
        );
        // The moment the caller HELD the answer: the change the call ordered is still to
        // come, so the event that reports it cannot be dated before this.
        let served_at = epoch_seconds(SystemTime::now());

        // Nothing but the QUEUE is read from here to the assertion — no call of this
        // service — so an event that waited for the next control-plane call would never
        // arrive and this poll would run out.
        let delivered = collect(&router, queue_url, 1).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {:?}: the change's own rule receives it exactly once, with no further call \
             of this service, got {delivered:?}",
            row.case
        );
        let event: Value = serde_json::from_str(&delivered[0]).unwrap_or_else(|error| {
            panic!("case {:?}: {} must be JSON: {error}", row.case, delivered[0])
        });
        for (pointer, expected) in [
            ("/detail/EventID", json!(row.event_id)),
            ("/detail/Message", json!(row.message)),
            ("/detail/SourceArn", json!(&arn)),
        ] {
            let actual = event.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {pointer} to be {expected}, got {actual} in {event}",
                row.case
            );
        }
        let dated = event["detail"]["Date"].as_str().unwrap_or_default();
        let happened_at = instant_of(row.case, dated);
        assert!(
            happened_at >= served_at,
            "case {:?}: expected the change to be dated at or after the moment its call was \
             answered ({served_at}), got {dated:?} ({happened_at})",
            row.case
        );

        // The instance has settled on the status the change leaves it in — the change really
        // happened — and the control plane reports the SAME record, down to its date.
        wait_for_status(&router, INSTANCE, row.settled_status).await;
        let reported = reported_date(&router, row).await;
        assert_eq!(
            instant_of(row.case, &reported),
            happened_at,
            "case {:?}: expected the reported date to be the published one ({dated:?}), got \
             {reported:?}",
            row.case
        );
    }
}
