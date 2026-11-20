// SPDX-License-Identifier: CC0-1.0

//! The `AWS/SQS` series a queue publishes about itself, read where a client reads them: the
//! WHOLE emulator over one engine exactly as the binary assembles it, so the store the queues
//! publish into is the one CloudWatch's `GetMetricStatistics` and `ListMetrics` answer from.
//!
//! RULE: every queue that exists publishes the `AWS/SQS` namespace dimensioned on `QueueName`
//! alone. Each message the queue holds is counted in exactly ONE of the depth gauges —
//! `ApproximateNumberOfMessagesVisible`, `ApproximateNumberOfMessagesNotVisible`,
//! `ApproximateNumberOfMessagesDelayed` — by where it stands when the queue is read, so a
//! delivery moves it into the in-flight gauge, a returned delivery moves it back, a delete
//! takes it out of all three, and a message still inside its delay is counted as delayed and
//! never as visible. Beside the gauges the queue counts what its data plane really did:
//! `NumberOfMessagesSent` per message a send added (with `SentMessageSize` measuring those
//! messages in bytes), `NumberOfMessagesReceived` per message a poll handed over,
//! `NumberOfEmptyReceives` per poll that got none, and `NumberOfMessagesDeleted` per message a
//! consumer deleted; a message a redrive MOVED into a dead-letter queue is no send to that
//! queue, so its sent counter keeps reporting zero while the message shows up in its depth. A
//! counter of events reports an explicit zero for a period that carried none of them, and only
//! a queue that does not exist has no series at all. The namespace is the service's own:
//! `PutMetricData` into it is refused, and the genuine series keeps carrying only the real
//! traffic.
//!
//! Only a cross-service suite can ask this: the datapoints are published into CloudWatch's own
//! metric store and read back over CloudWatch's wire, while the traffic is driven over Amazon
//! SQS's.
//!
//! The depth table's rows differ along what the rule generalises over: how many messages the
//! queue holds, how many of them are in flight, whether a delivery was returned or deleted,
//! whether the message is inside a delay, and a queue driven with nothing at all. The counter
//! table's rows differ along which data-plane calls were made and how many messages one call
//! carried.
//!
//! Nothing here waits a fixed span. A reading is taken once a minute, so where a test needs the
//! NEXT one it makes it due through the engine seam — the recorded sample moves a minute into
//! the past, exactly as the wall clock would — instead of waiting for the minute to turn; and
//! where it waits for the clock to publish, it polls for that CONDITION until a generous
//! deadline.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
// The namespace, dimension, metric names and units under test, read from the publishing
// service's own rule so this suite and the publisher cannot disagree about which series is
// meant.
use service_com_amazonaws_sqs_amazonsqs::state::queue_depth_metrics::{
    APPROXIMATE_NUMBER_OF_MESSAGES_DELAYED, APPROXIMATE_NUMBER_OF_MESSAGES_NOT_VISIBLE,
    APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, BYTES_UNIT, COUNT_UNIT, DEPTH_PUBLISH_INTERVAL,
    NAMESPACE, NUMBER_OF_EMPTY_RECEIVES, NUMBER_OF_MESSAGES_DELETED, NUMBER_OF_MESSAGES_RECEIVED,
    NUMBER_OF_MESSAGES_SENT, QUEUE_NAME_DIMENSION, SENT_MESSAGE_SIZE,
};
use service_com_amazonaws_sqs_amazonsqs::state::ComAmazonawsSqsAmazonsqsState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch's `X-Amz-Target` prefix and the signing name that routes a call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The three depth gauges, in the order a row states its expected peaks.
const SERIES: [&str; 3] = [
    APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE,
    APPROXIMATE_NUMBER_OF_MESSAGES_NOT_VISIBLE,
    APPROXIMATE_NUMBER_OF_MESSAGES_DELAYED,
];

