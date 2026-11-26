// SPDX-License-Identifier: CC0-1.0

//! The canonical Application Load Balancer -> `awsvpc` Fargate Amazon ECS topology CONVERGES on
//! its own, and the traffic a client then drives at the published address is readable back out of
//! CloudWatch at every granularity a caller may ask for, through either reader.
//!
//! RULE: **an Application Load Balancer in front of an `awsvpc` Fargate Amazon ECS service settles
//! without any further call — the service's `runningCount` reaches its `desiredCount` and the
//! registered `ip` target leaves its opening state for `healthy` — and from then on every request
//! a client drives at the address that load balancer published is answered by the container and
//! metered into `AWS/ApplicationELB` on the `LoadBalancer` dimension naming it, so a read over a
//! window COVERING that traffic reports a `RequestCount` `Sum` and a `TargetResponseTime`
//! `SampleCount` of exactly the number of requests answered — whatever granularity the read asks
//! for and whichever reader (`GetMetricStatistics` or `GetMetricData`) asks.**
//!
//! The grounds. `TargetHealth$State` is the state a target group's own health checks determined
//! for a registration, and `DescribeTargetHealth` is the read that reports it, so a registration
//! whose checks can arrive settles at `healthy` without a caller doing anything; `Service$
//! runningCount` is "the number of tasks in the cluster that are in the `RUNNING` state", which a
//! scheduler drives to `desiredCount`. `RequestCount` is "the number of requests processed over
//! IPv4 and IPv6" and `TargetResponseTime` "the time elapsed, in seconds, after the request leaves
//! the load balancer until a response from the target is received", both of the Application Load
//! Balancer's own namespace and dimensioned by the load balancer (`app/<name>/<id>`). That those
//! two figures do not depend on the read's granularity is CloudWatch's own arithmetic: a `Period`
//! is "the granularity, in seconds, of the returned data points", so it decides how the samples of
//! a window are GROUPED and not which samples the window holds — the `Sum` of a count and the
//! number of observations behind it are the same totals however the window is divided, and
//! `GetMetricData` reads the same store `GetMetricStatistics` does.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here: Amazon ECS runs
//! the container and drives the service's count, Amazon EC2 owns the network the checks and the
//! traffic cross, Elastic Load Balancing v2 publishes the address, settles the registration and
//! answers the client, and CloudWatch owns the metric store the load balancer publishes into and a
//! client reads back. Everything is driven through the real operations on ONE shared engine,
//! through the fixtures the suites that own this topology and this mounting already keep
//! ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`] for the network, the revision, the front
//! door, the service, the health poll and the one raw HTTP/1.1 exchange on a published address;
//! [`crate::alb_published_address_meters_its_ecs_traffic`] for the four-crate mounting; and
//! [`crate::ecs_service_utilization_metrics`] for the CloudWatch caller), so no fixture of any of
//! them is copied here.
//!
//! What is NEW here is the GRANULARITY and the READER: the sibling that owns this mounting reads
//! every series it measures at one period through one reader, so neither it nor any other suite can
//! see whether the load balancer's request series survives being asked for an hour at a time, or
//! being asked for by the reader a dashboard uses. The recorded scenarios this suite stands for
//! read exactly that way, over "the covering 60 s periods".
//!
//! The rows differ along what the rule generalises over: HOW MANY requests one batch drives (one,
//! four and fifteen, with the totals asserted cumulatively after every batch so each batch is
//! counted exactly once), HOW the client addressed the published load balancer (a real connection
//! DIALLED to the published authority, and that same authority sent as a `Host`), the GRANULARITY
//! the read asks for ([`PERIODS`]) and WHICH reader asks ([`Reader`]).
//!
//! Every clock-owed move — the task's start, the registration it owes, the count the scheduler
//! drives, the health the checks determine, the datapoints the publisher owes — is POLLED for until
//! it holds, never slept for, and each deadline is generous: a row asserts that the move HAPPENS,
//! not how fast this host runs.

