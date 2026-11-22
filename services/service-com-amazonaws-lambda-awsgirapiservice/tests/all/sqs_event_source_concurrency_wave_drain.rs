// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an Amazon SQS event source mapping's
//! `ScalingConfig$MaximumConcurrency` bounds when the HANDLER itself spends a span of its own
//! per invocation — observed through the instants the handler reports, which is what a client
//! sizing that setting for a drain rate measures.
//!
//! RULE: `ScalingConfig$MaximumConcurrency` — "limits the number of concurrent instances that
//! the Amazon SQS event source can invoke" (`smithy_query(query="shape", shape="ScalingConfig")`)
//! — bounds how many invocations of the mapping's function are RUNNING at one instant and
//! nothing else: the mapping goes on handing batches over, in waves of at most that many, until
//! the whole backlog is drained, so a backlog of any depth is consumed by a handler that takes
//! real time per record, and the peak number of runs that overlap is that ceiling — never above
//! it, and not below it while a backlog is still waiting. A mapping configuring no concurrency
//! is bounded by the depth of its backlog instead
//! ([`crate::event_source_mapping_backlog_scaling`]).
//!
//! The sibling suites pin the same ceiling through other observations and are not copied here:
//! [`crate::event_source_mapping_scaling_concurrency`] and
//! [`crate::event_source_mapping_concurrency_ceiling`] read it off Lambda's own
//! `ConcurrentExecutions` series over passes the test makes itself, and
//! [`crate::sqs_event_source_maximum_concurrency_drain`] holds every invocation open on a
//! side-channel queue so that "how many are running" is a fact the suite controls. What this
//! suite adds is the shape a client really deploys: a handler whose run is a WAIT OF ITS OWN
//! that nothing releases, reporting the instants it started and finished each record, so both
//! the overlap of those runs and the completion of the drain are read from the invocations
//! themselves. It shares that suite's spans and budgets rather than restating them.
//!
//! The rows differ along what the rule generalises over: the concurrency configured (the
//! published minimum of `MaximumConcurrency`, a value inside its range, and none at all), how
//! deep the backlog is, and how many records one batch holds.
//!
//! Nothing here waits a span of its own, asserts on elapsed time or bounds a wait by a short
//! timeout: every wait looks again until what it waits for has arrived or a generous deadline
//! passes, and the only span anything counts on is the one the HANDLER asks for inside its own
//! run.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::event_source_mappings::{
    create_queue, create_running_function, forwarded_records, given_enabled_mapping, queue_held,
    router_with_queues, send_message, LOOK_AGAIN, RESULTS_QUEUE_URL,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::sqs_event_source_maximum_concurrency_drain::{BUDGET, TIMEOUT_SECONDS};

/// The environment entry the handler below reads the span it spends per record from.
const HELD_SECONDS_VARIABLE: &str = "HELD_SECONDS";

/// How long the handler spends on ONE record, in the seconds `time.sleep` takes.
///
/// Long enough that two invocations a mapping started in the same wave are each still inside
/// their own run while the other is — which is what makes an overlap observable at all — and
/// short enough that a row's whole drain is a handful of waves. It is the HANDLER's own span,
/// spent inside its invocation, and no wait this suite makes.
const HELD_SECONDS: f64 = 3.0;

/// The least overlap a row accepts as evidence that the ceiling is one the mapping really USES
/// rather than one it stays under by running its batches one after another: two runs of the
/// function inside their own spans at once.
///
/// The same floor as the gauge suites' `OVERLAP`, stated as the count of WINDOWS this suite
/// measures rather than as a reading of a metric.
const OVERLAPPED: usize = 2;

/// The handler every row runs: for each record it was handed it notes the instant it starts,
/// spends [`HELD_SECONDS`] in its own run, notes the instant it finishes, and reports the
/// record's body with both instants on the results queue its environment names.
///
/// The report goes out AFTER the span, so a record on the results queue is one whose run is
/// over: what a reader of that queue holds is the window each run really occupied.
const HANDLER_SOURCE: &str = r#"
import json
import os
import time

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = os.environ["RESULTS_QUEUE_URL"]
    held = float(os.environ["HELD_SECONDS"])
    for record in event["Records"]:
        start = time.time()
        time.sleep(held)
        end = time.time()
        client.send_message(
            QueueUrl=url,
            MessageBody=json.dumps(
                {"body": record["body"], "start": start, "end": end}
            ),
        )
    return {"records": len(event["Records"])}
"#;

/// One row: what the mapping configures, how deep the backlog is, how many records one batch
/// may hold, and the greatest number of runs that may then overlap.
struct Row {
    case: &'static str,
    /// The prefix every resource of the row is named from.
    name: &'static str,
    /// `ScalingConfig$MaximumConcurrency`, or `None` for a mapping that configures none.
    maximum_concurrency: Option<i64>,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
    /// How many messages are waiting before anything polls the queue.
    backlog: usize,
    /// The ceiling on overlapping runs that follows: the concurrency configured, or the batches
    /// the backlog itself holds for a mapping configuring none — and never more than the
    /// batches that backlog holds either way, since a wave cannot be deeper than its queue.
    overlapping: usize,
}

impl Row {
    /// How long this row waits for the work the emulator owes it: the budget its sibling suite
    /// allows one attempt, plus the spans the HANDLER spends draining this backlog — one per
    /// record, which is more than the waves of them really take.
    ///
    /// A ceiling on patience, never a span anything counts on.
    fn budget(&self) -> Duration {
        let records = u32::try_from(self.backlog).unwrap_or(u32::MAX);
        BUDGET.saturating_add(Duration::from_secs_f64(HELD_SECONDS).saturating_mul(records))
    }
}