/// The counters of events, in the order a counter row states its expected totals.
const EVENT_COUNTERS: [&str; 4] = [
    NUMBER_OF_MESSAGES_SENT,
    NUMBER_OF_MESSAGES_RECEIVED,
    NUMBER_OF_EMPTY_RECEIVES,
    NUMBER_OF_MESSAGES_DELETED,
];

/// The period a read folds its window into: one minute, the resolution these series are
/// published at.
const PERIOD_SECS: i64 = 60;

/// How many seconds either side of the calls a read spans: wide enough that every observation
/// a test makes falls inside it whichever minute the clock is in.
const WINDOW_SECS: f64 = 300.0;

/// How long a test waits for a datapoint the CLOCK owes, and how often it looks again. Far
/// longer than the one tick the clock needs, so a loaded machine does not fail a case the
/// clock would have served.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine
/// handed back so a test can make the next minute's reading due through its seam.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. Nothing here is a credential: this router runs with request authentication
/// off, and the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch call (`awsJson1_0`), as its status and parsed body.
async fn cloudwatch_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "1.0",
        operation,
        body,
    )
    .await
}

/// The same, requiring a 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = cloudwatch_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name` with `attributes`: its url.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    attributes: &Value,
) -> String {
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({"QueueName": name, "Attributes": attributes}),
    )
    .await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string()
}

/// The ARN the queue at `url` reports for itself.
async fn queue_arn(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string()
}

/// Take the next minute's reading of every queue the store holds, and answer once it is in the
/// metric store.
///
/// The gauges are read once a minute, so a test driving several depths inside one minute would
/// otherwise see only the first of them. The next reading is made DUE through the engine seam
/// and then one SQS call takes it, which is what every request of that service does on its way
/// out — so no test waits for a minute to turn.
async fn take_reading(router: &MultiServiceRouter, engine: &engine::InMemoryEngine, case: &str) {
    reopen_sample_slot(engine);
    sqs(router, case, "ListQueues", &json!({})).await;
}

/// Move the last reading recorded for every queue a minute into the past, so the next look at
/// the store takes a fresh one.
fn reopen_sample_slot(engine: &engine::InMemoryEngine) {
    let store = engine.state::<ComAmazonawsSqsAmazonsqsState>();
    let mut store = store.lock();
    for sampled in store.last_sampled_at.values_mut() {
        *sampled = sampled
            .checked_sub(DEPTH_PUBLISH_INTERVAL)
            .unwrap_or(*sampled);
    }
}

