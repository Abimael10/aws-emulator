// SPDX-License-Identifier: CC0-1.0

//! What a response of this service puts INSIDE a collection member on the `rpcv2Cbor` wire.
//!
//! This service's model declares three wire protocols side by side — `awsJson1_0`, `awsQuery`
//! and `rpcv2Cbor`
//! (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`)
//! — so a client may read every metric and every alarm over
//! `POST /service/GraniteServiceVersion20100801/operation/<Operation>` with
//! `smithy-protocol: rpc-v2-cbor`, and the CBOR body it gets back is as much a requirement as
//! the JSON one.
//!
//! RULE, which every suite below asserts: on that protocol a structure member whose target is
//! a COLLECTION is a bare CBOR container holding exactly the modelled element values and
//! nothing else — a list is an array of its elements, a map is a map of its entries — and the
//! member's own name appears exactly ONCE, as the key of the enclosing map, never as an item
//! inside the container it keys. So `MetricDataResult$Values`
//! (`com.amazonaws.cloudwatch#DatapointValues` = `list<double>`) holds only numbers,
//! `MetricDataResult$Timestamps` (`com.amazonaws.cloudwatch#Timestamps` = `list<timestamp>`)
//! holds only instants, and `MetricDataResult$Values`'s own documentation — "The number of
//! values always matches the number of timestamps and the timestamp for Values[x] is
//! Timestamps[x]" — is only readable if neither array carries an extra leading item.
//!
//! Each case asserts the rule twice over the same answer, once at each level it is visible at:
//!
//! * the body DECODES through the protocol's own schema-driven reader into the modelled
//!   output types. That reader is strict — a text item where the schema says `double` or
//!   `timestamp` is a decode failure, exactly as a strict client's is — and the items it
//!   recovers are compared against what the request wrote;
//! * the member NAME occurs in the body bytes exactly as many times as there are maps
//!   carrying that member. A name re-emitted inside its own container shows up here as one
//!   occurrence too many, whatever a lenient reader would have made of it.
//!
//! The rows span what the rule generalises over rather than one request: the element type
//! (`double`, `timestamp`, `string`, a structure, and a `map<string, double>` entry's value),
//! the item count (none, one, several), and the form of a number (fractional, whole, and
//! negative, since a CBOR encoder is free to put a whole number on the wire as an integer
//! item).
//!
//! The last two suites are about the protocol itself rather than about what a response puts in
//! a collection: that the two RPC protocols are served over ONE metric store, so a datapoint
//! written in either is readable in the other; and that a request is served in the protocol its
//! own MARKER HEADER names, so a request carrying no marker of any protocol this service
//! declares is answered `404`.

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    DescribeAlarmsInput, DescribeAlarmsOutput, Dimension, GetMetricDataInput, GetMetricDataOutput,
    GetMetricStatisticsInput, GetMetricStatisticsOutput, Metric, MetricDataQuery, MetricDatum,
    MetricStat, PutMetricAlarmInput, PutMetricDataInput, DESCRIBE_ALARMS_INPUT_SCHEMA,
    GET_METRIC_DATA_INPUT_SCHEMA, GET_METRIC_STATISTICS_INPUT_SCHEMA, PUT_METRIC_ALARM_INPUT_SCHEMA,
    PUT_METRIC_DATA_INPUT_SCHEMA,
};

use crate::harness::build_router;

/// One minute, the window width every query below asks for, in the epoch seconds a published
/// time stamp carries.
const MINUTE: f64 = 60.0;

/// The namespace every series in this suite is published under.
const NAMESPACE: &str = "Sim/CborWire";

/// The dimension set every series here carries, so a query names one exact series.
fn dimensions() -> Vec<Dimension> {
    vec![
        Dimension {
            name: "Writer".to_string(),
            value: "cbor".to_string(),
        },
        Dimension {
            name: "Run".to_string(),
            value: "one".to_string(),
        },
    ]
}

/// A minute-aligned instant ten minutes in the past: recent enough to sit inside CloudWatch's
/// retention, and aligned so a 60-second window starts exactly here.
fn base_time() -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    (now / MINUTE).floor() * MINUTE - 10.0 * MINUTE
}

