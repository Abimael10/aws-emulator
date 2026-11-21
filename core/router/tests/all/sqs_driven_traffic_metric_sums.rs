// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/SQS` counters of a DRIVEN queue total, read through CloudWatch's
//! `GetMetricData`.
//!
//! RULE: a queue publishes one observation into its own `AWS/SQS` series — dimensioned on
//! `QueueName` and on nothing else — per thing its data plane really did: per message a send
//! added (`NumberOfMessagesSent`, with `SentMessageSize` measuring that message's body in the
//! bytes it takes), per message a poll handed over (`NumberOfMessagesReceived`, so a poll that
//! handed over k messages counts k rather than 1), per poll that handed over none
//! (`NumberOfEmptyReceives`) and per message a consumer deleted (`NumberOfMessagesDeleted`). So
//! the `Sum` of each series over a window holding the whole run equals exactly the traffic that
//! run carried, the counters are published in `Count` and the size in `Bytes` — a read naming
//! another unit answers nothing, because CloudWatch converts no units — and a queue driven with
//! nothing reports none of another queue's traffic under its own name.
//!
//! The suite beside this one ([`crate::sqs_queue_metric_series`]) asserts the same series as
//! `GetMetricStatistics` answers them, and this one reads them through the OTHER reader a client
//! has, over the traffic a queue was driven with; its harness — the whole emulator on one
//! engine, and the SQS and CloudWatch calls — is that suite's, so there is one of each here.
//!
//! The table's rows differ along what the rule generalises over: how many messages were sent,
//! how they were sent (one call each, or one batch call carrying several), how large the bodies
//! are and whether they are single- or multi-byte UTF-8, how many polls there were and how many
//! messages each asked for, how many of those polls got nothing, whether what a poll handed over
//! was deleted, and a queue driven with no traffic at all. The recorded case — seven sends
//! drained by one poll and three polls that got nothing — is ONE row.
//!
//! Nothing here waits a fixed span: where a test waits for the series to settle it polls for that
//! CONDITION until a generous deadline.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
// The namespace, dimension, metric names and units under test, read from the publishing
// service's own rule so this suite and the publisher cannot disagree about which series is
// meant.
use service_com_amazonaws_sqs_amazonsqs::state::queue_depth_metrics::{
    BYTES_UNIT, COUNT_UNIT, NAMESPACE, NUMBER_OF_EMPTY_RECEIVES, NUMBER_OF_MESSAGES_DELETED,
    NUMBER_OF_MESSAGES_RECEIVED, NUMBER_OF_MESSAGES_SENT, QUEUE_NAME_DIMENSION, SENT_MESSAGE_SIZE,
};
use smithy_server_runtime::MultiServiceRouter;

// The ONE harness this test crate keeps for the `AWS/SQS` series: the whole emulator on one
// engine, the SQS and CloudWatch calls, and the counters a queue publishes.
use crate::sqs_queue_metric_series::{
    cloudwatch, emulator, given_queue, now_secs, receive, send, sqs, EVENT_COUNTERS, PERIOD_SECS,
    WINDOW_SECS,
};

/// How long the settling test waits for the series to hold the driven totals, and how often it
/// looks again. Far longer than the publish needs — it happens as part of serving the call — so
/// a loaded machine does not fail a reading that was on its way.
const DEADLINE: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Send `bodies` to the queue at `url` as ONE `SendMessageBatch` call, requiring every entry to
/// have been accepted: a batch that dropped an entry sent fewer messages than the row drove.
async fn send_batch(router: &MultiServiceRouter, case: &str, url: &str, bodies: &[&str]) {
    let entries: Vec<Value> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| json!({"Id": format!("entry-{index}"), "MessageBody": body}))
        .collect();
    let answered = sqs(
        router,
        case,
        "SendMessageBatch",
        &json!({"QueueUrl": url, "Entries": entries}),
    )
    .await;
    let accepted = answered["Successful"]
        .as_array()
        .map_or(0, |successful| successful.len());
    assert_eq!(
        accepted,
        bodies.len(),
        "case {case:?}: expected the batch's {} entries to be accepted, got {answered}",
        bodies.len()
    );
}

