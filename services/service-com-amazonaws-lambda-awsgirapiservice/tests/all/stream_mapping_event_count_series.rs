// SPDX-License-Identifier: CC0-1.0

//! The per-mapping EVENT-COUNT metrics of an event source mapping on a SHARD source — an
//! Amazon DynamoDB table's stream — end to end over the wire with DynamoDB, DynamoDB Streams
//! and CloudWatch mounted beside Lambda on ONE engine, so the items are written where a
//! client writes them and the series are read back where a client reads them
//! (`GetMetricStatistics`).
//!
//! The RULE asserted over the table below: while an event source mapping's `MetricsConfig`
//! selects `EventCount` ("the metrics you want your event source mapping to produce […]
//! `EventCount` to receive metrics related to the number of events processed by your event
//! source mapping"), every pass of that mapping's poller publishes what it did with the
//! events it took out of the source — how many it polled, how many its `FilterCriteria`
//! rejected and how many an invocation of its function accepted — into the `AWS/Lambda`
//! namespace, each series dimensioned on the mapping's own `UUID` and carrying the unit
//! `Count`. That holds whichever KIND of event source the mapping reads: the counts are
//! quantities of the mapping's own work, and the queue sources' own table is
//! [`crate::event_source_mapping_event_counts`]. A mapping whose `MetricsConfig` does not
//! select `EventCount` publishes none of them, however much its poller is doing.
//!
//! What a shard source does NOT have is a deleted count: a record stays in the shard it was
//! read from for that stream's retention period, so nothing is ever deleted from this source
//! and the series that measures deletions has no datapoint at all rather than a zero.
//!
//! Two invariants hold in every row, whatever the pass came to, and are asserted as such
//! rather than against one run's numbers: no more events are invoked than the filter admitted
//! (`Invoked ≤ Polled − FilteredOut`), and every record the row wrote is eventually accounted
//! for by the pass that read it (`Polled ≥ records written`).
//!
//! The rows differ along what the rule generalises over: how many records are waiting, whether
//! the mapping filters at all, which member of the record its pattern names, how many of the
//! waiting records the filter admits, and the `BatchSize` they are polled in.
//!
//! Every barrier here is a CONDITION looked for again until it holds or the generous ceiling
//! [`crate::dynamodb_stream_event_source_delivery::BUDGET`] passes: nothing asserts on how
//! long a pass took, and each row retires its mapping once it has read the counts, so one
//! wake-up of the poll loop does the work of the row that is running and not also the work of
//! every mapping an earlier row left polling.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::metrics::{
    COUNT_UNIT, DELETED_EVENT_COUNT_METRIC, EVENT_SOURCE_MAPPING_UUID_DIMENSION,
    FILTERED_OUT_EVENT_COUNT_METRIC, INVOKED_EVENT_COUNT_METRIC, METRIC_NAMESPACE,
    POLLED_EVENT_COUNT_METRIC,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    emulator, given_function, given_table, results_until, stop_mapping, write_item, Write, BUDGET,
    LOOK_AGAIN, NEW_AND_OLD_IMAGES, RESULT_OK, TRIM_HORIZON,
};
use crate::event_source_mappings::{create_mapping, get_mapping, STATE_ENABLED};

/// The region every call below signs for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of the CloudWatch service.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The `EventSourceMappingMetric` that turns the event-count family on
/// (`smithy_query(query="shape", shape="EventSourceMappingMetric")`: `EventCount`,
/// `ErrorCount`, `KafkaMetrics`, the last two of which are scoped to the Kafka sources).
const EVENT_COUNT: &str = "EventCount";

/// How far either side of now a series is read, in seconds: a window wide enough to hold
/// every datapoint a row's passes publish, at the one-minute resolution they are stored at.
const WINDOW_SECONDS: f64 = 600.0;

/// The handler every function below runs: it accepts every record it is handed, so what the
/// mapping polled is what an invocation accepted.
const ACCEPTS_EVERY_RECORD: &str = "def lambda_handler(event, context):\n    \
     return {'records': len(event['Records'])}\n";

