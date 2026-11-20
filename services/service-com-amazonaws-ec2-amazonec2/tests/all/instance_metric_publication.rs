// SPDX-License-Identifier: CC0-1.0

//! The `AWS/EC2` series a RUNNING instance publishes about itself, end to end over the wire.
//!
//! THE RULE: a running EC2 instance publishes each of its own instance metrics into the
//! CloudWatch namespace `AWS/EC2`, dimensioned on exactly `{InstanceId}` and carrying that
//! metric's own unit, once per period of the cadence its MONITORING STATE selects — one minute
//! for the status-check metrics whatever that state is ("Basic monitoring | Status check metrics
//! are available in 1-minute periods"), one minute for every other metric while detailed
//! monitoring is on ("Detailed monitoring | You can get metrics in 1-minute periods") and five
//! minutes while it is off ("All other metrics are available in 5-minute periods"). Each period
//! is a wall-clock window anchored at a multiple of its own length and the datapoint in it is
//! stamped with that window's start, which is what makes the cadence observable: a 5-minute
//! period begins on a 5-minute boundary, so a stamp off that grid can only have come from a
//! 1-minute period. Only an instance that is RUNNING is being measured, so one that is stopped
//! publishes nothing for the periods it was not running in, and publication resumes when it runs
//! again.
//!
//! The rows span what the rule generalises over: the monitoring state (basic and detailed), the
//! cadence class (a guest measurement and a status check), the metric (the whole published
//! catalog, not only the CPU series) and the unit each is measured in. Every expected name,
//! unit and period is spelled out here rather than imported, so a wrong constant in the
//! implementation cannot make a test pass.
//!
//! Nothing here reads either service's state: Amazon CloudWatch is mounted BESIDE Amazon EC2 on
//! one engine, exactly as the assembled router mounts them, and every datapoint is read back
//! through CloudWatch's own `GetMetricStatistics`. The cadence is driven by advancing the
//! sampler's own clock over a span of periods rather than by waiting for those periods to pass;
//! one test keeps the wall clock, to pin that a launched instance's series appears with no
//! further EC2 call at all.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// The namespace an instance's own metrics are published under.
pub(crate) const NAMESPACE: &str = "AWS/EC2";

/// The one dimension they are dimensioned on.
pub(crate) const INSTANCE_ID_DIMENSION: &str = "InstanceId";

/// The shortest period any of these series is published at, in seconds.
pub(crate) const MINUTE: i64 = 60;

/// The period the guest measurements of an instance with BASIC monitoring are published at.
pub(crate) const FIVE_MINUTES: i64 = 300;

/// How many minutes of the sampler's clock one simulated span covers: wide enough to hold
/// several 5-minute periods and so to tell the two cadences apart.
pub(crate) const SPAN_MINUTES: i64 = 10;

/// How far into a minute each simulated sample is taken, in seconds: NOT on a period boundary,
/// so a datapoint stamped with the boundary can only have been stamped with the period's start
/// rather than with the instant the sample was taken.
const SAMPLE_OFFSET_SECS: i64 = 7;

/// How long a poll keeps asking before giving up. Generous: every move it waits for is owed in
/// a few seconds, so this is many times the work and still lets a broken case fail rather than
/// hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the one case that keeps the WALL clock waits for the first reading of a 1-minute
/// period.
///
/// A reading is of a period that is OVER, so the first one of a freshly launched instance is
/// owed when the minute it came up in closes — within a minute of the launch. Generous: several
/// times that, so a loaded host is not what decides the case.
const FIRST_READING_DEADLINE: Duration = Duration::from_secs(180);

/// One row of the catalog table: a metric a running instance publishes, the unit it is measured
/// in and the period it is published at in each monitoring state.
///
/// The catalog and the units come from the metrics Amazon EC2 documents for one INSTANCE (Amazon
/// EC2 User Guide, "Instance metrics" and "Status check metrics"), which are documented metrics
/// rather than Smithy shapes; the periods come from the monitoring sentences the module docs
/// quote.
struct MetricRow {
    /// The `MetricName` the series is published under.
    metric: &'static str,
    /// The `StandardUnit` its datapoints must carry.
    unit: &'static str,
    /// The period it is published at while detailed monitoring is OFF.
    basic_period: i64,
    /// The period it is published at while detailed monitoring is ON.
    detailed_period: i64,
}