/// What `GetMetricData` totals for one `AWS/SQS` series of `queue` across the window around the
/// calls, and `None` for a series holding no datapoint in it at all.
///
/// `unit` is the caller's unit filter: a read naming one takes only the datapoints published in
/// that unit, which is how this suite asks what unit a series carries.
async fn total(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    queue: &str,
    unit: Option<&str>,
) -> Option<f64> {
    let now = now_secs();
    let mut metric_stat = json!({
        "Metric": {
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
        },
        "Period": PERIOD_SECS,
        "Stat": "Sum",
    });
    if let Some(unit) = unit {
        metric_stat["Unit"] = json!(unit);
    }
    let answered = cloudwatch(
        router,
        case,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{"Id": "driven", "MetricStat": metric_stat}],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
        }),
    )
    .await;
    let values: Vec<f64> = answered["MetricDataResults"]
        .as_array()
        .and_then(|results| results.first())
        .and_then(|result| result.get("Values"))
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum())
}

/// The total of each counter of events for `queue`, in [`EVENT_COUNTERS`] order, as
/// `GetMetricData` answers it.
async fn counter_totals(
    router: &MultiServiceRouter,
    case: &str,
    queue: &str,
) -> [Option<f64>; 4] {
    let mut reported = [None; 4];
    for (index, metric) in EVENT_COUNTERS.iter().enumerate() {
        reported[index] = total(router, case, metric, queue, None).await;
    }
    reported
}

/// The `(metric name, dimension set)` of every `AWS/SQS` series `ListMetrics` indexes for
/// `queue`, each dimension set as its `(Name, Value)` pairs: what a reader selecting that
/// queue's series has to select it by.
async fn indexed_dimension_sets(
    router: &MultiServiceRouter,
    case: &str,
    queue: &str,
) -> Vec<(String, Vec<(String, String)>)> {
    let listed = cloudwatch(
        router,
        case,
        "ListMetrics",
        &json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
        }),
    )
    .await;
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            let name = metric["MetricName"].as_str().unwrap_or_default().to_string();
            let dimensions = metric["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            (name, dimensions)
        })
        .collect()
}

/// One row of the table: the queue it drives, and the traffic it drives on it.
struct Row {
    case: &'static str,
    queue: &'static str,
    /// The bodies the row sends, one `SendMessage` call each.
    sends: &'static [&'static str],
    /// The bodies the row sends as ONE `SendMessageBatch` call.
    batch: &'static [&'static str],
    /// The `MaxNumberOfMessages` of each poll the row makes, in order.
    polls: &'static [i32],
    /// Whether the row deletes what its polls handed over.
    deletes: bool,
    /// How many messages those polls must hand over in all: `MaxNumberOfMessages` is "the
    /// maximum number of messages to return", so a poll hands over every message the queue can
    /// deliver up to it — and the counter counts each of those messages, not each poll.
    received: f64,
    /// How many of those polls must hand over nothing at all.
    empty: f64,
    /// How many messages the row then deletes.
    deleted: f64,
}