/// `input` as the `rpcv2Cbor` request body a client sends: written through the protocol's own
/// codec against the operation's input schema, so nothing here encodes CBOR by hand.
fn request_body<I: SerializableStruct>(input_schema: &Schema, input: &I) -> bytes::Bytes {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(input_schema, input)
        .expect("the input shape serializes");
    bytes::Bytes::from(serializer.finish())
}

/// One `rpcv2Cbor` call: its HTTP status and the body bytes as answered.
async fn cbor_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: bytes::Bytes,
) -> (u16, bytes::Bytes) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "/service/GraniteServiceVersion20100801/operation/{operation}"
        ))
        .header("content-type", "application/cbor")
        .header("accept", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        // The SigV4 credential scope is what routes the request to the `monitoring` signing
        // name, exactly as it does for this service's JSON and query calls.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/monitoring/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=deadbeef",
        )
        .body(body)
        .expect("a well-formed rpcv2Cbor request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// Require a 200 from `operation` and decode its body with the protocol's own schema-driven
/// reader — the strict read a client performs.
async fn served<O: DeserializableStruct>(
    case: &str,
    router: &MultiServiceRouter,
    operation: &str,
    body: bytes::Bytes,
) -> (O, bytes::Bytes) {
    let (status, answered) = cbor_call(router, operation, body).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {operation} must answer 200 over rpc-v2-cbor, got {status} with body \
         {:?}",
        String::from_utf8_lossy(&answered)
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&answered);
    let decoded = O::deserialize(&mut deserializer).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {operation}'s CBOR body must decode against its output schema, got \
             {error} for {:?}",
            String::from_utf8_lossy(&answered)
        )
    });
    (decoded, answered)
}

