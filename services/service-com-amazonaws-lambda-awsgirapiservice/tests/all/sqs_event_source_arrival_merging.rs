// SPDX-License-Identifier: CC0-1.0

//! WHICH records of an Amazon SQS queue an event source mapping merges into ONE invocation of
//! its function.
//!
//! RULE: a read of a queue answers with the records that are available when it is made — "if a
//! message is available, the call returns sooner than `WaitTimeSeconds`"
//! (`ReceiveMessageRequest$WaitTimeSeconds`) — so one invocation carries the records that were
//! available TOGETHER, and the only span over which records that became available at different
//! instants are merged into one batch is "the maximum amount of time, in seconds, that Lambda
//! spends gathering records before invoking the function"
//! (`CreateEventSourceMapping$MaximumBatchingWindowInSeconds`). A mapping that spends no time
//! gathering — "for Kinesis, DynamoDB, and Amazon SQS event sources, the default batching window
//! is 0 seconds" — therefore invokes its function once per arrival however much its `BatchSize`
//! would admit, while a mapping stating a window gathers the arrivals of that span into one
//! batch; `BatchSize`, "the maximum number of records in each batch that Lambda pulls from your
//! stream or queue", bounds what one read takes whatever the arrivals were. A read OPENS when
//! the mapping's reads of its source begin and answers as soon as a record is available, so the
//! records already waiting then — a backlog — are what one read is answered with, while a record
//! that becomes available later ends the wait itself and the ones after it belong to the next
//! read.
//!
//! The first test asserts the decision itself over a table of availability instants
//! (`crate::state::records_of_one_read`), the counterpart of the decision about a batch that has
//! been gathered (`crate::state::batch_is_due`, pinned by
//! `crate::event_source_mapping_batching_window`). The second drives the whole chain: records are
//! sent through Amazon SQS's own surface, each with a `DelaySeconds` of its own so the arrivals
//! are spaced by the QUEUE's clock rather than by anything this suite waits for, the mapping's
//! poll passes are this suite's own (`DrivenPolling`) and made at instants it names, and what
//! each invocation carried is read back off the records the HANDLER forwarded — so only a
//! delivery that really happened is counted.
//!
//! The rows differ along what the rule generalises over: the window (none at all, the published
//! minimum of its `@range(min: 0, max: 300)`, a span above the arrival spacing, and the published
//! maximum), how far apart the records become available (together, inside the span one read can
//! tell apart, and seconds apart), whether they were already waiting when the reads opened or
//! arrived while they were open, how many records there are, and whether `BatchSize` bounds the
//! read before the arrivals do.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{records_of_one_read, POLL_INTERVAL};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    call_sqs, create_queue, create_running_function, driven_router_with_queues,
    given_enabled_mapping, LOOK_AGAIN, RESULTS_QUEUE_URL,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::sqs_event_source_maximum_concurrency_drain::{BUDGET, TIMEOUT_SECONDS};

/// The published bounds of `MaximumBatchingWindowInSeconds`
/// (`smithy_query(query="shape", shape="MaximumBatchingWindowInSeconds")`: `@range(min: 0, max:
/// 300)`).
const WINDOW_MIN: i64 = 0;
/// See [`WINDOW_MIN`].
const WINDOW_MAX: i64 = 300;

/// The handler every row runs: for the batch it was handed it forwards ONE report to the queue
/// its environment names, saying how many records that batch held and which bodies they were.
///
/// One report per INVOCATION, which is what makes batch composition observable: a reader of that
/// queue sees exactly how the arrivals were split into invocations. The client is built at module
/// scope, which is where a managed runtime's handler builds one.
const REPORTING_HANDLER: &str = r#"
import json
import os

import boto3

QUEUES = boto3.client("sqs")
REPORTS = os.environ["RESULTS_QUEUE_URL"]


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    QUEUES.send_message(
        QueueUrl=REPORTS,
        MessageBody=json.dumps(
            {
                "records": len(records),
                "bodies": [record["body"] for record in records],
            },
            sort_keys=True,
        ),
    )
    return {"records": len(records)}
"#;

