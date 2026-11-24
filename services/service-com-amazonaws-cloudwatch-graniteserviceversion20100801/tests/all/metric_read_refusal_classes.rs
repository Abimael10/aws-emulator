// SPDX-License-Identifier: CC0-1.0

//! Which error CLASS a metric read's parameter refusal carries, and the sentence it carries it
//! with.
//!
//! The rule under test: **which class a request-value refusal carries is a property of the
//! OPERATION, not of the violation** — a read whose refusals CloudWatch words as one of the
//! service's own parameter faults answers that fault, in the service's own fixed sentence, and a
//! read whose refusals come from the request-validation layer in front of the service answers
//! that layer's class with that layer's clause. So:
//!
//! * the metric LISTING refuses a filter it cannot use as the parameter-value fault its model
//!   declares (`smithy_query(query="operation",operation="ListMetrics")`, whose only client
//!   error that is) — a pagination cursor it did not mint, and a filter supplied with no
//!   characters — while a required member the request left out stays the missing-parameter
//!   fault;
//! * the two readers of a metric's datapoints refuse a `Period` that names no span of time as
//!   that same parameter-value fault, against the bound a span has to be above;
//! * a read whose range begins exactly where it ends is refused by the request-validation layer
//!   (the class the whole SHAPE of a request is judged under), and so is one whose range runs
//!   backwards on the reader that declares no parameter fault — stated as the order the two
//!   members have to be in, where the reader that declares one answers that fault instead;
//! * the ALARM-HISTORY read refuses a date range that runs backwards as the parameter-value
//!   fault, naming both instants the request carried;
//! * the metric DATA read's refusals are the request-validation layer's throughout, so a
//!   required member one of its queries left out is reported as the constraint it broke rather
//!   than as a parameter fault.
//!
//! Each table's rows differ along what the rule generalises over — which token, which filter
//! member and at which position in its collection, which non-positive period, which pair of
//! instants, which required member of which query — because none of those decides the class or
//! the sentence. The values a recorded request happened to carry are one row among them.

use serde_json::{json, Value};

use crate::harness::{
    build_router, call, call_in_query_mode, error_code, error_message, now_secs, ok,
};

/// The header the legacy query error code and the fault side are reported in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The local name of the modelled parameter-value fault, as a client reads it off the body.
const PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The local name of the modelled missing-parameter fault.
const MISSING_PARAMETER: &str = "MissingRequiredParameterException";

/// The local name of the request-validation class.
const VALIDATION: &str = "ValidationException";

/// The legacy query code of the parameter-value fault, as the model binds it.
const PARAMETER_VALUE_CODE: &str = "InvalidParameterValue;Sender";

/// The legacy query code of the request-validation class.
const VALIDATION_CODE: &str = "ValidationError;Sender";

/// The namespace the metric rows publish and read under.
const NAMESPACE: &str = "Custom/RefusalClasses";

/// The metric the rows read.
const METRIC: &str = "Latency";

