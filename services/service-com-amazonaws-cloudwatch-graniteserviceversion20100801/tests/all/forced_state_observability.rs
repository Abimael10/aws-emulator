// SPDX-License-Identifier: CC0-1.0

//! What a state `SetAlarmState` FORCES is observable to: the reads a client makes after the
//! force, and the composite alarms whose rule reads the alarm it was forced on.
//!
//! Two rules, each asserted over a table.
//!
//! 1. **A state forced on a metric alarm stands until that alarm's own next evaluation**, a span
//!    of the alarm's `Period` away ("the length, in seconds, used each time the metric specified
//!    in `MetricName` is evaluated",
//!    `smithy_query(query="doc",shape="PutMetricAlarmInput",member="Period")`) rather than at the
//!    next call — "Temporarily sets the state of an alarm for testing purposes. […] Metric alarms
//!    returns to their actual state quickly, often within seconds."
//!    (`smithy_query(query="operation",operation="SetAlarmState")`) — so every read a client makes
//!    inside that span reports the forced state, through `DescribeAlarms` and through
//!    `DescribeAlarmsForMetric` alike and however many reads come between, while an alarm of the
//!    same series nothing forced reports the state its own evaluation computed. The rows differ in
//!    the alarm's `Period`, in how many dimensions it carries, in what the series it watches holds,
//!    in the state forced and in how many reads follow the force.
//! 2. **A composite alarm's state is its `AlarmRule` over its children's CURRENT states** — "an
//!    expression that specifies which other alarms are to be evaluated to determine this composite
//!    alarm's state"
//!    (`smithy_query(query="doc",shape="PutCompositeAlarmInput",member="AlarmRule")`) — and a state
//!    forced on a child IS that child's current state, so a composite whose rule the forced states
//!    satisfy is reported in `ALARM` by `DescribeAlarms(AlarmTypes=["CompositeAlarm"])` and holds
//!    it while no child changes state, and falls back once one of them does. The rows differ in the
//!    rule's combinator and arity, in which of the children is forced and in the children's
//!    periods.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, graded_window_stamp, ok, wait_for_period_room};

/// The namespace every alarm in this suite watches.
const NAMESPACE: &str = "Custom/ForcedObservability";

/// The metric every alarm in this suite watches.
const METRIC: &str = "HealthStatus";

/// The threshold every alarm in this suite compares its series against.
const THRESHOLD: f64 = 10.0;

/// A value above [`THRESHOLD`]: a datapoint that breaches, so an alarm evaluated on it is in
/// `ALARM`.
const BREACHING: f64 = 42.0;

/// A value below [`THRESHOLD`]: a datapoint that does not breach, so an alarm evaluated on it is
/// in `OK`.
const CLEAR: f64 = 1.0;

/// How much of the current period window a row that publishes a datapoint asks for before it
/// writes: enough for the publish and the reads that follow to land in the window the row stamped
/// the datapoint relative to. Room, not a deadline.
const WINDOW_ROOM_SECS: f64 = 10.0;

/// How long a row watches a state it expects to HOLD, looking again every [`LOOK_AGAIN`].
///
/// A watch, not a timeout: it spans many of the store's own evaluation passes (each owed a
/// sub-second span) while staying far inside the span a forced state stands for, so a state
/// something takes back early is caught rather than waited out.
const HELD_WATCH: Duration = Duration::from_secs(2);

/// How long a row waits for a state the clock owes it, looking again every [`LOOK_AGAIN`].
///
/// Generous on purpose: a composite alarm's re-derivation is owed a sub-second span after the
/// child's transition, so only a state that NEVER arrives fails a row, whatever the host's load.
const REACHED_WITHIN: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The dimension set a row's alarms watch: one entry per name the row names, all of them in a
/// series of the row's own.
fn dimensions(names: &[&str], index: usize) -> Vec<Value> {
    names
        .iter()
        .map(|name| json!({"Name": name, "Value": format!("row-{index}")}))
        .collect()
}

/// A static-threshold alarm over the row's series, aggregated by `statistic`.
fn alarm_request(name: &str, period: i32, dims: &[Value], statistic: &str) -> Value {
    let mut request = json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Statistic": statistic,
        "Period": period,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "ActionsEnabled": false,
        "TreatMissingData": "missing",
    });
    if !dims.is_empty() {
        request["Dimensions"] = json!(dims);
    }
    request
}

/// The `DescribeAlarmsForMetric` query that names the row's series: the reader a client uses when
/// it knows the metric rather than the alarm's name.
fn for_metric_query(dims: &[Value]) -> Value {
    let mut query = json!({"Namespace": NAMESPACE, "MetricName": METRIC});
    if !dims.is_empty() {
        query["Dimensions"] = json!(dims);
    }
    query
}