/// One row of the decision table: the instants, in milliseconds from the row's own base, at
/// which the records a read would take became available — in the order the queue hands them
/// over — when the mapping's reads of that queue began, the window the mapping gathers over,
/// and how many of them ONE read takes.
struct ReadRow {
    case: &'static str,
    available_after_millis: Vec<i64>,
    /// When the reads opened, in milliseconds from the row's base; a backlog is what was
    /// already available then.
    reading_since_millis: i64,
    window_seconds: i64,
    takes: usize,
}

/// This table's base instant: an arbitrary point of the clock, since every row states its
/// records' availability relative to it.
fn base_instant() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// RULE: one read of a queue takes the records available together inside the span the mapping's
/// `MaximumBatchingWindowInSeconds` admits — counted from the later of the instant its reads
/// opened and the first record of the read, and never less than the span this emulator's reads
/// can tell apart — so a backlog already waiting when the reads opened goes in one batch and a
/// later arrival is the next read's.
#[test]
fn one_read_takes_the_records_that_were_available_together() {
    // How far apart two arrivals have to be for two reads to be able to fall between them: the
    // interval this emulator's poller reads on, which is the granularity of its reads.
    let granularity = i64::try_from(POLL_INTERVAL.as_millis()).unwrap_or(i64::MAX);
    let rows = [
        ReadRow {
            case: "no records waiting",
            available_after_millis: vec![],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 0,
        },
        ReadRow {
            case: "one record, no window",
            available_after_millis: vec![0],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 1,
        },
        ReadRow {
            case: "three records available at one instant, no window",
            available_after_millis: vec![0, 0, 0],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 3,
        },
        ReadRow {
            case: "arrivals inside the read granularity, no window",
            available_after_millis: vec![0, granularity / 2, granularity],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 3,
        },
        ReadRow {
            case: "the arrival past the read granularity is the next read's",
            available_after_millis: vec![0, granularity, granularity + 1],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 2,
        },
        ReadRow {
            case: "arrivals seconds apart, no window",
            available_after_millis: vec![0, 2_000, 4_000],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 1,
        },
        ReadRow {
            case: "a backlog that was waiting before the reads opened, no window",
            available_after_millis: vec![0, 2_000, 4_000],
            reading_since_millis: 10_000,
            window_seconds: WINDOW_MIN,
            takes: 3,
        },
        ReadRow {
            case: "a backlog, and an arrival past the span after the reads opened",
            available_after_millis: vec![0, 2_000, 12_000],
            reading_since_millis: 10_000,
            window_seconds: WINDOW_MIN,
            takes: 2,
        },
        ReadRow {
            case: "a window that admits the next arrival but not the one after it",
            available_after_millis: vec![0, 2_000, 4_000],
            reading_since_millis: 0,
            window_seconds: 3,
            takes: 2,
        },
        ReadRow {
            case: "a window above the whole arrival spacing",
            available_after_millis: vec![0, 2_000, 4_000],
            reading_since_millis: 0,
            window_seconds: 10,
            takes: 3,
        },
        ReadRow {
            case: "the published maximum window over arrivals a hundred seconds apart",
            available_after_millis: vec![0, 100_000, 200_000],
            reading_since_millis: 0,
            window_seconds: WINDOW_MAX,
            takes: 3,
        },
        ReadRow {
            case: "one span past the published maximum window",
            available_after_millis: vec![0, 200_000, 400_000],
            reading_since_millis: 0,
            window_seconds: WINDOW_MAX,
            takes: 2,
        },
        ReadRow {
            case: "a record available before the first of the read's order",
            available_after_millis: vec![5_000, 0, 60_000],
            reading_since_millis: 0,
            window_seconds: WINDOW_MIN,
            takes: 2,
        },
    ];
    for row in &rows {
        let ReadRow {
            case,
            available_after_millis,
            reading_since_millis,
            window_seconds,
            takes,
        } = row;
        let instants: Vec<SystemTime> = available_after_millis
            .iter()
            .map(|after| base_instant() + Duration::from_millis(after.unsigned_abs()))
            .collect();
        let answered = records_of_one_read(
            &instants,
            base_instant() + Duration::from_millis(reading_since_millis.unsigned_abs()),
            Duration::from_secs(window_seconds.unsigned_abs()),
        );
        assert_eq!(
            answered, *takes,
            "case {case:?}: a read opened at {reading_since_millis} ms of records available at \
             {available_after_millis:?} ms under a window of {window_seconds}s must take \
             {takes}, it took {answered}"
        );
    }
}