use std::collections::BTreeSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The four-crate mounting belongs to the suite that first needed it, and its header invites a
// sibling measuring another rule of the same metered topology to call it.
use crate::alb_published_address_meters_its_ecs_traffic::emulator;
// The topology is that suite's, including `dialled`, the one hand-written raw HTTP/1.1 exchange on
// a published address.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    create_service, dialled, ecs_ok, elbv2, front_door, get, health, healthy_target,
    register_revision, seed_network, serving_command, CLUSTER,
};
// The metric store has ONE home for every series, so the CloudWatch caller belongs once.
use crate::ecs_service_utilization_metrics::cloudwatch;
// The ONE element readers this test crate keeps for awsQuery suites.
use crate::query_wire::element;

/// The namespace an Application Load Balancer's metrics live in and the two series the recorded
/// scenarios read — spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass.
const NAMESPACE: &str = "AWS/ApplicationELB";
const REQUEST_COUNT: &str = "RequestCount";
const TARGET_RESPONSE_TIME: &str = "TargetResponseTime";

/// The dimension an Elastic Load Balancing series carries for the load balancer itself, the
/// statistics the rows read, and the units the two series are published in.
const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";
const SUM: &str = "Sum";
const SAMPLE_COUNT: &str = "SampleCount";
const MINIMUM: &str = "Minimum";
const MAXIMUM: &str = "Maximum";
const COUNT_UNIT: &str = "Count";
const SECONDS_UNIT: &str = "Seconds";

/// How an Elastic Load Balancing dimension names an Application Load Balancer:
/// `app/<name>/<id>`, as the Application Load Balancer metric reference writes it — which is the
/// resource part of the load balancer's own ARN with the resource type it opens with removed.
const APPLICATION_FAMILY_CODE: &str = "app";
const LOAD_BALANCER_RESOURCE_PREFIX: &str = "loadbalancer";

/// The granularities a read asks for. Source: CloudWatch `GetMetricStatisticsInput$Period` — "for
/// metrics with regular resolution, a period can be as short as one minute (60 seconds) and must
/// be a multiple of 60", which these series are. The catalog spans the shortest such period, the
/// multiples a dashboard rolls up to, and one an hour wide; the recorded scenarios cite only the
/// first.
const PERIODS: &[i64] = &[60, 120, 300, 900, 3600];

/// The granularity a poll looks at while it waits for the publisher to catch up: the shortest one
/// a regular-resolution series may be asked for.
const PUBLISHED_PERIOD: i64 = 60;

/// How far back and forward a read's window reaches, which is wide enough to cover the whole
/// run-up a series opens with and every sample any row drives.
const WINDOW_BEFORE: f64 = 1_800.0;
const WINDOW_AFTER: f64 = 600.0;

/// The names this suite's resources take. The suite drives an engine of its own, and names of its
/// own keep a failure's message unambiguous.
const FAMILY: &str = "periodfamily";
const GROUP_NAME: &str = "period-group";
const LOAD_BALANCER_NAME: &str = "period-lb";
const SERVICE_NAME: &str = "periodsvc";

/// The port the container's server listens on, which is the `containerPort` the revision declares,
/// the port the binding names and the target group's own.
const PORT: i64 = 80;

/// The marker the container's own program writes into the document it serves, so a body anything
/// else produced fails the row.
const MARKER: &str = "ALB-PERIOD-TASK";

/// The document every row asks for, and the status the container's own server answers it with.
const ROOT_PATH: &str = "/";
const SERVED_STATUS: u16 = 200;

/// The task count the service is created with, which is the count its scheduler owes.
const DESIRED_COUNT: i64 = 1;

/// The state a registration whose checks can arrive settles at.
const HEALTHY: &str = "healthy";

/// How long one wait keeps looking before it gives up, and how often it looks again. Each wait
/// gets this budget of its OWN: the deadline is here so a failure is a failure rather than a hang,
/// and nothing below asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// What `MetricDataResult$StatusCode` reports for a result that holds the whole window:
/// "`Complete` indicates that all data points in the requested time range were returned".
const COMPLETE: &str = "Complete";