/// Wall-clock now as the epoch seconds a metric window is measured in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Every datapoint one `AWS/SQS` series of `queue` holds over the window around the calls, read
/// through `GetMetricStatistics` exactly as a client reads it.
async fn datapoints(router: &MultiServiceRouter, metric: &str, queue: &str) -> Vec<Value> {
    let now = now_secs();
    let answered = cloudwatch(
        router,
        metric,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Minimum", "Maximum", "Sum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The largest value `metric` ever reported for `queue`, or `None` for a series holding no
/// datapoint at all.
///
/// The peak is what a client watching a backlog reads, and what makes exclusivity assertable: a
/// gauge whose peak is zero never counted the message at any instant of the window.
async fn peak(router: &MultiServiceRouter, metric: &str, queue: &str) -> Option<f64> {
    fold(router, metric, queue, "Maximum", f64::max).await
}

/// The smallest value it ever reported: what tells a count that FELL back from one that never
/// moved.
async fn trough(router: &MultiServiceRouter, metric: &str, queue: &str) -> Option<f64> {
    fold(router, metric, queue, "Minimum", f64::min).await
}

/// What `metric` totals for `queue` across the window, or `None` when the series holds no
/// datapoint — which is what a measurement nothing contributed to reports.
async fn total(router: &MultiServiceRouter, metric: &str, queue: &str) -> Option<f64> {
    let points = datapoints(router, metric, queue).await;
    if points.is_empty() {
        return None;
    }
    Some(
        points
            .iter()
            .filter_map(|point| point.get("Sum").and_then(Value::as_f64))
            .sum(),
    )
}

/// One statistic of every datapoint a series holds, folded together.
async fn fold(
    router: &MultiServiceRouter,
    metric: &str,
    queue: &str,
    statistic: &str,
    combine: fn(f64, f64) -> f64,
) -> Option<f64> {
    let points = datapoints(router, metric, queue).await;
    let values: Vec<f64> = points
        .iter()
        .filter_map(|point| point.get(statistic).and_then(Value::as_f64))
        .collect();
    values
        .split_first()
        .map(|(first, rest)| rest.iter().copied().fold(*first, combine))
}

/// The peak of each depth gauge for `queue`, in [`SERIES`] order.
async fn depth_peaks(router: &MultiServiceRouter, queue: &str) -> [Option<f64>; 3] {
    let mut reported = [None; 3];
    for (index, metric) in SERIES.iter().enumerate() {
        reported[index] = peak(router, metric, queue).await;
    }
    reported
}

/// The total of each counter of events for `queue`, in [`EVENT_COUNTERS`] order.
async fn counter_totals(router: &MultiServiceRouter, queue: &str) -> [Option<f64>; 4] {
    let mut reported = [None; 4];
    for (index, metric) in EVENT_COUNTERS.iter().enumerate() {
        reported[index] = total(router, metric, queue).await;
    }
    reported
}

/// The `AWS/SQS` metric names `ListMetrics` indexes for `queue`, sorted: the series a client
/// discovering that queue finds.
async fn indexed_series(router: &MultiServiceRouter, queue: &str) -> Vec<String> {
    let listed = cloudwatch(
        router,
        queue,
        "ListMetrics",
        &json!({
            "Namespace": NAMESPACE,
            "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
        }),
    )
    .await;
    let mut names: Vec<String> = listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|metric| metric.get("MetricName").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect();
    names.sort();
    names
}

/// Send one message of `body` to the queue at `url`, with `delay_seconds` when it carries one.
async fn send(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
    delay_seconds: Option<i32>,
) {
    let mut request = json!({"QueueUrl": url, "MessageBody": body});
    if let Some(delay) = delay_seconds {
        request["DelaySeconds"] = json!(delay);
    }
    sqs(router, case, "SendMessage", &request).await;
}

/// Poll the queue at `url` for up to `max` messages, answering every receipt handle it handed
/// over.
async fn receive(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    max: i32,
) -> Vec<String> {
    let handed = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": max, "WaitTimeSeconds": 0}),
    )
    .await;
    handed["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message.get("ReceiptHandle").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// One row of the depth table: the queue it drives, what it drives on it, and the
/// `(visible, not visible, delayed)` PEAK every gauge must then report.
struct DepthRow {
    case: &'static str,
    queue: &'static str,
    /// How many messages one send each adds, and the delay each send carries.
    sent: Vec<(&'static str, Option<i32>)>,
    /// How many messages a poll takes, when the row polls at all.
    received: Option<i32>,
    /// What the row then does with the delivery it took.
    then: Then,
    expected: (f64, f64, f64),
}

/// What a row does with the delivery it took.
#[derive(PartialEq, Eq)]
enum Then {
    /// Nothing: the delivery stays in flight.
    Hold,
    /// Returns it to the queue at once (`ChangeMessageVisibility` to no timeout).
    Return,
    /// Deletes it.
    Delete,
}

/// The depth table. Every row drives its queue to ONE end state and is read once, so the peak
/// each gauge reports is the depth that state stands at.
fn depth_rows() -> Vec<DepthRow> {
    vec![
        DepthRow {
            case: "a queue driven with nothing reports zero in all three",
            queue: "depth-idle",
            sent: Vec::new(),
            received: None,
            then: Then::Hold,
            expected: (0.0, 0.0, 0.0),
        },
        DepthRow {
            case: "one send leaves the message visible",
            queue: "depth-one-sent",
            sent: vec![("one", None)],
            received: None,
            then: Then::Hold,
            expected: (1.0, 0.0, 0.0),
        },
        DepthRow {
            case: "three sends leave all three visible",
            queue: "depth-three-sent",
            sent: vec![("one", None), ("two", None), ("three", None)],
            received: None,
            then: Then::Hold,
            expected: (3.0, 0.0, 0.0),
        },
        DepthRow {
            case: "a delivery holds the message in flight",
            queue: "depth-in-flight",
            sent: vec![("one", None)],
            received: Some(1),
            then: Then::Hold,
            expected: (0.0, 1.0, 0.0),
        },
        DepthRow {
            case: "one of three sent is in flight",
            queue: "depth-partly-in-flight",
            sent: vec![("one", None), ("two", None), ("three", None)],
            received: Some(1),
            then: Then::Hold,
            expected: (2.0, 1.0, 0.0),
        },
        DepthRow {
            case: "a returned delivery is visible again",
            queue: "depth-returned",
            sent: vec![("one", None)],
            received: Some(1),
            then: Then::Return,
            expected: (1.0, 0.0, 0.0),
        },
        DepthRow {
            case: "a deleted message is in none of the three",
            queue: "depth-deleted",
            sent: vec![("one", None)],
            received: Some(1),
            then: Then::Delete,
            expected: (0.0, 0.0, 0.0),
        },
        DepthRow {
            case: "a message inside its delay is counted as delayed alone",
            queue: "depth-delayed",
            sent: vec![("one", Some(900))],
            received: None,
            then: Then::Hold,
            expected: (0.0, 0.0, 1.0),
        },
        DepthRow {
            case: "a delayed send beside a plain one is counted in its own gauge",
            queue: "depth-delayed-and-visible",
            sent: vec![("one", None), ("two", Some(60))],
            received: None,
            then: Then::Hold,
            expected: (1.0, 0.0, 1.0),
        },
    ]
}

/// RULE: every message a queue holds is counted in exactly ONE of the three `AWS/SQS` depth
/// gauges dimensioned on its `QueueName`, by where that message stands when the queue is read —
/// so a delivery, a return, a delete and a delay each move the count into the one gauge that
/// state belongs to, and a queue holding nothing reports zero in all three.
#[tokio::test]
async fn every_message_a_queue_holds_is_counted_in_exactly_one_depth_gauge() {
    let (router, engine) = emulator();
    for row in depth_rows() {
        let case = row.case;
        let url = given_queue(&router, case, row.queue, &json!({})).await;
        for (body, delay) in &row.sent {
            send(&router, case, &url, body, *delay).await;
        }
        let mut handles = Vec::new();
        if let Some(max) = row.received {
            handles = receive(&router, case, &url, max).await;
            assert_eq!(
                handles.len(),
                max as usize,
                "case {case:?}: expected the poll to hand over {max} message(s), got {}",
                handles.len()
            );
        }
        match row.then {
            Then::Hold => {}
            Then::Return => {
                for handle in &handles {
                    sqs(
                        &router,
                        case,
                        "ChangeMessageVisibility",
                        &json!({
                            "QueueUrl": url,
                            "ReceiptHandle": handle,
                            "VisibilityTimeout": 0,
                        }),
                    )
                    .await;
                }
            }
            Then::Delete => {
                for handle in &handles {
                    sqs(
                        &router,
                        case,
                        "DeleteMessage",
                        &json!({"QueueUrl": url, "ReceiptHandle": handle}),
                    )
                    .await;
                }
            }
        }

        take_reading(&router, &engine, case).await;
        let (visible, not_visible, delayed) = row.expected;
        let expected = [Some(visible), Some(not_visible), Some(delayed)];
        let reported = depth_peaks(&router, row.queue).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {SERIES:?} to peak at {expected:?}, got {reported:?}"
        );
        for metric in SERIES {
            let points = datapoints(&router, metric, row.queue).await;
            assert!(
                points
                    .iter()
                    .all(|point| point.get("Unit").and_then(Value::as_str) == Some(COUNT_UNIT)),
                "case {case:?}: every datapoint of {metric} is a {COUNT_UNIT}, got {points:?}"
            );
        }
    }
}

/// RULE: a delivery moves the count out of the visible gauge and into the in-flight one, a
/// return moves it back, and a delete takes it out of both — each transition reported by the
/// reading taken after it, so a reader watching the two gauges sees the message move.
#[tokio::test]
async fn a_delivery_a_return_and_a_delete_move_the_count_between_the_gauges() {
    let (router, engine) = emulator();
    let case = "the in-flight, return and delete legs";
    let queue = "lifecycle";
    let url = given_queue(&router, case, queue, &json!({})).await;
    send(&router, case, &url, "one", None).await;
    take_reading(&router, &engine, case).await;
    assert_eq!(
        depth_peaks(&router, queue).await,
        [Some(1.0), Some(0.0), Some(0.0)],
        "case {case:?}: a sent message is visible and nothing else"
    );

    let handles = receive(&router, case, &url, 1).await;
    assert_eq!(
        handles.len(),
        1,
        "case {case:?}: expected one delivery, got {handles:?}"
    );
    take_reading(&router, &engine, case).await;
    assert_eq!(
        peak(&router, APPROXIMATE_NUMBER_OF_MESSAGES_NOT_VISIBLE, queue).await,
        Some(1.0),
        "case {case:?}: the delivery must have been reported in flight"
    );
    assert_eq!(
        trough(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, queue).await,
        Some(0.0),
        "case {case:?}: the visible gauge must have fallen to 0 while the message was in flight"
    );

    sqs(
        &router,
        case,
        "ChangeMessageVisibility",
        &json!({"QueueUrl": url, "ReceiptHandle": handles[0], "VisibilityTimeout": 0}),
    )
    .await;
    take_reading(&router, &engine, case).await;
    let returned = sqs(
        &router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["All"]}),
    )
    .await;
    assert_eq!(
        returned["Attributes"]["ApproximateNumberOfMessagesNotVisible"].as_str(),
        Some("0"),
        "case {case:?}: the returned delivery is no longer in flight, got {returned}"
    );
    assert_eq!(
        trough(
            &router,
            APPROXIMATE_NUMBER_OF_MESSAGES_NOT_VISIBLE,
            queue
        )
        .await,
        Some(0.0),
        "case {case:?}: the in-flight gauge must have fallen back after the return"
    );

    let handles = receive(&router, case, &url, 1).await;
    assert_eq!(
        handles.len(),
        1,
        "case {case:?}: a returned message must be deliverable again, got {handles:?}"
    );
    sqs(
        &router,
        case,
        "DeleteMessage",
        &json!({"QueueUrl": url, "ReceiptHandle": handles[0]}),
    )
    .await;
    take_reading(&router, &engine, case).await;
    let troughs = [
        trough(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, queue).await,
        trough(&router, APPROXIMATE_NUMBER_OF_MESSAGES_NOT_VISIBLE, queue).await,
        trough(&router, APPROXIMATE_NUMBER_OF_MESSAGES_DELAYED, queue).await,
    ];
    assert_eq!(
        troughs,
        [Some(0.0), Some(0.0), Some(0.0)],
        "case {case:?}: a deleted message is counted in no gauge, got {troughs:?}"
    );
}

/// What one row of the counter table drives on its queue.
struct CounterRow {
    case: &'static str,
    queue: &'static str,
    /// The bodies the row sends, one message each.
    sent: Vec<&'static str>,
    /// How many messages each poll asks for, one entry per poll.
    polls: Vec<i32>,
    /// Whether the row deletes what its polls handed over.
    deletes: bool,
    /// The `Sum` each counter of events must then report, in [`EVENT_COUNTERS`] order.
    expected: [f64; 4],
}

/// The counter table. Every row leaves at least one counter with nothing counted in it, so each
/// of the four is asserted both where it counted traffic and where it counted none.
fn counter_rows() -> Vec<CounterRow> {
    vec![
        CounterRow {
            case: "a queue nothing was driven on",
            queue: "counters-idle",
            sent: Vec::new(),
            polls: Vec::new(),
            deletes: false,
            expected: [0.0, 0.0, 0.0, 0.0],
        },
        CounterRow {
            case: "two sends and nothing else",
            queue: "counters-sent",
            sent: vec!["alpha", "beta"],
            polls: Vec::new(),
            deletes: false,
            expected: [2.0, 0.0, 0.0, 0.0],
        },
        CounterRow {
            case: "one send, taken and deleted",
            queue: "counters-drained",
            sent: vec!["alpha"],
            polls: vec![1],
            deletes: true,
            expected: [1.0, 1.0, 0.0, 1.0],
        },
        CounterRow {
            case: "three sends taken by one poll count three receives, not one",
            queue: "counters-batch-poll",
            sent: vec!["alpha", "beta", "gamma"],
            polls: vec![10],
            deletes: false,
            expected: [3.0, 3.0, 0.0, 0.0],
        },
        CounterRow {
            case: "two polls that got nothing",
            queue: "counters-empty-polls",
            sent: Vec::new(),
            polls: vec![1, 1],
            deletes: false,
            expected: [0.0, 0.0, 2.0, 0.0],
        },
    ]
}

/// RULE: a queue's counters count the data-plane calls it really served — one observation per
/// message a send added, per message a poll handed over, per poll that got none and per message
/// a consumer deleted — and a counter of events that carried none of them for a period reports
/// an explicit zero, so only a queue that does not exist has no series at all.
#[tokio::test]
async fn the_counters_report_the_data_plane_calls_each_queue_really_served() {
    let (router, engine) = emulator();
    for row in counter_rows() {
        let case = row.case;
        let url = given_queue(&router, case, row.queue, &json!({})).await;
        for body in &row.sent {
            send(&router, case, &url, body, None).await;
        }
        for max in &row.polls {
            let handles = receive(&router, case, &url, *max).await;
            if row.deletes {
                for handle in &handles {
                    sqs(
                        &router,
                        case,
                        "DeleteMessage",
                        &json!({"QueueUrl": url, "ReceiptHandle": handle}),
                    )
                    .await;
                }
            }
        }
        take_reading(&router, &engine, case).await;

        let expected = row.expected.map(Some);
        let reported = counter_totals(&router, row.queue).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {EVENT_COUNTERS:?} to total {expected:?}, got {reported:?}"
        );

        // The size of the messages a send added is measured only where a send added one: a
        // period no send added a message to has no message to measure.
        let sent_bytes: f64 = row.sent.iter().map(|body| body.len() as f64).sum();
        let measured = total(&router, SENT_MESSAGE_SIZE, row.queue).await;
        let expected_size = (!row.sent.is_empty()).then_some(sent_bytes);
        assert_eq!(
            measured, expected_size,
            "case {case:?}: expected {SENT_MESSAGE_SIZE} to total {expected_size:?}, got \
             {measured:?}"
        );
        for point in datapoints(&router, SENT_MESSAGE_SIZE, row.queue).await {
            assert_eq!(
                point.get("Unit").and_then(Value::as_str),
                Some(BYTES_UNIT),
                "case {case:?}: {SENT_MESSAGE_SIZE} is measured in {BYTES_UNIT}, got {point}"
            );
        }
        for metric in EVENT_COUNTERS {
            let points = datapoints(&router, metric, row.queue).await;
            assert!(
                !points.is_empty()
                    && points
                        .iter()
                        .all(|point| point.get("Unit").and_then(Value::as_str) == Some(COUNT_UNIT)),
                "case {case:?}: {metric} of a queue that exists carries datapoints in \
                 {COUNT_UNIT}, got {points:?}"
            );
        }
        let indexed = indexed_series(&router, row.queue).await;
        for metric in EVENT_COUNTERS.iter().chain(SERIES.iter()) {
            assert!(
                indexed.contains(&(*metric).to_string()),
                "case {case:?}: ListMetrics must index {metric} for the queue, got {indexed:?}"
            );
        }
    }

    // A queue that was never created is sampled by nothing, so it has no series at all — which
    // is what tells it apart from the idle queue above, whose counters report zero.
    let absent = "counters-never-created";
    for metric in EVENT_COUNTERS.iter().chain(SERIES.iter()) {
        let points = datapoints(&router, metric, absent).await;
        assert!(
            points.is_empty(),
            "case a queue that does not exist: {metric} must hold no datapoint, got {points:?}"
        );
    }
    let indexed = indexed_series(&router, absent).await;
    assert!(
        indexed.is_empty(),
        "case a queue that does not exist: it must have no series at all, got {indexed:?}"
    );
}