/// One CloudWatch call, the way a client makes it.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/monitoring/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Now, as epoch seconds: the instant a read's window is centred on.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The datapoints CloudWatch holds for one metric of the mapping `uuid` names, as a client
/// reads them.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    uuid: &str,
) -> Vec<Value> {
    let now = now_secs();
    let (status, read) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{
                "Name": EVENT_SOURCE_MAPPING_UUID_DIMENSION,
                "Value": uuid,
            }],
            "StartTime": now - WINDOW_SECONDS,
            "EndTime": now + WINDOW_SECONDS,
            "Period": 60,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics for {metric} answered {status} {read}"
    );
    read["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// What one metric of the mapping `uuid` names sums to over the whole window.
async fn metric_sum(router: &MultiServiceRouter, case: &str, metric: &str, uuid: &str) -> f64 {
    datapoints(router, case, metric, uuid)
        .await
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// Wait until one metric of the mapping `uuid` names sums to at least `wanted`, answering what
/// it summed to.
///
/// A CONDITION, looked for again until it holds: the pass that publishes it is the poller's
/// own work, and [`BUDGET`] is a backstop against a mapping that polls nothing at all.
async fn wait_for_sum(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    uuid: &str,
    wanted: f64,
) -> f64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let summed = metric_sum(router, case, metric, uuid).await;
        if summed >= wanted {
            return summed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric} of the mapping to reach {wanted}, got {summed}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The unit every datapoint of `metric` carries, as the answer reports it.
async fn units_of(router: &MultiServiceRouter, case: &str, metric: &str, uuid: &str) -> Vec<String> {
    datapoints(router, case, metric, uuid)
        .await
        .iter()
        .map(|point| point["Unit"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Create the mapping `request` asks for and wait it out to `Enabled`, answering its UUID: a
/// mapping still settling reads nothing, so a row that looked before it settled would be
/// reading the lifecycle rather than the rule.
async fn enabled_mapping(router: &MultiServiceRouter, case: &str, request: Value) -> String {
    let created = create_mapping(router, &request).await;
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
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_mapping(router, &uuid).await;
        if read.string("State").as_deref() == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping of one row on `stream_arn`: the row's `BatchSize` and patterns, and the
/// `MetricsConfig` it is created with when it opts in.
async fn given_metered_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: i64,
    patterns: &[&str],
    meters: bool,
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": TRIM_HORIZON,
        "BatchSize": batch_size,
        "Enabled": true,
    });
    if !patterns.is_empty() {
        request["FilterCriteria"] = json!({
            "Filters": patterns
                .iter()
                .map(|pattern| json!({ "Pattern": pattern }))
                .collect::<Vec<Value>>(),
        });
    }
    if meters {
        request["MetricsConfig"] = json!({ "Metrics": [EVENT_COUNT] });
    }
    let uuid = enabled_mapping(router, case, request).await;
    let read = get_mapping(router, &uuid).await;
    let selected = read.json()["MetricsConfig"].clone();
    let expected = if meters {
        json!({ "Metrics": [EVENT_COUNT] })
    } else {
        Value::Null
    };
    assert_eq!(
        selected, expected,
        "case {case:?}: the mapping must carry the MetricsConfig it was created with, got {}",
        read.text()
    );
    uuid
}

/// One row: what the mapping filters with, what is written to its table, and how many of the
/// records those writes produce the filter admits.
struct Row {
    case: &'static str,
    /// The `FilterCriteria.Filters[].Pattern` values, in order; empty for a mapping carrying
    /// no criteria at all, which admits every record.
    patterns: &'static [&'static str],
    /// The key and the change of each write, in the order they are made.
    writes: &'static [(&'static str, Write)],
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// How many of the records the writes produce the filter admits — the records an
    /// invocation is handed.
    admitted: f64,
    /// How many of them it rejects.
    rejected: f64,
}

