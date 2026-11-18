// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an Amazon SQS event source mapping's
//! `ScalingConfig$MaximumConcurrency` bounds, observed through a SIDE CHANNEL of the
//! invocations themselves rather than through a metric.
//!
//! RULE: `ScalingConfig$MaximumConcurrency` — "limits the number of concurrent instances that
//! the Amazon SQS event source can invoke" (`smithy_query(query="shape",
//! shape="ScalingConfig")`) — is a CEILING on how many invocations of the function the mapping
//! has running at ONE instant, and it is a ceiling and nothing more: the mapping goes on
//! handing batches over until the backlog is drained, so every message of a deep queue is
//! invoked exactly once however low the ceiling is. A mapping configuring none has one batch
//! in flight at a time.
//!
//! Each row drives real HTTP requests through one router carrying Lambda and Amazon SQS on ONE
//! engine, and observes the invocations the way an independent client would: the handler
//! reports its own record on a SECOND queue the moment it starts, and then stays in its
//! invocation until this suite releases it through a THIRD queue. So "how many invocations are
//! running at once" is a fact the suite controls rather than one it times — while nothing has
//! been released, every invocation the mapping started is still running, and the records
//! waiting on the side-channel queue are exactly those invocations. Released, the mapping
//! drains the rest of the backlog, and the records say which messages were invoked.
//!
//! The rows differ along what the rule generalises over: the concurrency configured (none, the
//! published minimum of `MaximumConcurrency` — `smithy_query(query="shape",
//! shape="MaximumConcurrency")`: `@range(min: 2, max: 1000)` — and a value inside the range)
//! and how deep the backlog is. The upper end of that range, and what the FUNCTION's own
//! reservation bounds the ceiling to, are [`crate::event_source_mapping_scaling_concurrency`]'s
//! and [`crate::event_source_mapping_concurrency_ceiling`]'s rows, which read the same ceiling
//! off Lambda's own `ConcurrentExecutions` series.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes, and the one window that asserts something did NOT
//! happen is a poll loop over that window.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};

use crate::event_source_mappings::{
    create_queue, create_running_function, given_enabled_mapping, queue_held, router_with_queues,
    send_message, taken_bodies, LOOK_AGAIN,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The mapped function's `Timeout`, in seconds, and the source queue's own
/// `VisibilityTimeout`: the same value, which is the pairing `CreateEventSourceMapping` accepts
/// (a queue must outlast the function it feeds). Long enough that an invocation this suite
/// holds is still its own when the suite releases it.
const TIMEOUT_SECONDS: i64 = 30;

/// How long a row waits for work the emulator owes — a batch polled, a handler run, a record
/// forwarded, the backlog drained.
///
/// A ceiling on PATIENCE, never a span anything counts on, and taken from what the emulator
/// itself allows ONE attempt (the wait for a share of the run capacity and the wait for the
/// run's own answer) rather than picked here: a row fails only when the work did not happen at
/// all.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// How long a row keeps looking for FURTHER invocations while every invocation the mapping
/// started is still running, before it accepts that the mapping started none.
///
/// Generous on purpose: it is far longer than the hand-over of a batch takes on any host — the
/// invocations the row waited for have already reported by the time this window opens — so an
/// invocation past the ceiling would be seen inside it.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The environment entries the handler below reads its two queues from.
const STARTED_QUEUE_URL: &str = "STARTED_QUEUE_URL";
/// See [`STARTED_QUEUE_URL`].
const RELEASE_QUEUE_URL: &str = "RELEASE_QUEUE_URL";

/// The body a release message carries, which the handler neither reads nor reports.
const RELEASE: &str = "go";

/// The handler every row runs: it reports each record it was handed on the side-channel queue
/// and then stays in its invocation, polling the release queue until this suite sends it a
/// message, which it deletes so no other invocation is released by it.
///
/// Nothing here sleeps for a span: the handler waits on a QUEUE, and the suite decides when
/// that wait ends.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    started = os.environ["STARTED_QUEUE_URL"]
    release = os.environ["RELEASE_QUEUE_URL"]
    for record in event["Records"]:
        client.send_message(QueueUrl=started, MessageBody=record["body"])
    while True:
        answered = client.receive_message(
            QueueUrl=release, MaxNumberOfMessages=1, WaitTimeSeconds=1
        )
        held = answered.get("Messages") or []
        if held:
            client.delete_message(
                QueueUrl=release, ReceiptHandle=held[0]["ReceiptHandle"]
            )
            return {"records": len(event["Records"])}
"#;

/// One row: what the mapping configures, how deep the backlog is, and how many invocations of
/// the function may then be running at one instant.
struct Row {
    case: &'static str,
    /// The prefix every resource of the row is named from.
    name: &'static str,
    /// `ScalingConfig$MaximumConcurrency`, or `None` for a mapping that configures none.
    maximum_concurrency: Option<i64>,
    /// How many messages are waiting before anything polls the queue.
    backlog: u32,
    /// The ceiling that follows: the concurrency configured, or the one batch a mapping
    /// configuring none has in flight.
    running: usize,
}

/// The catalog: a mapping configuring no concurrency, one at the published minimum of
/// `MaximumConcurrency`, and one inside the published range — each over a backlog deeper than
/// its own ceiling, so every row needs more than one hand-over to drain.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no configured concurrency",
            name: "esm-drain-none",
            maximum_concurrency: None,
            backlog: 3,
            running: 1,
        },
        Row {
            case: "the published minimum concurrency",
            name: "esm-drain-minimum",
            maximum_concurrency: Some(2),
            backlog: 4,
            running: 2,
        },
        Row {
            case: "a concurrency inside the published range",
            name: "esm-drain-inside",
            maximum_concurrency: Some(3),
            backlog: 6,
            running: 3,
        },
    ]
}