/// RULE: a message a redrive MOVED into a dead-letter queue is not a message sent to that
/// queue, so that queue's sent counter keeps reporting the zero it owes and the message shows
/// up in its depth instead — while a later send straight to it IS counted.
#[tokio::test]
async fn a_redrive_lands_in_the_dead_letter_queues_depth_and_is_no_send_to_it() {
    let (router, engine) = emulator();
    let case = "a redrive into a dead-letter queue";
    let dead_letter = "redrive-dead-letter";
    let source = "redrive-source";
    let dead_letter_url = given_queue(&router, case, dead_letter, &json!({})).await;
    let dead_letter_arn = queue_arn(&router, case, &dead_letter_url).await;
    let source_url = given_queue(
        &router,
        case,
        source,
        &json!({
            // One delivery is all this queue allows, and a delivery it makes is eligible again
            // at once — so the next poll is the one that finds the message over its allowance.
            "VisibilityTimeout": "0",
            "RedrivePolicy": json!({
                "deadLetterTargetArn": dead_letter_arn,
                "maxReceiveCount": 1,
            })
            .to_string(),
        }),
    )
    .await;

    send(&router, case, &source_url, "doomed", None).await;
    let handed = receive(&router, case, &source_url, 1).await;
    assert_eq!(
        handed.len(),
        1,
        "case {case:?}: expected the one allowed delivery, got {handed:?}"
    );
    // The poll past the allowance is the one that moves the message; it hands nothing over.
    let after = receive(&router, case, &source_url, 1).await;
    assert!(
        after.is_empty(),
        "case {case:?}: a message over its allowance is moved, not handed over again, got \
         {after:?}"
    );
    take_reading(&router, &engine, case).await;

    assert_eq!(
        peak(
            &router,
            APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE,
            dead_letter
        )
        .await,
        Some(1.0),
        "case {case:?}: the moved message must show in the dead-letter queue's depth"
    );
    assert_eq!(
        total(&router, NUMBER_OF_MESSAGES_SENT, dead_letter).await,
        Some(0.0),
        "case {case:?}: a redrive is no send, so the dead-letter queue's sent counter must \
         report the zero it owes"
    );
    assert_eq!(
        trough(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, source).await,
        Some(0.0),
        "case {case:?}: the source queue no longer holds the message"
    );

    // A send straight to the dead-letter queue IS counted, which is what makes the zero above
    // a statement about the redrive rather than about an empty series.
    send(&router, case, &dead_letter_url, "sent-directly", None).await;
    take_reading(&router, &engine, case).await;
    assert_eq!(
        total(&router, NUMBER_OF_MESSAGES_SENT, dead_letter).await,
        Some(1.0),
        "case {case:?}: a send straight to the dead-letter queue is counted"
    );
}

