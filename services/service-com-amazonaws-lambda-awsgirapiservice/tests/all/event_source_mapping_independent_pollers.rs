// SPDX-License-Identifier: CC0-1.0

//! Whether one event source mapping's deliveries wait on ANOTHER mapping's function.
//!
//! The RULE pinned here: **an event source mapping is a consumer of its own event source —
//! "Lambda reads items from the event source and invokes the function"
//! (`CreateEventSourceMapping`) for each mapping — so what one mapping's function is doing is
//! no part of what another mapping's records wait for: while one mapping's invocation is
//! still running, another mapping's message is polled and handed to ITS function, and the
//! mapping whose invocation was outstanding has still reported nothing about that attempt.**
//!
//! The observation is a STATE, not a span: the second mapping's own record arrives on the
//! results queue while the first mapping reports no `LastProcessingResult` at all — "the
//! result of the event source mapping's last processing attempt", which a mapping has only
//! once an attempt of it has been decided. A poller that handed the mappings over one after
//! another could not do that: the second mapping's records would wait for the first
//! mapping's invocation to be decided, and that decision is exactly what the absent
//! `LastProcessingResult` says has not happened yet. The reading is taken in the same look
//! that sees the record arrive, so it is of that instant and of no later one.
//!
//! Nothing here waits for a span of wall clock. The first mapping's handler stays in its
//! invocation until this suite RELEASES it — it polls a release queue for a message that
//! only the suite sends — so "its invocation is still running" is a fact the suite controls
//! rather than one it times, and every wait below is on a condition with a generous deadline.
//!
//! The rows differ along what the mapping whose function is busy is holding: ONE record, and
//! a batch of several records in the one invocation.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    create_queue, create_running_function, get_mapping, given_enabled_mapping, router_with_queues,
    send_message, taken_bodies, FORWARDING_HANDLER, LOOK_AGAIN, RESULTS_QUEUE_URL,
};

/// How long an observation of work the emulator owes — a batch polled, a handler run, a
/// message forwarded — waits before it is a failure. Generous, and taken from what the
/// emulator itself allows ONE attempt (the wait for a share of the run capacity and the wait
/// for the run's own answer) rather than picked here: a row fails only when the work did not
/// happen at all.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// The `VisibilityTimeout` of the queue the BUSY mapping polls, in seconds: a queue maps onto
/// a function only while it outlasts that function's `Timeout`, so it is the busy function's
/// own timeout.
const BUSY_TIMEOUT: i64 = 60;

/// The same pair for the mapping whose delivery the rule is about.
const PROMPT_TIMEOUT: i64 = 10;

/// What the busy handler reports before it blocks, and after it is released.
const STARTED: &str = "busy-invocation-started";
const FINISHED: &str = "busy-invocation-finished";

/// The body the prompt mapping's message carries, which is what its handler forwards.
const FORWARDED: &str = "prompt-mapping-record";

/// The handler of the BUSY mapping's function: it reports that its invocation has begun and
/// then stays in it, polling the release queue until this suite sends it a message. Nothing
/// here sleeps: the handler waits on the queue, and the suite decides when the wait ends.
const BLOCKING_HANDLER: &str = r#"
import os

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    results = os.environ["RESULTS_QUEUE_URL"]
    release = os.environ["RELEASE_QUEUE_URL"]
    client.send_message(QueueUrl=results, MessageBody=os.environ["STARTED_MARKER"])
    while True:
        answered = client.receive_message(
            QueueUrl=release, MaxNumberOfMessages=1, WaitTimeSeconds=1
        )
        if answered.get("Messages"):
            break
    client.send_message(QueueUrl=results, MessageBody=os.environ["FINISHED_MARKER"])
    return {"released": True}
"#;

/// Read the results queue until `marker` has arrived, answering what the BUSY mapping
/// reported as its `LastProcessingResult` in the same look that saw it — and every body seen
/// on the way.
///
/// The wait is on the MARKER, not on a span: a loaded host takes longer to get there and the
/// observation is the same, because the mapping's reading is of the instant the marker
/// arrived rather than of whenever this loop happened to stop.
async fn awaited(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    marker: &str,
    busy_mapping: &str,
    seen: &mut Vec<String>,
) -> Option<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let reported = get_mapping(router, busy_mapping)
            .await
            .string("LastProcessingResult");
        seen.extend(taken_bodies(router, case, url).await);
        if seen.iter().any(|body| body == marker) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {marker:?} to be reported, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: how many records the busy mapping's one invocation is holding, which is also its
/// `BatchSize` — so each row keeps exactly one invocation of it outstanding.
struct Row {
    case: &'static str,
    busy_messages: usize,
}

