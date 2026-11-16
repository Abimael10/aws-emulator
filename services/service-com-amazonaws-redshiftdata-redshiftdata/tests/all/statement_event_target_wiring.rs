// SPDX-License-Identifier: CC0-1.0

//! WHERE a statement's status change lands: the queues a matching rule's targets
//! really name, and what a purge of one of those queues does and does not remove.
//!
//! The RULE asserted here, one level above the suite beside this one (which pins
//! that a submission carrying `WithEvent` — "a value that indicates whether to send
//! an event to the Amazon EventBridge event bus after the SQL statement runs",
//! `smithy_query(query="doc",shape="ExecuteStatementInput",member="WithEvent")` —
//! is announced at all): the announcement is delivered to EXACTLY the queues the
//! matching rule's targets name. A target whose ARN names a queue the account does
//! not hold delivers nowhere — it is accepted by `PutTargets` all the same, which
//! validates the ARN's shape and not the existence of what it names — and it does
//! not stop the deliveries the rule's other targets get. And a queue's own
//! `PurgeQueue` "deletes available messages in a queue (including in-flight
//! messages)"
//! (`smithy_query(query="doc",shape="PurgeQueueRequest",model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
//! — those it already held, and only those, so the next statement's announcement is
//! delivered to it normally.
//!
//! Why this is the SAME rule twice: a client that drives itself off these events
//! cannot tell "nothing was published" from "it was published somewhere else" or
//! from "it was published and then swept away". Both halves below are the ways an
//! announcement can go missing on the delivery side of the bus, so both are pinned
//! on the state the two services really hold rather than on the publisher's own
//! queue.
//!
//! The rows span what the rule generalises over: how many targets a rule carries
//! and WHERE among them the mis-wired one sits (first, last, between two that
//! resolve), and — for the purge — whether the message the purge must remove was
//! still waiting or had already been handed to a receiver.
//!
//! EventBridge and Amazon SQS are mounted BESIDE this service on the SAME engine,
//! so every delivery here travels the real in-process callback through the real bus
//! to a real queue.
//!
//! One note on the purge half: AWS states that the deletion a `PurgeQueue` starts
//! "takes up to 60 seconds", so a client is told to wait that long before trusting
//! the queue to be empty. This emulator performs it in the call itself, which is
//! the stronger answer and the one asserted below — the immediate emptiness here is
//! this emulator's own, not the guarantee AWS publishes to its callers.

use serde_json::{json, Value};

use crate::emulator::{Warehouses, REGION};
use crate::statement_status_change_events::{
    collect, drain, given_queue, given_rule, statement_event_pattern, wait_for_terminal,
    DETAIL_TYPE, EVENT_SOURCE, POLL_BUDGET, POLL_INTERVAL,
};

/// The warehouse every statement in this suite runs on, and the database it is
/// created with.
const CLUSTER: &str = "target-wiring-cluster";
const DATABASE: &str = "devdb";
const DB_USER: &str = "adminuser";

/// A statement the engine runs to completion.
const QUICK_SQL: &str = "SELECT 1";

/// The suffix that turns a queue's ARN into one naming a queue no account holds:
/// a well-formed Amazon SQS ARN (`arn:<partition>:sqs:<region>:<account>:<name>`)
/// whose name is the mis-typed one. Amazon SQS queue names take "alphanumeric
/// characters, hyphens (-), and underscores (_)" (`CreateQueue$QueueName`), so
/// this is a name a queue COULD have had and none has.
const MISTYPED_SUFFIX: &str = "-typo";

/// Whether a rule's target slot names a queue that exists, or one that does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// The target names the queue created for this slot.
    Wired,
    /// The target names a queue nothing created, so the queue created for this
    /// slot is the one the caller MEANT and never gets anything.
    Mistyped,
}

/// One row of the wiring table: the target slots the rule carries, in order.
struct Wiring {
    case: &'static str,
    slots: &'static [Slot],
}

/// The table. Every row carries at least one target that resolves, because the
/// claim about the mis-wired target is only meaningful beside a delivery that did
/// land: a row where NOTHING arrives cannot tell a mis-wired target from an
/// announcement that was never published.
const WIRINGS: &[Wiring] = &[
    Wiring {
        case: "the mis-typed target is the rule's only one beside a wired one, and comes first",
        slots: &[Slot::Mistyped, Slot::Wired],
    },
    Wiring {
        case: "the mis-typed target comes last",
        slots: &[Slot::Wired, Slot::Mistyped],
    },
    Wiring {
        case: "the mis-typed target sits between two that resolve",
        slots: &[Slot::Wired, Slot::Mistyped, Slot::Wired],
    },
];

/// Whether the announcement the purge must remove was still waiting, or had
/// already been handed to a receiver and so was in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stale {
    Waiting,
    InFlight,
}

