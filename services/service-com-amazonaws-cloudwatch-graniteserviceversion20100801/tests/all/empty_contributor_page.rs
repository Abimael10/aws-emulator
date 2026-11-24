// SPDX-License-Identifier: CC0-1.0

//! What a contributor read answers when there is NOTHING to report.
//!
//! The rule under test: for an alarm this operation serves — one whose configuration evaluates a
//! query over several time series, so it has contributors at all — having no contributor in
//! `ALARM` right now is an EMPTY PAGE, never a refusal. `DescribeAlarmContributors` "Returns the
//! information of the current alarm contributors that are in ALARM state"
//! (`smithy_query(query="operation",operation="DescribeAlarmContributors")`), and its output
//! member `AlarmContributors` is the one `@required` collection of this service's reads
//! (`smithy_query(query="shape",shape="DescribeAlarmContributorsOutput")`) — so such a read
//! answers HTTP 200 with that member PRESENT and empty, with no `NextToken`, on every protocol
//! this service's model declares. The modelled refusals of this operation are `InvalidNextToken`
//! (400) and `ResourceNotFoundException` (404); neither is "this alarm has nothing to report".
//!
//! The rows of the first suite differ along what the rule generalises over: the FORM the
//! configuration states the query in (a query returning a vector, a Metrics Insights query that
//! groups its result), and WHY there is nothing in `ALARM` at this instant (no evaluation has
//! decided the alarm yet, the alarm is in each of the other states a caller can put it in, it has
//! left `ALARM` again, and every entry its query resolved was graded under the alarm's
//! threshold — the last of which is the case where the store holds contributors and none of them
//! is in `ALARM`).
//!
//! The second suite reads one such alarm over each of the three protocols the model declares
//! (`awsJson1_0`, `awsQuery`, `rpcv2Cbor`), because "present and empty" is a claim about what the
//! answer carries on the wire rather than about one protocol's rendering of it.

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::ShapeSerializer;
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    DescribeAlarmContributorsInput, DescribeAlarmContributorsOutput,
    DESCRIBE_ALARM_CONTRIBUTORS_INPUT_SCHEMA,
};

use crate::harness::{
    build_router, call, described_alarm, graded_window_stamp, merged, metric_alarm_request, ok,
    query_call, wait_for_period_room, AlarmKind, LOOK_INTERVAL, WORK_WAIT,
};

/// One minute, the granularity every alarm of this suite is evaluated at.
const MINUTE: f64 = 60.0;

/// How much of the period a row that publishes datapoints needs left, so its publish and the
/// evaluation that grades it land in the window it stamped them relative to.
const PERIOD_ROOM: f64 = 20.0;

/// The namespace and metric the grouped-query rows select over.
const NAMESPACE: &str = "Custom/EmptyPage";
const METRIC: &str = "Latency";

/// The media type an `awsQuery` client of this service sends its form body under.
const FORM: &str = "application/x-www-form-urlencoded";

/// The members that make an alarm one whose state comes from a query returning a VECTOR of
/// series: "The query must return a result of vector type. Each entry in the vector result
/// represents an alarm contributor."
/// (`smithy_query(query="doc",shape="AlarmPromQLCriteria",member="Query")`).
fn promql_patch(query: &str) -> Value {
    json!({
        "Namespace": null,
        "MetricName": null,
        "Statistic": null,
        "Period": null,
        "EvaluationPeriods": null,
        "Threshold": null,
        "ComparisonOperator": null,
        "EvaluationCriteria": {"PromQLCriteria": {"Query": query}},
        "EvaluationInterval": 60,
    })
}

/// The members that replace the single-metric form with one `Metrics` query stating
/// `expression` — the other configuration whose result is a vector, when the query groups it
/// ("This field can contain either a Metrics Insights query, or a metric math expression",
/// `smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`).
///
/// "If you use the Metrics parameter, you cannot include the Namespace, MetricName, Dimensions,
/// Period, Unit, Statistic, or ExtendedStatistic parameters of PutMetricAlarm in the same
/// operation" (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Metrics")`), so each
/// of them is dropped from the base request.
fn grouped_query_patch(expression: &str) -> Value {
    json!({
        "Namespace": null,
        "MetricName": null,
        "Statistic": null,
        "Period": null,
        "Metrics": [{
            "Id": "grouped",
            "Expression": expression,
            "Period": 60,
            "ReturnData": true,
        }],
    })
}

/// The query the grouped rows watch: one series per `Service` of the metrics published below.
fn grouped_query() -> String {
    format!("SELECT MAX({METRIC}) FROM SCHEMA(\"{NAMESPACE}\", Service) GROUP BY Service")
}

/// Publish `values` into the series of [`METRIC`] that `service` names, stamped inside the one
/// window `at` falls in.
async fn publish(router: &MultiServiceRouter, service: &str, at: f64, values: &[f64]) {
    let data: Vec<Value> = values
        .iter()
        .map(|value| {
            json!({
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Service", "Value": service}],
                "Value": value,
                "Timestamp": at,
            })
        })
        .collect();
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": data}),
    )
    .await;
}

