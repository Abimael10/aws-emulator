// SPDX-License-Identifier: CC0-1.0

//! The GRANULARITY a client reads the `Errors` series of a failing function at, and the UNIT it
//! can read it under — the `AWS/Lambda` series CloudWatch answers for, with CloudWatch mounted
//! beside Lambda on one engine.
//!
//! The RULE asserted over the tables below: an invocation that came to a function error is
//! published as one observation of 1, in `Count`, on that function's own `Errors` series, so
//! `GetMetricStatistics` over a window covering those invocations answers with a datapoint for
//! every period that holds one — whatever `Period` the read lays over the window — and the `Sum`
//! totalled over those datapoints is the number of invocations that failed, each datapoint
//! reporting the unit the observations carry. A read filtered on another unit matches no
//! observation of the series and so reports nothing at all: "If you specify a unit that does not
//! match the data collected, the results of the operation are null"
//! (`GetMetricStatisticsInput$Unit`).
//!
//! The rows differ along what the rule generalises over: the `Period` the window is read at, and
//! the unit the read filters on. The granularities are the ones the member admits for a series of
//! this resolution — "For metrics with regular resolution, a period can be as short as one minute
//! (60 seconds) and must be a multiple of 60" (`GetMetricStatisticsInput$Period`) — read at that
//! published floor of a minute and at coarser multiples of one, plus the two values just outside
//! that rule, which are refused: a period naming no span of time at all, and one that is not a
//! whole number of minutes. Every expected figure is computed from the traffic the test itself
//! makes rather than taken from a recording.
//!
//! Both tables are asserted over the SAME traffic, in one test: the reads are what this suite is
//! about, and a function's invocations are the dearest thing in it to perform, so they are
//! performed once and read many times.
//!
//! Every metric, dimension and unit name here is a test-side literal, taken from the AWS Lambda
//! metric reference rather than from this crate's constants — the imported names below are that
//! sibling suite's literals, not the service's — so a series published under a name no AWS reader
//! would ask for fails here instead of agreeing with itself. The harness — the two services on
//! one engine, the function brought to `Active`, one invocation that fails or returns, the
//! dimension sets an invocation is reported on and the total of a statistic over an answer — is
//! `invocation_error_indicator.rs`'s, the suite over what the `Errors` series SAYS; what is this
//! suite's own is how that series is READ.
//!
//! Every wait here is for a CONDITION — the series holding what the invocations owe it — looked at
//! again until it holds or a generous ceiling passes, so how fast the host runs decides nothing
//! asserted here.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invocation_error_indicator::{
    dimension_sets, emulator, given_function, invoke, now_secs, total, COUNT_UNIT, ERRORS_SERIES,
    LOOK_INTERVAL, NAMESPACE, PUBLICATION_BUDGET, WINDOW_SECS,
};

/// The granularity the series is waited for at, in seconds: the one-minute granularity Lambda's
/// own observations are stored at.
const STORED_PERIOD_SECS: i64 = 60;

/// `GetMetricStatistics` over the `Errors` series of `dimensions`, at `period` seconds of
/// granularity and filtered on `unit` when one is given: the status and the whole answer.
async fn read_errors(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
    period: i64,
    unit: Option<&str>,
) -> (u16, Value) {
    let now = now_secs();
    let mut request = json!({
        "Namespace": NAMESPACE,
        "MetricName": ERRORS_SERIES,
        "Dimensions": dimensions,
        "StartTime": now - WINDOW_SECS,
        "EndTime": now + WINDOW_SECS,
        "Period": period,
        "Statistics": ["Sum", "SampleCount"],
    });
    if let Some(unit) = unit {
        request["Unit"] = json!(unit);
    }
    let (status, answer) =
        crate::kinesis_event_source_delivery::cloudwatch(router, "GetMetricStatistics", &request)
            .await;
    assert!(
        answer.is_object(),
        "case {case:?}: GetMetricStatistics at Period {period} must answer a body, got {answer}"
    );
    (status, answer)
}