/// The rows: one per shape of the work a pass can do, over a table whose stream captures both
/// images so every member a pattern could name is in the record.
fn rows() -> Vec<Row> {
    vec![
        // One record, no criteria: the smallest pass there is, and the one a mapping with no
        // filter makes.
        Row {
            case: "one-record-no-filter",
            patterns: &[],
            writes: &[("row-1", Write::Insert)],
            batch_size: 10,
            admitted: 1.0,
            rejected: 0.0,
        },
        // The recorded shape: several records of which the criteria reject one, polled in one
        // batch.
        Row {
            case: "five-records-one-rejected",
            patterns: &[r#"{"eventName": ["INSERT"]}"#],
            writes: &[
                ("row-1", Write::Insert),
                ("row-2", Write::Insert),
                ("row-3", Write::Insert),
                ("row-4", Write::Insert),
                ("row-4", Write::Overwrite),
            ],
            batch_size: 10,
            admitted: 4.0,
            rejected: 1.0,
        },
        // A pattern naming a value nested inside the item's own keys, admitting half of what
        // is waiting, and a `BatchSize` smaller than the backlog — so the pass takes several
        // batches and the counts are the whole pass's.
        Row {
            case: "a-key-pattern-over-several-batches",
            patterns: &[r#"{"dynamodb": {"Keys": {"pk": {"S": ["keep-1", "keep-2"]}}}}"#],
            writes: &[
                ("keep-1", Write::Insert),
                ("drop-1", Write::Insert),
                ("keep-2", Write::Insert),
                ("drop-2", Write::Insert),
            ],
            batch_size: 2,
            admitted: 2.0,
            rejected: 2.0,
        },
        // A pattern no record matches: a pass that polls everything, hands over nothing and
        // still says so.
        Row {
            case: "a-filter-admitting-nothing",
            patterns: &[r#"{"eventName": ["REMOVE"]}"#],
            writes: &[("row-1", Write::Insert), ("row-2", Write::Insert)],
            batch_size: 10,
            admitted: 0.0,
            rejected: 2.0,
        },
    ]
}

/// RULE: while a mapping's `MetricsConfig` selects `EventCount`, each pass of its poller
/// publishes per mapping what it did with the records it took out of its SHARD source — the
/// records polled, the records its `FilterCriteria` rejected and the records an invocation
/// accepted — and a shard source has no deleted count at all, because nothing is taken out of
/// a shard.
#[tokio::test]
async fn the_event_count_family_reports_what_each_pass_of_a_stream_mapping_did() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        let table = format!("counted-{case}");
        let function = format!("counted-fn-{case}");
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        given_function(
            &router,
            case,
            &function,
            ACCEPTS_EVERY_RECORD,
            json!({}),
        )
        .await;
        // Every write is made before the mapping exists, so the backlog the pass meets is the
        // row's own and nothing else writes to the stream while it is read.
        for (key, write) in row.writes {
            write_item(&router, case, &table, key, *write).await;
        }
        let polled = row.admitted + row.rejected;
        let uuid = given_metered_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.batch_size,
            row.patterns,
            true,
        )
        .await;

        // The poll count is the barrier: once the pass that read the backlog has published
        // it, the other counts of that pass are published too.
        let polled_sum = wait_for_sum(&router, case, POLLED_EVENT_COUNT_METRIC, &uuid, polled).await;
        let filtered_sum = if row.rejected > 0.0 {
            wait_for_sum(
                &router,
                case,
                FILTERED_OUT_EVENT_COUNT_METRIC,
                &uuid,
                row.rejected,
            )
            .await
        } else {
            metric_sum(&router, case, FILTERED_OUT_EVENT_COUNT_METRIC, &uuid).await
        };
        let invoked_sum = if row.admitted > 0.0 {
            wait_for_sum(
                &router,
                case,
                INVOKED_EVENT_COUNT_METRIC,
                &uuid,
                row.admitted,
            )
            .await
        } else {
            metric_sum(&router, case, INVOKED_EVENT_COUNT_METRIC, &uuid).await
        };

        assert_eq!(
            polled_sum, polled,
            "case {case:?}: the pass polled the {polled} record(s) the row wrote, got \
             {polled_sum}"
        );
        assert_eq!(
            filtered_sum, row.rejected,
            "case {case:?}: the criteria {:?} reject {} of those records, got {filtered_sum}",
            row.patterns, row.rejected
        );
        assert_eq!(
            invoked_sum, row.admitted,
            "case {case:?}: an invocation accepted the {} admitted record(s), got {invoked_sum}",
            row.admitted
        );
        assert!(
            invoked_sum <= polled_sum - filtered_sum,
            "case {case:?}: no more records may be invoked than the filter admitted, got \
             invoked {invoked_sum} of {polled_sum} polled less {filtered_sum} filtered out"
        );
        // Nothing is deleted from a shard, so that series of the family is not published at
        // all — which is what tells a client "this source is not one records leave" from "the
        // pass deleted none of them".
        let deleted = datapoints(&router, case, DELETED_EVENT_COUNT_METRIC, &uuid).await;
        assert!(
            deleted.is_empty(),
            "case {case:?}: a shard source deletes no record, so {DELETED_EVENT_COUNT_METRIC} \
             must have no datapoint, got {deleted:?}"
        );
        // Each series of the family counts events, which `StandardUnit` spells `Count`.
        for metric in [
            POLLED_EVENT_COUNT_METRIC,
            FILTERED_OUT_EVENT_COUNT_METRIC,
            INVOKED_EVENT_COUNT_METRIC,
        ] {
            let units = units_of(&router, case, metric, &uuid).await;
            assert!(
                !units.is_empty() && units.iter().all(|unit| unit == COUNT_UNIT),
                "case {case:?}: every datapoint of {metric} must carry the unit \
                 {COUNT_UNIT:?}, got {units:?}"
            );
        }
        stop_mapping(&router, case, &uuid).await;
    }
}