/// How many times the bytes of `name` occur in `body`.
fn occurrences(body: &[u8], name: &str) -> usize {
    let needle = name.as_bytes();
    body.windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Assert that the member name `name` occurs in `body` exactly `expected` times — once per map
/// that carries the member, and never as an item inside the container it keys.
///
/// `expected` is counted off the DECODED answer by each caller, never written out here, so a
/// response that carries the member in fewer or more places is still read against the rule
/// rather than against a number this file fixed.
fn names_its_container_once(case: &str, body: &[u8], name: &str, expected: usize) {
    let actual = occurrences(body, name);
    assert_eq!(
        actual, expected,
        "case {case:?}: the member name {name:?} belongs to the {expected} map key(s) that \
         carry it and to nothing inside the container, got {actual} occurrence(s) in the \
         response body"
    );
}

/// Require that none of the `names` being counted in a body occurs inside a VALUE this suite
/// put there, so an occurrence of one of them can only be a map key.
///
/// The counting above reads bytes rather than CBOR items, which is what makes it independent of
/// the reader that decodes the same answer; this is the condition that makes such a count
/// unambiguous, stated against the fixtures rather than assumed.
fn no_fixture_value_spells_a_counted_name(case: &str, names: &[&str], values: &[&str]) {
    for name in names {
        for value in values {
            assert!(
                !value.contains(name),
                "case {case:?}: the fixture value {value:?} spells the counted member name \
                 {name:?}, which would make an occurrence of it ambiguous"
            );
        }
    }
}

/// Publish `(offset from `base`, value)` into the `metric` series of [`NAMESPACE`], over the
/// same `rpcv2Cbor` wire the reads below use.
async fn publish(router: &MultiServiceRouter, metric: &str, base: f64, points: &[(f64, f64)]) {
    let data: Vec<MetricDatum> = points
        .iter()
        .map(|(offset, value)| MetricDatum {
            metric_name: metric.to_string(),
            dimensions: Some(dimensions()),
            timestamp: Some(DateTime::from_secs_f64(base + offset)),
            value: Some(*value),
            unit: Some("Count".to_string()),
            ..Default::default()
        })
        .collect();
    let input = PutMetricDataInput {
        namespace: NAMESPACE.to_string(),
        metric_data: Some(data),
        ..Default::default()
    };
    let (status, answered) = cbor_call(
        router,
        "PutMetricData",
        request_body(PUT_METRIC_DATA_INPUT_SCHEMA, &input),
    )
    .await;
    assert_eq!(
        status,
        200,
        "publishing {metric} must be accepted, got {status} with body {:?}",
        String::from_utf8_lossy(&answered)
    );
}

/// One `GetMetricData` request over the `metric` series, folded into `Sum` over 60-second
/// windows and read oldest first.
fn metric_data_request(metric: &str, base: f64, windows: f64) -> GetMetricDataInput {
    GetMetricDataInput {
        metric_data_queries: vec![MetricDataQuery {
            id: "q".to_string(),
            metric_stat: Some(MetricStat {
                metric: Metric {
                    namespace: Some(NAMESPACE.to_string()),
                    metric_name: Some(metric.to_string()),
                    dimensions: Some(dimensions()),
                },
                period: 60,
                stat: "Sum".to_string(),
                unit: None,
            }),
            return_data: Some(true),
            ..Default::default()
        }],
        start_time: DateTime::from_secs_f64(base),
        end_time: DateTime::from_secs_f64(base + windows * MINUTE),
        next_token: None,
        scan_by: Some("TimestampAscending".to_string()),
        max_datapoints: None,
        label_options: None,
    }
}

/// One row of the `GetMetricData` table: what it is about, the metric it publishes into, the
/// `(offset, value)` pairs published, and the `(window start offset, Sum)` pairs the two
/// scalar lists must then carry in step.
struct DatapointCase {
    label: &'static str,
    metric: &'static str,
    published: &'static [(f64, f64)],
    expected: &'static [(f64, f64)],
}

/// `GetMetricData`'s two lists of scalars carry exactly the modelled items, whatever the
/// element type, the item count or the form of the numbers.
#[tokio::test]
async fn the_scalar_lists_of_a_metric_result_carry_only_their_modelled_items() {
    let router = build_router();
    let base = base_time();

    let cases = [
        DatapointCase {
            label: "a series with no datapoint in the range",
            metric: "NoDatapoint",
            published: &[],
            expected: &[],
        },
        DatapointCase {
            label: "one fractional datapoint",
            metric: "OneDatapoint",
            published: &[(0.0, 1.5)],
            expected: &[(0.0, 1.5)],
        },
        DatapointCase {
            label: "three datapoints in three windows",
            metric: "ThreeWindows",
            published: &[(0.0, 1.5), (60.0, 2.25), (120.0, 4.75)],
            expected: &[(0.0, 1.5), (60.0, 2.25), (120.0, 4.75)],
        },
        DatapointCase {
            // A whole number: a CBOR encoder may put it on the wire as an integer item rather
            // than a floating-point one, and it is still the modelled `double`.
            label: "a window whose statistic is a whole number",
            metric: "WholeNumber",
            published: &[(0.0, 2.0), (30.0, 2.0)],
            expected: &[(0.0, 4.0)],
        },
        DatapointCase {
            label: "a negative value",
            metric: "NegativeValue",
            published: &[(0.0, -0.75)],
            expected: &[(0.0, -0.75)],
        },
    ];

    for case in cases {
        // A publish states the datums it publishes, so a call carrying none is refused rather
        // than accepted as a publish of nothing: the row that reads a series with no datapoint
        // in the range therefore publishes nothing at all.
        if !case.published.is_empty() {
            publish(&router, case.metric, base, case.published).await;
        }
        let (output, body): (GetMetricDataOutput, _) = served(
            case.label,
            &router,
            "GetMetricData",
            request_body(
                GET_METRIC_DATA_INPUT_SCHEMA,
                &metric_data_request(case.metric, base, 3.0),
            ),
        )
        .await;

        let results = output.metric_data_results.clone().unwrap_or_default();
        let operation_wide_messages = usize::from(output.messages.is_some());
        assert_eq!(
            results.len(),
            1,
            "case {:?}: a one-query request answers one result, got {results:?}",
            case.label
        );
        let result = &results[0];
        let values = result.values.clone().unwrap_or_default();
        let timestamps: Vec<f64> = result
            .timestamps
            .clone()
            .unwrap_or_default()
            .iter()
            .map(DateTime::as_secs_f64)
            .collect();
        assert_eq!(
            values.len(),
            timestamps.len(),
            "case {:?}: the modelled pairing needs as many values as timestamps, got \
             {values:?} against {timestamps:?}",
            case.label
        );
        let expected_values: Vec<f64> = case.expected.iter().map(|(_, value)| *value).collect();
        let expected_stamps: Vec<f64> = case
            .expected
            .iter()
            .map(|(offset, _)| base + offset)
            .collect();
        assert_eq!(
            values, expected_values,
            "case {:?}: expected Values {expected_values:?}, got {values:?}",
            case.label
        );
        assert_eq!(
            timestamps, expected_stamps,
            "case {:?}: expected Timestamps {expected_stamps:?}, got {timestamps:?}",
            case.label
        );
        // A list of STRUCTURES in the same object, which the rule covers too: this store
        // answered every point it was asked for, so the result reports no message.
        assert!(
            result.messages.clone().unwrap_or_default().is_empty(),
            "case {:?}: a result answered in full carries no message, got {:?}",
            case.label,
            result.messages
        );

        // Each member name keys exactly the maps that carry the member, and those are counted
        // off the DECODED answer rather than written here: `Messages` keys each result's own
        // and the operation-wide one beside it.
        no_fixture_value_spells_a_counted_name(
            case.label,
            &["MetricDataResults", "Values", "Timestamps", "Messages"],
            &[
                NAMESPACE,
                case.metric,
                "Writer",
                "cbor",
                "Run",
                "one",
                "Count",
                "Sum",
                "Complete",
            ],
        );
        for (member, carriers) in [
            (
                "MetricDataResults",
                usize::from(output.metric_data_results.is_some()),
            ),
            (
                "Values",
                results.iter().filter(|it| it.values.is_some()).count(),
            ),
            (
                "Timestamps",
                results.iter().filter(|it| it.timestamps.is_some()).count(),
            ),
            (
                "Messages",
                results.iter().filter(|it| it.messages.is_some()).count()
                    + operation_wide_messages,
            ),
        ] {
            names_its_container_once(case.label, &body, member, carriers);
        }
    }
}

/// A list of STRINGS, and the list of structures beside it, in a `DescribeAlarms` answer.
#[tokio::test]
async fn the_string_lists_of_an_alarm_carry_only_their_modelled_items() {
    let router = build_router();
    let alarm = "cbor-wire-alarm";
    // Two actions in one member and one in another, so the case spans both a multi-item and a
    // single-item `com.amazonaws.cloudwatch#ResourceList` (= `list<string>`).
    let alarm_actions = vec![
        "arn:aws:sns:us-east-1:000000000000:first-topic".to_string(),
        "arn:aws:sns:us-east-1:000000000000:second-topic".to_string(),
    ];
    let ok_actions = vec!["arn:aws:sns:us-east-1:000000000000:recovered".to_string()];

    let created = PutMetricAlarmInput {
        alarm_name: alarm.to_string(),
        namespace: Some(NAMESPACE.to_string()),
        metric_name: Some("AlarmedSeries".to_string()),
        dimensions: Some(dimensions()),
        statistic: Some("Sum".to_string()),
        period: Some(60),
        evaluation_periods: Some(1),
        threshold: Some(1.0),
        comparison_operator: Some("GreaterThanThreshold".to_string()),
        alarm_actions: Some(alarm_actions.clone()),
        ok_actions: Some(ok_actions.clone()),
        ..Default::default()
    };
    let (status, answered) = cbor_call(
        &router,
        "PutMetricAlarm",
        request_body(PUT_METRIC_ALARM_INPUT_SCHEMA, &created),
    )
    .await;
    assert_eq!(
        status,
        200,
        "creating the alarm must be accepted, got {status} with body {:?}",
        String::from_utf8_lossy(&answered)
    );

    let case = "an alarm read back over rpc-v2-cbor";
    let (output, body): (DescribeAlarmsOutput, _) = served(
        case,
        &router,
        "DescribeAlarms",
        request_body(
            DESCRIBE_ALARMS_INPUT_SCHEMA,
            // The request carries a list of strings too, which the same serializer writes.
            &DescribeAlarmsInput {
                alarm_names: Some(vec![alarm.to_string()]),
                ..Default::default()
            },
        ),
    )
    .await;

    let alarms = output.metric_alarms.clone().unwrap_or_default();
    assert_eq!(
        alarms.len(),
        1,
        "case {case:?}: the named alarm is reported once, got {alarms:?}"
    );
    let reported = &alarms[0].alarm;
    assert_eq!(
        reported.alarm_actions.clone().unwrap_or_default(),
        alarm_actions,
        "case {case:?}: expected AlarmActions {alarm_actions:?}, got {:?}",
        reported.alarm_actions
    );
    assert_eq!(
        reported.ok_actions.clone().unwrap_or_default(),
        ok_actions,
        "case {case:?}: expected OKActions {ok_actions:?}, got {:?}",
        reported.ok_actions
    );
    // The list of STRUCTURES beside them, whose elements are maps rather than scalars.
    let reported_dimensions: Vec<(String, String)> = reported
        .dimensions
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|dimension| (dimension.name, dimension.value))
        .collect();
    let expected_dimensions: Vec<(String, String)> = dimensions()
        .into_iter()
        .map(|dimension| (dimension.name, dimension.value))
        .collect();
    assert_eq!(
        reported_dimensions, expected_dimensions,
        "case {case:?}: expected Dimensions {expected_dimensions:?}, got {reported_dimensions:?}"
    );

    // Each member name keys exactly the maps that carry the member, counted off the DECODED
    // answer rather than written here.
    let counted = ["MetricAlarms", "AlarmActions", "OKActions", "Dimensions"];
    let mut fixture_values: Vec<&str> = vec![alarm, NAMESPACE, "AlarmedSeries", "Sum", "GreaterThanThreshold"];
    fixture_values.extend(alarm_actions.iter().map(String::as_str));
    fixture_values.extend(ok_actions.iter().map(String::as_str));
    fixture_values.extend(
        expected_dimensions
            .iter()
            .flat_map(|(name, value)| [name.as_str(), value.as_str()]),
    );
    no_fixture_value_spells_a_counted_name(case, &counted, &fixture_values);
    for (member, carriers) in [
        ("MetricAlarms", usize::from(output.metric_alarms.is_some())),
        (
            "AlarmActions",
            alarms
                .iter()
                .filter(|it| it.alarm.alarm_actions.is_some())
                .count(),
        ),
        (
            "OKActions",
            alarms
                .iter()
                .filter(|it| it.alarm.ok_actions.is_some())
                .count(),
        ),
        (
            "Dimensions",
            alarms
                .iter()
                .filter(|it| it.alarm.dimensions.is_some())
                .count(),
        ),
    ] {
        names_its_container_once(case, &body, member, carriers);
    }
}