/// The `Datapoints` of a read that must succeed.
async fn error_datapoints(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
    period: i64,
    unit: Option<&str>,
) -> Vec<Value> {
    let (status, answer) = read_errors(router, case, dimensions, period, unit).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics({ERRORS_SERIES}, {dimensions}) at Period {period} \
         under {unit:?} answered {status} {answer}"
    );
    assert_eq!(
        answer["Label"],
        json!(ERRORS_SERIES),
        "case {case:?}: the read must be labelled with the metric it aggregated, got {answer}"
    );
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Look at the series at the granularity it is stored at until it reports on `wanted`
/// invocations. Driven off what has ARRIVED rather than off a span of wall clock, and the series
/// having fallen short of the invocations by the ceiling is itself the failure, said here rather
/// than left to whichever assertion reads the short series next.
async fn await_invocations_reported(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
    wanted: f64,
) {
    let deadline = Instant::now() + PUBLICATION_BUDGET;
    loop {
        let points =
            error_datapoints(router, case, dimensions, STORED_PERIOD_SECS, Some(COUNT_UNIT)).await;
        let reported = total(&points, "SampleCount");
        if reported >= wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {ERRORS_SERIES} of {dimensions} must come to report on each of the \
             {wanted} invocation(s), got {reported} over {points:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The invocations the reads below are made over: the ones that fail are what the window's `Sum`
/// must come to, whatever period the window is laid in and whichever unit the read names.
const RUNS: &[bool] = &[true, false, true];

/// The granularities the window is read at, each a value `GetMetricStatisticsInput$Period` admits
/// for a series of this resolution: its published floor of one minute, and coarser whole minutes.
const PERIODS: &[i64] = &[60, 120, 300, 900, 3600];

/// The granularities just outside that rule, each with the reason it names no period this series
/// can be read at: zero names no span of time ("range min 1"), and a value that is not a whole
/// number of minutes is finer than a regular-resolution series is stored at ("must be a multiple
/// of 60").
const REFUSED_PERIODS: &[i64] = &[0, 90];

/// The unit filters the series is read under, with whether the read can match the `Count`
/// observations Lambda publishes: the units are `StandardUnit` values
/// (`GetMetricStatisticsInput$Unit`).
const UNIT_FILTERS: &[(Option<&str>, bool)] = &[
    (None, true),
    (Some("Count"), true),
    (Some("Milliseconds"), false),
    (Some("Bytes"), false),
    (Some("Count/Second"), false),
];

/// RULE: the `Errors` observations a function's failed invocations publish are aggregated into
/// whatever `Period` the read lays over the window, so the `Sum` totalled over the answer is the
/// number of invocations that failed at every granularity the member admits and under the unit
/// those observations carry — while a granularity the series is not stored at is refused, and a
/// read naming another unit matches nothing.
#[tokio::test]
async fn the_failed_invocation_count_is_read_at_every_granularity_and_only_in_its_own_unit() {
    let case = "three invocations, two of them failing";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let function = "errors-granularity";
    given_function(&router, &engine, case, function).await;

    for raises in RUNS {
        invoke(&router, case, function, *raises).await;
    }

    #[allow(clippy::cast_precision_loss)]
    let invocations = RUNS.len() as f64;
    #[allow(clippy::cast_precision_loss)]
    let failures = RUNS.iter().filter(|raises| **raises).count() as f64;

    // Both of the dimension sets an invocation is reported on: a reader asking for the function's
    // failures and one asking for the invoked resource's are asking for two different series.
    for dimensions in dimension_sets(function) {
        await_invocations_reported(&router, case, &dimensions, invocations).await;

        for period in PERIODS {
            let points = error_datapoints(&router, case, &dimensions, *period, None).await;
            assert!(
                !points.is_empty(),
                "case {case:?}: the window holds {failures} failure(s), so the read of \
                 {dimensions} at Period {period} must answer with at least one datapoint, got \
                 {points:?}"
            );
            let sum = total(&points, "Sum");
            assert_eq!(
                sum, failures,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} read at Period {period} must sum \
                 to the {failures} failure(s) of the window, got {sum} over {points:?}"
            );
            let reported_runs = total(&points, "SampleCount");
            assert_eq!(
                reported_runs, invocations,
                "case {case:?}: {ERRORS_SERIES} of {dimensions} read at Period {period} must \
                 report on each of the {invocations} invocation(s), got {reported_runs} over \
                 {points:?}"
            );
            for point in &points {
                assert_eq!(
                    point["Unit"],
                    json!(COUNT_UNIT),
                    "case {case:?}: every {ERRORS_SERIES} datapoint of the read at Period \
                     {period} must report the unit the observations carry, got {point:?}"
                );
                assert!(
                    point["Timestamp"].is_string() || point["Timestamp"].is_number(),
                    "case {case:?}: every {ERRORS_SERIES} datapoint of the read at Period \
                     {period} must carry the instant of its window, got {point:?}"
                );
            }
        }

        // Just outside the rule the member states: a granularity this series cannot be read at is
        // refused rather than answered with an aggregate over windows it is not stored in.
        for period in REFUSED_PERIODS {
            let (status, answer) = read_errors(&router, case, &dimensions, *period, None).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a Period of {period} is no granularity this series can be read \
                 at, so the read must be refused, got {status} {answer}"
            );
        }

        // The unit a read may select the observations by is the one they were published with:
        // naming it, or naming none, reads the window's failures; naming any other reads nothing.
        for (unit, matches) in UNIT_FILTERS {
            let points =
                error_datapoints(&router, case, &dimensions, STORED_PERIOD_SECS, *unit).await;
            if *matches {
                let sum = total(&points, "Sum");
                assert_eq!(
                    sum, failures,
                    "case {case:?}: the read of {dimensions} under {unit:?} must sum to the \
                     {failures} failure(s) of the window, got {sum} over {points:?}"
                );
            } else {
                assert!(
                    points.is_empty(),
                    "case {case:?}: no observation of {ERRORS_SERIES} carries {unit:?}, so the \
                     read of {dimensions} under it must answer with no datapoint, got {points:?}"
                );
            }
        }
    }
}