/// The table. Every row differs from the others in how much was sent, how it was sent, how large
/// and what shape its bodies are, how its polls were answered and what it did with them.
const ROWS: &[Row] = &[
    Row {
        // The recorded shape: a queue driven with several sends, drained by one poll, then
        // polled again with nothing left to hand over.
        case: "seven sends drained by one poll, then three polls that got nothing",
        queue: "driven-sums-seven",
        sends: &[
            "order-0001",
            "order-0002",
            "order-0003-with-a-longer-body",
            "o",
            "order-0005",
            "order-0006",
            "order-0007",
        ],
        batch: &[],
        polls: &[10, 10, 10, 10],
        deletes: false,
        received: 7.0,
        empty: 3.0,
        deleted: 0.0,
    },
    Row {
        case: "one send taken by one poll and deleted",
        queue: "driven-sums-single",
        sends: &["one"],
        batch: &[],
        polls: &[1],
        deletes: true,
        received: 1.0,
        empty: 0.0,
        deleted: 1.0,
    },
    Row {
        // A body is measured in the bytes it takes, not the characters it spells: each of these
        // spells fewer characters than it takes bytes in UTF-8.
        case: "multi-byte bodies are measured in their UTF-8 bytes",
        queue: "driven-sums-utf8",
        sends: &["héllo", "日本語", "🚀🚀🚀"],
        batch: &[],
        polls: &[],
        deletes: false,
        received: 0.0,
        empty: 0.0,
        deleted: 0.0,
    },
    Row {
        // The first poll asks for fewer than are there and takes that many; the second takes
        // the rest. Four messages over two polls, which the counter reports as four.
        case: "one batch call carrying four messages, drained by two polls",
        queue: "driven-sums-batch",
        sends: &[],
        batch: &["alpha", "beta", "gamma", "delta"],
        polls: &[2, 10],
        deletes: false,
        received: 4.0,
        empty: 0.0,
        deleted: 0.0,
    },
    Row {
        case: "sends of both kinds, and a poll asking for fewer than are there",
        queue: "driven-sums-mixed",
        sends: &["single-send"],
        batch: &["batched-one", "batched-two"],
        polls: &[2],
        deletes: true,
        received: 2.0,
        empty: 0.0,
        deleted: 2.0,
    },
    Row {
        case: "polls that got nothing and no send at all",
        queue: "driven-sums-empty-polls",
        sends: &[],
        batch: &[],
        polls: &[1, 5, 10],
        deletes: false,
        received: 0.0,
        empty: 3.0,
        deleted: 0.0,
    },
    Row {
        case: "a queue driven with no traffic at all",
        queue: "driven-sums-idle",
        sends: &[],
        batch: &[],
        polls: &[],
        deletes: false,
        received: 0.0,
        empty: 0.0,
        deleted: 0.0,
    },
];

