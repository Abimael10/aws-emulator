// SPDX-License-Identifier: CC0-1.0

//! The SERIES the metric readers of this service answer with for a resource
//! that is NOT an instance — a managed relational database, a load balancer, a
//! distribution and a container service — driven over the wire through the same
//! front door a client uses.
//!
//! The rule under test: a resource this service holds publishes its metrics
//! from the moment the service creates it, so a read over a window covering the
//! instants it has been publishing in answers one datapoint per `period` window
//! of them, each carrying the metric's published `unit`, the start of its own
//! window as its `timestamp`, and exactly the `MetricStatistic` members the
//! request asked for — whose values hold together over the window's samples.
//! An empty series is the answer for a window the resource published no sample
//! in, and for nothing else.
//!
//! What each sample MEASURED is the other half of it, and it is the quantity the
//! store knows: zero for an amount of work this emulator performed none of (no
//! guest, no container, no request served through a load balancer or a
//! distribution, no health check run), and the resource's own provisioned
//! capacity for a metric that reports one — a managed database's free storage
//! and freeable memory, which its `hardware` states.
//!
//! A datapoint takes a moment to land — the resource's first sampling instant
//! has to pass — which is why every case here POLLS for the series instead of
//! asserting on how fast the host it runs on is.
//!
//! The sibling suite `instance_metric_series.rs` is this rule over an instance;
//! the suites named after each reader cover their refusals, their `metricName`
//! echo and the windows that carry nothing. The harnesses and the resource
//! builders are the ones those suites already publish.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::distribution_cache_reset::{create_distribution, create_origin, OriginKind};
use crate::distribution_certificate_attachment::wait_for_distribution_status;
use crate::instance_lifecycle::Emulator;
use crate::load_balancer_instance_attachment::{
    create_load_balancer, wait_for_load_balancer_state,
};
use crate::relational_database_start_stop::{database, with_available_database, ROWS};

/// How long a case waits for the series to carry its first datapoint.
///
/// Generous on purpose: the condition it waits for is the resource's FIRST
/// sampling instant passing, so a deadline this far out is only ever reached
/// when no sample lands at all.
const SERIES_DEADLINE: Duration = Duration::from_secs(60);

/// How long between two polls of the series.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How far before a resource's creation a window starts, in seconds, so the
/// window covers everything the resource has published.
const WINDOW_LEAD: i64 = 2 * 60 * 60;

/// How far past a resource's creation a window ends, in seconds — beyond any
/// instant this suite's own polling can reach, so the window stays open over
/// the whole of a case.
const WINDOW_TRAIL: i64 = 2 * 60 * 60;

/// The members of the modelled `MetricStatistic` enum, each with the member of
/// a datapoint it selects.
///
/// SOURCE: `smithy_query(query="shape",shape="MetricStatistic")` for the names,
/// and `smithy_query(query="shape",shape="MetricDatapoint")` for the members.
const STATISTIC_MEMBERS: &[(&str, &str)] = &[
    ("Minimum", "minimum"),
    ("Maximum", "maximum"),
    ("Sum", "sum"),
    ("Average", "average"),
    ("SampleCount", "sampleCount"),
];

/// Every member of the modelled `MetricStatistic` enum, so one read asserts the
/// whole datapoint and every invariant between its members.
const EVERY_STATISTIC: &[&str] = &["Minimum", "Maximum", "Sum", "Average", "SampleCount"];

/// The bounds of the modelled `MetricPeriod` range (`@range(min: 60, max:
/// 86400)` on `MetricPeriod`).
const MIN_PERIOD: i64 = 60;
/// See [`MIN_PERIOD`].
const MAX_PERIOD: i64 = 86_400;

/// What a sample of a metric measuring work measured, for work nothing
/// performed: nothing.
const NOTHING_MEASURED: f64 = 0.0;

/// How many bytes one gibibyte is, for a metric published in `Bytes` over a
/// provisioned size a database's `hardware` states in GB.
const BYTES_PER_GIB: f64 = 1_073_741_824.0;

// ---------------------------------------------------------------------------
// The published catalogs
// ---------------------------------------------------------------------------