/// How a client ADDRESSES the load balancer, which is one dimension the rows differ along.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// A real connection to the authority the control plane published, which is what the client
    /// that was handed that address opens.
    Dialled,
    /// That same published authority sent as the request's `Host`.
    HostStyle,
}

/// Which of CloudWatch's two readers of the one metric store asks, the other dimension the rows
/// differ along.
#[derive(Debug, Clone, Copy)]
enum Reader {
    /// `GetMetricStatistics`, which answers a datapoint per period slot with every statistic the
    /// call asked for.
    Statistics,
    /// `GetMetricData`, which answers one value per period slot for the one `Stat` of its query —
    /// the reader a dashboard graphs a series with.
    Data,
}

const READERS: &[Reader] = &[Reader::Statistics, Reader::Data];

/// One batch of client traffic: how it addresses the load balancer and how many requests it drives.
#[derive(Debug)]
struct Batch {
    case: &'static str,
    addressing: Addressing,
    requests: usize,
}

const BATCHES: &[Batch] = &[
    Batch {
        case: "one request dialled on the published address",
        addressing: Addressing::Dialled,
        requests: 1,
    },
    Batch {
        case: "four further requests dialled on the published address",
        addressing: Addressing::Dialled,
        requests: 4,
    },
    Batch {
        case: "fifteen further requests sending the published authority as the Host",
        addressing: Addressing::HostStyle,
        requests: 15,
    },
];