/// A map of scalars — `Datapoint$ExtendedStatistics` (`map<string, double>`) — in a
/// `GetMetricStatistics` answer: its entries are the map's own, and the member name keys the
/// map rather than sitting inside it.
#[tokio::test]
async fn the_scalar_map_of_a_datapoint_carries_only_its_modelled_entries() {
    let router = build_router();
    let base = base_time();
    let metric = "StatisticsSeries";
    // Four observations in ONE window, so the window has a distribution to take percentiles
    // of and the answer carries exactly one datapoint.
    let published: &[(f64, f64)] = &[(0.0, 1.5), (10.0, 2.5), (20.0, 3.5), (30.0, 4.5)];
    publish(&router, metric, base, published).await;

    let case = "two percentiles over one window";
    let (output, body): (GetMetricStatisticsOutput, _) = served(
        case,
        &router,
        "GetMetricStatistics",
        request_body(
            GET_METRIC_STATISTICS_INPUT_SCHEMA,
            &GetMetricStatisticsInput {
                namespace: NAMESPACE.to_string(),
                metric_name: metric.to_string(),
                dimensions: Some(dimensions()),
                start_time: DateTime::from_secs_f64(base),
                end_time: DateTime::from_secs_f64(base + MINUTE),
                period: 60,
                statistics: Some(vec!["Sum".to_string(), "Average".to_string()]),
                extended_statistics: Some(vec!["p50".to_string(), "p90".to_string()]),
                unit: None,
            },
        ),
    )
    .await;

    let datapoints = output.datapoints.clone().unwrap_or_default();
    assert_eq!(
        datapoints.len(),
        1,
        "case {case:?}: one window answers one datapoint, got {datapoints:?}"
    );
    let datapoint = &datapoints[0];
    assert_eq!(
        datapoint.sum,
        Some(12.0),
        "case {case:?}: expected the window's Sum 12.0, got {:?}",
        datapoint.sum
    );
    assert_eq!(
        datapoint.average,
        Some(3.0),
        "case {case:?}: expected the window's Average 3.0, got {:?}",
        datapoint.average
    );

    let extended = datapoint.extended_statistics.clone().unwrap_or_default();
    let mut keys: Vec<&str> = extended.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["p50", "p90"],
        "case {case:?}: the map holds exactly the percentiles the request asked for, got \
         {extended:?}"
    );
    // Each entry is a number out of the window's own distribution, and the higher percentile
    // is not below the lower one.
    let (lowest, highest) = (1.5_f64, 4.5_f64);
    for (name, value) in &extended {
        assert!(
            (lowest..=highest).contains(value),
            "case {case:?}: {name:?} must lie inside the published values {lowest}..={highest}, \
             got {value}"
        );
    }
    assert!(
        extended["p50"] <= extended["p90"],
        "case {case:?}: p50 must not exceed p90, got {:?} against {:?}",
        extended["p50"],
        extended["p90"]
    );

    // Both member names key exactly the maps that carry them, counted off the DECODED answer.
    no_fixture_value_spells_a_counted_name(
        case,
        &["Datapoints", "ExtendedStatistics"],
        &[NAMESPACE, metric, "Writer", "cbor", "Run", "one", "Count", "p50", "p90"],
    );
    for (member, carriers) in [
        ("Datapoints", usize::from(output.datapoints.is_some())),
        (
            "ExtendedStatistics",
            datapoints
                .iter()
                .filter(|it| it.extended_statistics.is_some())
                .count(),
        ),
    ] {
        names_its_container_once(case, &body, member, carriers);
    }
}