/// Every relational database metric and the unit its data is published in.
///
/// SOURCE: the modelled `GetRelationalDatabaseMetricDataRequest$metricName`
/// documentation ("Valid relational database metric names are listed below,
/// along with the most useful `statistics` to include in your request, and the
/// published `unit` value"), which states one published unit for each of the
/// first six rows; the modelled `RelationalDatabaseMetricName` enum carries two
/// further members that list leaves out — `FreeableMemory` and `SwapUsage` —
/// and both are amounts of memory, published in `Bytes`.
const DATABASE_PUBLISHED_UNITS: &[(&str, &str)] = &[
    ("CPUUtilization", "Percent"),
    ("DatabaseConnections", "Count"),
    ("DiskQueueDepth", "Count"),
    ("FreeStorageSpace", "Bytes"),
    ("NetworkReceiveThroughput", "Bytes/Second"),
    ("NetworkTransmitThroughput", "Bytes/Second"),
    ("FreeableMemory", "Bytes"),
    ("SwapUsage", "Bytes"),
];

/// The granularity relational database metrics are published at, in seconds.
///
/// SOURCE: the modelled `GetRelationalDatabaseMetricDataRequest$period`
/// documentation: "All relational database metric data is available in 1-minute
/// (60 seconds) granularity."
const DATABASE_GRANULARITY: i64 = 60;

/// Every load balancer metric and the unit its data is published in.
///
/// SOURCE:
/// `smithy_query(query="doc",shape="GetLoadBalancerMetricDataRequest",member="metricName")`
/// — "Valid load balancer metric names are listed below, along with the most
/// useful `statistics` to include in your request, and the published `unit`
/// value", which states "The published unit is `Count`" for every entry but
/// `InstanceResponseTime`, which is "The time elapsed, in seconds" and
/// published in `Seconds`.
const LOAD_BALANCER_PUBLISHED_UNITS: &[(&str, &str)] = &[
    ("ClientTLSNegotiationErrorCount", "Count"),
    ("HealthyHostCount", "Count"),
    ("UnhealthyHostCount", "Count"),
    ("HTTPCode_Instance_2XX_Count", "Count"),
    ("HTTPCode_Instance_3XX_Count", "Count"),
    ("HTTPCode_Instance_4XX_Count", "Count"),
    ("HTTPCode_Instance_5XX_Count", "Count"),
    ("HTTPCode_LB_4XX_Count", "Count"),
    ("HTTPCode_LB_5XX_Count", "Count"),
    ("InstanceResponseTime", "Seconds"),
    ("RejectedConnectionCount", "Count"),
    ("RequestCount", "Count"),
];

/// Every distribution metric and the unit its data is published in.
///
/// SOURCE: the modelled `GetDistributionMetricDataRequest$metricName`
/// documentation, whose two error-rate entries are written `4xxErrorRate` and
/// `5xxErrorRate` and are the `Http4xxErrorRate` and `Http5xxErrorRate` members
/// of the modelled `DistributionMetricName` enum.
const DISTRIBUTION_PUBLISHED_UNITS: &[(&str, &str)] = &[
    ("Requests", "None"),
    ("BytesDownloaded", "None"),
    ("BytesUploaded", "None"),
    ("TotalErrorRate", "Percent"),
    ("Http4xxErrorRate", "Percent"),
    ("Http5xxErrorRate", "Percent"),
];

/// Every container service metric and the unit its data is published in.
///
/// SOURCE: the modelled `GetContainerServiceMetricDataRequest$metricName`
/// documentation, which states "Unit: The published unit is `Percent`" for
/// both. This request carries no `unit` member, so the unit is only ever read
/// OFF a datapoint.
const CONTAINER_SERVICE_PUBLISHED_UNITS: &[(&str, &str)] =
    &[("CPUUtilization", "Percent"), ("MemoryUtilization", "Percent")];

/// The granularity container service metrics are published at, in seconds.
///
/// SOURCE: the modelled `GetContainerServiceMetricDataRequest$period`
/// documentation: "All container service metric data is available in 5-minute
/// (300 seconds) granularity."
const CONTAINER_SERVICE_GRANULARITY: i64 = 300;

/// The granularity this service publishes a load balancer's and a
/// distribution's metrics at, in seconds — the finest a read can ask for, since
/// neither operation's documentation states one.
const UNSTATED_GRANULARITY: i64 = MIN_PERIOD;

// ---------------------------------------------------------------------------
// Driving one read
// ---------------------------------------------------------------------------

/// The instant `record` was created, in whole epoch seconds — the anchor every
/// window in this suite is placed relative to.
fn created_at_seconds(case: &str, record: &Value) -> i64 {
    record["createdAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case}: expected a createdAt on the record, got {record:?}"))
        as i64
}