/// The catalog: the published minimum of `MaximumConcurrency`
/// (`smithy_query(query="shape", shape="MaximumConcurrency")`: `@range(min: 2, max: 1000)`) over
/// a backlog that needs several waves, a value inside that range over a deeper backlog, the same
/// minimum over batches that hold more than one record, and a mapping configuring no concurrency
/// at all, whose ceiling is its backlog.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the published minimum concurrency over a backlog of four",
            name: "esm-wave-minimum",
            maximum_concurrency: Some(2),
            batch_size: 1,
            backlog: 4,
            overlapping: 2,
        },
        Row {
            case: "a concurrency inside the published range over a backlog of six",
            name: "esm-wave-inside",
            maximum_concurrency: Some(3),
            batch_size: 1,
            backlog: 6,
            overlapping: 3,
        },
        Row {
            case: "the published minimum concurrency over batches of two",
            name: "esm-wave-batched",
            maximum_concurrency: Some(2),
            batch_size: 2,
            backlog: 4,
            overlapping: 2,
        },
        Row {
            case: "no configured concurrency over a backlog of two",
            name: "esm-wave-none",
            maximum_concurrency: None,
            batch_size: 1,
            backlog: 2,
            overlapping: 2,
        },
    ]
}

/// The greatest number of `(start, end)` windows that hold one instant together.
///
/// Such a peak can only be reached at the START of a window — nothing else adds to the count —
/// so the starts are the instants to measure at.
fn peak_overlap(windows: &[(f64, f64)]) -> usize {
    windows
        .iter()
        .map(|(instant, _)| {
            windows
                .iter()
                .filter(|(from, until)| from <= instant && instant <= until)
                .count()
        })
        .max()
        .unwrap_or_default()
}

/// The `(start, end)` window of every record a run of this suite's handler reported, read off
/// the two members it reports them under.
fn windows(case: &str, records: &[&Value]) -> Vec<(f64, f64)> {
    records
        .iter()
        .map(|record| match (record["start"].as_f64(), record["end"].as_f64()) {
            (Some(start), Some(end)) => (start, end),
            _ => panic!("case {case:?}: a reported record carrying no window: {record}"),
        })
        .collect()
}

/// RULE: a mapping whose handler spends a span of its own per record drains its whole backlog in
/// waves of at most its configured concurrency — every message handed to a run of the function,
/// the queue left empty, and the peak overlap of those runs the ceiling the mapping asked for.
#[tokio::test]
async fn a_capped_mapping_drains_its_backlog_in_waves_of_its_configured_concurrency() {
    // This emulator serves at most `state::execution::MAX_CONCURRENT_RUNS` invocations at once
    // for the whole process, and the rows below keep several of those slots busy: one stream of
    // invocations, as every suite of this target that runs handlers for these rules takes.
    let _stream = one_invocation_stream_at_a_time().await;

    for row in rows() {
        let case = row.case;
        let router = router_with_queues();
        let (results_url, _) =
            create_queue(&router, case, &format!("{}-results", row.name), &json!({})).await;
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
            &json!({
                RESULTS_QUEUE_URL: results_url,
                HELD_SECONDS_VARIABLE: HELD_SECONDS.to_string(),
            }),
        )
        .await;

        // The whole backlog is waiting BEFORE anything polls it, which is what makes how many
        // invocations the mapping runs at once its own decision rather than a question of how
        // fast the messages arrived.
        let expected: Vec<String> = (0..row.backlog)
            .map(|index| format!("record-{index}"))
            .collect();
        for body in &expected {
            send_message(&router, case, &source_url, body, None).await;
        }
        let mut request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": row.batch_size,
            "Enabled": true,
        });
        if let Some(concurrency) = row.maximum_concurrency {
            request["ScalingConfig"] = json!({ "MaximumConcurrency": concurrency });
        }
        given_enabled_mapping(&router, case, &request).await;

        // The drain: every record of the backlog, reported by the run that handled it. The wait
        // is for those reports and never for a span — a slower host takes longer to answer it
        // instead of failing it.
        let reported = forwarded_records(&router, &results_url, row.backlog, row.budget()).await;
        let bodies: Vec<String> = reported.keys().cloned().collect();
        assert_eq!(
            bodies, expected,
            "case {case:?}: every message of the backlog of {} must be handed to a run of the \
             function, got {bodies:?}",
            row.backlog
        );

        // And the source queue is left empty: the records were processed and deleted, not merely
        // taken. A message leaves the queue once the run holding it has answered, so this looks
        // again rather than reading once.
        let deadline = Instant::now() + row.budget();
        let mut held = queue_held(&router, case, &source_url).await;
        while held > 0 {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: a drained backlog must leave the queue empty, got {held} \
                 message(s) still held"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
            held = queue_held(&router, case, &source_url).await;
        }

        // The ceiling, read off the runs themselves: the most runs that were ever inside their
        // own spans together.
        let records: Vec<&Value> = reported.values().collect();
        let spans = windows(case, &records);
        let peak = peak_overlap(&spans);
        assert!(
            peak <= row.overlapping,
            "case {case:?}: at most {} run(s) of the function may overlap, got {peak} over the \
             windows {spans:?}",
            row.overlapping
        );
        // And it is a ceiling the mapping really USES: a mapping that consumed its backlog one
        // batch after another would have left no two runs overlapping at all.
        let reached = row.overlapping.min(OVERLAPPED);
        assert!(
            peak >= reached,
            "case {case:?}: expected at least {reached} run(s) of the function to overlap, got \
             {peak} over the windows {spans:?}"
        );
    }
}