/// The path a Smithy RPC v2 CBOR request of this service is POSTed to:
/// `/service/{ServiceName}/operation/{OperationName}` (that protocol's specification), with the
/// service shape's name as this model declares it.
fn rpc_v2_path(operation: &str) -> String {
    format!("/service/GraniteServiceVersion20100801/operation/{operation}")
}

/// The `Authorization` header every request below carries: the credential scope names the
/// `monitoring` signing name, which is what routes a request to this service whichever protocol
/// it is in. Neither the key id nor the signature is a credential — the harness turns request
/// authentication off, so nothing here is verified.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
                             Credential=AKIDTEST/20260101/us-east-1/monitoring/aws4_request, \
                             SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// One request as written: the method is always POST, and the URI, the headers and the body are
/// what a row varies. Answers the HTTP status and the body bytes.
async fn posted(
    router: &MultiServiceRouter,
    uri: &str,
    headers: &[(&str, &str)],
    body: bytes::Bytes,
) -> (u16, bytes::Bytes) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", AUTHORIZATION);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(request.body(body).expect("a well-formed request"))
        .await;
    (response.status().as_u16(), response.into_body())
}

/// A `PutMetricData` request publishing one datum into [`NAMESPACE`], as the input shape.
fn one_datum(metric: &str, stamp: f64, value: f64) -> PutMetricDataInput {
    PutMetricDataInput {
        namespace: NAMESPACE.to_string(),
        metric_data: Some(vec![MetricDatum {
            metric_name: metric.to_string(),
            dimensions: Some(dimensions()),
            timestamp: Some(DateTime::from_secs_f64(stamp)),
            value: Some(value),
            unit: Some("Count".to_string()),
            ..Default::default()
        }]),
        ..Default::default()
    }
}