/// RULE: the `MetricsConfig` is the switch — of two mappings doing the same polling,
/// filtering and invoking on one shard source, the one that selects `EventCount` publishes the
/// family and the one that selects nothing publishes none of it.
#[tokio::test]
async fn only_the_opted_in_twin_of_two_stream_mappings_publishes_its_event_counts() {
    let case = "twinned-mappings-on-one-stream";
    let router = emulator();
    let table = "twinned";
    // One mapping per (source, function) pair is what the service admits, so the twins differ
    // in the function they invoke and do the same work on the same stream.
    let metered_function = "twinned-metered-fn";
    let plain_function = "twinned-plain-fn";
    let stream_arn = given_table(&router, case, table, Some(NEW_AND_OLD_IMAGES)).await;
    for function in [metered_function, plain_function] {
        given_function(&router, case, function, ACCEPTS_EVERY_RECORD, json!({})).await;
    }
    for key in ["row-1", "row-2"] {
        write_item(&router, case, table, key, Write::Insert).await;
    }
    let metered =
        given_metered_mapping(&router, case, metered_function, &stream_arn, 10, &[], true).await;
    let unmetered =
        given_metered_mapping(&router, case, plain_function, &stream_arn, 10, &[], false).await;

    // Each twin's own work is waited for as a CONDITION: the opted-in one until its poll count
    // holds the records that were waiting, the other until it reports the attempt it made on
    // the same records — which is the very pass that would have published its counts.
    let polled = wait_for_sum(&router, case, POLLED_EVENT_COUNT_METRIC, &metered, 2.0).await;
    assert_eq!(
        polled, 2.0,
        "case {case:?}: the opted-in mapping polled the 2 records that were waiting, got \
         {polled}"
    );
    let results = results_until(
        &router,
        case,
        &unmetered,
        RESULT_OK,
        Instant::now() + BUDGET,
    )
    .await;
    assert!(
        results.contains(&RESULT_OK.to_string()),
        "case {case:?}: the opted-out mapping must report an attempt it made, saw {results:?}"
    );
    for metric in [
        POLLED_EVENT_COUNT_METRIC,
        FILTERED_OUT_EVENT_COUNT_METRIC,
        INVOKED_EVENT_COUNT_METRIC,
        DELETED_EVENT_COUNT_METRIC,
    ] {
        let points = datapoints(&router, case, metric, &unmetered).await;
        assert!(
            points.is_empty(),
            "case {case:?}: a mapping that selects no metric publishes none of the family, so \
             {metric} must have no datapoint, got {points:?}"
        );
    }
    stop_mapping(&router, case, &metered).await;
    stop_mapping(&router, case, &unmetered).await;
}
