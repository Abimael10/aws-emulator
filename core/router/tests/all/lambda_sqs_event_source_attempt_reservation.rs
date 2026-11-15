// SPDX-License-Identifier: CC0-1.0

//! The reservation a Lambda event source mapping holds on the Amazon SQS records it took
//! while it is HANDING THEM TO ITS FUNCTION, across the two services that own the halves of
//! it: Amazon SQS holds the queue and decides who may receive a record, and Lambda's mapping
//! is the consumer holding it.
//!
//! This suite needs more than one service by construction: the reservation is observable only
//! as the relation between what the mapping is doing with a record and what an INDEPENDENT
//! consumer of the same queue is handed, so no single-service test can see it.
//!
//! The RULE asserted over the table below: the reservation a mapping holds on a record is
//! renewed FROM THE INSTANT each span of holding it begins, never merely inherited from the
//! delivery that took the record — so when the mapping stops gathering and hands its batch to
//! the function, that batch is reserved again for the whole span one attempt may take, and the
//! record stays invisible to every other consumer for that span even though the gathering
//! window has already spent most of what the delivery reserved. A consumer that is still
//! working on a record keeps its reservation; the queue hands the record to a second consumer
//! only once that consumer is done with it.
//!
//! Nothing here invokes the function and nothing runs a deployment package: each row's
//! function carries a package no engine can run, so the attempt the mapping makes with the
//! gathered batch FAILS — which is what makes the instant the mapping stopped gathering
//! observable (`LastProcessingResult`), and is where each row's watch over the queue begins.
//! The mapping is then taken out of polling (an update puts it into `Updating`, which the poll
//! loop skips) so the only consumer that could be handed the record during the watch is the
//! watch itself: a record it is handed is a reservation the mapping did not keep.
//!
//! The rows differ along what the rule generalises over: the window the mapping gathers over,
//! the queue's own `VisibilityTimeout` (which is all the delivery's hold has left once that
//! window is spent) and how many records the reservation covers.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED;
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_sqs_event_source_batching_window::{
    await_state, given_queue_hiding_for, given_unrunnable_function, independent_receive, lambda,
    last_processing_result, send_message, shared_emulator, BUDGET, FUNCTION_TIMEOUT, LOOK_AGAIN,
    MAPPINGS_URI, STATE_ENABLED,
};

/// One row: the queue's own `VisibilityTimeout`, the window the mapping gathers over and how
/// many records are sent.
struct Row {
    case: &'static str,
    /// The queue's `VisibilityTimeout`, in seconds — at least the function's own `Timeout`,
    /// which is what Lambda accepts a mapping on, and SHORTER than the watch below, so a
    /// reservation the attempt did not renew would have lapsed inside it.
    visibility_timeout: i64,
    /// `MaximumBatchingWindowInSeconds`: how long the mapping spends gathering, which is the
    /// part of the delivery's hold that is already spent when the attempt begins.
    window: i64,
    /// How many records are sent, all of them inside one batch (`BATCH_SIZE` is larger), so
    /// the reservation covers each of them.
    records: usize,
}

/// The `BatchSize` every row polls under: larger than the records it sends, so no row's batch
/// fills and each waits its whole window out.
const BATCH_SIZE: i64 = 10;

/// How long each row keeps looking for a record the mapping should still have reserved, once
/// the mapping has made its attempt.
///
/// Longer than every row's `VisibilityTimeout` — what the delivery's hold has left once the
/// gathering window is spent — and well inside the span a renewed reservation covers (that
/// window plus the timeout again), so a row fails only when the reservation was not renewed
/// and passes with room to spare when it was.
const WATCH: Duration = Duration::from_secs(3);

/// Wait until the mapping `uuid` names reports the attempt it made with the batch it gathered,
/// and answer what it reported.
async fn await_attempt(router: &MultiServiceRouter, case: &str, uuid: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(result) = last_processing_result(router, uuid).await {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must hand the batch it gathered to its function"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take the mapping `uuid` names out of polling: any update puts it into `Updating`, and the
/// poll loop polls only the mappings that report `Enabled`, so after this the watch below is
/// the only consumer that could be handed a record.
async fn stop_polling(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let (status, updated) = lambda(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        Some(&json!({ "Enabled": false })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: UpdateEventSourceMapping answered {status} {updated}"
    );
}

/// RULE: a batch the mapping has handed to its function is reserved afresh for that attempt,
/// so no other consumer of the queue is handed one of its records while the attempt is the
/// mapping's — however much of the delivery's own hold the gathering window spent.
#[tokio::test]
async fn an_attempt_reserves_the_batch_it_was_handed_afresh() {
    let rows = [
        Row {
            case: "a window that spends the whole delivery hold but the visibility timeout",
            visibility_timeout: FUNCTION_TIMEOUT,
            window: 6,
            records: 1,
        },
        Row {
            case: "a longer window over a longer visibility timeout, two records",
            visibility_timeout: FUNCTION_TIMEOUT + 1,
            window: 8,
            records: 2,
        },
    ];
    let router = shared_emulator();
    for row in &rows {
        let case = row.case;
        let (url, arn) = given_queue_hiding_for(
            &router,
            case,
            &format!("attempt-reservation-{}", row.window),
            row.visibility_timeout,
        )
        .await;
        let function = format!("attempt-reservation-{}", row.window);
        given_unrunnable_function(&router, case, &function).await;

        let (status, created) = lambda(
            &router,
            "POST",
            MAPPINGS_URI,
            Some(&json!({
                "FunctionName": function,
                "EventSourceArn": arn,
                "BatchSize": BATCH_SIZE,
                "MaximumBatchingWindowInSeconds": row.window,
                "Enabled": true,
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: CreateEventSourceMapping answered {status} {created}"
        );
        let uuid = created["UUID"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
            .to_string();
        await_state(&router, case, &uuid, STATE_ENABLED).await;

        for index in 0..row.records {
            send_message(&router, case, &url, &format!("{case}-{index}")).await;
        }

        // The attempt is where the reservation this suite is about begins: the mapping spent
        // its whole window gathering, so what the delivery reserved is nearly up.
        let attempt = await_attempt(&router, case, &uuid).await;
        assert_eq!(
            attempt, LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED,
            "case {case:?}: the attempt is one its function could not take, so the batch it \
             held is still in flight, got {attempt:?}"
        );
        stop_polling(&router, case, &uuid).await;

        // The watch: look again and again until the deadline, and fail the moment one of
        // those receives is handed a record the mapping took.
        let deadline = Instant::now() + WATCH;
        let mut receives: u32 = 0;
        while Instant::now() < deadline {
            let stolen = independent_receive(&router, case, &url).await;
            assert!(
                stolen.is_empty(),
                "case {case:?}: a record the mapping is holding must stay reserved for the \
                 attempt it was handed to (the queue hides a delivery for {}s of its own, the \
                 mapping gathered over {}s), an independent receive was handed {stolen:?} \
                 after {receives} receive(s)",
                row.visibility_timeout,
                row.window
            );
            receives = receives.saturating_add(1);
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert!(
            receives > 1,
            "case {case:?}: the watch must have looked more than once while the mapping held \
             the batch, it looked {receives} time(s)"
        );
    }
}