/// RULE: a mapping has at most its `ScalingConfig$MaximumConcurrency` invocations of its
/// function running at one instant, and still drains the whole backlog — every message invoked
/// exactly once.
#[tokio::test]
async fn an_sqs_mapping_runs_at_most_its_configured_concurrency_and_drains_the_backlog() {
    // This emulator serves at most `state::execution::MAX_CONCURRENT_RUNS` invocations at once
    // for the whole process, and the rows below keep several of those slots held while they
    // observe them: one stream of invocations, as every suite of this target that runs
    // handlers for these rules takes.
    let _stream = one_invocation_stream_at_a_time().await;

    for row in rows() {
        let case = row.case;
        let router = router_with_queues();
        let (started_url, _) =
            create_queue(&router, case, &format!("{}-started", row.name), &json!({})).await;
        let (release_url, _) =
            create_queue(&router, case, &format!("{}-release", row.name), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("{}-source", row.name),
            &json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        let function = format!("{}-fn", row.name);
        create_running_function(
            &router,
            case,
            &function,
            HANDLER_SOURCE,
            TIMEOUT_SECONDS,
            &json!({ STARTED_QUEUE_URL: started_url, RELEASE_QUEUE_URL: release_url }),
        )
        .await;

        // The whole backlog is waiting BEFORE anything polls it, which is what makes how many
        // invocations the mapping runs its own decision rather than a question of how fast the
        // messages arrived.
        let mut expected: Vec<String> = (0..row.backlog)
            .map(|index| format!("record-{index}"))
            .collect();
        for body in &expected {
            send_message(&router, case, &source_url, body, None).await;
        }
        let mut request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
            "Enabled": true,
        });
        if let Some(concurrency) = row.maximum_concurrency {
            request["ScalingConfig"] = json!({ "MaximumConcurrency": concurrency });
        }
        given_enabled_mapping(&router, case, &request).await;

        // Every invocation the mapping starts reports itself and then stays in its run, so the
        // records that have arrived are the invocations that are RUNNING.
        let mut invoked: Vec<String> = Vec::new();
        let deadline = Instant::now() + BUDGET;
        loop {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            if invoked.len() >= row.running {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {} invocation(s) of the backlog of {} to be running, \
                 got {invoked:?}",
                row.running,
                row.backlog
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        let quiet_until = Instant::now() + QUIET_WINDOW;
        while Instant::now() < quiet_until {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            assert!(
                invoked.len() <= row.running,
                "case {case:?}: at most {} invocation(s) may run at once while every one of \
                 them is still running, got {} ({invoked:?})",
                row.running,
                invoked.len()
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            invoked.len(),
            row.running,
            "case {case:?}: expected exactly {} invocation(s) to be running, got {invoked:?}",
            row.running
        );

        // Released, the mapping drains what is left: one release message per invocation the
        // backlog needs, and more than that so no invocation waits on a message another one is
        // holding.
        for _ in 0..row.backlog.saturating_mul(2) {
            send_message(&router, case, &release_url, RELEASE, None).await;
        }
        let deadline = Instant::now() + BUDGET;
        loop {
            invoked.extend(taken_bodies(&router, case, &started_url).await);
            let held = queue_held(&router, case, &source_url).await;
            if held == 0 && invoked.len() == row.backlog as usize {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the backlog of {} message(s) to be drained by one \
                 invocation each, got {held} still held and {invoked:?}",
                row.backlog
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        invoked.sort();
        expected.sort();
        assert_eq!(
            invoked, expected,
            "case {case:?}: expected every message of the backlog to be invoked exactly once, \
             got {invoked:?}"
        );
    }
}
