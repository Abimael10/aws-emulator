// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/ECS` utilization series a SERVICE publishes MEASURES: the containers of its
//! running tasks.
//!
//! RULE: the two series a running Amazon ECS service publishes on its `ClusterName` +
//! `ServiceName` pair are measurements of that service's own containers, so each datapoint
//! reports what those containers did over the interval it closes rather than a fixed number —
//! a service whose container's program keeps WORKING reports `CPUUtilization` above zero while
//! one whose program only WAITS does not, and a service whose containers are OCCUPYING memory
//! reports `MemoryUtilization` above zero. Both are shares of what the containers were given,
//! so neither is ever negative or past 100.
//!
//! The rows differ along what the rule generalises over: WHAT the container's program does —
//! one that spins its CPU without ever waiting (the recorded shape, a `while` loop over an
//! arithmetic assignment), one that does nothing but wait between sleeps, and one that puts
//! bytes on its filesystem and then waits. Every other input is held equal: one Fargate
//! service of `desiredCount` 1 per row, each in a cluster of its own with Container Insights
//! off, so what a row reads is the series of its own service.
//!
//! Two crates have to agree for that (Amazon ECS performs the containers and meters the
//! service, CloudWatch stores the series and answers for it) and the `awsvpc` placements
//! resolve a subnet in a third (Amazon EC2 holds the network), so the suite lives here.
//! Nothing reaches into a store: the services are created through the Amazon ECS front door
//! and every datapoint is read back through CloudWatch's own `GetMetricStatistics`.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The namespace, metric names and dimension names this suite reads — spelled out here rather
/// than imported, so a wrong constant in the implementation cannot make a test pass.
const NAMESPACE: &str = "AWS/ECS";
const CPU_UTILIZATION: &str = "CPUUtilization";
const MEMORY_UTILIZATION: &str = "MemoryUtilization";
const CLUSTER_DIMENSION: &str = "ClusterName";
const SERVICE_DIMENSION: &str = "ServiceName";

/// The `StandardUnit` a percentage is published in.
const PERCENT: &str = "Percent";

/// The network the `awsvpc` placements name.
const VPC: &str = "vpc-0123456789abcdef0";
const SUBNET: &str = "subnet-0123456789abcdef0";

/// The image every container names.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// How often a poll re-reads what it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one wait keeps trying before it gives up. Each wait gets this budget of its OWN,
/// never one shared between two: Amazon ECS creates and attaches the interface an `awsvpc`
/// task is allocated one documented step at a time and only then starts it, so a correct run
/// is far inside this and a broken one is reported promptly. Nothing here asserts on elapsed
/// time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How many datapoints of one series a row reads before it judges the series: enough that the
/// interval the first of them closes (none — it is published the instant the service becomes
/// metered) is not the only one read.
const DATAPOINTS_READ: usize = 3;

/// The share of an interval a container that spends it WAITING may read as, at most.
///
/// Generous on purpose: a program sleeping between two commands holds its performer for
/// microseconds of each interval, so a correct reading is a tiny fraction of a per cent and
/// anything near this ceiling would mean the waiting was counted as work. It is not a figure
/// the implementation holds — it is the loosest bound that still tells working from waiting.
const WAITING_CEILING: f64 = 50.0;

/// What a row's container PROGRAM is, and what the series of it must therefore report.
struct Row {
    case: &'static str,
    cluster: &'static str,
    service: &'static str,
    /// The argument vector the container runs.
    command: &'static [&'static str],
    /// Whether this program keeps WORKING — so some datapoint of its service's CPU series
    /// reports a share above zero — or only waits, so every one of them stays under
    /// [`WAITING_CEILING`].
    works: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: a program that spins its CPU",
        cluster: "measured-spinning",
        service: "spinner",
        // A loop that never waits: every iteration performs an arithmetic assignment, so the
        // container's program holds its performer for as long as it is being performed.
        command: &["sh", "-c", "i=0; while true; do i=$((i+1)); done"],
        works: true,
    },
    Row {
        case: "a program that does nothing but wait",
        cluster: "measured-waiting",
        service: "waiter",
        command: &["sh", "-c", "while true; do sleep 1; done"],
        works: false,
    },
    Row {
        case: "a program that holds bytes on its filesystem and then waits",
        cluster: "measured-holding",
        service: "holder",
        command: &[
            "sh",
            "-c",
            "echo holding-a-kibibyte-of-bytes-in-this-containers-own-writable-layer > /held; \
             while true; do sleep 1; done",
        ],
        works: false,
    },
];

