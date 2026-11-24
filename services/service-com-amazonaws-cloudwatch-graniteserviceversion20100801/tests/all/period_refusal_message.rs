// SPDX-License-Identifier: CC0-1.0

//! The TEXT a metric read is refused with for a `Period` that is not a granularity a metric
//! can be read at, and the granularities it accepts.
//!
//! The rule under test: **a `Period` outside the granularities a metric is stored at is refused
//! against the requirement its own span broke — a period SHORTER than a minute against the set of
//! sub-minute granularities there are, and a period of a minute or more against the multiple of a
//! minute it has to be — with nothing of the submitted value in the sentence**, while every
//! documented granularity (the sub-minute divisions of a minute, and any multiple of 60) is
//! accepted. `GetMetricStatisticsInput$Period` and `MetricStat$Period`
//! document the same set ("For metrics with regular resolution, a period can be as short as one
//! minute (60 seconds) and must be a multiple of 60. For high-resolution metrics […] the period
//! can be 1, 5, 10, 20, 30, 60, or any multiple of 60"), so both readers answer the same
//! requirement: below a minute the only granularities there are are the enumerated ones, and from
//! a minute up every granularity is a whole number of minutes.
//!
//! WHICH CLASS states it is a property of the reader: the aggregating read declares this
//! service's parameter faults and answers `InvalidParameterValue`, while the retrieving read
//! declares none of them and keeps the request-validation class — naming the member path the
//! period arrived on and ending in that layer's terminator.
//!
//! The rows differ along the period itself — each published sub-minute granularity, values on
//! either side of them, a value just under and just over a minute, and one between two
//! multiples of 60 — and along which reader the period arrived on. The period a recorded
//! request happened to carry is one row among them.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, now_secs, ok};

/// The sentence CloudWatch refuses a period of a minute or more that is no whole number of
/// minutes with: the member and the multiple it was not, and nothing of the submitted value.
const MULTIPLE_REFUSAL: &str = "The parameter Period must be a multiple of 60.";

/// The sentence it refuses a period SHORTER than a minute that is none of the sub-minute
/// granularities with: the member and the whole set of them, and nothing of the submitted
/// value. The set is the one `GetMetricStatisticsInput$Period` / `MetricStat$Period` enumerate
/// ("the period can be 1, 5, 10, 20, 30, 60, or any multiple of 60") below a minute.
const SET_REFUSAL: &str = "The parameter Period must be a value in the set [1, 5, 10, 20, 30].";

/// The wire error code of that refusal, as an SDK of this service sees it.
const REFUSAL_CODE: &str = "InvalidParameterValueException";

/// A `GetMetricStatistics` request reading `period`-wide windows of the ten periods before now.
///
/// The range is ten periods whatever the row's period, so no row is refused for the range
/// instead of the period: a short period keeps its datapoints readable (they are kept for
/// three hours below a minute) and a long one stays inside the datapoints one call returns.
fn statistics_request(period: i64) -> Value {
    #[allow(clippy::cast_precision_loss)]
    let span = (period * 10) as f64;
    let now = now_secs();
    json!({
        "Namespace": "Custom/Periods",
        "MetricName": "Latency",
        "StartTime": now - span,
        "EndTime": now,
        "Period": period,
        "Statistics": ["Sum"],
    })
}

/// A `GetMetricData` request reading the same series at the same granularity, with the period
/// on the query's `MetricStat`.
fn data_request(period: i64) -> Value {
    #[allow(clippy::cast_precision_loss)]
    let span = (period * 10) as f64;
    let now = now_secs();
    json!({
        "MetricDataQueries": [{
            "Id": "m1",
            "MetricStat": {
                "Metric": {"Namespace": "Custom/Periods", "MetricName": "Latency"},
                "Period": period,
                "Stat": "Sum",
            },
        }],
        "StartTime": now - span,
        "EndTime": now,
    })
}

#[tokio::test]
async fn a_period_outside_the_readable_granularities_is_refused_with_the_requirement_it_broke() {
    let router = build_router();
    // (the period, the requirement its refusal states): a period shorter than a minute that is
    // none of the sub-minute granularities is told the SET of them, and a period of a minute or
    // more that is no whole number of minutes is told the multiple it was not. The rows walk
    // both sides of every published granularity and both sides of the minute boundary.
    // Source for the granularities: `GetMetricStatisticsInput$Period` / `MetricStat$Period`.
    let periods: [(i64, &str); 9] = [
        (2, SET_REFUSAL),
        (4, SET_REFUSAL),
        (7, SET_REFUSAL),
        (11, SET_REFUSAL),
        (21, SET_REFUSAL),
        (31, SET_REFUSAL),
        (45, SET_REFUSAL),
        (59, SET_REFUSAL),
        (61, MULTIPLE_REFUSAL),
    ];

    for (period, expected) in periods {
        let (status, body) = call(&router, "GetMetricStatistics", &statistics_request(period)).await;
        assert_eq!(
            status, 400,
            "case period {period} on GetMetricStatistics: expected HTTP 400, got {status} with \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            REFUSAL_CODE,
            "case period {period} on GetMetricStatistics: expected {REFUSAL_CODE}, got {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case period {period} on GetMetricStatistics: expected the fixed refusal \
             {expected:?}, got {message:?}"
        );

        // The retrieving read states the SAME requirement, in the class and the words of the
        // layer that states it there: it declares none of this service's parameter faults, so
        // every refusal about its input is the request-validation class's — naming the member
        // path the period arrived on, since one request carries a period per query, with that
        // layer's padded brackets and its terminator (`crate::metric_data` tables the class).
        let (status, body) = call(&router, "GetMetricData", &data_request(period)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, DATA_REFUSAL_CODE),
            "case period {period} on GetMetricData: expected {DATA_REFUSAL_CODE}, got {status} \
             with {body}"
        );
        let stated = error_message(&body);
        let expected = if period < ONE_MINUTE {
            format!(
                "The parameter {DATA_PERIOD_PATH} must be a value in the set [ 1, 5, 10, 20, 30 \
                 ].\n"
            )
        } else {
            format!("The parameter {DATA_PERIOD_PATH} must be a multiple of 60.\n")
        };
        assert_eq!(
            stated, expected,
            "case period {period} on GetMetricData: expected {expected:?}, got {stated:?}"
        );
    }

    // A period far above a minute that is not a whole number of them is told the same
    // requirement, so the second sentence is the rule and not the one value just over a minute.
    for period in [121, 3_599, 86_401] {
        let (_, body) = call(&router, "GetMetricStatistics", &statistics_request(period)).await;
        assert_eq!(
            error_message(&body),
            MULTIPLE_REFUSAL,
            "case period {period}: expected {MULTIPLE_REFUSAL:?}, got {body}"
        );
    }
}