/// RULE: a mapping's records are polled and handed to its function while ANOTHER mapping's
/// invocation is still running, and that other mapping has reported nothing about its own
/// attempt at the instant the delivery happens.
#[tokio::test]
async fn a_mappings_delivery_does_not_wait_on_another_mappings_function() {
    let rows = [
        Row {
            case: "one record in the busy mapping's invocation",
            busy_messages: 1,
        },
        Row {
            case: "a batch of records in the busy mapping's invocation",
            busy_messages: 3,
        },
    ];

    for row in rows {
        let case = row.case;
        let suffix = row.busy_messages;
        let router = router_with_queues();
        let (results_url, _) = create_queue(
            &router,
            case,
            &format!("independent-results-{suffix}"),
            &json!({}),
        )
        .await;
        let (release_url, _) = create_queue(
            &router,
            case,
            &format!("independent-release-{suffix}"),
            &json!({}),
        )
        .await;
        let (busy_url, busy_arn) = create_queue(
            &router,
            case,
            &format!("independent-busy-source-{suffix}"),
            &json!({ "VisibilityTimeout": BUSY_TIMEOUT.to_string() }),
        )
        .await;
        let (prompt_url, prompt_arn) = create_queue(
            &router,
            case,
            &format!("independent-prompt-source-{suffix}"),
            &json!({ "VisibilityTimeout": PROMPT_TIMEOUT.to_string() }),
        )
        .await;

        let busy_function = format!("independent-busy-fn-{suffix}");
        let prompt_function = format!("independent-prompt-fn-{suffix}");
        create_running_function(
            &router,
            case,
            &busy_function,
            BLOCKING_HANDLER,
            BUSY_TIMEOUT,
            &json!({
                RESULTS_QUEUE_URL: results_url,
                "RELEASE_QUEUE_URL": release_url,
                "STARTED_MARKER": STARTED,
                "FINISHED_MARKER": FINISHED,
            }),
        )
        .await;
        create_running_function(
            &router,
            case,
            &prompt_function,
            FORWARDING_HANDLER,
            PROMPT_TIMEOUT,
            &json!({ RESULTS_QUEUE_URL: results_url }),
        )
        .await;

        let busy_mapping = given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": busy_function,
                "EventSourceArn": busy_arn,
                // Every record the row sends goes over in ONE invocation, so the mapping has
                // exactly one outstanding however many records it is holding.
                "BatchSize": row.busy_messages,
                "MaximumBatchingWindowInSeconds": 1,
                "Enabled": true,
            }),
        )
        .await;
        let prompt_mapping = given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": prompt_function,
                "EventSourceArn": prompt_arn,
                "BatchSize": 1,
                "Enabled": true,
            }),
        )
        .await;

        // The busy mapping is handed its records first and reports that it is running.
        for index in 0..row.busy_messages {
            send_message(&router, case, &busy_url, &format!("busy-{index}"), None).await;
        }
        let mut seen: Vec<String> = Vec::new();
        awaited(&router, case, &results_url, STARTED, &busy_mapping, &mut seen).await;

        // Now the other mapping's message: it is polled and handed to its own function while
        // the busy mapping's invocation is still in it — which is what the busy mapping's own
        // reading, taken in the look that saw the record arrive, says.
        send_message(&router, case, &prompt_url, FORWARDED, None).await;
        let busy_reported = awaited(
            &router,
            case,
            &results_url,
            FORWARDED,
            &busy_mapping,
            &mut seen,
        )
        .await;
        assert_eq!(
            busy_reported, None,
            "case {case:?}: the busy mapping's invocation must still be running when the other \
             mapping's record is delivered, but it already reported {busy_reported:?}"
        );
        assert!(
            !seen.iter().any(|body| body == FINISHED),
            "case {case:?}: the busy handler was never released, so it cannot have finished; \
             saw {seen:?}"
        );

        // Released, the busy mapping finishes its own work too: both mappings consumed what
        // they were handed.
        send_message(&router, case, &release_url, "go", None).await;
        awaited(
            &router,
            case,
            &results_url,
            FINISHED,
            &busy_mapping,
            &mut seen,
        )
        .await;
        let deadline = Instant::now() + BUDGET;
        let mut reported = None;
        while Instant::now() < deadline {
            reported = get_mapping(&router, &busy_mapping)
                .await
                .string("LastProcessingResult");
            if reported.as_deref() == Some(LAST_PROCESSING_RESULT_OK) {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            reported.as_deref(),
            Some(LAST_PROCESSING_RESULT_OK),
            "case {case:?}: the released invocation must leave the busy mapping reporting \
             {LAST_PROCESSING_RESULT_OK}, got {reported:?}"
        );
        let prompt = get_mapping(&router, &prompt_mapping).await;
        assert_eq!(
            prompt.string("LastProcessingResult").as_deref(),
            Some(LAST_PROCESSING_RESULT_OK),
            "case {case:?}: the mapping that delivered its record must report \
             {LAST_PROCESSING_RESULT_OK}, got {}",
            prompt.text()
        );
    }
}
