// SPDX-License-Identifier: CC0-1.0

//! Which `AdditionalStatistics` entry `PutMetricStream` refuses a request for, and what a
//! refused request leaves behind.
//!
//! The member names the statistics that are ADDITIONAL to the ones a stream sends anyway —
//! "a metric stream always sends the MAX, MIN, SUM, and SAMPLECOUNT statistics for each
//! metric that is streamed. You can use this parameter to have the metric stream also send
//! additional statistics in the stream" (`PutMetricStreamInput$StatisticsConfigurations`) —
//! and the output format narrows them further: "If the OutputFormat for the stream is
//! opentelemetry1.0 or opentelemetry0.7, the only valid values are p?? percentile statistics
//! such as p90, p99 and so on. If the OutputFormat for the stream is json, the valid values
//! include the abbreviations for all of the statistics listed in CloudWatch statistics
//! definitions. For example, this includes tm98, wm90, PR(:300), and so on."
//! (`MetricStreamStatisticsConfiguration$AdditionalStatistics`)
//!
//! Which statistic each format admits is tabled once, in `metric_streams.rs`. What this suite
//! adds is the rest of the rule: a configuration carries a LIST, so the refusal names the
//! first entry the format cannot carry; both of its list members have to carry an entry at
//! all; a percentile is admissible only strictly inside the distribution, since its ends are
//! the MIN and MAX a stream sends anyway; and a refused request changes no state — on the
//! create path and on the upsert path of a stream that is already live. The read that
//! confirms the refused create is answered by its error code alone, with no message
//! (`not_found_messages.rs` owns that contract for every not-found this service words that
//! way; here it is the pair a client compares after the refusal).

use crate::harness::{build_router, call, error_code, error_message, ok};
use crate::metric_streams::put_request;
use serde_json::{json, Value};

/// Every output format a metric stream can carry, so a rule that holds whatever the format
/// is gets asserted over all of them (`MetricStreamOutputFormat`:
/// `smithy_query(query="shape",shape="MetricStreamOutputFormat")` — json, opentelemetry0.7,
/// opentelemetry1.0).
const OUTPUT_FORMATS: [&str; 3] = ["json", "opentelemetry0.7", "opentelemetry1.0"];

/// A `PutMetricStream` request for `name` in `output_format`, asking for `statistics` on one
/// metric — the suite's required members come from the shared builder.
fn statistics_request(name: &str, output_format: &str, statistics: &[&str]) -> Value {
    let mut request = put_request(name, output_format);
    request["StatisticsConfigurations"] = json!([{
        "IncludeMetrics": [{"Namespace": "MyApp", "MetricName": "Latency"}],
        "AdditionalStatistics": statistics,
    }]);
    request
}

#[tokio::test]
async fn the_refusal_names_the_first_statistic_the_format_cannot_carry() {
    // Rows differ along WHERE in the list the inadmissible entry sits and how many there
    // are: a list of one, an inadmissible entry behind an admissible one, two inadmissible
    // entries (only the first is named), and an entry inadmissible in one letter case while
    // nothing of the request is normalised on the way into the sentence.
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        ("a list of one", vec!["Sum"], "Sum"),
        (
            "behind an admissible percentile",
            vec!["p99", "Average"],
            "Average",
        ),
        (
            "the first of two inadmissible entries",
            vec!["Minimum", "Maximum"],
            "Minimum",
        ),
        (
            "an entry in another letter case",
            vec!["p50", "sampleCOUNT", "Sum"],
            "sampleCOUNT",
        ),
        (
            "a name no statistic family produces",
            vec!["p99", "Median"],
            "Median",
        ),
    ];

    for (case, statistics, offending) in cases {
        for format in OUTPUT_FORMATS {
            let router = build_router();
            let case = format!("{case} ({statistics:?}) with {format}");
            let request = statistics_request("listed-statistics", format, &statistics);

            let (status, body) = call(&router, "PutMetricStream", &request).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidParameterValueException"),
                "case {case:?}: expected a 400 InvalidParameterValueException, got {status} \
                 {body}"
            );
            assert_eq!(
                error_message(&body),
                format!("Unsupported statistic {offending} for selected OutputFormat"),
                "case {case:?}: expected the refusal to name the first inadmissible statistic \
                 in CloudWatch's sentence, got {body}"
            );

            // The refused create left nothing behind, and the not-found that says so is
            // answered by its error code alone: a client confirming the refusal compares
            // the code with an empty message.
            let (get_status, get_body) = call(
                &router,
                "GetMetricStream",
                &json!({"Name": "listed-statistics"}),
            )
            .await;
            assert_eq!(
                (
                    get_status,
                    error_code(&get_body),
                    error_message(&get_body)
                ),
                (404, "ResourceNotFoundException".to_string(), String::new()),
                "case {case:?}: a refused PutMetricStream must persist nothing, got \
                 {get_status} {get_body}"
            );

            // Nor is the stream reported among the account's streams.
            let listed = ok(&router, "ListMetricStreams", &json!({})).await;
            assert_eq!(
                listed.get("Entries"),
                Some(&json!([])),
                "case {case:?}: a refused create must list no stream, got {listed}"
            );
        }
    }
}