/// Epoch seconds, for the window a read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint `GetMetricStatistics` holds for `metric` on exactly `dimensions`, grouped into
/// `period` slots.
async fn statistics_datapoints(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &Value,
    period: i64,
) -> Vec<Value> {
    let now = epoch_seconds();
    let answered = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_BEFORE,
            "EndTime": now + WINDOW_AFTER,
            "Period": period,
            "Statistics": [SUM, SAMPLE_COUNT, MINIMUM, MAXIMUM],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What `GetMetricData` answers for the one `stat` of `metric` on exactly `dimensions`, grouped
/// into `period` slots: the values it reports, once the result is asserted to hold the whole
/// window.
async fn graphed_values(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &Value,
    period: i64,
    stat: &str,
) -> Vec<f64> {
    let now = epoch_seconds();
    let answered = cloudwatch(
        router,
        "GetMetricData",
        json!({
            "MetricDataQueries": [{
                "Id": "series",
                "MetricStat": {
                    "Metric": {
                        "Namespace": NAMESPACE,
                        "MetricName": metric,
                        "Dimensions": dimensions,
                    },
                    "Period": period,
                    "Stat": stat,
                },
                "ReturnData": true,
            }],
            "StartTime": now - WINDOW_BEFORE,
            "EndTime": now + WINDOW_AFTER,
        }),
    )
    .await;
    let results = answered["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        1,
        "case {case:?}: one query must answer one result for {metric:?} {stat:?} at {period}s, \
         got {answered}"
    );
    let result = &results[0];
    assert_eq!(
        result["StatusCode"].as_str().unwrap_or_default(),
        COMPLETE,
        "case {case:?}: the result for {metric:?} {stat:?} at {period}s must hold the whole \
         window, got {result}"
    );
    result["Values"]
        .as_array()
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

/// The total `stat` of `metric` on `dimensions` as `reader` reports it at `period`: the sum over
/// the slots of the window, which for a count and for a number of observations is the figure the
/// whole window holds.
async fn reported_total(
    router: &MultiServiceRouter,
    case: &str,
    reader: Reader,
    metric: &str,
    dimensions: &Value,
    period: i64,
    stat: &str,
) -> f64 {
    match reader {
        Reader::Statistics => statistics_datapoints(router, metric, dimensions, period)
            .await
            .iter()
            .filter_map(|point| point[stat].as_f64())
            .sum(),
        Reader::Data => graphed_values(router, case, metric, dimensions, period, stat)
            .await
            .iter()
            .sum(),
    }
}

/// Wait until the publisher has caught up: the `stat` of `metric` read at the published
/// granularity reaches `expected`. Polls the CONDITION rather than waiting a fixed time.
async fn caught_up(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &Value,
    stat: &str,
    expected: f64,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let total = reported_total(
            router,
            case,
            Reader::Statistics,
            metric,
            dimensions,
            PUBLISHED_PERIOD,
            stat,
        )
        .await;
        if total >= expected {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "case {case:?}: expected the {metric:?} {stat:?} of the window to reach \
                 {expected}, got {total}"
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Assert that every reader, at every granularity of [`PERIODS`], reports exactly `expected` for
/// the `stat` of `metric` on `dimensions`.
async fn same_total_everywhere(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    dimensions: &Value,
    stat: &str,
    expected: f64,
) {
    caught_up(router, case, metric, dimensions, stat, expected).await;
    for &period in PERIODS {
        for &reader in READERS {
            let total =
                reported_total(router, case, reader, metric, dimensions, period, stat).await;
            assert!(
                (total - expected).abs() < 1e-9,
                "case {case:?}: expected {reader:?} at {period}s to report a {metric:?} {stat:?} \
                 of {expected} over the covering window, got {total}"
            );
        }
    }
}

/// Every `Unit` the datapoints of a series report, as `GetMetricStatistics` carries it.
async fn reported_units(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &Value,
) -> BTreeSet<String> {
    statistics_datapoints(router, metric, dimensions, PUBLISHED_PERIOD)
        .await
        .iter()
        .map(|point| point["Unit"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// How an Elastic Load Balancing dimension names the load balancer an ARN identifies: that ARN's
/// resource part from the family code onwards — `app/<name>/<id>`, as the Application Load
/// Balancer metric reference writes it, where the ARN itself writes
/// `loadbalancer/app/<name>/<id>`.
fn load_balancer_dimension_of(arn: &str) -> String {
    let resource = arn.splitn(6, ':').nth(5).unwrap_or_default();
    resource
        .strip_prefix(&format!("{LOAD_BALANCER_RESOURCE_PREFIX}/"))
        .unwrap_or(resource)
        .to_string()
}

/// Wait until the service's scheduler has placed what it owes: `runningCount` reaches
/// `desiredCount`, which is the count the creation asked for. Polls the CONDITION — the task's
/// start is clock-owed — rather than waiting a fixed time.
async fn running_count_reaches_desired(router: &MultiServiceRouter, case: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs_ok(
            router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [SERVICE_NAME] }),
        )
        .await;
        let service = &described["services"][0];
        let running = service["runningCount"].as_i64();
        let desired = service["desiredCount"].as_i64();
        if running == Some(DESIRED_COUNT) && desired == Some(DESIRED_COUNT) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "case {case:?}: expected runningCount and desiredCount of {DESIRED_COUNT}, got \
                 running {running:?} and desired {desired:?}; the service reports {described}"
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// RULE: the topology settles on its own, and the traffic driven at the published address is
/// reported with the same totals by every reader at every granularity that covers it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metered_request_series_reads_the_same_at_every_covering_period() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let program = serving_command(MARKER);
    let command: Vec<&str> = program.iter().map(String::as_str).collect();
    let revision = register_revision(&router, FAMILY, PORT, &command).await;
    let setup = "an ALB forwarding to the ip target of one awsvpc Fargate ECS task";
    let (group, dns_name) = front_door(
        &router,
        &network,
        setup,
        GROUP_NAME,
        LOAD_BALANCER_NAME,
        PORT,
    )
    .await;
    create_service(&router, &network, SERVICE_NAME, &revision, &group, PORT).await;

    // The convergence the recorded scenarios poll for, in their own order: the scheduler's count
    // first, then the registration the start owes and the health its checks determine.
    running_count_reaches_desired(&router, setup).await;
    let registered = healthy_target(&router, setup, &group, SERVICE_NAME).await;
    assert!(
        !registered.is_empty(),
        "case {setup:?}: a settled registration names the address it is registered at"
    );
    let reported = health(&router, &group).await;
    assert_eq!(
        reported,
        vec![(registered.clone(), HEALTHY.to_string())],
        "case {setup:?}: the one registration must be reported {HEALTHY}"
    );

    // The load balancer's own dimension is read off the ARN its control plane minted, which is the
    // one value no assertion below states by hand.
    let described = elbv2(
        &router,
        "DescribeLoadBalancers",
        &[("Names.member.1", LOAD_BALANCER_NAME)],
    )
    .await;
    let load_balancer_dimension =
        load_balancer_dimension_of(&element(&described, "LoadBalancerArn"));
    let segments: Vec<&str> = load_balancer_dimension.split('/').collect();
    assert!(
        segments.len() == 3
            && segments[0] == APPLICATION_FAMILY_CODE
            && segments[1] == LOAD_BALANCER_NAME
            && !segments[2].is_empty(),
        "case {setup:?}: expected the load balancer's dimension to be \
         {APPLICATION_FAMILY_CODE}/{LOAD_BALANCER_NAME}/<id>, got {load_balancer_dimension:?}"
    );
    let own = json!([
        { "Name": LOAD_BALANCER_DIMENSION, "Value": load_balancer_dimension },
    ]);

    let mut answered_requests = 0.0_f64;
    for batch in BATCHES {
        for _ in 0..batch.requests {
            let answered = match batch.addressing {
                Addressing::Dialled => dialled(batch.case, &dns_name, ROOT_PATH).await,
                Addressing::HostStyle => get(&router, &dns_name, ROOT_PATH).await,
            };
            assert_eq!(
                answered.status, SERVED_STATUS,
                "case {:?}: expected the container's own HTTP {SERVED_STATUS}, got {} with body \
                 {:?}",
                batch.case, answered.status, answered.body
            );
            assert!(
                answered.body.contains(MARKER),
                "case {:?}: only the document the container's program wrote carries {MARKER:?}, \
                 got {:?}",
                batch.case,
                answered.body
            );
        }
        answered_requests += batch.requests as f64;

        // Every request the load balancer answered is one request processed, and one response a
        // target was measured for — the same two totals however the window is divided and
        // whichever reader divides it.
        same_total_everywhere(
            &router,
            batch.case,
            REQUEST_COUNT,
            &own,
            SUM,
            answered_requests,
        )
        .await;
        same_total_everywhere(
            &router,
            batch.case,
            TARGET_RESPONSE_TIME,
            &own,
            SAMPLE_COUNT,
            answered_requests,
        )
        .await;

        // The units each series is published in, which no granularity changes either.
        assert_eq!(
            reported_units(&router, REQUEST_COUNT, &own).await,
            BTreeSet::from([COUNT_UNIT.to_string()]),
            "case {:?}: a number of requests is published in {COUNT_UNIT}",
            batch.case
        );
        assert_eq!(
            reported_units(&router, TARGET_RESPONSE_TIME, &own).await,
            BTreeSet::from([SECONDS_UNIT.to_string()]),
            "case {:?}: an elapsed time is published in {SECONDS_UNIT}",
            batch.case
        );

        // A latency series is a MEASUREMENT: every slot the window holds reports a positive
        // figure, at every granularity, so a sample count standing for nothing measured fails.
        for &period in PERIODS {
            let slots = graphed_values(
                &router,
                batch.case,
                TARGET_RESPONSE_TIME,
                &own,
                period,
                MINIMUM,
            )
            .await;
            assert!(
                !slots.is_empty() && slots.iter().all(|value| *value > 0.0),
                "case {:?}: expected every {TARGET_RESPONSE_TIME} slot at {period}s to report a \
                 positive elapsed time, got {slots:?}",
                batch.case
            );
        }
    }
}