/// The datapoints an answer carries, or a panic naming `case`.
fn metric_data(case: &str, body: &Value) -> Vec<Value> {
    body["metricData"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a metricData array, got {body:?}"))
        .clone()
}

/// Poll the metric read `operation` with `body` in `region` until it answers a
/// series carrying at least one datapoint, which it also asserts is reported
/// under the requested `metricName`.
async fn wait_for_series(
    emulator: &Emulator,
    region: Option<&str>,
    operation: &str,
    metric_name: &str,
    body: Value,
    case: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + SERIES_DEADLINE;
    loop {
        let answer = emulator
            .ok_at(None, region, operation, body.clone())
            .await;
        assert_eq!(
            answer["metricName"].as_str(),
            Some(metric_name),
            "case {case}: expected the answer to report the requested metric, got {answer:?}"
        );
        let data = metric_data(case, &answer);
        if !data.is_empty() {
            return data;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the series to carry a datapoint within {SERIES_DEADLINE:?}, \
             got {answer:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What the rule says every datapoint of one read carries.
struct Expected<'a> {
    case: &'a str,
    /// The unit the metric is published in.
    unit: &'a str,
    /// The granularity the request asked for.
    period: i64,
    /// The granularity the metric is published at.
    granularity: i64,
    /// The instant the resource was created, where its series begins.
    created: i64,
    /// The statistics the request asked for.
    statistics: &'a [&'a str],
    /// What every sample of the window measured.
    measured: f64,
}

/// The `f64` a datapoint member carries, or a panic naming the case.
fn number(case: &str, datapoint: &Value, member: &str) -> f64 {
    datapoint[member].as_f64().unwrap_or_else(|| {
        panic!("case {case}: expected the member {member} on the datapoint, got {datapoint:?}")
    })
}

/// Assert everything the rule says about a whole answered series.
fn assert_series(data: &[Value], want: &Expected<'_>) {
    let case = want.case;
    assert!(
        !data.is_empty(),
        "case {case}: expected the series to carry a datapoint"
    );
    for datapoint in data {
        assert_eq!(
            datapoint["unit"].as_str(),
            Some(want.unit),
            "case {case}: expected the metric's published unit, got {datapoint:?}"
        );

        // "The timestamp": the start of the datapoint's own window, so it is a
        // multiple of the requested granularity, and it lies between the window
        // the resource was created in and the window this read happened in.
        let timestamp = number(case, datapoint, "timestamp") as i64;
        assert_eq!(
            timestamp.rem_euclid(want.period),
            0,
            "case {case}: expected a timestamp on a {}-second window boundary, got {timestamp}",
            want.period
        );
        assert!(
            timestamp >= want.created - want.created.rem_euclid(want.period)
                && timestamp <= want.created + WINDOW_TRAIL,
            "case {case}: expected a window of the resource's own lifetime, got {timestamp} for \
             a resource created at {}",
            want.created
        );

        // Each statistic the request named selects one member OF the datapoint,
        // so those are present and the rest are absent.
        for (statistic, member) in STATISTIC_MEMBERS {
            let asked = want.statistics.contains(statistic);
            assert_eq!(
                !datapoint[member].is_null(),
                asked,
                "case {case}: expected the member {member} to be present = {asked} for \
                 statistics {:?}, got {datapoint:?}",
                want.statistics
            );
        }

        // A window in the answer holds at least the one sample that put it
        // there, and at most the samples the metric's own published
        // granularity fits in a window of this `period` — plus the resource's
        // first sample, which lands before its first whole granularity
        // interval has passed.
        let samples = if want.statistics.contains(&"SampleCount") {
            let samples = number(case, datapoint, "sampleCount");
            let most = (want.period / want.granularity + 1) as f64;
            assert!(
                (1.0..=most).contains(&samples),
                "case {case}: expected between one and {most} samples in a {}-second window of a \
                 metric published every {} seconds, got {samples}",
                want.period,
                want.granularity
            );
            Some(samples)
        } else {
            None
        };

        // What every sample of the window measured, and so what the extremes
        // and the mean of the window are: "Minimum - The lowest value observed
        // during the specified period", "Maximum - The highest" and "Average -
        // The value of Sum / SampleCount".
        for statistic in ["Minimum", "Maximum", "Average"] {
            if !want.statistics.contains(&statistic) {
                continue;
            }
            let member = STATISTIC_MEMBERS
                .iter()
                .find(|(name, _)| *name == statistic)
                .map(|(_, member)| *member)
                .expect("every statistic selects a member");
            let value = number(case, datapoint, member);
            assert_eq!(
                value, want.measured,
                "case {case}: expected the {member} of the window to be the {} every sample \
                 measured, got {value}",
                want.measured
            );
        }

        // "Sum - All values submitted for the matching metric added together":
        // the total is the measurement over the count of samples.
        if let (true, Some(samples)) = (want.statistics.contains(&"Sum"), samples) {
            let sum = number(case, datapoint, "sum");
            let expected = want.measured * samples;
            assert!(
                (sum - expected).abs() <= expected.abs() * 1e-9 + 1e-9,
                "case {case}: expected sum = the measurement over the sample count = {expected}, \
                 got {sum}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The managed relational database
// ---------------------------------------------------------------------------

/// One `GetRelationalDatabaseMetricData` request body.
fn database_request(
    name: &str,
    metric_name: &str,
    unit: &str,
    period: i64,
    created: i64,
    statistics: &[&str],
) -> Value {
    json!({
        "relationalDatabaseName": name,
        "metricName": metric_name,
        "unit": unit,
        "period": period,
        "startTime": created - WINDOW_LEAD,
        "endTime": created + WINDOW_TRAIL,
        "statistics": statistics,
    })
}

/// What a sample of the database metric `metric_name` measured on a database
/// whose bundle provisions `disk_gb` of storage and `ram_gb` of memory.
///
/// Two of the metrics report an amount of the PROVISIONED capacity rather than
/// work the engine did — "FreeStorageSpace - The amount of available storage
/// space" and the available memory `FreeableMemory` reports — and nothing has
/// been written to that disk or taken out of that memory here. Every other
/// metric of the resource type measures work this emulator performed none of.
fn database_measurement(metric_name: &str, disk_gb: f64, ram_gb: f64) -> f64 {
    match metric_name {
        "FreeStorageSpace" => disk_gb * BYTES_PER_GIB,
        "FreeableMemory" => ram_gb * BYTES_PER_GIB,
        _ => NOTHING_MEASURED,
    }
}

/// RULE: every metric a managed relational database publishes serves datapoints
/// over a window covering what the database has published, each carrying the
/// metric's unit, the statistics the request asked for, and the quantity the
/// store knows — the provisioned capacity for the two metrics that report one,
/// and nothing for the work no engine of this emulator performed.
///
/// The rows span the databases a sibling suite publishes: both engines this
/// service has blueprints for, two engine versions and two bundle sizes, so the
/// provisioned capacity a datapoint reports differs per row.
#[tokio::test]
async fn every_relational_database_metric_serves_the_series_of_its_own_database() {
    let name = "series-db";
    for row in ROWS.iter().take(2) {
        let emulator = with_available_database(row, name).await;
        let record = database(&emulator, name).await;
        let created = created_at_seconds(row.case, &record);
        let disk_gb = number(row.case, &record["hardware"], "diskSizeInGb");
        let ram_gb = number(row.case, &record["hardware"], "ramSizeInGb");

        for (metric_name, unit) in DATABASE_PUBLISHED_UNITS {
            let case = format!("{metric_name} on {}", row.case);
            let data = wait_for_series(
                &emulator,
                None,
                "GetRelationalDatabaseMetricData",
                metric_name,
                database_request(
                    name,
                    metric_name,
                    unit,
                    MIN_PERIOD,
                    created,
                    EVERY_STATISTIC,
                ),
                &case,
            )
            .await;
            assert_series(
                &data,
                &Expected {
                    case: &case,
                    unit,
                    period: MIN_PERIOD,
                    granularity: DATABASE_GRANULARITY,
                    created,
                    statistics: EVERY_STATISTIC,
                    measured: database_measurement(metric_name, disk_gb, ram_gb),
                },
            );
        }
    }
}

/// RULE: the granularity the windows are cut at is the REQUEST's, and
/// `statistics` selects which members a datapoint carries and nothing else — so
/// the same database and the same metric answered at every `period` the model
/// admits, for statistic sets differing in count, members and order, carries
/// exactly those members over windows of exactly that width.
#[tokio::test]
async fn a_database_series_is_cut_at_the_requested_period_for_the_requested_statistics() {
    let name = "granularity-db";
    let row = &ROWS[0];
    let emulator = with_available_database(row, name).await;
    let created = created_at_seconds(row.case, &database(&emulator, name).await);

    // Both published bounds of the modelled `MetricPeriod` range and two values
    // between them, crossed with statistic sets differing along the count, the
    // members and the order.
    let sets: &[&[&str]] = &[
        &["Average"],
        &["Sum", "SampleCount"],
        &["Maximum", "Minimum"],
        EVERY_STATISTIC,
        &["SampleCount", "Average", "Sum", "Maximum", "Minimum"],
    ];
    for (index, period) in [MIN_PERIOD, 300, 3_600, MAX_PERIOD].into_iter().enumerate() {
        let statistics = sets[index % sets.len()];
        let case = format!("CPUUtilization at period {period} for {statistics:?}");
        let data = wait_for_series(
            &emulator,
            None,
            "GetRelationalDatabaseMetricData",
            "CPUUtilization",
            database_request(name, "CPUUtilization", "Percent", period, created, statistics),
            &case,
        )
        .await;
        assert_series(
            &data,
            &Expected {
                case: &case,
                unit: "Percent",
                period,
                granularity: DATABASE_GRANULARITY,
                created,
                statistics,
                measured: NOTHING_MEASURED,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// The load balancer
// ---------------------------------------------------------------------------

/// The Region the load balancer of this suite lives in, and the port it serves
/// its registered instances on: neither is what any case here is about.
const LOAD_BALANCER_REGION: &str = "us-west-2";
const INSTANCE_PORT: i64 = 80;

/// One `GetLoadBalancerMetricData` request body.
fn load_balancer_request(
    name: &str,
    metric_name: &str,
    unit: &str,
    period: i64,
    created: i64,
    statistics: &[&str],
) -> Value {
    json!({
        "loadBalancerName": name,
        "metricName": metric_name,
        "unit": unit,
        "period": period,
        "startTime": created - WINDOW_LEAD,
        "endTime": created + WINDOW_TRAIL,
        "statistics": statistics,
    })
}

/// RULE: every metric a load balancer publishes serves datapoints over a window
/// covering what the load balancer has published, each carrying the metric's
/// unit, the statistics the request asked for, and nothing as the measurement —
/// no request was served through it and no health check was run against a
/// target, so neither its traffic counters nor its host counts observed
/// anything.
#[tokio::test]
async fn every_load_balancer_metric_serves_the_series_of_its_own_load_balancer() {
    let emulator = Emulator::new();
    let name = "series-balancer";
    create_load_balancer(&emulator, LOAD_BALANCER_REGION, name, INSTANCE_PORT).await;
    let record =
        wait_for_load_balancer_state(&emulator, LOAD_BALANCER_REGION, name, "active").await;
    let created = created_at_seconds("the load balancer", &record);

    // The granularity rows alternate, so the cutting rule is exercised across
    // the metrics rather than only at one width.
    let periods = [MIN_PERIOD, 300, 3_600];
    for (index, (metric_name, unit)) in LOAD_BALANCER_PUBLISHED_UNITS.iter().enumerate() {
        let period = periods[index % periods.len()];
        let case = format!("{metric_name} in {unit} at period {period}");
        let data = wait_for_series(
            &emulator,
            Some(LOAD_BALANCER_REGION),
            "GetLoadBalancerMetricData",
            metric_name,
            load_balancer_request(name, metric_name, unit, period, created, EVERY_STATISTIC),
            &case,
        )
        .await;
        assert_series(
            &data,
            &Expected {
                case: &case,
                unit,
                period,
                granularity: UNSTATED_GRANULARITY,
                created,
                statistics: EVERY_STATISTIC,
                measured: NOTHING_MEASURED,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// The distribution
// ---------------------------------------------------------------------------

/// The distribution bundle every case creates on, from the catalog
/// `GetDistributionBundles` publishes, and the origin resource it pulls from:
/// neither is what any case here is about.
const DISTRIBUTION_BUNDLE: &str = "small_1_0";
const ORIGIN_INSTANCE: &str = "series-origin";

/// One `GetDistributionMetricData` request body.
fn distribution_request(
    name: &str,
    metric_name: &str,
    unit: &str,
    period: i64,
    created: i64,
    statistics: &[&str],
) -> Value {
    json!({
        "distributionName": name,
        "metricName": metric_name,
        "unit": unit,
        "period": period,
        "startTime": created - WINDOW_LEAD,
        "endTime": created + WINDOW_TRAIL,
        "statistics": statistics,
    })
}

/// RULE: every metric a distribution publishes serves datapoints over a window
/// covering what the distribution has published, each carrying the metric's
/// unit, the statistics the request asked for, and nothing as the measurement —
/// no viewer request was served through it, which is also the error RATE over
/// no request.
#[tokio::test]
async fn every_distribution_metric_serves_the_series_of_its_own_distribution() {
    let emulator = Emulator::new();
    let name = "series-cdn";
    create_origin(&emulator, OriginKind::Instance, ORIGIN_INSTANCE).await;
    create_distribution(&emulator, name, DISTRIBUTION_BUNDLE, ORIGIN_INSTANCE).await;
    let record = wait_for_distribution_status(&emulator, name, "Deployed").await;
    let created = created_at_seconds("the distribution", &record);

    let periods = [MIN_PERIOD, 300, 3_600];
    for (index, (metric_name, unit)) in DISTRIBUTION_PUBLISHED_UNITS.iter().enumerate() {
        let period = periods[index % periods.len()];
        let case = format!("{metric_name} in {unit} at period {period}");
        let data = wait_for_series(
            &emulator,
            None,
            "GetDistributionMetricData",
            metric_name,
            distribution_request(name, metric_name, unit, period, created, EVERY_STATISTIC),
            &case,
        )
        .await;
        assert_series(
            &data,
            &Expected {
                case: &case,
                unit,
                period,
                granularity: UNSTATED_GRANULARITY,
                created,
                statistics: EVERY_STATISTIC,
                measured: NOTHING_MEASURED,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// The container service
// ---------------------------------------------------------------------------

/// The Region the container service of this suite lives in, and the node power
/// and scale it runs at: none of them is what any case here is about.
const CONTAINER_SERVICE_REGION: &str = "eu-west-1";
const POWER: &str = "nano";
const SCALE: i64 = 1;

/// `CreateContainerService`'s modelled success status.
const CREATED: u16 = 201;

/// Create the container service `name` and answer the record
/// `GetContainerServices` reports for it.
async fn container_service(emulator: &Emulator, name: &str) -> Value {
    let (status, created) = emulator
        .call_in(
            Some(CONTAINER_SERVICE_REGION),
            "CreateContainerService",
            json!({"serviceName": name, "power": POWER, "scale": SCALE}),
        )
        .await;
    assert_eq!(
        status, CREATED,
        "case creating {name}: expected HTTP {CREATED}, got {status} {created:?}"
    );
    let body = emulator
        .ok_at(
            None,
            Some(CONTAINER_SERVICE_REGION),
            "GetContainerServices",
            json!({"serviceName": name}),
        )
        .await;
    body["containerServices"]
        .as_array()
        .and_then(|services| services.first())
        .cloned()
        .unwrap_or_else(|| panic!("case reading {name}: expected the one service, got {body:?}"))
}

/// One `GetContainerServiceMetricData` request body. This request carries no
/// `unit` member — the unit is a property of the metric, and the answer's own
/// datapoints report it.
fn container_service_request(
    name: &str,
    metric_name: &str,
    period: i64,
    created: i64,
    statistics: &[&str],
) -> Value {
    json!({
        "serviceName": name,
        "metricName": metric_name,
        "period": period,
        "startTime": created - WINDOW_LEAD,
        "endTime": created + WINDOW_TRAIL,
        "statistics": statistics,
    })
}

/// RULE: both metrics a container service publishes serve datapoints over a
/// window covering what the service has published, each carrying the metric's
/// published unit — which this request never names — the statistics the request
/// asked for, and nothing as the measurement, since no container runs on any
/// node of it.
#[tokio::test]
async fn both_container_service_metrics_serve_the_series_of_their_own_service() {
    let emulator = Emulator::new();
    let name = "series-containers";
    let record = container_service(&emulator, name).await;
    let created = created_at_seconds("the container service", &record);

    let periods = [MIN_PERIOD, MAX_PERIOD];
    for (index, (metric_name, unit)) in CONTAINER_SERVICE_PUBLISHED_UNITS.iter().enumerate() {
        let period = periods[index % periods.len()];
        let case = format!("{metric_name} at period {period}");
        let data = wait_for_series(
            &emulator,
            Some(CONTAINER_SERVICE_REGION),
            "GetContainerServiceMetricData",
            metric_name,
            container_service_request(name, metric_name, period, created, EVERY_STATISTIC),
            &case,
        )
        .await;
        assert_series(
            &data,
            &Expected {
                case: &case,
                unit,
                period,
                granularity: CONTAINER_SERVICE_GRANULARITY,
                created,
                statistics: EVERY_STATISTIC,
                measured: NOTHING_MEASURED,
            },
        );
    }
}