/// The same datum as the awsJson1_0 document a JSON client sends, where an instant travels as
/// epoch seconds.
fn one_datum_as_json(metric: &str, stamp: f64, value: f64) -> serde_json::Value {
    serde_json::json!({
        "Namespace": NAMESPACE,
        "MetricData": [{
            "MetricName": metric,
            "Dimensions": dimensions()
                .into_iter()
                .map(|dimension| serde_json::json!({
                    "Name": dimension.name,
                    "Value": dimension.value,
                }))
                .collect::<Vec<_>>(),
            "Timestamp": stamp,
            "Value": value,
            "Unit": "Count",
        }],
    })
}

/// The `(Sum, SampleCount)` of the one window `[base, base + MINUTE)` of `metric`, read over
/// the awsJson1_0 wire.
async fn folded_over_json(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    base: f64,
) -> (Option<f64>, Option<f64>) {
    let (status, body) = crate::harness::call(
        router,
        "GetMetricStatistics",
        &serde_json::json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions()
                .into_iter()
                .map(|dimension| serde_json::json!({
                    "Name": dimension.name,
                    "Value": dimension.value,
                }))
                .collect::<Vec<_>>(),
            "StartTime": base,
            "EndTime": base + MINUTE,
            "Period": 60,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics over awsJson1_0 must answer 200, got {status} with \
         {body}"
    );
    let datapoints = body
        .get("Datapoints")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        datapoints.len(),
        1,
        "case {case:?}: one window answers one datapoint, got {datapoints:?}"
    );
    (
        datapoints[0].get("Sum").and_then(serde_json::Value::as_f64),
        datapoints[0]
            .get("SampleCount")
            .and_then(serde_json::Value::as_f64),
    )
}

