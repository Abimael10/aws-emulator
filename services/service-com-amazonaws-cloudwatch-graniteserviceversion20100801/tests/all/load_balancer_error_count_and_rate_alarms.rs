// SPDX-License-Identifier: CC0-1.0

//! The DIFFERENTIAL two alarms draw over ONE window of a load balancer's published counts: a
//! COUNT of the server errors its target returned, and the SHARE those errors are of the
//! requests that target answered.
//!
//! The rule under test: **a metric alarm is decided by the value its OWN configuration computes
//! over the datapoints stored for the series it reads — the `Statistic` of the one series an
//! alarm that names a metric watches, and the value of the RETURNING query for an alarm
//! configured with a `Metrics` array — compared against that alarm's own `Threshold` by its own
//! `ComparisonOperator`, with a window the series published no datapoint into decided by the
//! alarm's `TreatMissingData` arm rather than by a value; so two alarms over the SAME window of
//! two counted series reach OPPOSITE verdicts whenever the value each of them computes falls on
//! a different side of its own threshold.**
//!
//! Where the series come from is Elastic Load Balancing's — `HTTPCode_Target_5XX_Count` and
//! `RequestCount`, counted per minute under the `LoadBalancer` and `TargetGroup` dimensions
//! (asserted on the publishing side by that service's own suites) — and what a reader then
//! makes of them is CloudWatch's own, which is what is asserted here:
//!
//! * "Use the `Metrics` parameter to create an alarm based on a metric math expression"
//!   (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Metrics")`) is what grades
//!   the rate, and "specify true for the one expression result to use as the alarm"
//!   (`smithy_query(query="doc",shape="MetricDataQuery",member="ReturnData")`) is which query of
//!   the array that is;
//! * "A math expression can use the `Id` of the other metrics or queries to refer to those
//!   metrics" (`smithy_query(query="doc",shape="MetricDataQuery",member="Expression")`) is how
//!   the rate names the two series it is the quotient of;
//! * "The number of data points that must be breaching to trigger the alarm"
//!   (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="DatapointsToAlarm")`) is the
//!   1-of-1 grading every alarm here uses;
//! * "Sets how this alarm is to handle missing data points"
//!   (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`) is what
//!   decides a window a series published nothing into — the hole a `FILL` of that series inside
//!   the expression closes with a zero instead;
//! * `DescribeAlarmHistory` read for `StateUpdate` items
//!   (`smithy_query(query="doc",shape="DescribeAlarmHistoryInput",member="HistoryItemType")`) is
//!   where a client sees WHICH of the two ever paged, which is the whole point of running a rate
//!   alarm beside a count one.
//!
//! The rows differ along what the rule generalises over: the error mix of the window (none at
//! all, a count published as zero, a minority, exactly the share the rate threshold sits at, one
//! request past it, a majority, every request), how many datapoints the window's counts arrive
//! as (so the `Sum` really folds them), the magnitude of the traffic, and whether the error
//! series holds a datapoint for the window at all. Each row therefore pins both directions of
//! both thresholds, including the published bound of the rate threshold and the value one step
//! past it.
//!
//! A reserved namespace is one no caller may write: `PutMetricData` refuses a `Namespace`
//! beginning with `AWS/`, so the counts of such a series reach the store the way the service
//! that owns them puts them there — through this store's own append, one observation per
//! counted request — and everything the suite then asserts is read back over the wire through
//! CloudWatch's own API.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use crate::harness::{
    alarm_history_items, build_router_with_engine, graded_window_stamp, ok, AlarmKind,
    PAST_EVALUATION_DELAY,
};

/// The namespace an Application Load Balancer's request counts are published under.
const NAMESPACE: &str = "AWS/ApplicationELB";

/// The series the count alarm watches and the rate's numerator reads.
const ERRORS: &str = "HTTPCode_Target_5XX_Count";

/// The series the rate's denominator reads.
const REQUESTS: &str = "RequestCount";

/// The period both alarms are evaluated on, the minute these series are counted into.
const PERIOD: i32 = 60;