/// One row of the delivery table: what the mapping configures, when each record becomes
/// available, when the passes are made and how the records must then be split into invocations.
struct DeliveryRow {
    case: &'static str,
    /// The prefix every resource of the row is named from.
    name: &'static str,
    /// `MaximumBatchingWindowInSeconds`, or `None` for a mapping that states none.
    window: Option<i64>,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
    /// How many records are sent.
    records: usize,
    /// How much later than the record before it each record becomes available, in seconds — the
    /// `DelaySeconds` each send states, so the spacing is the queue's own.
    spacing_seconds: i64,
    /// How long after the row's sends the FIRST record becomes available, in seconds: the
    /// `DelaySeconds` the first send states, so a row can have its reads open before anything is
    /// available (an arrival) or after it already is (a backlog).
    first_available_after_seconds: i64,
    /// The instants the passes are made at, in seconds past the row's first send.
    passes_after_seconds: &'static [u64],
    /// How many invocations the records must come to, and how many records the largest of them
    /// must carry.
    invocations: usize,
    /// See [`DeliveryRow::invocations`].
    largest_batch: usize,
}

/// The catalog: every row sends three records, and what changes is when they become available,
/// when the mapping's reads open relative to that, and what it gathers over.
fn delivery_rows() -> Vec<DeliveryRow> {
    vec![
        DeliveryRow {
            case: "no window stated, arrivals two seconds apart",
            name: "esm-merge-no-window",
            window: None,
            batch_size: 10,
            records: 3,
            spacing_seconds: 2,
            // Nothing is available when the reads open, so each record is an arrival of its
            // own: the first pass finds an empty queue, the second is past all three.
            first_available_after_seconds: 2,
            passes_after_seconds: &[0, 10],
            invocations: 3,
            largest_batch: 1,
        },
        DeliveryRow {
            case: "the published minimum window, arrivals two seconds apart",
            name: "esm-merge-zero-window",
            window: Some(WINDOW_MIN),
            batch_size: 10,
            records: 3,
            spacing_seconds: 2,
            first_available_after_seconds: 2,
            passes_after_seconds: &[0, 10],
            invocations: 3,
            largest_batch: 1,
        },
        DeliveryRow {
            case: "a window above the arrival spacing gathers them into one batch",
            name: "esm-merge-windowed",
            window: Some(10),
            batch_size: 10,
            records: 3,
            spacing_seconds: 2,
            first_available_after_seconds: 2,
            // The second pass gathers, the third is past the window it gathers over.
            passes_after_seconds: &[0, 10, 21],
            invocations: 1,
            largest_batch: 3,
        },
        DeliveryRow {
            case: "no window stated, arrivals together",
            name: "esm-merge-together",
            window: None,
            batch_size: 10,
            records: 3,
            spacing_seconds: 0,
            first_available_after_seconds: 2,
            passes_after_seconds: &[0, 10],
            invocations: 1,
            largest_batch: 3,
        },
        DeliveryRow {
            case: "a backlog waiting before the reads opened goes in one batch",
            name: "esm-merge-backlog",
            window: None,
            batch_size: 10,
            records: 3,
            spacing_seconds: 2,
            // No pass before the records are available, so the mapping's first read finds all
            // three already waiting: a backlog, which one read takes up to `BatchSize`.
            first_available_after_seconds: 2,
            passes_after_seconds: &[10],
            invocations: 1,
            largest_batch: 3,
        },
        DeliveryRow {
            case: "one record per batch bounds a read of arrivals that came together",
            name: "esm-merge-batch-of-one",
            window: None,
            batch_size: 1,
            records: 3,
            spacing_seconds: 0,
            first_available_after_seconds: 2,
            passes_after_seconds: &[0, 10],
            invocations: 3,
            largest_batch: 1,
        },
    ]
}

/// `SendMessage` of `body` to the queue `url` addresses, available `delay_seconds` after the send
/// — "the length of time, in seconds, for which to delay a specific message"
/// (`SendMessageRequest$DelaySeconds`), which is what spaces a row's arrivals.
async fn send_available_later(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
    delay_seconds: i64,
) {
    let (status, sent) = call_sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body, "DelaySeconds": delay_seconds }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SendMessage to answer 200, got {status} {sent}"
    );
}