/// Force `name` into `state` — the one seam that puts an alarm into a state a reader can then be
/// asked about without waiting for a metric to be published and graded.
async fn force_state(router: &MultiServiceRouter, name: &str, state: &str) {
    ok(
        router,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": format!("{state} for {name}"),
        }),
    )
    .await;
}

/// The state `DescribeAlarms` reports for `name` right now, or the empty string when it reports
/// no such alarm.
async fn reported_state(router: &MultiServiceRouter, name: &str) -> String {
    described_alarm(router, AlarmKind::Metric, name)
        .await
        .as_ref()
        .and_then(|alarm| alarm.get("StateValue"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Look at `name` until it reports `wanted`, or until a generous deadline passes: the state is
/// computed by an evaluation the clock owes, so a test waits for the CONDITION rather than for a
/// span.
async fn until_state(router: &MultiServiceRouter, name: &str, wanted: &str) -> String {
    let deadline = std::time::Instant::now() + WORK_WAIT;
    loop {
        let seen = reported_state(router, name).await;
        if seen == wanted || std::time::Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Why a row has no contributor in `ALARM` at the instant it is read.
#[derive(Debug, Clone, Copy)]
enum Nothing {
    /// Nothing has decided the alarm yet: it is read as the write that configured it left it.
    AsConfigured,
    /// The alarm stands in a state that is not `ALARM`.
    In(&'static str),
    /// The alarm reported a contributor in `ALARM` and has since left that state.
    LeftAlarm,
    /// Every entry the alarm's query resolves was graded UNDER its threshold, so the store holds
    /// contributors and none of them is in `ALARM`.
    EveryEntryGradedUnderThreshold,
}

/// One row of the first table: the case, the members that configure the alarm, and why it has
/// nothing to report.
struct Row {
    case: &'static str,
    patch: Value,
    nothing: Nothing,
}

/// The status and body of one contributor read of `name`, over `awsJson1_0`.
async fn page(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        "DescribeAlarmContributors",
        &json!({"AlarmName": name}),
    )
    .await
}

#[tokio::test]
async fn nothing_in_alarm_is_an_empty_page_rather_than_a_refusal() {
    let router = build_router();
    // The grouped rows are decided from published datapoints, so the window they stamp them in
    // has room left for the publish and for the evaluation that grades it.
    wait_for_period_room(MINUTE, PERIOD_ROOM).await;
    let at = graded_window_stamp(MINUTE);
    // Two series, both UNDER the base request's threshold of 10, so every entry of the grouped
    // query is graded under it.
    publish(&router, "api", at, &[2.0]).await;
    publish(&router, "web", at, &[4.0]).await;

    let rows = vec![
        Row {
            case: "a vector query no evaluation has decided yet",
            patch: promql_patch("sum by (host) (rate(errors[1m]))"),
            nothing: Nothing::AsConfigured,
        },
        Row {
            case: "a vector query whose alarm stands in OK",
            patch: promql_patch("avg_over_time(latency[5m])"),
            nothing: Nothing::In("OK"),
        },
        Row {
            case: "a vector query whose alarm stands in INSUFFICIENT_DATA",
            patch: promql_patch("errors / requests"),
            nothing: Nothing::In("INSUFFICIENT_DATA"),
        },
        Row {
            case: "a vector query whose alarm has left ALARM again",
            patch: promql_patch("sum by (host) (rate(latency[1m]))"),
            nothing: Nothing::LeftAlarm,
        },
        Row {
            case: "a grouped Metrics Insights query no datapoint of its own was published for",
            patch: grouped_query_patch(&format!(
                "SELECT MAX({METRIC}) FROM SCHEMA(\"Custom/Unpublished\", Service) GROUP BY Service"
            )),
            nothing: Nothing::AsConfigured,
        },
        Row {
            case: "a grouped Metrics Insights query whose every entry was graded under the \
                   threshold",
            patch: grouped_query_patch(&grouped_query()),
            nothing: Nothing::EveryEntryGradedUnderThreshold,
        },
        Row {
            case: "the same grouped query, read while its alarm stands in INSUFFICIENT_DATA",
            patch: grouped_query_patch(&grouped_query()),
            nothing: Nothing::In("INSUFFICIENT_DATA"),
        },
    ];

    for (index, Row { case, patch, nothing }) in rows.into_iter().enumerate() {
        let name = format!("empty-contributor-page-{index}");
        ok(
            &router,
            "PutMetricAlarm",
            &merged(metric_alarm_request(&name), &patch),
        )
        .await;
        // Whatever the read answers, it answers about an alarm this store holds: the two modelled
        // refusals of this operation are about a name no alarm carries and a token it did not
        // issue, and this row is neither.
        assert!(
            described_alarm(&router, AlarmKind::Metric, &name)
                .await
                .is_some(),
            "case {case:?}: the row's own alarm must exist before the read is judged"
        );

        match nothing {
            Nothing::AsConfigured => {}
            Nothing::In(state) => force_state(&router, &name, state).await,
            Nothing::LeftAlarm => {
                force_state(&router, &name, "ALARM").await;
                let (_, reported) = page(&router, &name).await;
                let standing = reported
                    .get("AlarmContributors")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_default();
                assert_eq!(
                    standing, 1,
                    "case {case:?}: the row has to report a contributor before leaving ALARM is \
                     a different answer, got {reported}"
                );
                force_state(&router, &name, "OK").await;
            }
            Nothing::EveryEntryGradedUnderThreshold => {
                let state = until_state(&router, &name, "OK").await;
                assert_eq!(
                    state, "OK",
                    "case {case:?}: a vector whose every entry is under the threshold puts the \
                     alarm in OK, got {state:?}"
                );
            }
        }

        let (status, body) = page(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: nothing to report is not a refusal, got {status} with {body}"
        );
        // The required member is PRESENT and empty — a client reading the output's required list
        // finds an empty page rather than an absent member.
        assert_eq!(
            body.get("AlarmContributors"),
            Some(&json!([])),
            "case {case:?}: expected the required member present and empty, got {body}"
        );
        assert!(
            body.get("NextToken").is_none(),
            "case {case:?}: an empty page has no more data to point at, got {body}"
        );

        // The read changed nothing a later caller sees: the same empty page comes back, and the
        // alarm still stands where it stood.
        let before = reported_state(&router, &name).await;
        let (again_status, again) = page(&router, &name).await;
        assert_eq!(
            (again_status, &again),
            (status, &body),
            "case {case:?}: a contributor read must answer the same twice, got {again}"
        );
        let after = reported_state(&router, &name).await;
        assert_eq!(
            after, before,
            "case {case:?}: reading the contributors must not move the alarm, got {after:?} \
             after {before:?}"
        );
    }
}

/// `input` as the `rpcv2Cbor` request body a client sends: written through the protocol's own
/// codec against the operation's input schema, so nothing here encodes CBOR by hand.
fn cbor_request(input_schema: &Schema, input: &DescribeAlarmContributorsInput) -> bytes::Bytes {
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

/// How many times the bytes of `name` occur in `body`.
fn occurrences(body: &[u8], name: &str) -> usize {
    let needle = name.as_bytes();
    body.windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[tokio::test]
async fn an_empty_contributor_page_carries_its_required_member_on_every_declared_protocol() {
    let router = build_router();
    let name = "empty-page-every-protocol";
    ok(
        &router,
        "PutMetricAlarm",
        &merged(
            metric_alarm_request(name),
            &promql_patch("sum by (host) (rate(errors[1m]))"),
        ),
    )
    .await;
    force_state(&router, name, "OK").await;

    // `awsJson1_0`: the member is a present, empty array.
    let (status, body) = page(&router, name).await;
    assert_eq!(
        (status, body.get("AlarmContributors")),
        (200, Some(&json!([]))),
        "case \"awsJson1_0\": expected the required member present and empty, got {status} with \
         {body}"
    );

    // `rpcv2Cbor`: the answer decodes against the operation's output schema with no contributor
    // and no token, and the member's own name is on the wire — the map carrying it keys it once,
    // which an omitted member would not.
    let input = DescribeAlarmContributorsInput {
        alarm_name: name.to_string(),
        next_token: None,
    };
    let (status, answered) = cbor_call(
        &router,
        "DescribeAlarmContributors",
        cbor_request(DESCRIBE_ALARM_CONTRIBUTORS_INPUT_SCHEMA, &input),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case \"rpcv2Cbor\": nothing to report is not a refusal, got {status} with {:?}",
        String::from_utf8_lossy(&answered)
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&answered);
    let decoded = DescribeAlarmContributorsOutput::deserialize(&mut deserializer)
        .unwrap_or_else(|error| {
            panic!(
                "case \"rpcv2Cbor\": the answer must decode against its output schema, got \
                 {error} for {:?}",
                String::from_utf8_lossy(&answered)
            )
        });
    assert_eq!(
        (decoded.alarm_contributors.len(), &decoded.next_token),
        (0, &None),
        "case \"rpcv2Cbor\": expected an empty page and no token, got {decoded:?}"
    );
    let named = occurrences(&answered, "AlarmContributors");
    assert_eq!(
        named, 1,
        "case \"rpcv2Cbor\": the required member is keyed by the one map that carries it, got \
         {named} occurrence(s) of its name in the answer"
    );

    // `awsQuery`: the same answer in the XML envelope, carrying the member's element rather than
    // a refusal.
    let (status, media_type, xml) = query_call(
        &router,
        FORM,
        &[],
        "DescribeAlarmContributors",
        &[("AlarmName", name)],
    )
    .await;
    assert_eq!(
        (status, media_type.as_str()),
        (200, "text/xml"),
        "case \"awsQuery\": nothing to report is not a refusal, got {status} / {media_type:?} \
         with {xml}"
    );
    assert!(
        xml.contains("<AlarmContributors") && !xml.contains("<Error>"),
        "case \"awsQuery\": expected the required member's element in the result envelope, got \
         {xml}"
    );
}