#[tokio::test]
async fn a_pagination_cursor_the_listing_did_not_mint_is_refused_as_an_unusable_parameter() {
    let router = build_router();
    // (case, the token the request carries): the rows differ along the FORM of the value — a
    // word, a symbol run, a padded base64 blob, an odd-length hex string and a very long one —
    // because none of them is a cursor this service minted and the refusal is about that alone.
    let tokens: [(&str, &str); 6] = [
        ("a word", "not-a-token"),
        ("a symbol run", "###"),
        ("a padded base64 blob", "YWJjZA=="),
        ("an odd number of hex digits", "abc"),
        ("hex digits that spell no text", "ffffff"),
        ("a long blob", &"zz".repeat(200)),
    ];

    for (case, token) in tokens {
        let (status, headers, body) = call_in_query_mode(
            &router,
            "ListMetrics",
            &json!({"Namespace": NAMESPACE, "NextToken": token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, PARAMETER_VALUE,
            "case {case:?}: expected {PARAMETER_VALUE}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, "Request parameter NextToken is invalid",
            "case {case:?}: expected the fixed sentence for an unreadable parameter, got \
             {message:?}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, PARAMETER_VALUE_CODE,
            "case {case:?}: expected the legacy parameter-value code, got {reported:?}"
        );
    }

    // The refusal is about a token the service could not have MINTED, so a value of the form it
    // mints its cursors in still names a page: the listing answers it rather than refusing it.
    // (`Custom/RefusalClasses` hex-encoded, which is the cursor form `ListMetrics` pages by.)
    let minted = "437573746f6d2f5265667573616c436c6173736573";
    let answered = ok(
        &router,
        "ListMetrics",
        &json!({"Namespace": NAMESPACE, "NextToken": minted}),
    )
    .await;
    assert!(
        answered.get("Metrics").is_some(),
        "a cursor of the form this service mints is paged from, got {answered}"
    );
}

#[tokio::test]
async fn a_listing_filter_supplied_with_no_characters_is_refused_as_an_unusable_value() {
    let router = build_router();
    // (case, request, the request member path the refusal names): the rows differ along WHICH
    // filter carries no characters and, for one inside a collection, along its position — the
    // index comes from the request, and the class and the sentence do not change with either.
    let rows: [(&str, Value, &str); 5] = [
        (
            "the namespace a listing filters by",
            json!({"Namespace": ""}),
            "Namespace",
        ),
        (
            "the metric name a listing filters by",
            json!({"MetricName": ""}),
            "MetricName",
        ),
        (
            "the value of the only dimension filter",
            json!({"Dimensions": [{"Name": "Probe", "Value": ""}]}),
            "Dimensions.member.1.Value",
        ),
        (
            "the value of the second dimension filter",
            json!({"Dimensions": [
                {"Name": "Probe", "Value": "north"},
                {"Name": "Shard", "Value": ""},
            ]}),
            "Dimensions.member.2.Value",
        ),
        (
            "the name of the second dimension filter",
            json!({"Dimensions": [
                {"Name": "Probe", "Value": "north"},
                {"Name": "", "Value": "south"},
            ]}),
            "Dimensions.member.2.Name",
        ),
    ];

    for (case, request, parameter) in rows {
        let (status, body) = call(&router, "ListMetrics", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, PARAMETER_VALUE,
            "case {case:?}: expected {PARAMETER_VALUE}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = format!("The value {{1}} for parameter {parameter} is invalid.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // A dimension filter the request did not give a NAME at all is the other class: a parameter
    // it supplied no value for, which is the missing-parameter fault — so the two are told apart
    // by the code as well as by the sentence.
    for (case, dimensions, parameter) in [
        (
            "an omitted name on the only filter",
            json!([{"Value": "north"}]),
            "Dimensions.member.1.Name",
        ),
        (
            "an omitted name on the second filter",
            json!([{"Name": "Probe", "Value": "north"}, {"Value": "south"}]),
            "Dimensions.member.2.Name",
        ),
    ] {
        let (status, body) =
            call(&router, "ListMetrics", &json!({"Dimensions": dimensions})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, MISSING_PARAMETER,
            "case {case:?}: expected {MISSING_PARAMETER}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = format!("The parameter {parameter} is required.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_period_that_names_no_span_of_time_is_refused_against_the_bound_it_broke() {
    let router = build_router();
    // The periods that name no span at all: zero, and negative values — including one that is a
    // whole number of minutes backwards and one at the far end of the member's own type, because
    // what the sentence states is the bound a span has to be above and not the value submitted.
    let periods: [i64; 5] = [0, -1, -30, -60, i64::from(i32::MIN)];

    for period in periods {
        let (status, headers, body) = call_in_query_mode(
            &router,
            "GetMetricStatistics",
            &statistics_request(period),
        )
        .await;
        assert_eq!(
            status, 400,
            "case period {period}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, PARAMETER_VALUE,
            "case period {period}: expected {PARAMETER_VALUE}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, "The parameter Period must be greater than 0.",
            "case period {period}: expected the bound a span has to be above, got {message:?}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, PARAMETER_VALUE_CODE,
            "case period {period}: expected the legacy parameter-value code, got {reported:?}"
        );
    }

    // The shortest span there IS, and the first whole minute, are read rather than refused: the
    // refusal above is about a value naming no span, not about a short one.
    for period in [1_i64, 60] {
        let (status, body) =
            call(&router, "GetMetricStatistics", &statistics_request(period)).await;
        assert_eq!(
            status, 200,
            "case period {period}: a documented granularity is read, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_range_that_begins_where_it_ends_is_refused_by_the_shape_of_the_request() {
    let router = build_router();
    let now = now_secs().floor();
    // (case, the instant both bounds carry): the rows differ along WHERE the window sits —
    // now, a whole hour back, and the epoch — because what is refused is the two bounds being
    // one instant.
    let instants: [(&str, f64); 3] = [
        ("the current instant", now),
        ("an instant an hour back", now - 3_600.0),
        ("the epoch", 0.0),
    ];

    for (case, instant) in instants {
        for (reader, request) in [
            (
                "GetMetricStatistics",
                window_statistics_request(instant, instant),
            ),
            ("GetMetricData", window_data_request(instant, instant)),
        ] {
            let (status, headers, body) = call_in_query_mode(&router, reader, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {reader}: expected HTTP 400, got {status} with {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, VALIDATION,
                "case {case:?} on {reader}: expected {VALIDATION}, got {code:?} in {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, "The parameter StartTime must not equal parameter EndTime.\n",
                "case {case:?} on {reader}: expected the request-validation sentence, got \
                 {message:?}"
            );
            let reported = headers
                .get(QUERY_ERROR_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            assert_eq!(
                reported, VALIDATION_CODE,
                "case {case:?} on {reader}: expected the legacy validation code, got {reported:?}"
            );
        }

        // A window that runs BACKWARDS is the other sentence: the order the two members have to
        // be in. WHICH CLASS states it is a property of the reader — the aggregating read
        // declares this service's parameter faults and answers one of them, while the retrieving
        // read declares none (`GetMetricData` models `InvalidNextToken` alone) and keeps the
        // request-validation class for every refusal about its input, terminator and all.
        for (reader, request, code, message) in [
            (
                "GetMetricStatistics",
                window_statistics_request(instant, instant - 60.0),
                PARAMETER_VALUE,
                "The parameter StartTime must be less than the parameter EndTime.",
            ),
            (
                "GetMetricData",
                window_data_request(instant, instant - 60.0),
                VALIDATION,
                "The parameter EndTime must be greater than StartTime.\n",
            ),
        ] {
            let (status, body) = call(&router, reader, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} on {reader}: expected HTTP 400, got {status} with {body}"
            );
            let answered = error_code(&body);
            assert_eq!(
                answered, code,
                "case {case:?} on {reader}: expected {code}, got {answered:?} in {body}"
            );
            let stated = error_message(&body);
            assert_eq!(
                stated, message,
                "case {case:?} on {reader}: expected the order of the two members, got \
                 {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_alarm_history_range_that_runs_backwards_names_both_instants() {
    let router = build_router();
    // (case, StartDate, EndDate, the two instants the sentence states). The instants are
    // written in the form CloudWatch writes one inside a refusal — the weekday and month in
    // English, the day of the month zero-padded, the zone, then the year — and the rows span
    // the epoch itself, a leap day, the end of a day and a whole year apart, because every
    // field of that form is part of the answer.
    let rows: [(&str, f64, f64, &str, &str); 4] = [
        (
            "a day's end before its start",
            86_399.0,
            0.0,
            "Thu Jan 01 23:59:59 UTC 1970",
            "Thu Jan 01 00:00:00 UTC 1970",
        ),
        (
            "a leap day before the epoch",
            951_782_400.0,
            0.0,
            "Tue Feb 29 00:00:00 UTC 2000",
            "Thu Jan 01 00:00:00 UTC 1970",
        ),
        (
            "a year later than the end",
            1_767_225_600.0,
            1_000_000_000.0,
            "Thu Jan 01 00:00:00 UTC 2026",
            "Sun Sep 09 01:46:40 UTC 2001",
        ),
        (
            "one second inverted",
            1_000_000_001.0,
            1_000_000_000.0,
            "Sun Sep 09 01:46:41 UTC 2001",
            "Sun Sep 09 01:46:40 UTC 2001",
        ),
    ];

    for (case, start, end, written_start, written_end) in rows {
        let (status, headers, body) = call_in_query_mode(
            &router,
            "DescribeAlarmHistory",
            &json!({"StartDate": start, "EndDate": end}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, PARAMETER_VALUE,
            "case {case:?}: expected {PARAMETER_VALUE}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = format!("Start date {written_start} must be before end date {written_end}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, PARAMETER_VALUE_CODE,
            "case {case:?}: expected the legacy parameter-value code, got {reported:?}"
        );
    }

    // A range that is not inverted is answered: the refusal is about the order of the two
    // dates, so a start before its end, and a start AT its end, both name a range to report.
    for (case, start, end) in [
        ("a start before its end", 0.0_f64, 86_399.0_f64),
        ("a start at its end", 1_000_000_000.0, 1_000_000_000.0),
    ] {
        let answered = ok(
            &router,
            "DescribeAlarmHistory",
            &json!({"StartDate": start, "EndDate": end}),
        )
        .await;
        assert!(
            answered.get("AlarmHistoryItems").is_some(),
            "case {case:?}: a range in order is reported, got {answered}"
        );
    }
}

#[tokio::test]
async fn the_metric_data_read_states_its_clauses_with_no_value_half() {
    let router = build_router();
    // (case, the queries the request carries, the member path the clause names, the requirement
    // it states): the rows differ along WHICH required member of a query is left out, along the
    // POSITION of the query that leaves it out, and along the CONSTRAINT broken — because this
    // read states every one of them in the request-validation class, with the member path and
    // the requirement and no value at all.
    let rows: [(&str, Value, &str, &str); 5] = [
        (
            "the granularity of the only query",
            json!([{"Id": "m1", "MetricStat": {
                "Metric": {"Namespace": NAMESPACE, "MetricName": METRIC},
                "Stat": "Sum",
            }}]),
            "metricDataQueries.1.member.metricStat.period",
            "Member must not be null",
        ),
        (
            "the granularity of the second query",
            json!([
                metric_stat_query("m1", 60),
                {"Id": "m2", "MetricStat": {
                    "Metric": {"Namespace": NAMESPACE, "MetricName": METRIC},
                    "Stat": "Sum",
                }},
            ]),
            "metricDataQueries.2.member.metricStat.period",
            "Member must not be null",
        ),
        (
            "the statistic of the only query",
            json!([{"Id": "m1", "MetricStat": {
                "Metric": {"Namespace": NAMESPACE, "MetricName": METRIC},
                "Period": 60,
            }}]),
            "metricDataQueries.1.member.metricStat.stat",
            "Member must not be null",
        ),
        (
            "the metric of the only query",
            json!([{"Id": "m1", "MetricStat": {"Period": 60, "Stat": "Sum"}}]),
            "metricDataQueries.1.member.metricStat.metric",
            "Member must not be null",
        ),
        (
            "a granularity below the bound its member declares",
            json!([metric_stat_query("m1", 0)]),
            "metricDataQueries.1.member.metricStat.period",
            "Member must have value greater than or equal to 1",
        ),
    ];

    let now = now_secs().floor();
    for (case, queries, parameter, requirement) in rows {
        let request = json!({
            "MetricDataQueries": queries,
            "StartTime": now - 600.0,
            "EndTime": now,
        });
        let (status, headers, body) = call_in_query_mode(&router, "GetMetricData", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = format!(
            "1 validation error detected: Value at '{parameter}' failed to satisfy constraint: \
             {requirement}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, VALIDATION_CODE,
            "case {case:?}: expected the legacy validation code, got {reported:?}"
        );
    }

    // The same read SERVES a request whose queries carry every required member, so the rows
    // above are refused for the member they left out and not for the shape of the call.
    let answered = ok(
        &router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [metric_stat_query("m1", 60)],
            "StartTime": now - 600.0,
            "EndTime": now,
        }),
    )
    .await;
    assert!(
        answered.get("MetricDataResults").is_some(),
        "a complete query is answered, got {answered}"
    );
}

#[tokio::test]
async fn a_collection_over_its_cap_is_refused_naming_the_collection_and_the_cap() {
    let router = build_router();
    // (case, request, the collection the refusal names, the cap it states). The caps are the
    // members' own declared maxima — `Statistics` 1..5, `ExtendedStatistics` 1..10 and
    // `Dimensions` 0..30 (`smithy_query(query="operation",operation="GetMetricStatistics")`) —
    // so the rows differ along which collection overflows and by how much.
    let statistic_names = ["SampleCount", "Average", "Sum", "Minimum", "Maximum"];
    // Six entries of a list whose member admits five values: the sixth repeats one, which is
    // what any request past this cap must do (`Statistic` declares exactly those five).
    let six_statistics: Vec<&str> = statistic_names
        .iter()
        .copied()
        .chain(std::iter::once(statistic_names[0]))
        .collect();
    let percentiles: Vec<Value> = (0..11).map(|index| json!(format!("p{index}0.5"))).collect();
    let dimensions: Vec<Value> = (0..31)
        .map(|index| json!({"Name": format!("D{index}"), "Value": "v"}))
        .collect();
    let rows: [(&str, Value, &str, usize); 3] = [
        (
            "one statistic more than the five a read may ask for",
            merged_statistics(json!({"Statistics": six_statistics})),
            "Statistics",
            5,
        ),
        (
            "one percentile more than the ten a read may ask for",
            merged_statistics(json!({"ExtendedStatistics": percentiles})),
            "ExtendedStatistics",
            10,
        ),
        (
            "one dimension more than a metric can carry",
            merged_statistics(json!({"Dimensions": dimensions})),
            "Dimensions",
            30,
        ),
    ];

    for (case, request, collection, cap) in rows {
        let (status, headers, body) =
            call_in_query_mode(&router, "GetMetricStatistics", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, PARAMETER_VALUE,
            "case {case:?}: expected {PARAMETER_VALUE}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected =
            format!("The collection {collection} must not have a size greater than {cap}.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, PARAMETER_VALUE_CODE,
            "case {case:?}: expected the legacy parameter-value code, got {reported:?}"
        );
    }

    // A collection AT its cap is read rather than refused: the refusal is about the entry past
    // the maximum, not about the member being filled.
    let answered = ok(
        &router,
        "GetMetricStatistics",
        &merged_statistics(json!({"Statistics": statistic_names})),
    )
    .await;
    assert!(
        answered.get("Datapoints").is_some(),
        "a collection at its cap is read, got {answered}"
    );
}

/// A `GetMetricStatistics` request over the suite's series with `patch`'s members written over
/// it, so a row varies one collection and nothing else.
fn merged_statistics(patch: Value) -> Value {
    let now = now_secs().floor();
    let mut request = window_statistics_request(now - 600.0, now);
    for (member, value) in patch.as_object().into_iter().flatten() {
        request[member] = value.clone();
    }
    request
}

/// One `MetricDataQuery` naming the suite's series at `period` seconds of granularity.
fn metric_stat_query(id: &str, period: i64) -> Value {
    json!({
        "Id": id,
        "MetricStat": {
            "Metric": {"Namespace": NAMESPACE, "MetricName": METRIC},
            "Period": period,
            "Stat": "Sum",
        },
    })
}

/// A `GetMetricStatistics` request reading `period`-wide windows of the ten minutes before now.
///
/// The range is fixed and the period is what a row varies, so a row is refused for its period
/// rather than for the window: ten minutes is inside the datapoints one call returns at every
/// granularity the rows use.
fn statistics_request(period: i64) -> Value {
    let now = now_secs().floor();
    let mut request = window_statistics_request(now - 600.0, now);
    request["Period"] = json!(period);
    request
}

/// A `GetMetricStatistics` request over `[start, end)` at the shortest documented granularity.
fn window_statistics_request(start: f64, end: f64) -> Value {
    json!({
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "StartTime": start,
        "EndTime": end,
        "Period": 60,
        "Statistics": ["Sum"],
    })
}

/// A `GetMetricData` request over the same window, naming the same series.
fn window_data_request(start: f64, end: f64) -> Value {
    json!({
        "MetricDataQueries": [metric_stat_query("m1", 60)],
        "StartTime": start,
        "EndTime": end,
    })
}