/// One metric pair a configuration can name, for the rows that vary the OTHER member.
fn one_pair() -> Value {
    json!({"Namespace": "MyApp", "MetricName": "Latency"})
}

/// A `PutMetricStream` request in `output_format` whose `StatisticsConfigurations` are
/// `configurations` verbatim — the seam for a row that names an EMPTY list member.
fn configured_request(name: &str, output_format: &str, configurations: Value) -> Value {
    let mut request = put_request(name, output_format);
    request["StatisticsConfigurations"] = configurations;
    request
}

#[tokio::test]
async fn a_statistics_configuration_names_at_least_one_metric_and_one_statistic() {
    // The rule: both list members of a statistics configuration are `required`
    // (`smithy_query(query="shape",shape="MetricStreamStatisticsConfiguration")`) and each is
    // the whole of one half of what the configuration says, so a list that arrived EMPTY is a
    // value fault naming the member and the floor it broke — `<Member> less than 1` — and
    // nothing is stored. Rows differ along WHICH member is empty, whether both are, and where
    // in the configuration array the offending configuration sits.
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "no additional statistic",
            json!([{"IncludeMetrics": [one_pair()], "AdditionalStatistics": []}]),
            "AdditionalStatistics less than 1",
        ),
        (
            "no included metric",
            json!([{"IncludeMetrics": [], "AdditionalStatistics": ["p99"]}]),
            "IncludeMetrics less than 1",
        ),
        (
            "neither member carries an entry",
            json!([{"IncludeMetrics": [], "AdditionalStatistics": []}]),
            // The members are judged in the order the structure declares them, so the first
            // empty one is the one the refusal names.
            "IncludeMetrics less than 1",
        ),
        (
            "the second configuration of two",
            json!([
                {"IncludeMetrics": [one_pair()], "AdditionalStatistics": ["p99"]},
                {"IncludeMetrics": [one_pair()], "AdditionalStatistics": []},
            ]),
            "AdditionalStatistics less than 1",
        ),
    ];

    for (case, configurations, expected) in cases {
        for format in OUTPUT_FORMATS {
            let router = build_router();
            let case = format!("{case} with {format}");
            let request = configured_request("floored-statistics", format, configurations.clone());

            let (status, body) = call(&router, "PutMetricStream", &request).await;
            assert_eq!(
                (status, error_code(&body).as_str(), error_message(&body)),
                (400, "InvalidParameterValueException", expected.to_string()),
                "case {case:?}: expected a 400 InvalidParameterValueException naming the empty \
                 member, got {status} {body}"
            );

            let (get_status, get_body) = call(
                &router,
                "GetMetricStream",
                &json!({"Name": "floored-statistics"}),
            )
            .await;
            assert_eq!(
                (get_status, error_code(&get_body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: a refused PutMetricStream must persist nothing, got {get_status} \
                 {get_body}"
            );
        }
    }
}

#[tokio::test]
async fn an_empty_metric_names_list_on_a_filter_is_normalised_away_instead() {
    // The contrast the rule above turns on: a filter's `MetricNames` is NOT required, and "if
    // you omit this parameter, all metrics in the namespace are included or excluded"
    // (`MetricStreamFilter$MetricNames`) — which is exactly what an empty list selects. So an
    // empty list there is stored as the omitted member rather than refused, whichever of the
    // two filter members carries it.
    for member in ["IncludeFilters", "ExcludeFilters"] {
        let router = build_router();
        let mut request = put_request("normalised-filter", "json");
        request[member] = json!([{"Namespace": "MyApp", "MetricNames": []}]);

        ok(&router, "PutMetricStream", &request).await;
        let stored = ok(
            &router,
            "GetMetricStream",
            &json!({"Name": "normalised-filter"}),
        )
        .await;
        assert_eq!(
            stored.get(member),
            Some(&json!([{"Namespace": "MyApp"}])),
            "case {member:?}: an empty MetricNames list must be stored as the omitted member, \
             got {stored}"
        );
    }
}

#[tokio::test]
async fn a_percentile_is_an_additional_statistic_only_strictly_inside_the_distribution() {
    // The rule: the member names the statistics a stream does not send anyway — "a metric
    // stream always sends the MAX, MIN, SUM, and SAMPLECOUNT statistics for each metric that is
    // streamed" (`PutMetricStreamInput$StatisticsConfigurations`) — and `p0` is the MIN of the
    // window while `p100` is its MAX, so the percentiles it admits are the ones strictly
    // between them, in every output format. Rows walk the interval: both ends (in both the
    // integral and the fractional spelling), one step inside each end, the middle, and a
    // percentile past the top, which names no percentile at all.
    let cases: Vec<(&str, &str, bool)> = vec![
        ("the bottom of the distribution", "p0", false),
        ("the bottom, spelled with a fraction", "p0.0", false),
        ("the top of the distribution", "p100", false),
        ("the top, spelled with a fraction", "p100.0", false),
        ("past the top", "p101", false),
        ("one step above the bottom", "p0.1", true),
        ("the middle", "p50", true),
        ("one step below the top", "p99.9", true),
    ];

    for (case, statistic, admitted) in cases {
        for format in OUTPUT_FORMATS {
            let router = build_router();
            let case = format!("{case} ({statistic}) with {format}");
            let request = statistics_request("interval-statistics", format, &[statistic]);

            let (status, body) = call(&router, "PutMetricStream", &request).await;
            if admitted {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a percentile inside the distribution must be stored, got \
                     {status} {body}"
                );
                let stored = ok(
                    &router,
                    "GetMetricStream",
                    &json!({"Name": "interval-statistics"}),
                )
                .await;
                assert_eq!(
                    stored.get("StatisticsConfigurations"),
                    request.get("StatisticsConfigurations"),
                    "case {case:?}: the configuration must round-trip, got {stored}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&body).as_str(), error_message(&body)),
                    (
                        400,
                        "InvalidParameterValueException",
                        format!("Unsupported statistic {statistic} for selected OutputFormat"),
                    ),
                    "case {case:?}: expected the refusal to name the statistic in CloudWatch's \
                     sentence, got {status} {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_refused_percentile_leaves_the_stored_statistics_untouched() {
    // The upsert path of the interval rule: a stream already streaming a percentile keeps the
    // configuration it has when a later put names one of the ends.
    let router = build_router();
    let live = statistics_request("interval-upsert", "json", &["p99"]);
    ok(&router, "PutMetricStream", &live).await;

    for statistic in ["p0", "p100", "p0.0"] {
        let (status, body) = call(
            &router,
            "PutMetricStream",
            &statistics_request("interval-upsert", "json", &[statistic]),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {statistic:?}: expected a 400 InvalidParameterValueException, got {status} \
             {body}"
        );
        let stored = ok(
            &router,
            "GetMetricStream",
            &json!({"Name": "interval-upsert"}),
        )
        .await;
        assert_eq!(
            stored.get("StatisticsConfigurations"),
            live.get("StatisticsConfigurations"),
            "case {statistic:?}: a refused put must leave the stored statistics untouched, got \
             {stored}"
        );
    }
}

#[tokio::test]
async fn a_refused_upsert_leaves_the_live_stream_exactly_as_it_was() {
    let router = build_router();
    let live = statistics_request("upserted-stream", "json", &["TM(10%:90%)"]);
    ok(&router, "PutMetricStream", &live).await;
    let before = ok(
        &router,
        "GetMetricStream",
        &json!({"Name": "upserted-stream"}),
    )
    .await;

    // Rows differ along WHY the upsert is refused: a statistic no format admits, one this
    // format does not admit (the same statistic the live stream legitimately carries), both
    // at once, and a name no statistic family produces.
    let refused: Vec<(&str, Value)> = vec![
        (
            "a basic statistic on the same format",
            statistics_request("upserted-stream", "json", &["Average"]),
        ),
        (
            "an abbreviation an OpenTelemetry stream cannot carry",
            statistics_request("upserted-stream", "opentelemetry1.0", &["TM(10%:90%)"]),
        ),
        (
            "a basic statistic and a new format",
            statistics_request("upserted-stream", "opentelemetry0.7", &["Sum"]),
        ),
        (
            "a statistic no family produces",
            statistics_request("upserted-stream", "json", &["Median"]),
        ),
    ];

    for (case, update) in refused {
        let (status, body) = call(&router, "PutMetricStream", &update).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?}: expected a 400 InvalidParameterValueException, got {status} {body}"
        );

        let after = ok(
            &router,
            "GetMetricStream",
            &json!({"Name": "upserted-stream"}),
        )
        .await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused upsert must leave the stream exactly as it was, got \
             {after}"
        );
    }

    // An upsert the rule admits still takes effect, so the refusals above are the
    // format/statistic relation and not a blanket refusal of the member.
    let accepted = statistics_request("upserted-stream", "opentelemetry1.0", &["p99"]);
    ok(&router, "PutMetricStream", &accepted).await;
    let after = ok(
        &router,
        "GetMetricStream",
        &json!({"Name": "upserted-stream"}),
    )
    .await;
    assert_eq!(
        (
            after.get("OutputFormat"),
            after.get("StatisticsConfigurations")
        ),
        (
            accepted.get("OutputFormat"),
            accepted.get("StatisticsConfigurations")
        ),
        "case \"an admitted upsert\": expected the new configuration, got {after}"
    );
}