/// Submit `sql` on the cluster with event emission asked for, WITHOUT waiting for
/// it inside the submit — the path a client driven by the event takes — and answer
/// the `Id` the submission was answered with.
async fn submit_announced(emulator: &Warehouses, case: &str, sql: &str, name: &str) -> String {
    let answered = emulator
        .data_ok(
            "ExecuteStatement",
            &json!({
                "ClusterIdentifier": CLUSTER,
                "Database": DATABASE,
                "DbUser": DB_USER,
                "Sql": sql,
                "WithEvent": true,
                "StatementName": name,
            }),
        )
        .await;
    answered["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Id in {answered}"))
        .to_string()
}

/// An ARN of the same shape as `arn` naming a queue nothing created.
fn mistyped(arn: &str) -> String {
    format!("{arn}{MISTYPED_SUFFIX}")
}

/// How many messages the queue at `url` holds, and how many of those are in
/// flight: `ApproximateNumberOfMessages` and
/// `ApproximateNumberOfMessagesNotVisible`.
async fn depth(emulator: &Warehouses, url: &str) -> (i64, i64) {
    let attributes = emulator
        .sqs_ok(
            "GetQueueAttributes",
            &json!({
                "QueueUrl": url,
                "AttributeNames": [
                    "ApproximateNumberOfMessages",
                    "ApproximateNumberOfMessagesNotVisible",
                ],
            }),
        )
        .await;
    let read = |name: &str| {
        attributes["Attributes"][name]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("a queue reports {name}: {attributes}"))
    };
    (
        read("ApproximateNumberOfMessages"),
        read("ApproximateNumberOfMessagesNotVisible"),
    )
}

/// Look again at the queue at `url` until it reports at least one message waiting,
/// WITHOUT receiving any of them, so the message the purge has to remove is still
/// visible when the purge arrives.
async fn wait_until_waiting(emulator: &Warehouses, case: &str, url: &str) {
    let started = std::time::Instant::now();
    loop {
        let (waiting, in_flight) = depth(emulator, url).await;
        if waiting > 0 {
            return;
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: the queue never reported a waiting announcement, got \
             {waiting} waiting and {in_flight} in flight"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The envelope of `delivered` that announces `statement_id`, which there must be
/// exactly one of.
fn announcement_of<'a>(case: &str, delivered: &'a [Value], statement_id: &str) -> &'a Value {
    let mine: Vec<&Value> = delivered
        .iter()
        .filter(|envelope| envelope.pointer("/detail/statementId") == Some(&json!(statement_id)))
        .collect();
    assert_eq!(
        mine.len(),
        1,
        "case {case:?}: expected exactly one announcement of {statement_id}, got {mine:?} among \
         {delivered:?}"
    );
    mine[0]
}

/// The envelope identity every announcement of this service carries, whichever
/// queue it was delivered to.
fn assert_identity(case: &str, envelope: &Value, statement_id: &str, status: &Value) {
    for (pointer, expected) in [
        ("/source", json!(EVENT_SOURCE)),
        ("/detail-type", json!(DETAIL_TYPE)),
        ("/region", json!(REGION)),
        ("/detail/statementId", json!(statement_id)),
        ("/detail/state", status.clone()),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
        );
    }
}

/// RULE: an announcement reaches exactly the queues the rule's targets name — a
/// target naming a queue nothing created delivers nowhere, is accepted by
/// `PutTargets` all the same, and takes none of the other targets' deliveries
/// with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mistyped_target_delivers_nowhere_and_leaves_the_other_targets_alone() {
    let emulator = Warehouses::new();
    emulator
        .create_cluster_with_database(CLUSTER, Some(DATABASE))
        .await;

    for (index, row) in WIRINGS.iter().enumerate() {
        // One queue per slot: the queue a wired target names, and the queue a
        // mis-typed target was MEANT to name.
        let mut queues: Vec<(Slot, String, String)> = Vec::new();
        for (slot, kind) in row.slots.iter().enumerate() {
            let (url, arn) = given_queue(&emulator, &format!("wiring-{index}-{slot}")).await;
            let target_arn = match kind {
                Slot::Wired => arn,
                Slot::Mistyped => mistyped(&arn),
            };
            queues.push((*kind, url, target_arn));
        }
        let targets: Vec<&str> = queues.iter().map(|(_, _, arn)| arn.as_str()).collect();
        // `PutTargets` accepting every entry is part of the rule: it validates the
        // ARN it is handed, not the existence of what that ARN names. `given_rule`
        // asserts the `FailedEntryCount` of 0 this requires.
        given_rule(
            &emulator,
            &format!("wiring-rule-{index}"),
            &statement_event_pattern(),
            &targets,
        )
        .await;

        let id = submit_announced(&emulator, row.case, QUICK_SQL, "wiring-statement").await;
        let record = wait_for_terminal(&emulator, row.case, &id).await;
        assert_eq!(
            record["Status"],
            json!("FINISHED"),
            "case {:?}: expected the statement to finish, got {record}",
            row.case
        );

        // Every target that resolves is delivered to, whatever the mis-typed one
        // beside it did.
        for (kind, url, target_arn) in &queues {
            if *kind != Slot::Wired {
                continue;
            }
            let delivered = collect(&emulator, url, 1).await;
            let envelope = announcement_of(row.case, &delivered, &id);
            assert_identity(row.case, envelope, &id, &record["Status"]);
            let resources = envelope.pointer("/resources").cloned().unwrap_or(Value::Null);
            assert!(
                resources
                    .as_array()
                    .is_some_and(|named| named.iter().any(|resource| resource
                        .as_str()
                        .is_some_and(|arn| arn.ends_with(&format!("cluster:{CLUSTER}"))))),
                "case {:?}: expected the announcement delivered to {target_arn} to name the \
                 cluster it ran on, got {resources} in {envelope}",
                row.case
            );
        }

        // And the queue a mis-typed target was meant to name got nothing at all:
        // read AFTER a delivery that did land, so the bus has already made every
        // attempt this announcement produces.
        for (kind, url, target_arn) in &queues {
            if *kind != Slot::Mistyped {
                continue;
            }
            let leaked = drain(&emulator, url).await;
            assert!(
                leaked.is_empty(),
                "case {:?}: the queue a mis-typed target ({target_arn}) was meant to name must \
                 receive nothing, got {leaked:?}",
                row.case
            );
            let (waiting, in_flight) = depth(&emulator, url).await;
            assert_eq!(
                (waiting, in_flight),
                (0, 0),
                "case {:?}: the queue a mis-typed target was meant to name must stay empty, got \
                 {waiting} waiting and {in_flight} in flight",
                row.case
            );
        }
    }
}