/// The same period as the seconds the window grid is laid on.
const PERIOD_SECS: f64 = 60.0;

/// The ids the rate array's queries carry, and the expression the returning one derives the
/// error share by: the errors filled with a zero where the series held nothing, over the
/// requests, as a percentage.
const ERRORS_ID: &str = "m1";
const REQUESTS_ID: &str = "m2";
const RATE_ID: &str = "rate";
const RATE_EXPRESSION: &str = "100 * FILL(m1, 0) / m2";

/// The count alarm's threshold: any server error the target returned at all is one too many.
const COUNT_THRESHOLD: f64 = 0.0;

/// The rate alarm's threshold: the share of the traffic that makes an error rate worth paging
/// on, which a low background rate stays under.
const RATE_THRESHOLD: f64 = 50.0;

/// The comparison both alarms grade their own value by.
const GREATER_THAN: &str = "GreaterThanThreshold";

/// How a `StateReason` names the arm that decided a window the series published nothing into:
/// the wording CloudWatch reports for a `notBreaching` treatment.
const NON_BREACHING_ARM: &str = "treated as [NonBreaching]";

/// The missing-data arm both alarms treat a window with no datapoint by.
const NOT_BREACHING: &str = "notBreaching";

/// The unit a counted request is stored under, and the granularity a standard-resolution
/// series is stored at.
const COUNT_UNIT: &str = "Count";
const STANDARD_RESOLUTION_SECS: i32 = 60;

/// The two states a 1-of-1 alarm over a graded window settles in.
const OK: &str = "OK";
const ALARM: &str = "ALARM";

/// How close two computed values have to be to count as the same number.
const TOLERANCE: f64 = 1e-9;

/// How many times the poll below reads the alarms before giving up.
///
/// A client waiting for an evaluated state polls: the first evaluation is owed a span after the
/// write that configured the alarm, so this many reads one third of that span apart leave room
/// for several of those spans without bounding the wait by the wall clock.
const POLL_READS: usize = 40;

/// One row of the table: the counts one window of traffic published into each series, and the
/// verdict each of the two alarms over that window must reach.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The datapoints published into the request series for the window, which the `Sum` folds.
    requests: &'static [f64],
    /// The datapoints published into the target-error series for the window. Empty for a window
    /// the target answered without a server error, which is a window that series holds no
    /// datapoint for at all.
    errors: &'static [f64],
    /// The value the rate array's returning query computes over the window, as a percentage.
    rate: f64,
    /// The state the count alarm's own value puts it in.
    count_state: &'static str,
    /// The state the rate alarm's own value puts it in.
    rate_state: &'static str,
}

/// The table: one row per error mix the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        // No server error at all: the error series holds no datapoint for the window, so the
        // count alarm is decided by its missing-data arm while the rate's `FILL` closes the same
        // hole with a zero.
        case: "ten requests the target answered without a server error",
        requests: &[10.0],
        errors: &[],
        rate: 0.0,
        count_state: OK,
        rate_state: OK,
    },
    Row {
        // A datapoint of zero is not a hole: the count alarm grades a real value here, and the
        // value sits exactly on its threshold, which `GreaterThanThreshold` does not breach.
        case: "an error count published as zero beside ten requests",
        requests: &[10.0],
        errors: &[0.0],
        rate: 0.0,
        count_state: OK,
        rate_state: OK,
    },
    Row {
        // The recorded mix: one server error among nine good answers pages the count alarm and
        // leaves the rate alarm alone.
        case: "one server error among ten requests",
        requests: &[5.0, 1.0, 4.0],
        errors: &[1.0],
        rate: 10.0,
        count_state: ALARM,
        rate_state: OK,
    },
    Row {
        // The rate threshold's own bound: a share EQUAL to it is not greater than it.
        case: "an error share exactly at the rate threshold",
        requests: &[10.0],
        errors: &[2.0, 3.0],
        rate: 50.0,
        count_state: ALARM,
        rate_state: OK,
    },
    Row {
        // One request past that bound, which is where the rate alarm pages.
        case: "an error share one request past the rate threshold",
        requests: &[10.0],
        errors: &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        rate: 60.0,
        count_state: ALARM,
        rate_state: ALARM,
    },
    Row {
        // Two orders of magnitude more traffic, whose error share still stays under the
        // threshold: the rate is a share, not a count.
        case: "a thousand requests whose error share stays just under the threshold",
        requests: &[600.0, 400.0],
        errors: &[250.0, 249.0],
        rate: 49.9,
        count_state: ALARM,
        rate_state: OK,
    },
    Row {
        // Every request answered with a server error: both alarms page.
        case: "traffic the target answered with a server error every time",
        requests: &[6.0],
        errors: &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        rate: 100.0,
        count_state: ALARM,
        rate_state: ALARM,
    },
];