/// Every metric a running instance publishes, with the unit and the two cadences of each.
const METRIC_ROWS: &[MetricRow] = &[
    MetricRow {
        metric: "CPUUtilization",
        unit: "Percent",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "NetworkIn",
        unit: "Bytes",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "NetworkOut",
        unit: "Bytes",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "NetworkPacketsIn",
        unit: "Count",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "NetworkPacketsOut",
        unit: "Count",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "DiskReadOps",
        unit: "Count",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "DiskWriteOps",
        unit: "Count",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "DiskReadBytes",
        unit: "Bytes",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "DiskWriteBytes",
        unit: "Bytes",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "MetadataNoToken",
        unit: "Count",
        basic_period: FIVE_MINUTES,
        detailed_period: MINUTE,
    },
    // The status-check metrics: "available in 1-minute periods" under basic monitoring too, so
    // their cadence is the same in both states.
    MetricRow {
        metric: "StatusCheckFailed",
        unit: "Count",
        basic_period: MINUTE,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "StatusCheckFailed_Instance",
        unit: "Count",
        basic_period: MINUTE,
        detailed_period: MINUTE,
    },
    MetricRow {
        metric: "StatusCheckFailed_System",
        unit: "Count",
        basic_period: MINUTE,
        detailed_period: MINUTE,
    },
];

/// A `(key, value)` request parameter.
pub(crate) fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Amazon EC2 and Amazon CloudWatch over ONE shared engine, exactly as the assembled router
/// mounts them: the series an instance publishes land in the metric store CloudWatch itself
/// serves its readers from. The engine is handed back too, so a case can advance the sampler's
/// own clock.
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One EC2 call that must be served, as its XML body.
pub(crate) async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = ec2_query::call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One Amazon CloudWatch call that must be served, as its JSON payload.
pub(crate) async fn cloudwatch_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://monitoring.{REGION}.amazonaws.com/"))
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDINSTANCEMETRICS/20260101/{REGION}/monitoring/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed JSON request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with \
         {payload}"
    );
    payload
}

/// Launch `count` instances and answer their ids, in launch order.
pub(crate) async fn launch(router: &MultiServiceRouter, case: &str, count: usize) -> Vec<String> {
    let body = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("MinCount", count.to_string()),
            p("MaxCount", count.to_string()),
        ],
    )
    .await;
    let ids = ec2_query::elements(&body, "instanceId");
    assert_eq!(
        ids.len(),
        count,
        "case {case:?}: RunInstances must report the {count} instances it minted, got {ids:?} \
         in {body}"
    );
    ids
}

/// The `(instance state, monitoring state)` `DescribeInstances` reports for one instance.
async fn reported_states(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, String) {
    let body = ec2_ok(
        router,
        case,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = super::wire::document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && super::wire::child_text(*node, "instanceId").as_deref() == Some(instance_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: DescribeInstances must report {instance_id}: {body}")
        });
    let state = super::wire::child(instance, "instanceState")
        .and_then(|state| super::wire::child_text(state, "name"))
        .unwrap_or_default();
    let monitoring = super::wire::child(instance, "monitoring")
        .and_then(|monitoring| super::wire::child_text(monitoring, "state"))
        .unwrap_or_default();
    (state, monitoring)
}

/// Look again until the instance `instance_id` names reports `state` and `monitoring`, or
/// [`POLL_DEADLINE`] has passed — waiting for the CONDITION the clock owes rather than for a
/// span of time.
pub(crate) async fn poll_until_reported(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    state: &str,
    monitoring: &str,
) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reported = reported_states(router, case, instance_id).await;
        if reported.0 == state && reported.1 == monitoring {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waited for {instance_id} to report ({state:?}, {monitoring:?}) and \
             it reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One datapoint as this suite asserts on it: the period it is stamped with, the unit it
/// carries, how many observations it folded and what they averaged.
#[derive(Debug, Clone, PartialEq)]
struct Datapoint {
    at: i64,
    unit: String,
    sample_count: f64,
    average: f64,
}

/// The datapoints of `metric` for `instance_id` inside `[start, end)`, read at one-minute
/// granularity so every published period is visible on its own, oldest first.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    metric: &str,
    start: i64,
    end: i64,
) -> Vec<Datapoint> {
    let answer = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": INSTANCE_ID_DIMENSION, "Value": instance_id}],
            "StartTime": start,
            "EndTime": end,
            "Period": MINUTE,
            "Statistics": ["Average", "SampleCount"],
        }),
    )
    .await;
    let mut points: Vec<Datapoint> = answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| Datapoint {
            at: point["Timestamp"].as_f64().unwrap_or_default().round() as i64,
            unit: point["Unit"].as_str().unwrap_or_default().to_string(),
            sample_count: point["SampleCount"].as_f64().unwrap_or_default(),
            average: point["Average"].as_f64().unwrap_or_default(),
        })
        .collect();
    points.sort_by_key(|point| point.at);
    points
}