/// RULE: each `AWS/SQS` counter of a queue sums exactly the traffic its data plane served — one
/// per message a send added, one per message a poll handed over, one per poll that handed over
/// none, one per message deleted, and the bodies' exact byte total — under `QueueName` and no
/// other dimension, while a queue created beside it and driven with nothing reports none of that
/// traffic under its own name.
#[tokio::test]
async fn the_counters_of_a_driven_queue_sum_exactly_its_traffic() {
    // The byte totals asserted below only tell a BYTE measurement from a count of characters
    // where a row's bodies take more bytes than they spell characters, so the table has to carry
    // one that does.
    assert!(
        ROWS.iter().any(|row| row
            .sends
            .iter()
            .chain(row.batch)
            .any(|body| body.chars().count() < body.len())),
        "the table must drive a row whose bodies are multi-byte UTF-8"
    );
    let (router, _engine) = emulator();
    for row in ROWS {
        let case = row.case;
        let url = given_queue(&router, case, row.queue, &json!({})).await;
        // The silent twin, created in the same breath as the driven queue: its series are its
        // own, so none of the traffic below may show up under its name.
        let silent = format!("{}-silent", row.queue);
        given_queue(&router, case, &silent, &json!({})).await;

        for body in row.sends {
            send(&router, case, &url, body, None).await;
        }
        if !row.batch.is_empty() {
            send_batch(&router, case, &url, row.batch).await;
        }
        let mut handed_over = 0.0;
        let mut empty_polls = 0.0;
        let mut deleted = 0.0;
        for max in row.polls {
            let handles = receive(&router, case, &url, *max).await;
            if handles.is_empty() {
                empty_polls += 1.0;
            } else {
                handed_over += handles.len() as f64;
            }
            if row.deletes {
                for handle in &handles {
                    sqs(
                        &router,
                        case,
                        "DeleteMessage",
                        &json!({"QueueUrl": url, "ReceiptHandle": handle}),
                    )
                    .await;
                    deleted += 1.0;
                }
            }
        }

        // What the row drove is what the row DECLARED it drives: a poll hands over every
        // message the queue can deliver up to the maximum it asked for, so the traffic the
        // counters are judged against is the table's, not the handler's own account of it.
        let driven = [handed_over, empty_polls, deleted];
        let declared = [row.received, row.empty, row.deleted];
        assert_eq!(
            driven, declared,
            "case {case:?}: expected the polls to hand over {} messages in all, to find nothing \
             {} times and to leave {} deleted, got {driven:?}",
            row.received, row.empty, row.deleted
        );

        let sent = (row.sends.len() + row.batch.len()) as f64;
        let body_bytes: f64 = row
            .sends
            .iter()
            .chain(row.batch)
            .map(|body| body.len() as f64)
            .sum();
        let expected = [
            Some(sent),
            Some(row.received),
            Some(row.empty),
            Some(row.deleted),
        ];
        let reported = counter_totals(&router, case, row.queue).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {EVENT_COUNTERS:?} of {:?} to total {expected:?} for the \
             traffic driven on it, got {reported:?}",
            row.queue
        );

        // The size of the messages a send added is measured only where a send added one: a
        // period no send added a message to has no message to measure rather than one of no
        // size.
        let expected_size = (sent > 0.0).then_some(body_bytes);
        let measured = total(&router, case, SENT_MESSAGE_SIZE, row.queue, None).await;
        assert_eq!(
            measured, expected_size,
            "case {case:?}: expected {SENT_MESSAGE_SIZE} to total the bodies' \
             {expected_size:?} bytes, got {measured:?}"
        );

        // The silent twin: it served none of those events, so each of its counters reports the
        // explicit zero a counter of events owes for a period that carried none — never the
        // driven queue's totals — and it has no message to measure at all.
        let quiet = counter_totals(&router, case, &silent).await;
        assert_eq!(
            quiet,
            [Some(0.0); 4],
            "case {case:?}: the silent queue {silent:?} served none of this traffic, so its \
             {EVENT_COUNTERS:?} must report its own zeroes rather than {expected:?}, got {quiet:?}"
        );
        let quiet_size = total(&router, case, SENT_MESSAGE_SIZE, &silent, None).await;
        assert_eq!(
            quiet_size, None,
            "case {case:?}: nothing was sent to {silent:?}, so it has no message to measure, \
             got {quiet_size:?}"
        );

        // The dimension set of every series the driven queue publishes is exactly the queue's
        // name, which is what a reader selects a queue's series by.
        let indexed = indexed_dimension_sets(&router, case, row.queue).await;
        let expected_dimensions = vec![(QUEUE_NAME_DIMENSION.to_string(), row.queue.to_string())];
        for (metric, dimensions) in &indexed {
            assert_eq!(
                dimensions, &expected_dimensions,
                "case {case:?}: {metric} must be dimensioned on {QUEUE_NAME_DIMENSION} alone, \
                 got {dimensions:?}"
            );
        }
        let names: Vec<&str> = indexed.iter().map(|(name, _)| name.as_str()).collect();
        for metric in EVENT_COUNTERS {
            assert!(
                names.contains(&metric),
                "case {case:?}: ListMetrics must index {metric} for {:?}, got {names:?}",
                row.queue
            );
        }
    }
}

