// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING v2 as a CONSUMER of the destination log group reads it: the access-log
//! record a served viewer request delivers arrives as an ordinary log event, so every reader of
//! that group reports it.
//!
//! RULE: a vended-log delivery whose destination is "a log group in CloudWatch Logs"
//! (`PutDeliveryDestination`'s own catalogue of what receives log records) delivers each record
//! by INGESTING it into that group, so the record is a log event like any other and every reader
//! of the group answers with it: `FilterLogEvents` ("lists log events from the specified log
//! group [...] You can also control how the results are ordered") reports it across the group,
//! with or without a `filterPattern` ("the filter pattern to use [...] If not provided, all the
//! events are matched") selecting it by the text the record carries; `DescribeLogStreams` names
//! the stream holding it, with that stream's reported first and last event instants spanning the
//! events it holds; and `GetLogEvents` ("lists log events from the specified log stream") over
//! that stream reports the same events, in time order. No reader of a group may see a delivered
//! record a sibling reader does not.
//!
//! This is what a client that wires standard logging v2 actually does with the pipeline — it
//! discovers the records, it does not inspect the delivery — and it is the half the sibling
//! suites leave open: [`super::cloudfront_access_log_deliveries_carry_each_edge_request`] pins
//! the fan-out of one served request to every live delivery and what a `DeleteDelivery` severs,
//! and [`super::cloudfront_dialled_address_vends_each_access_log_record`] pins the same for a
//! viewer holding a socket, both reading the destination through ONE reader, which says nothing
//! about whether the record is a log event the group's other readers hold at all.
//!
//! Only a cross-service suite can state it: the request and the answer it describes are
//! CloudFront's edge over a private Amazon S3 origin, and the group, the stream and the readers
//! are CloudWatch Logs'. The pipeline, the viewer request and what a record owes a request are
//! the sibling suite's, reused rather than restated.
//!
//! The rows span what the rule generalises over: the ANSWER each served request got (an object
//! at a bare key and one under a nested prefix, both HTTP 200, and a key the bucket does not
//! hold, HTTP 403), crossed with the READERS of the destination group — the whole group, the
//! group narrowed by a pattern naming each served URI and one naming a URI nothing requested,
//! and the stream the records were ingested into.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_access_log_deliveries_carry_each_edge_request::{
    expected_value, wire, Row, PRIMARY_FIELDS,
};
use super::cloudfront_oac_s3_origin_edge::view;
use super::logs_delivery_source_cloudfront_resource::logs;

/// How long a look for records that ARE due keeps looking, and how long it waits between looks.
/// Generous: a loaded machine must not fail a row the delivery leg would have written.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

