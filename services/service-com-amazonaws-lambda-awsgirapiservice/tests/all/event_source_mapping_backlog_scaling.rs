// SPDX-License-Identifier: CC0-1.0

//! How many batch consumers an Amazon SQS event source mapping that configures NO
//! `ScalingConfig` of its own puts on its queue, end to end over the wire.
//!
//! RULE: a queue event source scales its concurrent batch consumers with the DEPTH of its
//! backlog — one consumer per batch the backlog holds, each of at most the mapping's
//! `BatchSize` — bounded only by what a client really did limit it to: "limits the number of
//! concurrent instances that the Amazon SQS event source can invoke"
//! (`ScalingConfig$MaximumConcurrency`) where the mapping configures one, otherwise the most
//! concurrent instances such a source can be asked to invoke at all (that member's own
//! `@range(min: 2, max: 1000)`, `smithy_query(query="shape", shape="MaximumConcurrency")`),
//! and in either case "the number of simultaneous executions to reserve for the function"
//! (`PutFunctionConcurrency`). So a mapping nobody configured drains a deep queue with
//! overlapping invocations rather than one batch at a time.
//!
//! [`crate::event_source_mapping_scaling_concurrency`] pins what a CONFIGURED concurrency
//! hands over per pass and [`crate::event_source_mapping_concurrency_ceiling`] pins it over a
//! whole drain; this suite pins the other end of the same rule — what a mapping that
//! configures nothing scales to — and shares both their harness rather than copying it.
//!
//! The rows differ along what the rule generalises over: how deep the backlog is, how many
//! messages one batch may hold, and which of the two bounds (the function's reservation, a
//! configured concurrency) is the one that binds. Both tests make the poll passes THEMSELVES
//! (`crate::event_source_mappings::DrivenPolling`), so what each assertion reads is the state
//! one awaited pass left rather than a race against a background loop, and nothing here waits
//! for a span or reads the wall clock.

use serde_json::json;

// The harness of the suites over the configured half of this rule: the three services on one
// engine with the poll passes the test's own, the reservation call, the mapping a row asks
// for, the reading of the concurrency gauge, the handler that holds its environment, the
// function's `Timeout` and the queue's `VisibilityTimeout`, the span that handler holds for,
// and the peak that counts as an overlap.
use crate::event_source_mapping_scaling_concurrency::{
    driven_router, given_mapping, observed_concurrency, reserve, HANDLER_SOURCE, HELD_SECONDS,
    TIMEOUT_SECONDS,
};
use crate::event_source_mappings::{
    create_queue, create_running_function, create_timed_function, queue_message_counts,
    send_message,
};

/// The least concurrency the overlap test below accepts as evidence that a mapping configuring
/// nothing scaled out with its backlog: more than the two a sibling suite's configured mapping
/// has to show, and inside the four batches that backlog holds, so the margin is the gauge's
/// own sampling and not a weaker rule.
const SCALED_OUT: f64 = 3.0;

/// One row of the table: what is waiting on the queue, what one batch may hold, what the
/// function reserves and what the mapping limits itself to — and how many messages ONE pass
/// must then take out of the queue.
struct Row {
    case: &'static str,
    /// How many messages are waiting when the pass is made.
    waiting: u32,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: usize,
    /// `ReservedConcurrentExecutions`, or `None` for a function that reserves nothing.
    reserved: Option<i64>,
    /// `ScalingConfig$MaximumConcurrency`, or `None` for the mapping this suite is about —
    /// one that configures none.
    maximum_concurrency: Option<i64>,
    /// How many of the waiting messages one pass must take: one batch per consumer the
    /// backlog puts on the queue, each of at most `batch_size` messages.
    takes: u32,
}

