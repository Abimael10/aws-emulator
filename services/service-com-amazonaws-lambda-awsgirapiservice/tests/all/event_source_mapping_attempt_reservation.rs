// SPDX-License-Identifier: CC0-1.0

//! The reservation an event source mapping holds on the Amazon SQS records it took while it
//! is HANDING THEM TO ITS FUNCTION.
//!
//! `CreateEventSourceMapping` "creates a mapping between an event source and an Lambda Event
//! Function. Lambda reads items from the event source and invokes the function", and the
//! poller is the consumer that received those records: a queue hands a record to ONE consumer
//! for as long as that consumer holds it.
//!
//! The RULE under test: the reservation a mapping holds on a record is renewed FROM THE
//! INSTANT each span of holding it begins, never merely inherited from the delivery that took
//! the record — so when the mapping stops gathering and hands its batch to the function, that
//! batch is reserved again for the whole span one attempt may take (the queue's own
//! `VisibilityTimeout` or the function's `Timeout`, whichever is longer, plus the gathering
//! window), even though the `MaximumBatchingWindowInSeconds` it spent gathering has already
//! used up most of what the delivery reserved. A consumer still working on a record keeps it
//! reserved; the queue hands it to a second consumer only once that consumer is done.
//!
//! What a row reads is the instant the QUEUE ITSELF recorded for each record it is hiding
//! ([`held_until`]), which says the same thing whenever it is read — not a span of wall clock
//! a row has to catch a message inside. The poll passes are the row's own
//! ([`DrivenPolling`]), so the attempt happens when the row makes it happen, and each row's
//! function carries a package no engine can run: the attempt FAILS, which is what leaves the
//! batch in flight with the reservation this suite is about (a processed batch is deleted and
//! has no reservation to read).
//!
//! The rows differ along what the rule generalises over: the gathering window, the queue's own
//! `VisibilityTimeout` (which is all the delivery's reservation has left once that window is
//! spent) and how many records the reservation covers.

use std::time::{Duration, Instant, SystemTime};

use serde_json::json;

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_queue, create_timed_function, driven_router_with_queues,
    get_mapping, held_until, send_message, DrivenPolling, CLOCK_BUDGET, LOOK_AGAIN, STATE_ENABLED,
};

/// The `BatchSize` every row polls under: larger than the records it sends, so no row's batch
/// fills and each waits its whole window out.
const BATCH_SIZE: i64 = 10;

/// The function `Timeout` every row's function carries, in seconds. At most the smallest
/// `VisibilityTimeout` below, which is what Lambda accepts a mapping on.
const FUNCTION_TIMEOUT: i64 = 1;

/// One row: the queue's own `VisibilityTimeout`, the window the mapping gathers over and how
/// many records are sent.
struct Row {
    case: &'static str,
    /// The queue's `VisibilityTimeout`, in seconds — at least the function's own `Timeout`,
    /// and SHORTER than the window, which is the case the rule is about.
    visibility_timeout: i64,
    /// `MaximumBatchingWindowInSeconds`: how long the mapping spends gathering, and so how
    /// much of the delivery's reservation is already spent when the attempt begins.
    window: i64,
    /// How many records are sent, all of them inside one batch, so the reservation covers
    /// each of them.
    records: usize,
}

/// The span one delivery of a row's queue reserves a record for: the longer of the queue's own
/// `VisibilityTimeout` and the function's `Timeout`, plus the window the batch is gathered
/// over — the span the mapping holds the batch across.
fn reserved_span(row: &Row) -> Duration {
    let seconds = row.visibility_timeout.max(FUNCTION_TIMEOUT) + row.window;
    Duration::from_secs(u64::try_from(seconds).unwrap_or_default())
}

/// Drive passes until the mapping `uuid` names reports the attempt it made with the batch it
/// gathered, and answer what it reported.
///
/// The window is a span of the CLOCK, so a pass before it has run gathers rather than hands
/// over: the row looks again until the attempt is there, which is how every other span this
/// emulator owes is waited for.
async fn await_attempt(
    router: &smithy_server_runtime::MultiServiceRouter,
    driven: &mut DrivenPolling,
    case: &str,
    uuid: &str,
) -> String {
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        driven.poll_once().await;
        if let Some(result) = get_mapping(router, uuid).await.string("LastProcessingResult") {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must hand the batch it gathered to its function"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a batch the mapping has handed to its function is reserved afresh for that attempt,
/// so every record of it stays invisible to every other consumer for the whole span an attempt
/// may take — however much of the delivery's own reservation the gathering window spent.
#[tokio::test]
async fn an_attempt_reserves_the_batch_it_was_handed_afresh() {
    let rows = [
        Row {
            case: "a window that spends all of the delivery's reservation but the visibility \
                   timeout",
            visibility_timeout: FUNCTION_TIMEOUT,
            window: 3,
            records: 1,
        },
        Row {
            case: "a longer window over a longer visibility timeout, two records",
            visibility_timeout: FUNCTION_TIMEOUT + 2,
            window: 4,
            records: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, mut driven) = driven_router_with_queues();
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("attempt-reservation-{index}"),
            &json!({ "VisibilityTimeout": row.visibility_timeout.to_string() }),
        )
        .await;
        let function = format!("attempt-reservation-{index}");
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
            .map(|record| format!("attempt-reservation-{index}-{record}"))
            .collect();
        for body in &bodies {
            send_message(&router, case, &url, body, None).await;
        }

        // The attempt is where the reservation this suite is about begins: the mapping spent
        // its whole window gathering, so what the delivery reserved is nearly up by now.
        let attempt = await_attempt(&router, &mut driven, case, &uuid).await;
        assert!(
            !attempt.is_empty(),
            "case {case:?}: the mapping must report what its attempt came to"
        );
        let read_at = SystemTime::now();

        // The queue's own record of how long each message of the batch is hidden: with the
        // batch reserved afresh for the attempt, what is left of it at this instant is the
        // whole span a delivery reserves — more than the window alone, which is all the
        // delivery's own reservation had left when the attempt began.
        let held = held_until(driven.engine(), &arn);
        let span = reserved_span(row);
        let window = Duration::from_secs(u64::try_from(row.window).unwrap_or_default());
        for body in &bodies {
            let until = held
                .get(body)
                .copied()
                .flatten()
                .unwrap_or_else(|| panic!(
                    "case {case:?}: the queue must still be hiding {body:?} for the mapping, \
                     got {held:?}"
                ));
            let remaining = until.duration_since(read_at).unwrap_or(Duration::ZERO);
            assert!(
                remaining > window,
                "case {case:?}: the attempt must reserve {body:?} afresh, so more than the \
                 {window:?} window is left of its reservation, got {remaining:?} of a \
                 {span:?} span"
            );
            assert!(
                remaining <= span,
                "case {case:?}: the reservation is one span of a delivery, so no more than \
                 {span:?} is left of it, got {remaining:?}"
            );
        }
    }
}