/// RULE: the two RPC protocols this service declares are served over ONE metric store, so a
/// datapoint written in either is readable in the other.
///
/// The rows are the two directions, and each fold is taken after BOTH writes as well, so the
/// window a read reports is one the two protocols contributed to together rather than one each
/// wrote alone.
#[tokio::test]
async fn a_datapoint_written_in_either_protocol_is_read_in_the_other() {
    let router = build_router();
    let base = base_time();
    let metric = "SharedStore";
    let (first, second) = (2.5_f64, 4.0_f64);

    // 1. The CBOR write, read over awsJson1_0.
    let case = "a datapoint written over rpc-v2-cbor";
    let (status, answered) = cbor_call(
        &router,
        "PutMetricData",
        request_body(PUT_METRIC_DATA_INPUT_SCHEMA, &one_datum(metric, base, first)),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the CBOR publish must be accepted, got {status} with body {:?}",
        String::from_utf8_lossy(&answered)
    );
    let folded = folded_over_json(&router, case, metric, base).await;
    assert_eq!(
        folded,
        (Some(first), Some(1.0)),
        "case {case:?}: expected the JSON read to report the CBOR datapoint as \
         (Sum, SampleCount) ({first}, 1), got {folded:?}"
    );

    // 2. The awsJson1_0 write, read over rpc-v2-cbor — the same window, which now carries both.
    let case = "a datapoint written over awsJson1_0 beside it";
    let (status, body) = crate::harness::call(
        &router,
        "PutMetricData",
        &one_datum_as_json(metric, base + 10.0, second),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the JSON publish must be accepted, got {status} with {body}"
    );
    let (output, _): (GetMetricStatisticsOutput, _) = served(
        case,
        &router,
        "GetMetricStatistics",
        request_body(
            GET_METRIC_STATISTICS_INPUT_SCHEMA,
            &GetMetricStatisticsInput {
                namespace: NAMESPACE.to_string(),
                metric_name: metric.to_string(),
                dimensions: Some(dimensions()),
                start_time: DateTime::from_secs_f64(base),
                end_time: DateTime::from_secs_f64(base + MINUTE),
                period: 60,
                statistics: Some(vec!["Sum".to_string(), "SampleCount".to_string()]),
                extended_statistics: None,
                unit: None,
            },
        ),
    )
    .await;
    let datapoints = output.datapoints.clone().unwrap_or_default();
    assert_eq!(
        datapoints.len(),
        1,
        "case {case:?}: one window answers one datapoint, got {datapoints:?}"
    );
    assert_eq!(
        (datapoints[0].sum, datapoints[0].sample_count),
        (Some(first + second), Some(2.0)),
        "case {case:?}: expected the CBOR read to report both datapoints as (Sum, SampleCount) \
         ({}, 2), got {datapoints:?}",
        first + second
    );
    // And the JSON read of the same window agrees, so neither protocol holds a store of its
    // own.
    let folded = folded_over_json(&router, case, metric, base).await;
    assert_eq!(
        folded,
        (Some(first + second), Some(2.0)),
        "case {case:?}: expected the JSON read to report the same window, got {folded:?}"
    );
}