/// Publish one datapoint of the row's series, stamped inside the newest window an evaluation of a
/// `period`-second alarm grades.
async fn publish(router: &MultiServiceRouter, dims: &[Value], period: i32, value: f64) {
    let mut datum = json!({
        "MetricName": METRIC,
        "Value": value,
        "Timestamp": graded_window_stamp(f64::from(period)),
    });
    if !dims.is_empty() {
        datum["Dimensions"] = json!(dims);
    }
    ok(
        router,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": [datum]}),
    )
    .await;
}

/// Force the alarm `name` into `state`.
async fn force(router: &MultiServiceRouter, name: &str, state: &str) {
    ok(
        router,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "driven by the test",
        }),
    )
    .await;
}

/// The state `DescribeAlarms` reports for the metric alarm `name`, or the empty string when it
/// reports no alarm of that name at all.
async fn described_state(router: &MultiServiceRouter, name: &str) -> String {
    let described = ok(router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    described["MetricAlarms"][0]
        .get("StateValue")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The state `DescribeAlarmsForMetric` reports for `name` among the alarms of the queried series.
async fn state_for_metric(router: &MultiServiceRouter, query: &Value, name: &str) -> String {
    let reported = ok(router, "DescribeAlarmsForMetric", query).await;
    reported["MetricAlarms"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|alarm| alarm.get("AlarmName").and_then(Value::as_str) == Some(name))
        .and_then(|alarm| alarm.get("StateValue").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// Look at the metric alarm `name` until it reports `wanted` or a generous deadline passes, and
/// answer the state it last reported — a wait for the CONDITION.
async fn until_described_state(router: &MultiServiceRouter, name: &str, wanted: &str) -> String {
    let deadline = Instant::now() + REACHED_WITHIN;
    loop {
        let seen = described_state(router, name).await;
        if seen == wanted || Instant::now() >= deadline {
            return seen;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The state `DescribeAlarms` reports for the composite alarm `name`: "If you specify
/// CompositeAlarms, the operation returns only a list of composite alarms"
/// (`DescribeAlarmsInput$AlarmTypes`), so the kind is named and the answer is read out of
/// `CompositeAlarms`.
async fn composite_state(router: &MultiServiceRouter, name: &str) -> String {
    let described = ok(
        router,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["CompositeAlarm"]}),
    )
    .await;
    described["CompositeAlarms"][0]
        .get("StateValue")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Poll the composite alarm `name` until it reports `wanted` or a generous deadline passes, and
/// answer the state it last reported.
async fn until_composite_state(router: &MultiServiceRouter, name: &str, wanted: &str) -> String {
    let deadline = Instant::now() + REACHED_WITHIN;
    loop {
        let seen = composite_state(router, name).await;
        if seen == wanted || Instant::now() >= deadline {
            return seen;
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the forced-state table.
#[derive(Debug, Clone, Copy)]
struct ForcedRow {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The `Period` both alarms of the row are configured with.
    period: i32,
    /// The dimension names the row's series carries.
    dimension_names: &'static [&'static str],
    /// The value the row publishes into that series, or `None` for a metric nothing writes.
    published: Option<f64>,
    /// The state the row forces on its own alarm.
    forced: &'static str,
    /// How many reads the row makes between the force and the re-read.
    reads_between: usize,
}

/// The state an evaluation computes for an alarm over a series holding `published`: the datapoint's
/// own side of the threshold, and insufficient data where there is no datapoint to grade.
fn evaluated_state(published: Option<f64>) -> &'static str {
    match published {
        None => "INSUFFICIENT_DATA",
        Some(value) if value > THRESHOLD => "ALARM",
        Some(_) => "OK",
    }
}

#[tokio::test]
async fn a_forced_state_outlives_the_reads_a_client_makes_after_it() {
    let rows: &[ForcedRow] = &[
        // The recorded pair: a cluster-wide alarm of one dimension and a five-minute period, and
        // a per-node alarm of two dimensions and a half-hour period, both over a metric with no
        // datapoints at all.
        ForcedRow {
            case: "one dimension, a five-minute period, a metric nothing writes",
            period: 300,
            dimension_names: &["ClusterIdentifier"],
            published: None,
            forced: "ALARM",
            reads_between: 4,
        },
        ForcedRow {
            case: "two dimensions, a half-hour period, a metric nothing writes",
            period: 1_800,
            dimension_names: &["ClusterIdentifier", "NodeID"],
            published: None,
            forced: "ALARM",
            reads_between: 4,
        },
        // The shortest regular-resolution period, so the span the force stands for is this
        // alarm's own period rather than a longer one.
        ForcedRow {
            case: "the shortest regular period, read once after the force",
            period: 60,
            dimension_names: &["ClusterIdentifier"],
            published: None,
            forced: "ALARM",
            reads_between: 1,
        },
        // A series the evaluation CAN grade, so the forced state is not the one the datapoints
        // imply: the force stands over an evaluation that has something else to say.
        ForcedRow {
            case: "ALARM forced over a series under the threshold",
            period: 60,
            dimension_names: &["ClusterIdentifier"],
            published: Some(CLEAR),
            forced: "ALARM",
            reads_between: 8,
        },
        ForcedRow {
            case: "OK forced over a breaching series",
            period: 60,
            dimension_names: &["ClusterIdentifier"],
            published: Some(BREACHING),
            forced: "OK",
            reads_between: 4,
        },
        // A dimensionless alarm, and the third state `SetAlarmState` admits.
        ForcedRow {
            case: "INSUFFICIENT_DATA forced on a dimensionless alarm over a breaching series",
            period: 60,
            dimension_names: &[],
            published: Some(BREACHING),
            forced: "INSUFFICIENT_DATA",
            reads_between: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let ForcedRow {
            case,
            period,
            dimension_names,
            published,
            forced,
            reads_between,
        } = *row;
        let router = build_router();
        let dims = dimensions(dimension_names, index);
        let query = for_metric_query(&dims);
        let forced_name = format!("forced-{index}");
        // A second alarm over the SAME series, differing only in the statistic it aggregates by,
        // which nothing forces: what one alarm's forced state is cannot be what another alarm of
        // the same metric reports.
        let watching_name = format!("watching-{index}");
        ok(
            &router,
            "PutMetricAlarm",
            &alarm_request(&forced_name, period, &dims, "Maximum"),
        )
        .await;
        ok(
            &router,
            "PutMetricAlarm",
            &alarm_request(&watching_name, period, &dims, "Average"),
        )
        .await;
        if let Some(value) = published {
            wait_for_period_room(f64::from(period), WINDOW_ROOM_SECS).await;
            publish(&router, &dims, period, value).await;
        }

        // The state the EVALUATION computes over what the series holds, which the force then
        // replaces: the row forces a state the alarm is not already in, so the write really
        // moves it.
        let evaluated = evaluated_state(published);
        let settled = until_described_state(&router, &forced_name, evaluated).await;
        assert_eq!(
            settled, evaluated,
            "case {case:?}: before the force the alarm must report the state its evaluation \
             computes over the series, got {settled:?}"
        );

        force(&router, &forced_name, forced).await;
        let straight_after = described_state(&router, &forced_name).await;
        assert_eq!(
            straight_after, forced,
            "case {case:?}: the caller reading straight after the force must see the state it \
             forced, got {straight_after:?}"
        );

        // The reads a client makes between the force and its re-read: a read reports state, it
        // does not take one back.
        for _ in 0..reads_between {
            let _ = state_for_metric(&router, &query, &watching_name).await;
        }

        for (reader, seen) in [
            (
                "DescribeAlarms",
                described_state(&router, &forced_name).await,
            ),
            (
                "DescribeAlarmsForMetric",
                state_for_metric(&router, &query, &forced_name).await,
            ),
        ] {
            assert_eq!(
                seen, forced,
                "case {case:?}: after {reads_between} intervening read(s), {reader} must still \
                 report the forced state {forced}, got {seen:?}"
            );
        }

        // The alarm nothing forced reports the state ITS own evaluation computed over the same
        // series — the differential a client reads the force by.
        let unforced = until_described_state(&router, &watching_name, evaluated).await;
        assert_eq!(
            unforced, evaluated,
            "case {case:?}: the alarm nothing forced must report the state its evaluation \
             computes, got {unforced:?}"
        );

        // And the forced state HOLDS over the whole watch, through both readers: nothing the
        // evaluation passes those reads materialise takes it back inside its span.
        let watch_until = Instant::now() + HELD_WATCH;
        while Instant::now() < watch_until {
            let described = described_state(&router, &forced_name).await;
            let for_metric = state_for_metric(&router, &query, &forced_name).await;
            assert_eq!(
                described, forced,
                "case {case:?}: DescribeAlarms must report the forced state for the whole span it \
                 stands for, got {described:?}"
            );
            assert_eq!(
                for_metric, forced,
                "case {case:?}: DescribeAlarmsForMetric must report the same state as \
                 DescribeAlarms ({described:?}), got {for_metric:?}"
            );
            ::tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

/// One row of the composite table.
#[derive(Debug, Clone, Copy)]
struct CompositeRow {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The composite's `AlarmRule`, over the children below.
    rule: &'static str,
    /// The children the rule may reference, each with the `Period` it is configured with.
    children: &'static [(&'static str, i32)],
    /// The children the row forces into `ALARM`, which is what its rule is then evaluated over.
    forced: &'static [&'static str],
    /// The child the row afterwards forces into `OK`, so the rule no longer holds.
    released: &'static str,
}

#[tokio::test]
async fn a_composite_rule_reaches_alarm_over_a_forced_child_and_holds_it() {
    let rows: &[CompositeRow] = &[
        // A single-term rule: the composite is the child's state, and nothing else.
        CompositeRow {
            case: "a single-term rule over a one-minute child",
            rule: "ALARM(\"kid-a\")",
            children: &[("kid-a", 60)],
            forced: &["kid-a"],
            released: "kid-a",
        },
        // The same rule over a child whose own period is far longer: which period the child
        // carries cannot decide whether the parent ever sees the forced state.
        CompositeRow {
            case: "a single-term rule over a half-hour child",
            rule: "ALARM(\"kid-a\")",
            children: &[("kid-a", 1_800)],
            forced: &["kid-a"],
            released: "kid-a",
        },
        // An OR rule needs one of its terms; the first one is forced.
        CompositeRow {
            case: "an OR rule with its first child forced",
            rule: "ALARM(\"kid-a\") OR ALARM(\"kid-b\")",
            children: &[("kid-a", 60), ("kid-b", 300)],
            forced: &["kid-a"],
            released: "kid-a",
        },
        // The same rule with three terms, only the last of them forced.
        CompositeRow {
            case: "an OR rule of three children with its last child forced",
            rule: "ALARM(\"kid-a\") OR ALARM(\"kid-b\") OR ALARM(\"kid-c\")",
            children: &[("kid-a", 60), ("kid-b", 60), ("kid-c", 300)],
            forced: &["kid-c"],
            released: "kid-c",
        },
        // An AND rule needs every term, so both children are forced and releasing either one
        // takes the composite back.
        CompositeRow {
            case: "an AND rule with both children forced",
            rule: "ALARM(\"kid-a\") AND ALARM(\"kid-b\")",
            children: &[("kid-a", 60), ("kid-b", 300)],
            forced: &["kid-a", "kid-b"],
            released: "kid-b",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let CompositeRow {
            case,
            rule,
            children,
            forced,
            released,
        } = *row;
        let router = build_router();
        for (child, period) in children {
            ok(
                &router,
                "PutMetricAlarm",
                &alarm_request(child, *period, &dimensions(&["Child"], index), "Maximum"),
            )
            .await;
        }
        let composite = format!("composite-{index}");
        ok(
            &router,
            "PutCompositeAlarm",
            &json!({"AlarmName": composite, "AlarmRule": rule}),
        )
        .await;

        // The baseline: with no child in ALARM the rule does not hold, so the composite is
        // reported — and reported in a state that is not ALARM.
        let baseline = composite_state(&router, &composite).await;
        assert!(
            !baseline.is_empty() && baseline != "ALARM",
            "case {case:?}: before any force the composite must be reported in a state other \
             than ALARM, got {baseline:?}"
        );

        for child in forced {
            force(&router, child, "ALARM").await;
        }
        let reached = until_composite_state(&router, &composite, "ALARM").await;
        assert_eq!(
            reached, "ALARM",
            "case {case:?}: the rule {rule:?} over the forced children {forced:?} must put the \
             composite in ALARM, got {reached:?}"
        );

        // And it HOLDS while no child changes state: the composite's state is the rule over the
        // children's current states, and a forced state is a child's current state.
        let watch_until = Instant::now() + HELD_WATCH;
        while Instant::now() < watch_until {
            let held = composite_state(&router, &composite).await;
            assert_eq!(
                held, "ALARM",
                "case {case:?}: the composite must hold ALARM while no child changes state, got \
                 {held:?}"
            );
            ::tokio::time::sleep(LOOK_AGAIN).await;
        }

        // One child changing state is what takes it back: the rule no longer holds, so the
        // composite is re-derived to OK.
        force(&router, released, "OK").await;
        let fallen_back = until_composite_state(&router, &composite, "OK").await;
        assert_eq!(
            fallen_back, "OK",
            "case {case:?}: with {released} no longer in ALARM the rule must no longer hold, got \
             {fallen_back:?}"
        );
    }
}