/// Wall-clock now as epoch seconds.
fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default()
}

/// The start of the `period`-wide window `at` falls in: the boundary the windows are anchored
/// at, computed here rather than imported so the expected grid is this suite's own.
fn window_start(at: i64, period: i64) -> i64 {
    at - at.rem_euclid(period)
}

/// Advance the sampler's clock minute by minute over `[base, base + SPAN_MINUTES * 60)`,
/// sampling [`SAMPLE_OFFSET_SECS`] into the minute AFTER each of those minutes.
///
/// A reading is of a whole period, so it is published once that period is over: sampling just
/// past each minute of the span is therefore what makes the periods INSIDE the span due. Each
/// sample is the publish the running emulator makes on its own, asked for at an instant of this
/// suite's choosing: that is what lets one test cover ten periods of two cadences without
/// waiting ten minutes for them.
pub(crate) fn sample_the_span(engine: &engine::InMemoryEngine, base: i64) {
    for minute in 0..SPAN_MINUTES {
        let at = UNIX_EPOCH
            + Duration::from_secs(
                u64::try_from(base + (minute + 1) * MINUTE + SAMPLE_OFFSET_SECS)
                    .unwrap_or_default(),
            );
        ec2::state::instance_telemetry::publish_instance_metrics_at(engine, at);
    }
}

/// The periods of `[base, base + SPAN_MINUTES * 60)` a series published at `period` carries a
/// datapoint in.
fn expected_periods(base: i64, period: i64) -> Vec<i64> {
    (0..SPAN_MINUTES)
        .map(|minute| window_start(base + minute * MINUTE, period))
        .fold(Vec::new(), |mut periods, start| {
            if periods.last() != Some(&start) {
                periods.push(start);
            }
            periods
        })
}

/// The base of the `phase`th span of periods a case drives: a 5-minute boundary ahead of the
/// wall clock, so each span is laid over a whole number of periods of both cadences, and one
/// phase's periods never overlap another's or one the service's own clock publishes into on the
/// wall clock meanwhile.
pub(crate) fn span_base(phase: i64) -> i64 {
    window_start(now_secs(), FIVE_MINUTES) + FIVE_MINUTES + phase * SPAN_MINUTES * MINUTE
}

/// Rule: every metric of the catalog is published for a running instance, dimensioned on that
/// instance, in its own unit, once per period of the cadence the instance's monitoring state
/// selects.
#[tokio::test]
async fn a_running_instance_publishes_each_metric_once_per_period_of_its_monitoring_cadence() {
    let case = "two instances, one with detailed monitoring";
    let (router, engine) = emulator();
    let ids = launch(&router, case, 2).await;
    let (basic, detailed) = (ids[0].clone(), ids[1].clone());
    ec2_ok(
        &router,
        case,
        "MonitorInstances",
        &[p("InstanceId.1", detailed.clone())],
    )
    .await;
    // Both instances are RUNNING and have settled their monitoring state before the span is
    // sampled, so each row's cadence is the one its own state selects.
    poll_until_reported(&router, case, &basic, "running", "disabled").await;
    poll_until_reported(&router, case, &detailed, "running", "enabled").await;

    let base = span_base(0);
    sample_the_span(&engine, base);
    let end = base + SPAN_MINUTES * MINUTE;

    for row in METRIC_ROWS {
        for (instance_id, period, monitoring) in [
            (&basic, row.basic_period, "basic monitoring"),
            (&detailed, row.detailed_period, "detailed monitoring"),
        ] {
            let case = format!("{} under {monitoring}", row.metric);
            let points = datapoints(&router, &case, instance_id, row.metric, base, end).await;
            let periods: Vec<i64> = points.iter().map(|point| point.at).collect();
            assert_eq!(
                periods,
                expected_periods(base, period),
                "case {case:?}: expected one datapoint per {period}-second period of \
                 [{base}, {end}), got {points:?}"
            );
            for point in &points {
                assert_eq!(
                    point.unit, row.unit,
                    "case {case:?}: expected the unit {:?}, got {point:?}",
                    row.unit
                );
                assert_eq!(
                    point.sample_count, 1.0,
                    "case {case:?}: expected one observation per period, got {point:?}"
                );
            }
        }
    }
}

