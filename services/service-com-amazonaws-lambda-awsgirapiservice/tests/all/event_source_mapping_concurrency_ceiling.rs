// SPDX-License-Identifier: CC0-1.0

//! What an Amazon SQS event source mapping's `ScalingConfig$MaximumConcurrency` bounds over
//! the WHOLE drain of a backlog, end to end over the wire.
//!
//! RULE: `ScalingConfig$MaximumConcurrency` — "limits the number of concurrent instances that
//! the Amazon SQS event source can invoke" — is a CEILING on how many invocations of the
//! function a queue source has in flight at ONE instant, bounded further by what the function
//! itself reserves ("the number of simultaneous executions to reserve for the function",
//! `PutFunctionConcurrency`); it holds for as long as the mapping is draining, not merely on
//! the first pass, and the mapping goes on draining until the backlog is gone, so a deep queue
//! is emptied without the ceiling ever being passed. A mapping configuring no concurrency has
//! one batch in flight at a time.
//!
//! This is the other side of [`crate::event_source_mapping_scaling_concurrency`], which pins
//! how many batches ONE pass hands over and that the batches of one pass really overlap. Here
//! the whole backlog is drained — several passes of it — and what is asserted is the PEAK the
//! function was ever seen holding across all of them, read where a client reads it: off
//! Lambda's own `ConcurrentExecutions` series, whose datapoints are the observations of that
//! gauge at the instants it rises, so a burst past the ceiling anywhere in the drain leaves a
//! datapoint above it.
//!
//! The rows differ along what the rule generalises over: the concurrency configured (none, each
//! end of the published range of `MaximumConcurrency` — `smithy_query(query="shape",
//! shape="MaximumConcurrency")`: `@range(min: 2, max: 1000)` — and a value inside it), how many
//! messages one batch may hold, how deep the backlog is, and what the function reserves.
//!
//! The suite makes the poll passes ITSELF (`crate::event_source_mappings::DrivenPolling`): a
//! pass answers once every batch it took has been handed over, so the drain is a series of
//! awaited passes rather than a race against a background loop, and nothing here waits for a
//! span or reads the wall clock. The handler of each row holds its execution environment for
//! the span its event names — the HANDLER's own run, which is what makes a concurrency the
//! gauge can observe at all — and the suite takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]) while it runs, because this
//! emulator serves at most `max_concurrent_runs` invocations at once for the whole process.

use serde_json::json;

// The harness of the suite that pins the other half of this rule, shared rather than copied:
// the three services on one engine with the poll passes the test's own, the reservation call,
// the mapping a row asks for, the reading of the concurrency gauge, the handler that holds its
// environment, the function's `Timeout` and the queue's `VisibilityTimeout`, the span that
// handler holds for, and the peak that counts as an overlap.
use crate::event_source_mapping_scaling_concurrency::{
    driven_router, given_mapping, observed_concurrency, reserve, HANDLER_SOURCE, HELD_SECONDS,
    OVERLAP, TIMEOUT_SECONDS,
};
use crate::event_source_mappings::{
    create_queue, create_running_function, messages_held, queue_message_counts, send_message,
};

/// How many passes beyond the backlog's own length a row is given to drain: a pass hands over
/// at least one batch while anything is waiting, so a drain that needs more than this is a
/// drain that is not progressing.
const SPARE_PASSES: u32 = 8;

/// One row: what the mapping and the function ask for, how deep the backlog is, and the
/// greatest concurrency the function may then ever be seen holding.
struct Row {
    case: &'static str,
    /// `ScalingConfig$MaximumConcurrency`, or `None` for a mapping that configures none.
    maximum_concurrency: Option<i64>,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: usize,
    /// `ReservedConcurrentExecutions`, or `None` for a function that reserves nothing.
    reserved: Option<i64>,
    /// How many messages are waiting when the drain starts.
    waiting: u32,
    /// The ceiling on the invocations in flight at one instant that follows from the settings
    /// above — and from the backlog itself, which cannot put more batches in flight than it
    /// holds. A count of executions, as the gauge measures it.
    ceiling: u32,
}