/// RULE: the counters are published in `Count` and the message size in `Bytes`, and CloudWatch
/// performs no unit conversion — so a read naming the series' own unit answers its total and a
/// read naming another unit answers nothing at all.
#[tokio::test]
async fn the_counters_carry_count_and_the_message_size_carries_bytes() {
    let (router, _engine) = emulator();
    let case = "two sends, one draining poll, one delete and one empty poll";
    let queue = "driven-units";
    let url = given_queue(&router, case, queue, &json!({})).await;
    let bodies = ["first-body", "second-body-which-is-longer"];
    for body in bodies {
        send(&router, case, &url, body, None).await;
    }
    let handles = receive(&router, case, &url, 10).await;
    assert_eq!(
        handles.len(),
        bodies.len(),
        "case {case:?}: a poll asking for more than are there hands over both messages, got \
         {handles:?}"
    );
    let handle = handles
        .first()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the poll to hand a message over, got {handles:?}")
        })
        .clone();
    sqs(
        &router,
        case,
        "DeleteMessage",
        &json!({"QueueUrl": url, "ReceiptHandle": handle}),
    )
    .await;
    let empty = receive(&router, case, &url, 10).await;
    assert!(
        empty.is_empty(),
        "case {case:?}: the second poll must find nothing left, got {empty:?}"
    );
    let body_bytes: f64 = bodies.iter().map(|body| body.len() as f64).sum();

    let expectations = [
        (NUMBER_OF_MESSAGES_SENT, COUNT_UNIT, bodies.len() as f64),
        (
            NUMBER_OF_MESSAGES_RECEIVED,
            COUNT_UNIT,
            bodies.len() as f64,
        ),
        (NUMBER_OF_EMPTY_RECEIVES, COUNT_UNIT, 1.0),
        (NUMBER_OF_MESSAGES_DELETED, COUNT_UNIT, 1.0),
        (SENT_MESSAGE_SIZE, BYTES_UNIT, body_bytes),
    ];
    for (metric, unit, expected) in expectations {
        let in_unit = total(&router, case, metric, queue, Some(unit)).await;
        assert_eq!(
            in_unit,
            Some(expected),
            "case {case:?}: {metric} is published in {unit}, so a read in {unit} must total \
             {expected}, got {in_unit:?}"
        );
        let other = if unit == COUNT_UNIT {
            BYTES_UNIT
        } else {
            COUNT_UNIT
        };
        let in_other = total(&router, case, metric, queue, Some(other)).await;
        assert_eq!(
            in_other, None,
            "case {case:?}: {metric} carries no datapoint in {other}, and CloudWatch converts \
             no units, got {in_other:?}"
        );
    }
}

/// RULE: a counter sums the traffic that happened, so once the traffic stops the series settles
/// on that total and every later read answers it again — reading the series changes nothing.
#[tokio::test]
async fn the_driven_totals_settle_and_every_later_read_answers_the_same() {
    let (router, _engine) = emulator();
    let case = "a queue driven, drained and then only read";
    let queue = "driven-settles";
    let url = given_queue(&router, case, queue, &json!({})).await;
    let bodies = ["alpha", "beta", "gamma"];
    for body in bodies {
        send(&router, case, &url, body, None).await;
    }
    let handed = receive(&router, case, &url, 10).await;
    assert_eq!(
        handed.len(),
        bodies.len(),
        "case {case:?}: one poll asking for more than are there drains the queue, got {handed:?}"
    );
    let mut empty_polls = 0.0;
    for _ in 0..2 {
        let polled = receive(&router, case, &url, 10).await;
        assert!(
            polled.is_empty(),
            "case {case:?}: a poll after the drain must hand nothing over, got {polled:?}"
        );
        empty_polls += 1.0;
    }
    let expected = [
        Some(bodies.len() as f64),
        Some(bodies.len() as f64),
        Some(empty_polls),
        // Nothing was deleted, and the queue exists, so that counter owes its zero.
        Some(0.0),
    ];

    // Wait for the CONDITION — the series holding the driven totals — rather than for a span.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = counter_totals(&router, case, queue).await;
        if reported == expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {EVENT_COUNTERS:?} to come to total {expected:?}, last \
             reading was {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And it STAYS there: nothing but CloudWatch is called from here on, so a later read can
    // only answer the same traffic.
    for look in 1..=3 {
        let reported = counter_totals(&router, case, queue).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: read {look} after the traffic stopped must answer {expected:?} \
             again, got {reported:?}"
        );
    }
}