/// RULE: `AWS/SQS` is the service's own namespace — a `PutMetricData` into it is refused with
/// HTTP 400 `InvalidParameterValue`, a publish into a customer namespace is served, and the
/// genuine series keeps carrying only the traffic the queue really served.
#[tokio::test]
async fn a_publish_into_the_service_owned_namespace_is_refused_and_changes_no_genuine_series() {
    let (router, engine) = emulator();
    let case = "a publish into the service-owned namespace";
    let queue = "spoofed";
    let url = given_queue(&router, case, queue, &json!({})).await;
    send(&router, case, &url, "one", None).await;
    send(&router, case, &url, "two", None).await;
    take_reading(&router, &engine, case).await;
    let genuine = peak(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, queue).await;
    assert_eq!(
        genuine,
        Some(2.0),
        "case {case:?}: the genuine series must report the two messages sent"
    );

    let spoof = json!([{
        "MetricName": APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE,
        "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
        "Value": 9_999.0,
        "Unit": COUNT_UNIT,
    }]);
    let (status, refused) = cloudwatch_call(
        &router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": spoof}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a publish into {NAMESPACE} must be refused with HTTP 400, got {status} \
         with {refused}"
    );
    let refusal = refused.to_string();
    assert!(
        refusal.contains("InvalidParameterValue"),
        "case {case:?}: the refusal must name InvalidParameterValue, got {refusal}"
    );

    // The positive control: the same datapoint into a namespace the customer owns is served,
    // so the refusal above is about the NAMESPACE and not about the write path.
    let own_namespace = "Spoof/Control";
    cloudwatch(
        &router,
        case,
        "PutMetricData",
        &json!({"Namespace": own_namespace, "MetricData": spoof}),
    )
    .await;
    let now = now_secs();
    let control = cloudwatch(
        &router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": own_namespace,
            "MetricName": APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE,
            "Dimensions": [{"Name": QUEUE_NAME_DIMENSION, "Value": queue}],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Maximum"],
        }),
    )
    .await;
    let control_peak = control["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point.get("Maximum").and_then(Value::as_f64))
        .fold(f64::MIN, f64::max);
    assert_eq!(
        control_peak, 9_999.0,
        "case {case:?}: the customer's own namespace carries what was published, got {control}"
    );

    // And the genuine series still carries only the queue's real depth: the refused value did
    // not land, which is what a customer has to be able to see.
    take_reading(&router, &engine, case).await;
    let after = peak(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, queue).await;
    assert_eq!(
        after, genuine,
        "case {case:?}: the refused value must not reach the genuine series, which reported \
         {genuine:?} and now reports {after:?}"
    );
}

/// RULE: a queue that exists is read once a minute whether or not a client calls Amazon SQS, so
/// a client that only polls CloudWatch still sees its depth.
#[tokio::test]
async fn the_clock_takes_the_next_reading_with_no_further_sqs_call() {
    let (router, engine) = emulator();
    let case = "a reading nothing asked for";
    let queue = "clock-driven";
    let url = given_queue(&router, case, queue, &json!({})).await;
    send(&router, case, &url, "one", None).await;
    // The reading that carries what the send left behind is the next minute's. It is made DUE
    // here — as the turn of a minute does — and from this point on only CloudWatch is called,
    // so what the polling below observes is a reading the CLOCK took.
    reopen_sample_slot(&engine);

    let deadline = Instant::now() + DEADLINE;
    loop {
        if peak(&router, APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE, queue).await == Some(1.0) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {APPROXIMATE_NUMBER_OF_MESSAGES_VISIBLE} to report the one \
             message with no further SQS call, got {:?}",
            depth_peaks(&router, queue).await
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