/// The class the RETRIEVING read states that requirement in: it declares none of this service's
/// parameter faults (`GetMetricData` models `InvalidNextToken` alone), so every refusal about its
/// input keeps the request-validation class, whose shape an awsJson body names.
const DATA_REFUSAL_CODE: &str = "ValidationException";

/// The member path such a refusal names: the period of the first query's `MetricStat`, which is
/// where [`data_request`] puts it.
const DATA_PERIOD_PATH: &str = "MetricDataQueries.member.1.MetricStat.Period";

#[tokio::test]
async fn every_documented_granularity_is_read_rather_than_refused() {
    let router = build_router();
    ok(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": "Custom/Periods",
            "MetricData": [{"MetricName": "Latency", "Value": 1.0, "StorageResolution": 1}],
        }),
    )
    .await;

    // Every published sub-minute granularity, the minute itself, and multiples of it up to an
    // hour: the values the two members document as readable.
    let periods: [i64; 9] = [1, 5, 10, 20, 30, 60, 120, 300, 3_600];

    for period in periods {
        for (reader, request) in [
            ("GetMetricStatistics", statistics_request(period)),
            ("GetMetricData", data_request(period)),
        ] {
            let (status, body) = call(&router, reader, &request).await;
            assert_eq!(
                status, 200,
                "case period {period} on {reader}: a documented granularity is read, got \
                 {status} with {body}"
            );
        }
    }
}

/// The granularities shorter than a minute a metric may be read at, from
/// `GetMetricStatisticsInput$Period` / `MetricStat$Period` ("the period can be 1, 5, 10, 20,
/// 30, 60, or any multiple of 60").
const SUB_MINUTE_GRANULARITIES: [i64; 5] = [1, 5, 10, 20, 30];

/// A minute in seconds: the boundary that decides WHICH requirement a refused period is stated
/// against, and the step every granularity from it up has to be a whole number of.
const ONE_MINUTE: i64 = 60;

#[tokio::test]
async fn which_requirement_a_refused_period_is_stated_against_follows_the_minute_boundary() {
    let router = build_router();

    // Every period from one second to just over two minutes, so no row is hand-picked: each is
    // either a granularity the member documents (read, not refused) or a refusal stated against
    // the requirement its own span broke.
    for period in 1..=(2 * ONE_MINUTE + 3) {
        let documented =
            SUB_MINUTE_GRANULARITIES.contains(&period) || period % ONE_MINUTE == 0;
        let (status, body) = call(&router, "GetMetricStatistics", &statistics_request(period)).await;
        if documented {
            assert_eq!(
                status, 200,
                "case period {period}: a documented granularity is read, got {status} with {body}"
            );
            continue;
        }
        let expected = if period < ONE_MINUTE {
            SET_REFUSAL
        } else {
            MULTIPLE_REFUSAL
        };
        let message = error_message(&body);
        assert_eq!(
            (status, message.as_str()),
            (400, expected),
            "case period {period}: expected HTTP 400 with {expected:?}, got {status} with {body}"
        );
    }
}

#[tokio::test]
async fn each_sentence_is_fixed_across_the_periods_of_its_own_class() {
    let router = build_router();

    // Two different periods of each class: the sentence a class is refused with carries nothing
    // of the submitted value, so the rows of one class cannot be told apart by their answer.
    for (class, periods) in [
        ("shorter than a minute", [7_i64, 41]),
        ("a minute or more", [97_i64, 86_401]),
    ] {
        let mut answered: Vec<String> = Vec::new();
        for period in periods {
            let (status, body) =
                call(&router, "GetMetricStatistics", &statistics_request(period)).await;
            assert_eq!(
                status, 400,
                "case {class:?} / period {period}: expected HTTP 400, got {status} with {body}"
            );
            answered.push(error_message(&body));
        }
        assert_eq!(
            answered[0], answered[1],
            "case {class:?}: both periods of one class are refused with the same fixed \
             sentence, got {answered:?}"
        );
        for (period, message) in periods.iter().zip(answered.iter()) {
            assert!(
                !message.contains(&period.to_string()),
                "case {class:?} / period {period}: the sentence must carry nothing of the \
                 submitted value, got {message:?}"
            );
        }
    }
}