/// Rule: the cadence is observable in the stamps themselves — a 5-minute period begins on a
/// 5-minute boundary, so an off-grid stamp is proof of 1-minute publication and the grid is
/// restored when detailed monitoring is taken away again.
#[tokio::test]
async fn the_cpu_series_is_on_the_five_minute_grid_only_while_detailed_monitoring_is_off() {
    let case = "one instance through both monitoring states";
    let (router, engine) = emulator();
    let instance_id = launch(&router, case, 1).await.remove(0);
    poll_until_reported(&router, case, &instance_id, "running", "disabled").await;

    // Basic monitoring: every stamp on the 5-minute grid.
    let basic_base = span_base(0);
    sample_the_span(&engine, basic_base);
    let basic_end = basic_base + SPAN_MINUTES * MINUTE;
    let basic_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        basic_base,
        basic_end,
    )
    .await;
    assert!(
        !basic_points.is_empty(),
        "case {case:?}: an instance under basic monitoring must publish CPUUtilization, got \
         {basic_points:?}"
    );
    for point in &basic_points {
        assert_eq!(
            point.at.rem_euclid(FIVE_MINUTES),
            0,
            "case {case:?}: a 5-minute period begins on a 5-minute boundary, got {point:?}"
        );
    }

    // Detailed monitoring: stamps off that grid, which only a 1-minute period can produce.
    ec2_ok(
        &router,
        case,
        "MonitorInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    poll_until_reported(&router, case, &instance_id, "running", "enabled").await;
    let detailed_base = span_base(1);
    sample_the_span(&engine, detailed_base);
    let detailed_end = detailed_base + SPAN_MINUTES * MINUTE;
    let detailed_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        detailed_base,
        detailed_end,
    )
    .await;
    let off_grid: Vec<&Datapoint> = detailed_points
        .iter()
        .filter(|point| point.at.rem_euclid(FIVE_MINUTES) != 0)
        .collect();
    assert!(
        !off_grid.is_empty(),
        "case {case:?}: detailed monitoring publishes 1-minute periods, so some stamp must be \
         off the 5-minute grid, got {detailed_points:?}"
    );

    // And the 5-minute cadence is restored when it is taken away again.
    ec2_ok(
        &router,
        case,
        "UnmonitorInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    poll_until_reported(&router, case, &instance_id, "running", "disabled").await;
    let restored_base = span_base(2);
    sample_the_span(&engine, restored_base);
    let restored_end = restored_base + SPAN_MINUTES * MINUTE;
    let restored_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        restored_base,
        restored_end,
    )
    .await;
    assert!(
        !restored_points.is_empty(),
        "case {case:?}: the instance must keep publishing CPUUtilization once detailed \
         monitoring is off, got {restored_points:?}"
    );
    for point in &restored_points {
        assert_eq!(
            point.at.rem_euclid(FIVE_MINUTES),
            0,
            "case {case:?}: UnmonitorInstances restores the 5-minute cadence, got {point:?}"
        );
    }
}

/// Rule: a dry run changes nothing — it is refused `DryRunOperation` and the instance keeps
/// publishing at the cadence it was publishing at.
#[tokio::test]
async fn a_dry_run_unmonitor_leaves_the_one_minute_publication_in_force() {
    let case = "UnmonitorInstances with DryRun=true";
    let (router, engine) = emulator();
    let instance_id = launch(&router, case, 1).await.remove(0);
    ec2_ok(
        &router,
        case,
        "MonitorInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    poll_until_reported(&router, case, &instance_id, "running", "enabled").await;

    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "UnmonitorInstances",
        &[p("InstanceId.1", instance_id.clone()), p("DryRun", "true")],
    )
    .await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected the dry run to be refused, got {status} with {body}"
    );
    assert_eq!(
        ec2_query::error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    let (_, monitoring) = reported_states(&router, case, &instance_id).await;
    assert_eq!(
        monitoring, "enabled",
        "case {case:?}: a dry run changes nothing, so detailed monitoring must still be on"
    );

    let base = span_base(0);
    sample_the_span(&engine, base);
    let points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        base,
        base + SPAN_MINUTES * MINUTE,
    )
    .await;
    let periods: Vec<i64> = points.iter().map(|point| point.at).collect();
    assert_eq!(
        periods,
        expected_periods(base, MINUTE),
        "case {case:?}: the 1-minute publication must be untouched, got {points:?}"
    );
}