/// One row of the discriminator table: the case, the URI it is POSTed to, the headers it
/// carries, the body, and the status this service owes it. Named so the row type stays simple
/// enough to read.
type DiscriminatorCase = (
    &'static str,
    String,
    Vec<(&'static str, &'static str)>,
    bytes::Bytes,
    u16,
);

/// RULE: a request to this service is served in the protocol its own MARKER HEADER names, and a
/// request carrying no marker of any protocol this service declares names no route at all and is
/// answered `404` — never the malformed-body refusal of a protocol it never asked for.
///
/// The rows span every marker and every way of carrying none: the CBOR protocol's header, the
/// same path without it, a marker naming a protocol this service does not serve, the JSON
/// protocol's two markers, the query protocol's media type, and a body of a media type no
/// protocol of this service declares.
#[tokio::test]
async fn a_request_naming_no_protocol_of_this_service_is_not_found() {
    let router = build_router();
    let base = base_time();
    let cbor_body = request_body(
        PUT_METRIC_DATA_INPUT_SCHEMA,
        &one_datum("Discriminated", base, 1.0),
    );
    let json_body = bytes::Bytes::from(
        serde_json::to_vec(&one_datum_as_json("Discriminated", base, 1.0))
            .expect("the document serializes"),
    );
    let form_body = bytes::Bytes::from(format!(
        "Action=PutMetricData&Version=2010-08-01&Namespace={}\
         &MetricData.member.1.MetricName=Discriminated&MetricData.member.1.Value=1.0",
        NAMESPACE.replace('/', "%2F")
    ));

    // (case, uri, headers, body, the status the service owes)
    let rows: Vec<DiscriminatorCase> = vec![
        (
            "the CBOR protocol's marker on its own path",
            rpc_v2_path("PutMetricData"),
            vec![
                ("content-type", "application/cbor"),
                ("accept", "application/cbor"),
                ("smithy-protocol", "rpc-v2-cbor"),
            ],
            cbor_body.clone(),
            200,
        ),
        (
            "the same request with the marker omitted",
            rpc_v2_path("PutMetricData"),
            vec![
                ("content-type", "application/cbor"),
                ("accept", "application/cbor"),
            ],
            cbor_body.clone(),
            404,
        ),
        (
            "that path with no header of any protocol",
            rpc_v2_path("PutMetricData"),
            vec![],
            cbor_body.clone(),
            404,
        ),
        (
            "a marker naming a protocol this service does not serve",
            rpc_v2_path("PutMetricData"),
            vec![
                ("content-type", "application/cbor"),
                ("smithy-protocol", "rpc-v2-json"),
            ],
            cbor_body,
            404,
        ),
        (
            "the JSON protocol's target header",
            "/".to_string(),
            vec![
                ("content-type", "application/x-amz-json-1.0"),
                (
                    "x-amz-target",
                    "GraniteServiceVersion20100801.PutMetricData",
                ),
            ],
            json_body.clone(),
            200,
        ),
        (
            "a JSON document under a media type no protocol of this service declares",
            "/".to_string(),
            vec![("content-type", "application/json")],
            json_body,
            404,
        ),
        (
            "the query protocol's media type",
            "/".to_string(),
            vec![("content-type", "application/x-www-form-urlencoded")],
            form_body,
            200,
        ),
    ];

    for (case, uri, headers, body, expected) in rows {
        let (status, answered) = posted(&router, &uri, &headers, body).await;
        assert_eq!(
            status,
            expected,
            "case {case:?}: expected HTTP {expected}, got {status} with body {:?}",
            String::from_utf8_lossy(&answered)
        );
        if expected == 404 {
            // Stated separately, because the SHAPE of the refusal is the point: an unrouted
            // request is not a request whose body could not be read.
            let text = String::from_utf8_lossy(&answered);
            assert!(
                !text.contains("SerializationException"),
                "case {case:?}: an unrouted request must not be reported as a malformed one, \
                 got {text:?}"
            );
        }
    }
}
