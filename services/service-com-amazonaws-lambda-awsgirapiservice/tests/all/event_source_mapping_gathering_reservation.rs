// SPDX-License-Identifier: CC0-1.0

//! The reservation an event source mapping holds on the Amazon SQS records it took while it
//! is still GATHERING them into a batch.
//!
//! `CreateEventSourceMapping` "creates a mapping between an event source and an Lambda Event
//! Function. Lambda reads items from the event source and invokes the function", and
//! `MaximumBatchingWindowInSeconds` is "the maximum amount of time, in seconds, that Lambda
//! spends gathering records before invoking the function" — so between the delivery that took
//! a record and the attempt it is handed to, the mapping is the consumer holding that record.
//!
//! The RULE under test: a record a mapping has taken stays invisible to every other consumer
//! for as long as the mapping holds it, the whole gathering window included — so the
//! reservation is RENEWED on every pass that finds the mapping still gathering, and a queue
//! whose own `VisibilityTimeout` is SHORTER than the window does not hand the record to a
//! second consumer part way through the window.
//!
//! What a row reads is the instant the QUEUE ITSELF recorded for the record it is hiding
//! ([`held_until`]), which says the same thing whenever it is read, so no row waits a span of
//! wall clock out. The passes are the row's own ([`DrivenPolling`]), so the gathering pass
//! happens when the row makes it happen.
//!
//! The rows differ along what the rule generalises over: how much longer the window is than
//! the queue's own `VisibilityTimeout`, the `VisibilityTimeout` itself, and how many records
//! the mapping is gathering.

use std::time::{Duration, SystemTime};

use serde_json::json;

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_queue, create_timed_function,
    driven_router_with_queues, held_until, send_message, STATE_ENABLED,
};

/// The `BatchSize` every row polls under: larger than the records it sends, so no row's batch
/// fills and each one is still GATHERING when the row reads the queue's hold.
const BATCH_SIZE: i64 = 10;

/// The function `Timeout` every row's function carries, in seconds — at most the smallest
/// `VisibilityTimeout` below, which is what Lambda accepts a mapping on.
const FUNCTION_TIMEOUT: i64 = 1;

/// One row: the queue's own `VisibilityTimeout`, the window the mapping gathers over, and how
/// many records are sent.
struct Row {
    case: &'static str,
    /// The queue's `VisibilityTimeout`, in seconds — SHORTER than the window, which is the
    /// case the rule is about.
    visibility_timeout: i64,
    /// `MaximumBatchingWindowInSeconds`: how long the mapping keeps gathering, and so how
    /// long it holds what it has already taken.
    window: i64,
    /// How many records are sent, all of them into the one batch being gathered.
    records: usize,
}

#[tokio::test]
async fn a_gathering_mapping_holds_its_records_past_the_queues_own_timeout() {
    let rows = [
        Row {
            case: "a window several times the queue's own timeout",
            visibility_timeout: FUNCTION_TIMEOUT + 1,
            window: 8,
            records: 1,
        },
        Row {
            case: "a longer timeout under a longer window, two records",
            visibility_timeout: FUNCTION_TIMEOUT + 3,
            window: 12,
            records: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, mut driven) = driven_router_with_queues();
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("gathering-reservation-{index}"),
            &json!({ "VisibilityTimeout": row.visibility_timeout.to_string() }),
        )
        .await;
        let function = format!("gathering-reservation-{index}");
        create_timed_function(&router, case, &function, FUNCTION_TIMEOUT).await;

        let created = create_mapping(
            &router,
            &json!({
                "FunctionName": function,
                "EventSourceArn": arn,
                "BatchSize": BATCH_SIZE,
                "MaximumBatchingWindowInSeconds": row.window,
                "Enabled": true,
            }),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

        let bodies: Vec<String> = (0..row.records)
            .map(|record| format!("gathering-reservation-{index}-{record}"))
            .collect();
        for body in &bodies {
            send_message(&router, case, &url, body, None).await;
        }

        // ONE pass: the mapping takes what the queue has and keeps gathering, because neither
        // its `BatchSize` nor its window is up. From here on it is the consumer holding those
        // records.
        driven.poll_once().await;
        let read_at = SystemTime::now();

        let window = Duration::from_secs(u64::try_from(row.window).unwrap_or_default());
        let held = held_until(driven.engine(), &arn);
        for body in &bodies {
            let until = held.get(body).copied().flatten().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the queue must be hiding {body:?} for the gathering \
                     mapping, got {held:?}"
                )
            });
            let remaining = until.duration_since(read_at).unwrap_or(Duration::ZERO);
            assert!(
                remaining > window,
                "case {case:?}: {body:?} must stay invisible past the {window:?} the mapping \
                 keeps gathering for, got {remaining:?}"
            );
        }

        // A second pass finds the mapping still gathering, and the mapping is still the
        // consumer holding those records: each one's reservation is renewed, so the instant
        // the queue hides it until MOVES ON rather than staying the one the delivery set.
        driven.poll_once().await;
        let renewed = held_until(driven.engine(), &arn);
        for body in &bodies {
            let before = held.get(body).copied().flatten().unwrap_or(read_at);
            let after = renewed.get(body).copied().flatten().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the queue must still be hiding {body:?} for the gathering \
                     mapping, got {renewed:?}"
                )
            });
            assert!(
                after > before,
                "case {case:?}: a pass that finds the mapping still gathering must renew the \
                 reservation on {body:?}, got {after:?} where the delivery left {before:?}"
            );
        }
    }
}