/// Rule: only a RUNNING instance is measured — one that is stopped publishes nothing for the
/// periods it was not running in, and publication resumes when it runs again.
#[tokio::test]
async fn publication_stops_with_the_instance_and_resumes_with_it() {
    let case = "an instance stopped and started again";
    let (router, engine) = emulator();
    let instance_id = launch(&router, case, 1).await.remove(0);
    poll_until_reported(&router, case, &instance_id, "running", "disabled").await;

    // Running: the span carries its readings.
    let running_base = span_base(0);
    sample_the_span(&engine, running_base);
    let running_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        running_base,
        running_base + SPAN_MINUTES * MINUTE,
    )
    .await;
    assert!(
        !running_points.is_empty(),
        "case {case:?}: a running instance must publish, got {running_points:?}"
    );

    // Stopped: nothing measures it, so no period of the next span carries a reading.
    ec2_ok(
        &router,
        case,
        "StopInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    poll_until_reported(&router, case, &instance_id, "stopped", "disabled").await;
    let stopped_base = span_base(1);
    sample_the_span(&engine, stopped_base);
    let stopped_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        stopped_base,
        stopped_base + SPAN_MINUTES * MINUTE,
    )
    .await;
    assert!(
        stopped_points.is_empty(),
        "case {case:?}: a stopped instance is not being measured, so it must publish no \
         datapoint, got {stopped_points:?}"
    );

    // Started again: it is running again, so the next span carries its readings again.
    ec2_ok(
        &router,
        case,
        "StartInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    poll_until_reported(&router, case, &instance_id, "running", "disabled").await;
    let restarted_base = span_base(2);
    sample_the_span(&engine, restarted_base);
    let restarted_points = datapoints(
        &router,
        case,
        &instance_id,
        "CPUUtilization",
        restarted_base,
        restarted_base + SPAN_MINUTES * MINUTE,
    )
    .await;
    assert!(
        !restarted_points.is_empty(),
        "case {case:?}: an instance that is running again must publish again, got \
         {restarted_points:?}"
    );
}

/// Rule, on the WALL clock: the series of a launched instance appears without any further EC2
/// call, carrying the period it measured and not the one still open, and it is dimensioned on
/// exactly `{InstanceId}`.
#[tokio::test]
async fn a_launched_instance_publishes_its_cpu_series_with_no_further_call() {
    let case = "a launch with detailed monitoring and nothing else";
    // The engine is not reached at all here: this case keeps the WALL clock, so the reading it
    // waits for is the one the service's own clock publishes. The launch asks for detailed
    // monitoring, so the period it waits for is the 1-minute one.
    let (router, _engine) = emulator();
    let body = ec2_ok(
        &router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("Monitoring.Enabled", "true"),
        ],
    )
    .await;
    let instance_id = ec2_query::element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report its instance: {body}"));
    // The launch answers a `pending` instance, so nothing has been measured yet: what publishes
    // the first reading is that clock, once the instance is running and the period it is running
    // in is over. Nothing below calls EC2 again — the poll asks CloudWatch alone.
    let start = window_start(now_secs(), FIVE_MINUTES) - FIVE_MINUTES;
    let end = start + 4 * FIVE_MINUTES;
    let deadline = Instant::now() + FIRST_READING_DEADLINE;
    let points = loop {
        let points = datapoints(&router, case, &instance_id, "CPUUtilization", start, end).await;
        if !points.is_empty() {
            break points;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waited for the first CPUUtilization datapoint of {instance_id} and \
             none was published"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    for point in &points {
        assert_eq!(
            point.unit, "Percent",
            "case {case:?}: CPUUtilization is a percentage, got {point:?}"
        );
        assert_eq!(
            point.at.rem_euclid(MINUTE),
            0,
            "case {case:?}: a 1-minute period begins on a whole minute, got {point:?}"
        );
        assert!(
            point.at + MINUTE <= now_secs(),
            "case {case:?}: a reading is of a period that is OVER, so the period it is stamped \
             with must have closed by now ({}), got {point:?}",
            now_secs()
        );
        assert!(
            point.average.is_finite(),
            "case {case:?}: a datapoint must carry a measurement, got {point:?}"
        );
    }
    // Dimensioned on EXACTLY that instance: the same read with no dimension at all names a
    // different metric identity ("a metric is uniquely defined by a name, a namespace, and zero
    // or more dimensions"), which nothing published into.
    let undimensioned = cloudwatch_ok(
        &router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": "CPUUtilization",
            "StartTime": start,
            "EndTime": end,
            "Period": MINUTE,
            "Statistics": ["Average"],
        }),
    )
    .await;
    assert_eq!(
        undimensioned["Datapoints"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: the series is dimensioned on {INSTANCE_ID_DIMENSION} alone, so the \
         undimensioned metric holds nothing, got {undimensioned}"
    );
}