/// Every event `FilterLogEvents` reports for `group`, WHOLE — the event as the reader hands it
/// over, not only the record inside it — optionally narrowed by a `filterPattern`.
async fn filtered(router: &MultiServiceRouter, group: &str, pattern: Option<&str>) -> Vec<Value> {
    let body = match pattern {
        Some(pattern) => json!({"logGroupName": group, "filterPattern": pattern}),
        None => json!({"logGroupName": group}),
    };
    let (status, answered) = logs(router, "FilterLogEvents", &body).await;
    assert_eq!(
        status, 200,
        "case reading {group:?} under pattern {pattern:?}: FilterLogEvents must succeed, got \
         {status} {answered}"
    );
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Look for events in `group` until it holds `wanted` of them or [`DEADLINE`] passes, then
/// answer whatever it holds. Waits on the CONDITION, never on a span.
async fn filtered_until(router: &MultiServiceRouter, group: &str, wanted: usize) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = filtered(router, group, None).await;
        if held.len() >= wanted || Instant::now() >= deadline {
            return held;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Every stream `DescribeLogStreams` reports for `group`.
async fn streams(router: &MultiServiceRouter, group: &str) -> Vec<Value> {
    let (status, answered) =
        logs(router, "DescribeLogStreams", &json!({"logGroupName": group})).await;
    assert_eq!(
        status, 200,
        "case reading the streams of {group:?}: DescribeLogStreams must succeed, got {status} \
         {answered}"
    );
    answered["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Every event `GetLogEvents` reports for one stream, read from the head so the page starts at
/// the oldest event the stream holds.
async fn stream_events(router: &MultiServiceRouter, group: &str, stream: &str) -> Vec<Value> {
    let (status, answered) = logs(
        router,
        "GetLogEvents",
        &json!({"logGroupName": group, "logStreamName": stream, "startFromHead": true}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case reading {stream:?} of {group:?}: GetLogEvents must succeed, got {status} {answered}"
    );
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `(timestamp, message)` pairs of `events`, sorted: what two readers of the same events
/// have to agree on, independent of the order each reader chose to page them in.
fn carried(events: &[Value]) -> Vec<(i64, String)> {
    let mut pairs: Vec<(i64, String)> = events
        .iter()
        .map(|event| {
            (
                event["timestamp"].as_i64().unwrap_or_default(),
                event["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The record inside one delivered log event, parsed from the event's own message.
fn record_of(case: &str, event: &Value) -> Value {
    let message = event["message"].as_str().unwrap_or_default();
    serde_json::from_str(message)
        .unwrap_or_else(|error| panic!("case {case}: a delivered record is not JSON ({error}): {message:?}"))
}

#[tokio::test]
async fn every_reader_of_the_destination_group_reports_the_delivered_access_log_record() {
    // Rows differ along the answer each request got: the key form it addressed, who produced
    // the answer and the status the viewer was given.
    let rows: &[Row] = &[
        Row {
            case: "a readable object at a bare key",
            path: "/present.txt",
            status: 200,
            result_type: "Miss",
        },
        Row {
            case: "a readable object under a nested prefix",
            path: "/reports/2026/held.csv",
            status: 200,
            result_type: "Miss",
        },
        Row {
            case: "a key the bucket does not hold, which the grant does not let the edge list",
            path: "/absent.txt",
            status: 403,
            result_type: "Error",
        },
    ];

    let pipeline = wire(
        "cloudfront-access-log-readers",
        &[
            ("present.txt", "the readable object"),
            ("reports/2026/held.csv", "the nested object"),
        ],
        &[("the delivery into the destination group", PRIMARY_FIELDS)],
    )
    .await;
    let delivery = &pipeline.deliveries[0];
    let group = delivery.log_group.as_str();

    // The requests the records are about, and the identifier the edge stamped on each answer.
    let mut request_ids: Vec<String> = Vec::new();
    for row in rows {
        let viewed = view(&pipeline.viewers, &pipeline.domain, row.path).await;
        assert_eq!(
            viewed.status, row.status,
            "case {:?}: expected the edge to answer {}, got {} with {:?}",
            row.case,
            row.status,
            viewed.status,
            viewed.text()
        );
        let request_id = viewed.header("x-amz-cf-id");
        assert!(
            !request_id.is_empty(),
            "case {:?}: the edge answered no X-Amz-Cf-Id (status {})",
            row.case,
            viewed.status
        );
        request_ids.push(request_id);
    }

    // READER ONE: the whole group. Every served request is one event of it, and each event is
    // an ordinary log event — it names the stream it was ingested into, carries an id of its
    // own and an instant, and its message is this delivery's record of one of those requests.
    let held = filtered_until(&pipeline.setup, group, rows.len()).await;
    assert_eq!(
        held.len(),
        rows.len(),
        "case the whole group: expected one event per served request ({}), got {held:?}",
        rows.len()
    );
    let mut by_request: BTreeMap<String, Value> = BTreeMap::new();
    let mut named_streams: BTreeSet<String> = BTreeSet::new();
    for event in &held {
        let case = "the whole group";
        let stream = event["logStreamName"].as_str().unwrap_or_default();
        assert!(
            !stream.is_empty(),
            "case {case}: a delivered event names no stream of the group: {event}"
        );
        named_streams.insert(stream.to_string());
        assert!(
            !event["eventId"].as_str().unwrap_or_default().is_empty(),
            "case {case}: a delivered event carries no event id: {event}"
        );
        assert!(
            event["timestamp"].as_i64().unwrap_or_default() > 0,
            "case {case}: a delivered event carries no instant: {event}"
        );
        let record = record_of(case, event);
        let request_id = record["x-edge-request-id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        by_request.insert(request_id, record);
    }
    for (row, request_id) in rows.iter().zip(&request_ids) {
        let record = by_request.get(request_id).unwrap_or_else(|| {
            panic!(
                "case {:?}: no event of the group carries the record of the answer's \
                 X-Amz-Cf-Id {request_id:?}, got {held:?}",
                row.case
            )
        });
        for field in ["cs-uri-stem", "sc-status"] {
            let expected = expected_value(row, request_id, field).unwrap_or_default();
            let got = record[field].as_str().unwrap_or_default();
            assert_eq!(
                got, expected,
                "case {:?}: expected the group's event to state {field:?} as {expected:?}, got \
                 {got:?} in {record}",
                row.case
            );
        }
    }

    // READER TWO: the streams of the group. Every record of ONE sender is ingested into one
    // stream, so exactly one stream of the group HOLDS events — the stream the events of reader
    // one name — and that stream's own reported first and last instants span what it holds. The
    // group may hold streams that carry no event at all, which is what the stream a delivery
    // validates its subscription through is.
    let listed = streams(&pipeline.setup, group).await;
    let holding_events: BTreeSet<String> = listed
        .iter()
        .filter(|stream| stream["firstEventTimestamp"].as_i64().is_some())
        .map(|stream| {
            stream["logStreamName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        holding_events, named_streams,
        "case the streams of the group: expected exactly the streams the group's events name \
         ({named_streams:?}) to hold events, got {listed:?}"
    );
    assert_eq!(
        named_streams.len(),
        1,
        "case the streams of the group: expected one sender's records to be held in one stream, \
         got {named_streams:?}"
    );
    let stream = named_streams
        .iter()
        .next()
        .map(String::as_str)
        .unwrap_or_default();
    let described = listed
        .iter()
        .find(|described| described["logStreamName"].as_str() == Some(stream))
        .unwrap_or_else(|| {
            panic!(
                "case the streams of the group: {stream:?} is not among the described streams: \
                 {listed:?}"
            )
        });
    let instants: Vec<i64> = held
        .iter()
        .map(|event| event["timestamp"].as_i64().unwrap_or_default())
        .collect();
    let oldest = instants.iter().copied().min().unwrap_or_default();
    let newest = instants.iter().copied().max().unwrap_or_default();
    let first = described["firstEventTimestamp"].as_i64().unwrap_or_default();
    let last = described["lastEventTimestamp"].as_i64().unwrap_or_default();
    assert!(
        first <= oldest && last >= newest,
        "case the stream's reported span: expected it to cover the events it holds \
         ({oldest}..={newest}), got {first}..={last} in {described:?}"
    );

    // READER THREE: that stream. It reports the same events the group did, in time order.
    let from_stream = stream_events(&pipeline.setup, group, stream).await;
    assert_eq!(
        carried(&from_stream),
        carried(&held),
        "case the stream {stream:?}: expected the events the group reports, got {from_stream:?}"
    );
    let ordered: Vec<i64> = from_stream
        .iter()
        .map(|event| event["timestamp"].as_i64().unwrap_or_default())
        .collect();
    assert!(
        ordered.windows(2).all(|pair| pair[0] <= pair[1]),
        "case the stream {stream:?}: expected the events in time order, got {ordered:?}"
    );

    // READER FOUR: the group NARROWED by a pattern. A quoted term of the unstructured filter
    // syntax is text a matching message carries, so a pattern naming a URI selects exactly the
    // records of the requests for it — and a URI nothing requested selects none.
    for (case, pattern_uri, wanted) in [
        ("the bare key that was served", "/present.txt", 1_usize),
        ("the nested key that was served", "/reports/2026/held.csv", 1),
        ("the key the bucket does not hold, which was still served an answer", "/absent.txt", 1),
        ("a key nothing ever requested", "/never-requested.txt", 0),
    ] {
        let pattern = format!("\"{pattern_uri}\"");
        let matched = filtered(&pipeline.setup, group, Some(&pattern)).await;
        assert_eq!(
            matched.len(),
            wanted,
            "case {case:?}: expected {wanted} event(s) under pattern {pattern:?}, got {matched:?}"
        );
        for event in &matched {
            let record = record_of(case, event);
            let got = record["cs-uri-stem"].as_str().unwrap_or_default();
            assert_eq!(
                got, pattern_uri,
                "case {case:?}: expected the matched event to name {pattern_uri:?}, got {got:?} \
                 in {record}"
            );
        }
    }
}