/// The dimension set one row's series are keyed by: the load balancer that answered and the
/// target group that did, each named for the row so the rows are independent series of one
/// store rather than one series several rows publish into.
fn dimensions(index: usize) -> Value {
    json!([
        {"Name": "LoadBalancer", "Value": format!("app/alarm-mix-{index}/0123456789abcdef")},
        {
            "Name": "TargetGroup",
            "Value": format!("targetgroup/alarm-mix-{index}/fedcba9876543210"),
        },
    ])
}

/// The count alarm over one row's error series: the `Sum` of that minute against a threshold of
/// zero, 1 of 1, with a window that held nothing treated as not breaching.
fn count_alarm(name: &str, index: usize) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": ERRORS,
        "Dimensions": dimensions(index),
        "Statistic": "Sum",
        "Period": PERIOD,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": COUNT_THRESHOLD,
        "ComparisonOperator": GREATER_THAN,
        "TreatMissingData": NOT_BREACHING,
        "ActionsEnabled": false,
    })
}

/// The query array a rate is computed by: the two series it reads, neither of which returns
/// data, and the expression over them, which does.
fn rate_queries(index: usize) -> Value {
    let read = |id: &str, metric: &str| {
        json!({
            "Id": id,
            "ReturnData": false,
            "MetricStat": {
                "Metric": {
                    "Namespace": NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": dimensions(index),
                },
                "Period": PERIOD,
                "Stat": "Sum",
            },
        })
    };
    json!([
        read(ERRORS_ID, ERRORS),
        read(REQUESTS_ID, REQUESTS),
        {
            "Id": RATE_ID,
            "Expression": RATE_EXPRESSION,
            "Label": "target 5xx rate",
            "ReturnData": true,
            "Period": PERIOD,
        },
    ])
}

/// The rate alarm over one row's two series: the share the returning query computes against a
/// threshold of half the traffic, 1 of 1, under the same missing-data arm.
fn rate_alarm(name: &str, index: usize) -> Value {
    json!({
        "AlarmName": name,
        "Metrics": rate_queries(index),
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": RATE_THRESHOLD,
        "ComparisonOperator": GREATER_THAN,
        "TreatMissingData": NOT_BREACHING,
        "ActionsEnabled": false,
    })
}