/// The catalog: a mapping that configures no concurrency, one at each end of the published
/// range of `MaximumConcurrency` (`smithy_query(query="shape", shape="MaximumConcurrency")`:
/// `@range(min: 2, max: 1000)`), one inside it, one whose batches hold several messages, and one
/// whose function reserves less concurrency than the mapping asks for. Every backlog is deeper
/// than one pass can take, so each row is drained over several passes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no configured concurrency over a backlog of three",
            maximum_concurrency: None,
            batch_size: 1,
            reserved: None,
            waiting: 3,
            ceiling: 1,
        },
        Row {
            case: "the published minimum concurrency over a backlog of six",
            maximum_concurrency: Some(2),
            batch_size: 1,
            reserved: None,
            waiting: 6,
            ceiling: 2,
        },
        Row {
            case: "a concurrency inside the published range over a backlog of six",
            maximum_concurrency: Some(3),
            batch_size: 1,
            reserved: None,
            waiting: 6,
            ceiling: 3,
        },
        Row {
            case: "the published maximum concurrency over a backlog shorter than it",
            maximum_concurrency: Some(1_000),
            batch_size: 1,
            reserved: None,
            waiting: 2,
            ceiling: 2,
        },
        Row {
            case: "a concurrency of two over batches of two",
            maximum_concurrency: Some(2),
            batch_size: 2,
            reserved: None,
            waiting: 8,
            ceiling: 2,
        },
        Row {
            case: "a function reserving less than the mapping asks for",
            maximum_concurrency: Some(4),
            batch_size: 1,
            reserved: Some(2),
            waiting: 4,
            ceiling: 2,
        },
    ]
}

/// RULE: the backlog drains, and the greatest concurrency the function was ever seen holding
/// across the whole drain is at most the ceiling the mapping and the function's own reservation
/// leave it — and reaches it, wherever that ceiling admits more than one batch.
#[tokio::test]
async fn the_configured_concurrency_bounds_the_whole_drain_of_a_backlog() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (router, mut driven, _engine) = driven_router();
        let function = format!("esm-ceiling-fn-{index}");
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("esm-ceiling-queue-{index}"),
            &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        create_running_function(
            &router,
            case,
            &function,
            HANDLER_SOURCE,
            TIMEOUT_SECONDS,
            &json!({}),
        )
        .await;
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
        for _ in 0..row.waiting {
            send_message(&router, case, &url, &HELD_SECONDS.to_string(), None).await;
        }

        // The drain: one awaited pass at a time until the queue holds nothing. A pass that
        // handed its batches over has processed them, so a queue that still holds messages is a
        // queue with passes left to make rather than a span to wait out.
        let mut counts = queue_message_counts(&router, case, &url).await;
        let mut passes = 0u32;
        while messages_held(counts) > 0 {
            assert!(
                passes < row.waiting + SPARE_PASSES,
                "case {case:?}: the backlog of {} must drain, and after {passes} pass(es) the \
                 queue still reports {counts:?} as (visible, in flight, delayed)",
                row.waiting
            );
            driven.poll_once().await;
            passes = passes.saturating_add(1);
            counts = queue_message_counts(&router, case, &url).await;
        }

        // The peak the gauge was seen at over the whole drain, which is where the ceiling shows.
        let ceiling = f64::from(row.ceiling);
        let observed = observed_concurrency(&router, case, &function).await;
        assert!(
            observed <= ceiling,
            "case {case:?}: expected the function never to hold more than {ceiling} concurrent \
             execution(s) over the whole drain, got {observed}"
        );
        // And the ceiling is a ceiling the mapping really USES, not one it stays under by
        // running the batches of a pass one after another: where it admits more than one batch,
        // the drain was seen holding more than one invocation at once. A ceiling of one batch
        // has no such half — what it has to show is that the drain ran at all.
        let reached = if ceiling >= OVERLAP { OVERLAP } else { 1.0 };
        assert!(
            observed >= reached,
            "case {case:?}: expected the function to be seen holding at least {reached} \
             concurrent execution(s), got {observed}"
        );
    }
}