/// One emulator carrying Amazon ECS and CloudWatch — which owns the metric store the series is
/// published into — over ONE shared engine, exactly as the production router builds them, with
/// the network the `awsvpc` placements name held in the account's Amazon EC2 store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::ec2_network::register_network(&engine, VPC, &[SUBNET], &format!("{REGION}a"));
    // A task placed in this subnet reaches the registry its containers' image comes from, so
    // no task of these services is stopped before it starts.
    crate::ec2_network::register_internet_route(&engine, VPC);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One awsJson call that must succeed, answered with the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    host: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}\
                 /{host}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call, the way a client makes it.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One CloudWatch call, the way a client makes it. CloudWatch's `sigv4Name` is `monitoring`.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "monitoring",
        "GraniteServiceVersion20100801",
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds of `metric` for one row's service.
async fn datapoints(router: &MultiServiceRouter, row: &Row, metric: &str) -> Vec<Value> {
    let now = epoch_seconds();
    let stats = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [
                { "Name": CLUSTER_DIMENSION, "Value": row.cluster },
                { "Name": SERVICE_DIMENSION, "Value": row.service },
            ],
            // Wide enough to hold the whole run-up a series opens with.
            "StartTime": now - 1_800.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Minimum", "Maximum", "Average", "SampleCount"],
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// Create one row's cluster, the revision its service runs and the service itself.
///
/// The cluster pins `containerInsights` to `disabled`, so what is read here is the BASE
/// `AWS/ECS` namespace and not the Container Insights one.
async fn create_service(router: &MultiServiceRouter, row: &Row) {
    ecs(
        router,
        "CreateCluster",
        json!({
            "clusterName": row.cluster,
            "settings": [{ "name": "containerInsights", "value": "disabled" }],
        }),
    )
    .await;
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.service,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
                "command": row.command,
            }],
        }),
    )
    .await;
    ecs(
        router,
        "CreateService",
        json!({
            "cluster": row.cluster,
            "serviceName": row.service,
            "taskDefinition": row.service,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [SUBNET],
                    "securityGroups": [],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
}

/// Wait until `DescribeServices` reports the row's service holding its task `RUNNING` — the
/// state its containers are measured in.
async fn converged(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            "DescribeServices",
            json!({ "cluster": row.cluster, "services": [row.service] }),
        )
        .await;
        let service = described["services"][0].clone();
        if service["runningCount"].as_i64().unwrap_or_default() >= 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a running task, got {service}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the row's service has published `DATAPOINTS_READ` datapoints of `metric`, and
/// answer every datapoint the series then holds.
async fn published(
    router: &MultiServiceRouter,
    row: &Row,
    metric: &str,
    wanted: usize,
) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, row, metric).await;
        if points.len() >= wanted {
            return points;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {wanted} {metric} datapoints, got {}: {points:?}",
            points.len()
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Average` of one datapoint, which is the share the collection measured.
fn average(case: &str, metric: &str, point: &Value) -> f64 {
    point["Average"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Average for {metric}, got {point}"))
}

/// The highest `Average` among `points`.
fn highest(case: &str, metric: &str, points: &[Value]) -> f64 {
    points
        .iter()
        .map(|point| average(case, metric, point))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// Every datapoint of a utilization series is a share of what the containers were given, in
/// `Percent`: never negative, never past 100, and never reporting a `Minimum` above its
/// `Maximum`.
fn assert_is_a_share(case: &str, metric: &str, points: &[Value]) {
    for point in points {
        assert_eq!(
            point["Unit"].as_str(),
            Some(PERCENT),
            "case {case:?}: expected {metric} in {PERCENT}, got {point}"
        );
        let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: expected a Minimum for {metric}, got {point}")
        });
        let maximum = point["Maximum"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: expected a Maximum for {metric}, got {point}")
        });
        assert!(
            (0.0..=100.0).contains(&minimum) && (0.0..=100.0).contains(&maximum),
            "case {case:?}: expected {metric} within 0..100, got {point}"
        );
        assert!(
            minimum <= maximum,
            "case {case:?}: expected {metric} Minimum at most Maximum, got {point}"
        );
    }
}

#[tokio::test]
async fn the_cpu_series_reports_what_each_services_container_program_actually_did() {
    let router = emulator();
    // Every row's service is created up front, so the clock-owed placements of all of them
    // fall due at once rather than one row's wait after another's — and each row's program is
    // therefore performed while that row's own series is opening.
    for row in ROWS {
        create_service(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        converged(&router, row).await;
        let points = published(&router, row, CPU_UTILIZATION, DATAPOINTS_READ).await;
        assert_is_a_share(case, CPU_UTILIZATION, &points);
        let highest = highest(case, CPU_UTILIZATION, &points);
        if row.works {
            assert!(
                highest > 0.0,
                "case {case:?}: expected a {CPU_UTILIZATION} datapoint above zero for a \
                 program that never stops working, got {highest} among {points:?}"
            );
        } else {
            assert!(
                highest <= WAITING_CEILING,
                "case {case:?}: expected every {CPU_UTILIZATION} datapoint under \
                 {WAITING_CEILING} for a program that only waits, got {highest} among \
                 {points:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_memory_series_reports_the_bytes_each_services_containers_occupy() {
    let router = emulator();
    for row in ROWS {
        create_service(&router, row).await;
    }

    for row in ROWS {
        let case = row.case;
        converged(&router, row).await;
        let points = published(&router, row, MEMORY_UTILIZATION, DATAPOINTS_READ).await;
        assert_is_a_share(case, MEMORY_UTILIZATION, &points);
        // Every one of these containers is holding something while its program runs — the
        // environment the runtime gave it, the variables the program set, the file it wrote —
        // so the share of the memory its task definition reserves is above zero.
        let highest = highest(case, MEMORY_UTILIZATION, &points);
        assert!(
            highest > 0.0,
            "case {case:?}: expected a {MEMORY_UTILIZATION} datapoint above zero for a \
             container that is occupying memory, got {highest} among {points:?}"
        );
    }
}