/// RULE: a purge of the target queue removes the announcements it already held —
/// whether they were waiting or already in flight — and nothing else, so the next
/// statement's announcement is delivered to that same queue normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_purge_removes_the_announcements_the_queue_held_and_no_later_one() {
    let emulator = Warehouses::new();
    emulator
        .create_cluster_with_database(CLUSTER, Some(DATABASE))
        .await;

    // One queue per row: a second `PurgeQueue` on the same queue inside the
    // interval the first one is still deleting over is a refused call, not a
    // second purge.
    for (index, stale) in [Stale::Waiting, Stale::InFlight].into_iter().enumerate() {
        let case: &str = match stale {
            Stale::Waiting => "a purge of an announcement still waiting",
            Stale::InFlight => "a purge of an announcement already in flight",
        };
        let (url, arn) = given_queue(&emulator, &format!("purged-events-{index}")).await;
        given_rule(
            &emulator,
            &format!("purged-events-rule-{index}"),
            &statement_event_pattern(),
            &[arn.as_str()],
        )
        .await;

        // The announcement the purge has to remove.
        let stale_id = submit_announced(&emulator, case, QUICK_SQL, "stale-statement").await;
        wait_for_terminal(&emulator, case, &stale_id).await;
        match stale {
            Stale::Waiting => wait_until_waiting(&emulator, case, &url).await,
            Stale::InFlight => {
                let delivered = collect(&emulator, &url, 1).await;
                announcement_of(case, &delivered, &stale_id);
                // Received and not deleted, so it is in flight when the purge
                // arrives — the message a purge "including in-flight messages"
                // is about.
                let (_waiting, in_flight) = depth(&emulator, &url).await;
                assert_eq!(
                    in_flight, 1,
                    "case {case:?}: a received announcement is in flight until it is deleted, \
                     got {in_flight}"
                );
            }
        }

        emulator.sqs_ok("PurgeQueue", &json!({"QueueUrl": &url})).await;

        let left = drain(&emulator, &url).await;
        assert!(
            left.is_empty(),
            "case {case:?}: a purged queue holds none of the announcements it held, got {left:?}"
        );
        let (waiting, in_flight) = depth(&emulator, &url).await;
        assert_eq!(
            (waiting, in_flight),
            (0, 0),
            "case {case:?}: a purged queue is empty, got {waiting} waiting and {in_flight} in \
             flight"
        );

        // What the purge did NOT remove: the next statement's announcement, which
        // the queue was never emptied of because it had not been delivered yet.
        let fresh_id = submit_announced(&emulator, case, QUICK_SQL, "fresh-statement").await;
        let record = wait_for_terminal(&emulator, case, &fresh_id).await;
        let delivered = collect(&emulator, &url, 1).await;
        let envelope = announcement_of(case, &delivered, &fresh_id);
        assert_identity(case, envelope, &fresh_id, &record["Status"]);
        let stale_again: Vec<&Value> = delivered
            .iter()
            .filter(|envelope| {
                envelope.pointer("/detail/statementId") == Some(&json!(stale_id.clone()))
            })
            .collect();
        assert!(
            stale_again.is_empty(),
            "case {case:?}: a purged announcement never comes back, got {stale_again:?}"
        );
    }
}