/// The dimension set one row's series are keyed by, as the store holds it.
fn dimension_set(index: usize) -> cloudwatch::DimensionSet {
    dimensions(index)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|dimension| {
            let name = dimension.get("Name").and_then(Value::as_str)?;
            let value = dimension.get("Value").and_then(Value::as_str)?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// Count one row's requests and target errors into its own two series, each count one
/// observation of the series it belongs to, stamped inside the newest window an alarm
/// evaluation grades — the newest one that has closed, since an evaluation grades the periods
/// that are complete rather than the one the clock is in.
///
/// Written through the store's own append, the way the service that owns a reserved namespace
/// publishes into it: no caller may write such a namespace through `PutMetricData`.
fn count_into_the_series(engine: &engine::InMemoryEngine, index: usize, row: &Row, stamp: f64) {
    let dimensions = dimension_set(index);
    let published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> =
        [(REQUESTS, row.requests), (ERRORS, row.errors)]
            .into_iter()
            .flat_map(|(metric, values)| {
                let dimensions = dimensions.clone();
                values.iter().map(move |value| {
                    (
                        cloudwatch::MetricKey {
                            namespace: NAMESPACE.to_string(),
                            metric_name: metric.to_string(),
                            dimensions: dimensions.clone(),
                        },
                        cloudwatch::StoredDatum {
                            timestamp_secs: stamp,
                            sample_count: 1.0,
                            sum: *value,
                            minimum: *value,
                            maximum: *value,
                            unit: Some(COUNT_UNIT.to_string()),
                            storage_resolution: STANDARD_RESOLUTION_SECS,
                        },
                    )
                })
            })
            .collect();
    let state = cloudwatch::service_state(engine).expect("the CloudWatch store exists");
    let mut store = state.lock();
    let mut written: Vec<cloudwatch::WrittenSeries> = Vec::with_capacity(published.len());
    for (key, datum) in published {
        let unit = datum.unit().to_string();
        store
            .append_datapoints(key.clone(), vec![datum])
            .expect("the metric store takes this series");
        written.push(cloudwatch::WrittenSeries {
            key,
            units: vec![unit],
        });
    }
    store.make_alarms_due(&written, stamp);
}

/// What `GetMetricData` answers one row's OWN rate array with over the window the row published
/// into: the values of the returning query, newest last.
async fn read_rate(router: &MultiServiceRouter, index: usize, stamp: f64) -> Vec<f64> {
    let window = (stamp / PERIOD_SECS).floor() * PERIOD_SECS;
    let body = ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": rate_queries(index),
            "StartTime": window - PERIOD_SECS,
            "EndTime": window + PERIOD_SECS,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    body.get("MetricDataResults")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|result| result.get("Id").and_then(Value::as_str) == Some(RATE_ID))
        .map(|result| {
            result
                .get("Values")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Every alarm `DescribeAlarms` reports for `names`, by name.
async fn described(router: &MultiServiceRouter, names: &[String]) -> BTreeMap<String, Value> {
    let body = ok(router, "DescribeAlarms", &json!({"AlarmNames": names})).await;
    body.get("MetricAlarms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|alarm| {
            alarm
                .get("AlarmName")
                .and_then(Value::as_str)
                .map(|name| (name.to_string(), alarm.clone()))
        })
        .collect()
}

/// The `StateValue` a client reads off the alarm `name`.
fn state_of(alarms: &BTreeMap<String, Value>, name: &str) -> String {
    member_of(alarms, name, "StateValue")
}

/// The `StateReason` a client reads off the alarm `name`.
fn reason_of(alarms: &BTreeMap<String, Value>, name: &str) -> String {
    member_of(alarms, name, "StateReason")
}

/// One string member of a described alarm, as the empty string where the read reports none.
fn member_of(alarms: &BTreeMap<String, Value>, name: &str, member: &str) -> String {
    alarms
        .get(name)
        .and_then(|alarm| alarm.get(member))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every state the alarm `name` transitioned INTO, as its history records them: newest first.
async fn transitions(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    alarm_history_items(router, AlarmKind::Metric, name, "StateUpdate")
        .await
        .iter()
        .filter_map(|item| item.get("HistoryData").and_then(Value::as_str))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter_map(|history| {
            history
                .get("newState")
                .and_then(|state| state.get("stateValue"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The value an alarm explanation renders a measured quantity as: a decimal that always carries
/// a fraction.
fn decimal(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{value:.1}");
    }
    format!("{value}")
}

/// The `Sum` of the datapoints one row published into a series.
fn total(values: &[f64]) -> f64 {
    values.iter().sum()
}

#[tokio::test]
async fn a_count_alarm_and_a_rate_alarm_over_one_window_each_grade_their_own_value() {
    // Every row keys its own series, so all the rows are driven on ONE emulator and the
    // evaluation the clock owes is waited out once rather than per row.
    let (router, engine) = build_router_with_engine();
    let stamp = graded_window_stamp(PERIOD_SECS);
    let mut names: Vec<(String, String)> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        count_into_the_series(&engine, index, row, stamp);
        let count = format!("count-{index}");
        let rate = format!("rate-{index}");
        ok(&router, "PutMetricAlarm", &count_alarm(&count, index)).await;
        ok(&router, "PutMetricAlarm", &rate_alarm(&rate, index)).await;
        names.push((count, rate));
    }

    let flat: Vec<String> = names
        .iter()
        .flat_map(|(count, rate)| [count.clone(), rate.clone()])
        .collect();

    // A client polls for the evaluated states, waiting for the CONDITION rather than for a
    // fixed time: every alarm having reached the state its own value decides.
    let mut last = BTreeMap::new();
    for _ in 0..POLL_READS {
        std::thread::sleep(PAST_EVALUATION_DELAY / 3);
        last = described(&router, &flat).await;
        if ROWS.iter().zip(&names).all(|(row, (count, rate))| {
            state_of(&last, count) == row.count_state && state_of(&last, rate) == row.rate_state
        }) {
            break;
        }
    }

    for (index, (row, (count, rate))) in ROWS.iter().zip(&names).enumerate() {
        let case = row.case;
        let errors = total(row.errors);
        let requests = total(row.requests);

        // Each alarm reaches the state ITS OWN value decides, over the same window of the same
        // two series — which is the differential a rate alarm is run beside a count one for.
        let count_state = state_of(&last, count);
        assert_eq!(
            count_state, row.count_state,
            "case {case:?}: the count alarm over a window summing to {errors} errors must \
             report {:?}, got {count_state:?} with {:?}",
            row.count_state,
            last.get(count)
        );
        let rate_state = state_of(&last, rate);
        assert_eq!(
            rate_state, row.rate_state,
            "case {case:?}: the rate alarm over {errors} errors in {requests} requests (a rate \
             of {}) must report {:?}, got {rate_state:?} with {:?}",
            row.rate, row.rate_state,
            last.get(rate)
        );

        // The count alarm's explanation says what decided it: the value it graded where the
        // series held a datapoint for the window, and the missing-data arm where it held none.
        let count_reason = reason_of(&last, count);
        if row.errors.is_empty() {
            assert!(
                count_reason.contains(NON_BREACHING_ARM),
                "case {case:?}: expected the count alarm's StateReason to name the arm that \
                 decided a window with no datapoint, got {count_reason:?}"
            );
        } else {
            assert!(
                count_reason.contains(&format!("{} (", decimal(errors))),
                "case {case:?}: expected the count alarm's StateReason to cite the summed error \
                 count {}, got {count_reason:?}",
                decimal(errors)
            );
        }

        // The rate the alarm graded is the one a reader of the SAME array sees, so the verdict
        // rests on a value computed from the stored datapoints rather than on anything else.
        let read = read_rate(&router, index, stamp).await;
        let answered = read.last().copied().unwrap_or(f64::NAN);
        assert!(
            (answered - row.rate).abs() <= TOLERANCE,
            "case {case:?}: expected GetMetricData over the rate array to answer {}, got \
             {answered} from {read:?}",
            row.rate
        );

        // What a client reads to tell which alarm ever paged: a rate alarm that never breached
        // has no breach in its record, however often the count alarm beside it did.
        let recorded = transitions(&router, rate).await;
        if row.rate_state == OK {
            assert!(
                !recorded.iter().any(|state| state == ALARM),
                "case {case:?}: the rate alarm stayed under its threshold, so its history must \
                 hold no {ALARM} transition, got {recorded:?}"
            );
        }
        assert_eq!(
            recorded.first().map(String::as_str),
            Some(row.rate_state),
            "case {case:?}: expected the rate alarm's newest recorded transition to be into \
             {:?}, got {recorded:?}",
            row.rate_state
        );
        let count_recorded = transitions(&router, count).await;
        assert_eq!(
            count_recorded.first().map(String::as_str),
            Some(row.count_state),
            "case {case:?}: expected the count alarm's newest recorded transition to be into \
             {:?}, got {count_recorded:?}",
            row.count_state
        );
    }
}