/// Every report the handler has forwarded to the queue `url` addresses, read until the reports
/// account for `wanted` records or the budget is out.
///
/// The wait is for the REPORTS and never for a span: each receive hides what it was handed for
/// long enough that a report counted here is not counted twice, and nothing is deleted.
async fn reports(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
    budget: Duration,
) -> Vec<Value> {
    let mut found: BTreeMap<String, Value> = BTreeMap::new();
    let deadline = Instant::now() + budget;
    loop {
        let counted: usize = found
            .values()
            .map(|report| usize::try_from(report["records"].as_u64().unwrap_or_default()).unwrap_or_default())
            .sum();
        if counted >= wanted || Instant::now() >= deadline {
            return found.into_values().collect();
        }
        let (status, answered) = call_sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 300 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ReceiveMessage on the report queue to answer 200, got \
             {status} {answered}"
        );
        for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
            let text = message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a report with no Body: {message}"))
                .to_string();
            let report: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!("case {case:?}: a report that is not JSON: {text} ({error})")
            });
            found.insert(text, report);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a mapping merges records into one invocation only while they became available inside the
/// span it gathers over, so a mapping stating no batching window invokes its function once per
/// arrival and one stating a window above the spacing invokes it once with all of them; `BatchSize`
/// bounds either way.
#[tokio::test]
async fn a_mapping_merges_arrivals_only_inside_the_window_it_gathers_over() {
    // This emulator serves a bounded number of invocations at once for the whole process, and the
    // rows below keep several of those slots busy: one stream of invocations, as every suite of
    // this target that runs handlers takes.
    let _stream = one_invocation_stream_at_a_time().await;

    for row in delivery_rows() {
        let case = row.case;
        let (router, mut passes) = driven_router_with_queues();
        let (reports_url, _) =
            create_queue(&router, case, &format!("{}-reports", row.name), &json!({})).await;
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
            REPORTING_HANDLER,
            TIMEOUT_SECONDS,
            &json!({ RESULTS_QUEUE_URL: reports_url }),
        )
        .await;
        let mut request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": row.batch_size,
            "Enabled": true,
        });
        if let Some(window) = row.window {
            request["MaximumBatchingWindowInSeconds"] = json!(window);
        }
        given_enabled_mapping(&router, case, &request).await;

        // The arrivals: each record becomes available its own span after the send, so the spacing
        // is the queue's own clock and this suite waits for none of it.
        let base = SystemTime::now();
        let bodies: Vec<String> = (0..row.records)
            .map(|index| format!("{}-record-{index}", row.name))
            .collect();
        for (index, body) in bodies.iter().enumerate() {
            let delay = row.first_available_after_seconds
                + row.spacing_seconds * i64::try_from(index).unwrap_or_default();
            send_available_later(&router, case, &source_url, body, delay).await;
        }

        // The passes, each made at the instant the row names rather than whenever a background
        // poller happened to wake.
        for after in row.passes_after_seconds {
            passes
                .poll_once_at(base + Duration::from_secs(*after))
                .await;
        }

        let found = reports(&router, case, &reports_url, row.records, BUDGET).await;
        let counts: Vec<usize> = found
            .iter()
            .map(|report| {
                usize::try_from(report["records"].as_u64().unwrap_or_default()).unwrap_or_default()
            })
            .collect();
        assert_eq!(
            found.len(),
            row.invocations,
            "case {case:?}: expected the {} record(s) to come to {} invocation(s), got {counts:?} \
             over the reports {found:?}",
            row.records,
            row.invocations
        );
        let largest = counts.iter().copied().max().unwrap_or_default();
        assert_eq!(
            largest, row.largest_batch,
            "case {case:?}: expected the largest invocation to carry {} record(s), it carried \
             {largest} over the reports {found:?}",
            row.largest_batch
        );
        // Every record reached a handler, exactly once: a split that dropped or duplicated one
        // would not be the same set.
        let mut delivered: Vec<String> = found
            .iter()
            .flat_map(|report| report["bodies"].as_array().cloned().unwrap_or_default())
            .map(|body| body.as_str().unwrap_or_default().to_string())
            .collect();
        delivered.sort();
        let mut expected = bodies.clone();
        expected.sort();
        assert_eq!(
            delivered, expected,
            "case {case:?}: expected every record to be handed to a handler once, got \
             {delivered:?}"
        );
    }
}