/// The catalog: a backlog of one message, backlogs of three and nine messages over batches of
/// one, a backlog that does not divide into whole batches, and the two bounds that still hold
/// over a mapping configuring no concurrency of its own — what the function reserves, and a
/// `MaximumConcurrency` shallower than the backlog.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a backlog of one message",
            waiting: 1,
            batch_size: 1,
            reserved: None,
            maximum_concurrency: None,
            takes: 1,
        },
        Row {
            case: "a backlog of three messages over batches of one",
            waiting: 3,
            batch_size: 1,
            reserved: None,
            maximum_concurrency: None,
            takes: 3,
        },
        Row {
            case: "a backlog of nine messages over batches of one",
            waiting: 9,
            batch_size: 1,
            reserved: None,
            maximum_concurrency: None,
            takes: 9,
        },
        Row {
            case: "a backlog of nine messages over batches of four",
            waiting: 9,
            batch_size: 4,
            reserved: None,
            maximum_concurrency: None,
            takes: 9,
        },
        Row {
            case: "a backlog deeper than what the function reserves",
            waiting: 9,
            batch_size: 1,
            reserved: Some(2),
            maximum_concurrency: None,
            takes: 2,
        },
        Row {
            case: "a backlog deeper than the concurrency the mapping limits itself to",
            waiting: 9,
            batch_size: 1,
            reserved: None,
            maximum_concurrency: Some(3),
            takes: 3,
        },
    ]
}

/// RULE: one pass of a mapping that configures no concurrency takes a batch for every batch
/// its backlog holds — so what it consumes grows with the queue's depth — and stops at the
/// first bound a client really did set.
#[tokio::test]
async fn one_pass_scales_its_batch_consumers_with_the_depth_of_the_backlog() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (router, mut driven, _engine) = driven_router();
        let function = format!("esm-backlog-fn-{index}");
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("esm-backlog-queue-{index}"),
            &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        create_timed_function(&router, case, &function, TIMEOUT_SECONDS).await;
        if let Some(reserved) = row.reserved {
            reserve(&router, case, &function, reserved).await;
        }
        given_mapping(
            &router,
            case,
            &function,
            &arn,
            row.batch_size,
            row.maximum_concurrency,
        )
        .await;
        for message in 0..row.waiting {
            send_message(
                &router,
                case,
                &url,
                &json!({ "mark": message }).to_string(),
                None,
            )
            .await;
        }

        driven.poll_once().await;

        // Nothing this function is handed can be processed — its package runs no handler — so
        // every message the pass took is still held by the queue, out of sight: what is
        // VISIBLE is what the pass left behind.
        let counts = queue_message_counts(&router, case, &url).await;
        let taken = row.waiting.saturating_sub(counts.0);
        assert_eq!(
            taken, row.takes,
            "case {case:?}: one pass must take {} of the {} message(s) waiting, got {counts:?} \
             as (visible, in flight, delayed)",
            row.takes, row.waiting
        );
    }
}

/// RULE, the other half: those consumers really are IN FLIGHT TOGETHER, so a mapping nobody
/// configured drains a backlog with overlapping invocations of its function rather than one
/// batch after another.
///
/// The handler holds its environment while the others start, and the concurrency the function
/// reached is read off its own `ConcurrentExecutions` series, where a client reads it: a
/// mapping that consumed its backlog serially would have published no reading above one.
#[tokio::test]
async fn a_mapping_configuring_no_concurrency_drains_a_backlog_with_overlapping_invocations() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a backlog of four messages, no configured concurrency";
    let (router, mut driven, _engine) = driven_router();
    let function = "esm-backlog-overlap";
    let (url, arn) = create_queue(
        &router,
        case,
        "esm-backlog-overlap-queue",
        &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
    )
    .await;
    // The handler holds its environment for the span its event names, which is what makes the
    // overlap of the batches of one pass observable.
    create_running_function(
        &router,
        case,
        function,
        HANDLER_SOURCE,
        TIMEOUT_SECONDS,
        &json!({}),
    )
    .await;
    given_mapping(&router, case, function, &arn, 1, None).await;
    for _ in 0..4 {
        send_message(&router, case, &url, &HELD_SECONDS.to_string(), None).await;
    }

    driven.poll_once().await;

    // Every message the pass handed over was processed, so the queue is empty: the batches
    // were really run and not merely taken. A pass answers once each of its batches has been
    // handed over, so this is what that one pass came to.
    let counts = queue_message_counts(&router, case, &url).await;
    assert_eq!(
        counts,
        (0, 0, 0),
        "case {case:?}: one pass must drain the whole backlog, got {counts:?} as \
         (visible, in flight, delayed)"
    );
    let observed = observed_concurrency(&router, case, function).await;
    assert!(
        observed >= SCALED_OUT,
        "case {case:?}: expected the function to be seen holding at least {SCALED_OUT} \
         concurrent executions, got {observed}"
    );
}
